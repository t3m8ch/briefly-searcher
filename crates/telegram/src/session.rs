//! Сессия `grammers` в таблице `telegram_session`, а не файлом на диске (ADR-0002).
//!
//! [`DbSession`] держит состояние сессии в памяти и после каждого изменения,
//! которое делает клиент, записывает его в БД целиком. Сессия — секрет: её
//! содержимое не выводится в логи.

use std::collections::hash_map::Entry;
use std::sync::{Mutex, MutexGuard};

use briefly_searcher_storage::Storage;
use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use grammers_session::{BoxFuture, Session, SessionData};
use serde::{Deserialize, Serialize};

/// Ошибка хранилища сессии.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(
        "в БД нет сессии Telegram: сначала войдите в аккаунт командой `briefly-searcher login`"
    )]
    NotLoggedIn,
    #[error(transparent)]
    Storage(#[from] briefly_searcher_storage::Error),
    #[error("сессия Telegram в БД повреждена: {0}")]
    Corrupted(#[source] serde_json::Error),
    #[error("состояние сессии Telegram недоступно: поток, менявший его, упал")]
    Poisoned,
}

/// Сессия `grammers`, сохранённая в PostgreSQL.
pub struct DbSession {
    storage: Storage,
    data: Mutex<SessionData>,
    /// Записывать ли изменения в БД сразу. Асинхронная блокировка ещё и
    /// выстраивает записи в очередь, чтобы старое состояние не затёрло новое.
    write_through: tokio::sync::Mutex<bool>,
}

impl DbSession {
    /// Читает сессию, сохранённую командой `login`; изменения, которые затем
    /// делает клиент, сразу записываются в БД.
    pub async fn load(storage: Storage) -> Result<Self, SessionError> {
        let Some(bytes) = storage.telegram_session().await? else {
            return Err(SessionError::NotLoggedIn);
        };
        let stored: StoredSession =
            serde_json::from_slice(&bytes).map_err(SessionError::Corrupted)?;
        Ok(Self::new(storage, stored.into(), true))
    }

    /// Новая сессия для входа в аккаунт. Она остаётся только в памяти, пока
    /// вход не завершён и не вызван [`DbSession::save`]: незавершённый вход не
    /// оставляет в БД сессию без пользователя.
    pub fn fresh(storage: Storage) -> Self {
        Self::new(storage, SessionData::default(), false)
    }

    fn new(storage: Storage, data: SessionData, write_through: bool) -> Self {
        Self {
            storage,
            data: Mutex::new(data),
            write_through: tokio::sync::Mutex::new(write_through),
        }
    }

    /// Записывает сессию в БД целиком, заменяя прежнюю; дальнейшие изменения
    /// записываются сразу.
    pub async fn save(&self) -> Result<(), SessionError> {
        let mut write_through = self.write_through.lock().await;
        let bytes = serialize(&*self.data()?);
        self.storage.save_telegram_session(&bytes).await?;
        *write_through = true;
        Ok(())
    }

    fn data(&self) -> Result<MutexGuard<'_, SessionData>, SessionError> {
        self.data.lock().map_err(|_| SessionError::Poisoned)
    }

    /// Применяет изменение и, если оно что-то поменяло, записывает сессию.
    /// `change` возвращает, изменилось ли состояние.
    async fn update(
        &self,
        change: impl FnOnce(&mut SessionData) -> bool,
    ) -> Result<(), SessionError> {
        let write_through = self.write_through.lock().await;
        let bytes = {
            let mut data = self.data()?;
            if !change(&mut data) || !*write_through {
                return Ok(());
            }
            serialize(&data)
        };
        self.storage.save_telegram_session(&bytes).await?;
        Ok(())
    }
}

impl Session for DbSession {
    type Error = SessionError;

    fn home_dc_id(&self) -> Result<i32, SessionError> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), SessionError>> {
        Box::pin(self.update(move |data| std::mem::replace(&mut data.home_dc, dc_id) != dc_id))
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, SessionError> {
        Ok(self.data()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), SessionError>> {
        let dc_option = dc_option.clone();
        Box::pin(self.update(move |data| {
            data.dc_options.insert(dc_option.id, dc_option.clone()) != Some(dc_option)
        }))
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, SessionError>> {
        Box::pin(async move {
            let data = self.data()?;
            let info = if peer == PeerId::self_user() {
                data.peer_infos.values().find(|info| is_self(info)).cloned()
            } else {
                data.peer_infos.get(&peer).cloned()
            };
            Ok(info)
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), SessionError>> {
        let peer = peer.clone();
        Box::pin(
            self.update(move |data| match data.peer_infos.entry(peer.id()) {
                Entry::Vacant(entry) => {
                    entry.insert(peer);
                    true
                }
                Entry::Occupied(mut entry) => {
                    let before = entry.get().clone();
                    entry.get_mut().extend_info(&peer);
                    *entry.get() != before
                }
            }),
        )
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, SessionError>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), SessionError>> {
        Box::pin(self.update(move |data| {
            let state = &mut data.updates_state;
            let before = state.clone();
            match update {
                UpdateState::All(updates_state) => *state = updates_state,
                UpdateState::Primary { pts, date, seq } => {
                    state.pts = pts;
                    state.date = date;
                    state.seq = seq;
                }
                UpdateState::Secondary { qts } => state.qts = qts,
                UpdateState::Channel { id, pts } => {
                    match state.channels.iter_mut().find(|c| c.id == id) {
                        Some(channel) => channel.pts = pts,
                        None => state.channels.push(ChannelState { id, pts }),
                    }
                }
            }
            *state != before
        }))
    }
}

fn is_self(info: &PeerInfo) -> bool {
    matches!(
        info,
        PeerInfo::User {
            is_self: Some(true),
            ..
        }
    )
}

/// Форма сессии в столбце `telegram_session.session`: JSON.
#[derive(Serialize, Deserialize)]
struct StoredSession {
    home_dc: i32,
    dc_options: Vec<DcOption>,
    peer_infos: Vec<PeerInfo>,
    updates_state: UpdatesState,
}

impl From<StoredSession> for SessionData {
    fn from(stored: StoredSession) -> Self {
        Self {
            home_dc: stored.home_dc,
            dc_options: stored.dc_options.into_iter().map(|o| (o.id, o)).collect(),
            peer_infos: stored.peer_infos.into_iter().map(|p| (p.id(), p)).collect(),
            updates_state: stored.updates_state,
        }
    }
}

fn serialize(data: &SessionData) -> Vec<u8> {
    let stored = StoredSession {
        home_dc: data.home_dc,
        dc_options: data.dc_options.values().cloned().collect(),
        peer_infos: data.peer_infos.values().cloned().collect(),
        updates_state: data.updates_state.clone(),
    };
    serde_json::to_vec(&stored).expect("сессия всегда сериализуется в JSON")
}
