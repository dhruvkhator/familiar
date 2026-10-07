//! familiar-server: HTTP API (axum) over the shared Postgres. Every query is scoped to the session's owner.
//! Runs standalone (`main.rs`, e.g. on Render) or embedded in the desktop app.

mod auth;
mod error;
mod routes;

pub use auth::mint_owner_session;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, Method, Request, header},
    routing::{delete, get, patch, post},
};
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::HashMap, net::SocketAddr, sync::Arc, sync::Mutex, time::Duration, time::Instant,
};
use tokio::sync::broadcast;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use routes::{
    approvals, artifacts, bots, channels, connectors, desktop, folders, live, memories, models, overview, rules, runs,
    schedules, skills, stream, templates, threads, triggers,
};

pub struct AppState {
    pub pool: sqlx::PgPool,
    pub events: broadcast::Sender<stream::Push>,
    /// Failed login attempts per key (email, plus "*" globally): (count, last failure).
    pub fails: Mutex<HashMap<String, (u32, Instant)>>,
    /// Webhook hits per trigger within the last minute.
    pub hooks: Mutex<HashMap<uuid::Uuid, std::collections::VecDeque<Instant>>>,
    pub secret: Option<familiar_crypto::SecretBox>,
    pub s3: Option<routes::artifacts::S3>,
    pub http: reqwest::Client,
    pub public_url: Option<String>,
    /// The teammates' workspaces, when the API runs on the PC that has them (see [`Config::bots_dir`]).
    pub bots_dir: Option<std::path::PathBuf>,
}
pub type S = Arc<AppState>;

fn cors(web_origins: &[String]) -> CorsLayer {
    let mut origins: Vec<String> = if web_origins.is_empty() {
        vec!["http://localhost:47173".into()]
    } else {
        web_origins
            .iter()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .collect()
    };
    origins.extend(["tauri://localhost".into(), "http://tauri.localhost".into()]);
    let origins: Vec<HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(Duration::from_secs(600))
}

fn router(state: S, web_origins: &[String]) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/auth/state", get(auth::state))
        .route("/api/auth/setup", post(auth::setup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/account", patch(auth::update_account))
        .route("/api/me", get(auth::me))
        .route("/api/overview", get(overview::get))
        .route("/api/models", get(models::get))
        .route("/api/bots", get(bots::list).post(bots::create))
        .route(
            "/api/bots/{id}",
            get(bots::get).patch(bots::update).delete(bots::remove),
        )
        .route(
            "/api/bots/{id}/threads",
            get(threads::list).post(threads::create),
        )
        .route(
            "/api/threads/{id}",
            patch(threads::rename).delete(threads::remove),
        )
        .route(
            "/api/threads/{id}/messages",
            get(threads::messages).post(threads::post_message),
        )
        .route("/api/threads/{id}/runs", get(runs::for_thread))
        .route("/api/bots/{id}/runs", get(runs::for_bot))
        .route("/api/runs/{id}", get(runs::get))
        .route("/api/runs/{id}/events", get(runs::events))
        .route("/api/runs/{id}/cancel", post(runs::cancel))
        .route("/api/approvals", get(approvals::list))
        .route("/api/approvals/{id}", post(approvals::decide))
        .route("/api/approvals/{id}/preview", get(approvals::preview))
        .route(
            "/api/bots/{id}/schedules",
            get(schedules::list).post(schedules::create),
        )
        .route("/api/schedules", get(schedules::list_all))
        .route(
            "/api/schedules/{id}",
            patch(schedules::update).delete(schedules::remove),
        )
        .route("/api/schedules/{id}/run", post(schedules::run_now))
        .route(
            "/api/bots/{id}/memories",
            get(memories::list).post(memories::create),
        )
        .route(
            "/api/memories/{id}",
            patch(memories::update).delete(memories::remove),
        )
        .route("/api/bots/{id}/setup", patch(templates::setup))
        .route("/api/templates", get(templates::list))
        .route("/api/templates/{id}/create", post(templates::create))
        .route("/api/bots/{id}/dream", post(live::dream))
        .route("/api/bots/{id}/live", get(live::info))
        .route("/api/bots/{id}/live.jpg", get(live::frame))
        .route("/api/bots/{id}/live/input", post(live::input))
        .route("/api/desktop/stop", post(desktop::stop))
        .route("/api/bots/{id}/folders", get(folders::list).post(folders::create))
        .route("/api/folders/{id}", patch(folders::update).delete(folders::remove))
        .route("/api/rules", get(rules::list).post(rules::create))
        .route("/api/rules/{id}", delete(rules::remove))
        .route("/api/bots/{id}/skills", get(skills::list))
        .route("/api/runs/{id}/artifacts", get(artifacts::for_run))
        .route("/api/bots/{id}/artifacts", get(artifacts::for_bot))
        .route("/api/artifacts/{id}/download", get(artifacts::download))
        .route("/api/connectors/presets", get(connectors::presets))
        .route(
            "/api/connectors",
            get(connectors::list).post(connectors::create),
        )
        .route(
            "/api/connectors/{id}",
            patch(connectors::update).delete(connectors::remove),
        )
        .route(
            "/api/bots/{id}/connectors",
            get(connectors::for_bot).put(connectors::set_for_bot),
        )
        .route("/api/channels", get(channels::list).post(channels::create))
        .route(
            "/api/channels/{id}",
            patch(channels::update).delete(channels::remove),
        )
        .route(
            "/api/bots/{id}/triggers",
            get(triggers::list).post(triggers::create),
        )
        .route(
            "/api/triggers/{id}",
            patch(triggers::update).delete(triggers::remove),
        )
        .route("/api/triggers/{id}/rotate", post(triggers::rotate))
        .route(
            "/hooks/{token}",
            post(triggers::fire).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/api/stream", get(stream::stream))
        .fallback(|| async { error::ApiError::NotFound })
        .layer(
            TraceLayer::new_for_http()
                // path only: the SSE token travels in the query string and must not be logged
                .make_span_with(|r: &Request<_>| {
                    let p = r.uri().path();
                    // webhook tokens live in the path
                    let p = if p.starts_with("/hooks/") {
                        "/hooks/*"
                    } else {
                        p
                    };
                    tracing::info_span!("http", method = %r.method(), path = %p)
                }),
        )
        .layer(cors(web_origins))
        .with_state(state)
}

/// Everything the server needs; `main.rs` fills it from env vars, the desktop app from its config.
pub struct Config {
    pub database_url: String,
    pub host: [u8; 4],
    pub port: u16,
    pub secret_key: Option<String>,
    pub public_url: Option<String>,
    pub web_origins: Vec<String>,
    /// The teammates' workspaces on this PC (the desktop app). Sharing folders with a teammate needs it: the API checks
    /// them on the PC they are on. None = a hosted API, where folders can't be shared.
    pub bots_dir: Option<std::path::PathBuf>,
}

/// Serve the API until `shutdown` resolves.
pub async fn serve(
    cfg: Config,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let addr = SocketAddr::from((cfg.host, cfg.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("familiar-server listening on {addr}");
    serve_listener(listener, cfg, shutdown).await
}

/// Like [`serve`] but on a listener the caller already bound (tests bind `127.0.0.1:0`); `cfg.host`/`cfg.port` are ignored.
pub async fn serve_listener(
    listener: tokio::net::TcpListener,
    cfg: Config,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let database_url = cfg.database_url;

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(15))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("set time zone 'UTC'").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    let (tx, _) = broadcast::channel(1024);
    stream::spawn_listener(pool.clone(), tx.clone());
    let secret = match cfg.secret_key.as_deref() {
        Some(k) if !k.trim().is_empty() => Some(familiar_crypto::SecretBox::from_base64(k)?),
        _ => None,
    };
    let state = Arc::new(AppState {
        pool,
        events: tx,
        fails: Mutex::new(HashMap::new()),
        hooks: Mutex::new(HashMap::new()),
        secret,
        s3: routes::artifacts::S3::from_env(),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?,
        public_url: cfg
            .public_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty()),
        bots_dir: cfg.bots_dir,
    });

    axum::serve(listener, router(state, &cfg.web_origins))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

impl AppState {
    /// The secret box, or 503 when `FAMILIAR_SECRET_KEY` isn't configured.
    pub fn secret(&self) -> error::R<&familiar_crypto::SecretBox> {
        self.secret
            .as_ref()
            .ok_or_else(|| error::ApiError::Unavailable("FAMILIAR_SECRET_KEY not configured".into()))
    }
}
