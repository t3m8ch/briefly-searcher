//! Хранилище сессии Telegram поверх настоящей PostgreSQL: всё, что клиент
//! `grammers` записывает через trait `Session`, переживает перезапуск процесса.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

use briefly_searcher_storage::Storage;
use briefly_searcher_telegram::session::{DbSession, SessionError};
use grammers_session::Session;
use grammers_session::types::{
    ChannelKind, ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use sqlx::PgPool;

fn dc_option(id: i32, auth_key: u8) -> DcOption {
    DcOption {
        id,
        ipv4: SocketAddrV4::new(Ipv4Addr::new(149, 154, 167, 91), 443),
        ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0),
        auth_key: Some([auth_key; 256]),
    }
}

fn self_user() -> PeerInfo {
    PeerInfo::User {
        id: 12345,
        auth: Some(PeerAuth::from_hash(777)),
        bot: Some(false),
        is_self: Some(true),
    }
}

fn channel(id: i64) -> PeerInfo {
    PeerInfo::Channel {
        id,
        auth: Some(PeerAuth::from_hash(id * 10)),
        kind: Some(ChannelKind::Broadcast),
    }
}

/// Сессия, которую `login` сохраняет после успешного входа.
async fn logged_in(storage: &Storage) -> DbSession {
    let session = DbSession::fresh(storage.clone());
    session.set_home_dc_id(4).await.unwrap();
    session.set_dc_option(&dc_option(4, 1)).await.unwrap();
    session.cache_peer(&self_user()).await.unwrap();
    session.save().await.unwrap();
    session
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn loading_session_from_database_without_login_asks_to_run_login(pool: PgPool) {
    let error = DbSession::load(Storage::from_pool(pool))
        .await
        .err()
        .unwrap();

    assert!(matches!(error, SessionError::NotLoggedIn), "{error:?}");
    assert!(
        error.to_string().contains("briefly-searcher login"),
        "{error}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn session_saved_after_login_is_loaded_by_next_process(pool: PgPool) {
    let storage = Storage::from_pool(pool);
    let session = DbSession::fresh(storage.clone());
    session.set_home_dc_id(4).await.unwrap();
    session.set_dc_option(&dc_option(4, 1)).await.unwrap();
    session.cache_peer(&self_user()).await.unwrap();
    let updates = UpdatesState {
        pts: 10,
        qts: 20,
        date: 30,
        seq: 40,
        channels: vec![ChannelState { id: 5, pts: 50 }],
    };
    session
        .set_update_state(UpdateState::All(updates.clone()))
        .await
        .unwrap();

    // Пока вход не завершён, в БД ничего не пишется.
    let before_save = DbSession::load(storage.clone()).await.err().unwrap();
    assert!(matches!(before_save, SessionError::NotLoggedIn));

    session.save().await.unwrap();
    let loaded = DbSession::load(storage).await.unwrap();

    assert_eq!(loaded.home_dc_id().unwrap(), 4);
    assert_eq!(loaded.dc_option(4).unwrap(), Some(dc_option(4, 1)));
    assert_eq!(
        loaded.peer(PeerId::self_user()).await.unwrap(),
        Some(self_user())
    );
    assert_eq!(
        loaded.peer(PeerId::user(12345).unwrap()).await.unwrap(),
        Some(self_user())
    );
    assert_eq!(loaded.updates_state().await.unwrap(), updates);
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn changes_made_by_client_are_saved_to_database(pool: PgPool) {
    let storage = Storage::from_pool(pool);
    logged_in(&storage).await;
    let session = DbSession::load(storage.clone()).await.unwrap();

    // Клиент переехал в другой ДЦ, получил ключ, разрешил канал и
    // продвинул состояние обновлений.
    session.set_home_dc_id(2).await.unwrap();
    session.set_dc_option(&dc_option(2, 9)).await.unwrap();
    session.cache_peer(&channel(1_000_001)).await.unwrap();
    session
        .set_update_state(UpdateState::Channel {
            id: 1_000_001,
            pts: 77,
        })
        .await
        .unwrap();

    // Процесс перезапущен: новая сессия видит всё сделанное.
    let restarted = DbSession::load(storage).await.unwrap();
    assert_eq!(restarted.home_dc_id().unwrap(), 2);
    assert_eq!(restarted.dc_option(2).unwrap(), Some(dc_option(2, 9)));
    assert_eq!(restarted.dc_option(4).unwrap(), Some(dc_option(4, 1)));
    assert_eq!(
        restarted
            .peer(PeerId::channel(1_000_001).unwrap())
            .await
            .unwrap(),
        Some(channel(1_000_001))
    );
    assert_eq!(
        restarted.peer(PeerId::self_user()).await.unwrap(),
        Some(self_user())
    );
    assert_eq!(
        restarted.updates_state().await.unwrap().channels,
        [ChannelState {
            id: 1_000_001,
            pts: 77
        }]
    );
}
