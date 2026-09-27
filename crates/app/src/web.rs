//! Веб-админка только для чтения: `GET /` — страница, `GET /status` — фрагмент
//! сводки, который страница через HTMX запрашивает раз в несколько секунд.

mod view;

use askama::Template;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use briefly_searcher_storage::Storage;

use std::net::IpAddr;
use std::time::Duration;

use tokio::net::TcpListener;
use tower_http::timeout::TimeoutLayer;

use self::view::StatusView;

/// Сколько ждать запрос, прежде чем ответить 503. Заодно ограничивает, как
/// долго остановка сервера ждёт незавершённые запросы.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Обслуживает админку на `listener`, пока не завершится `shutdown`; затем
/// дорабатывает текущие запросы и возвращается.
pub async fn serve(
    listener: TcpListener,
    storage: Storage,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = router(storage).layer(TimeoutLayer::with_status_code(
        StatusCode::SERVICE_UNAVAILABLE,
        REQUEST_TIMEOUT,
    ));
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

/// Роутер админки над хранилищем.
pub fn router(storage: Storage) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/status", get(status))
        .route("/htmx.min.js", get(htmx))
        .layer(middleware::from_fn(require_loopback_host))
        .with_state(storage)
}

/// htmx 2.0.11, вшитый в бинарник: релиз — это бинарник и окружение, без CDN.
const HTMX: &str = include_str!("../assets/htmx.min.js");

/// Страница целиком.
#[derive(Template)]
#[template(path = "admin.html")]
struct Page {
    s: StatusView,
}

/// Фрагмент сводки: блок `status` того же шаблона, что и страница.
#[derive(Template)]
#[template(path = "admin.html", block = "status")]
struct StatusFragment {
    s: StatusView,
}

async fn page(State(storage): State<Storage>) -> Response {
    match storage.admin_summary().await {
        Ok(summary) => render(Page {
            s: StatusView::new(&summary),
        }),
        Err(error) => unavailable(error),
    }
}

async fn status(State(storage): State<Storage>) -> Response {
    match storage.admin_summary().await {
        Ok(summary) => render(StatusFragment {
            s: StatusView::new(&summary),
        }),
        Err(error) => unavailable(error),
    }
}

async fn htmx() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        HTMX,
    )
}

/// Пропускает только запросы к `localhost` и loopback-адресам — защита от DNS
/// rebinding: чужой сайт, чьё имя указывает на 127.0.0.1, получит 403.
async fn require_loopback_host(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());
    if host.is_some_and(is_loopback_host) {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, "недопустимый Host").into_response()
    }
}

/// `localhost` или loopback-адрес, с портом или без: `127.0.0.1:3000`, `[::1]:3000`.
fn is_loopback_host(host: &str) -> bool {
    let (name, port) = match host.strip_prefix('[') {
        // IPv6 в квадратных скобках.
        Some(rest) => match rest.split_once(']') {
            Some((address, "")) => (address, None),
            Some((address, port)) => match port.strip_prefix(':') {
                Some(port) => (address, Some(port)),
                None => return false,
            },
            None => return false,
        },
        None => match host.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        },
    };
    port.is_none_or(|port| port.parse::<u16>().is_ok())
        && (name.eq_ignore_ascii_case("localhost")
            || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

fn render(template: impl Template) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(error) => unavailable(error),
    }
}

/// Ответ 5xx: HTMX 2 его не вставляет, и на экране остаются последние данные.
fn unavailable(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "сводка для админки недоступна");
    (StatusCode::INTERNAL_SERVER_ERROR, "сводка недоступна").into_response()
}
