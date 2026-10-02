//! familiar-host: runs a whole local Familiar install inside the calling process, the same way for every shell (the
//! Tauri tray app, the native GPUI app). [`Host::start`] brings up the database (the built-in Postgres unless
//! `database_url` is configured), serves the Familiar API on `127.0.0.1:local_api_port` and runs the daemon
//! (familiar-core); the shell only renders [`Host::boot_status`], forwards [`Host::signals`] to notifications and calls
//! [`Host::shutdown`] on quit. No UI-toolkit dependencies here.
//!
//! One host per machine: `start` takes an exclusive lock on `~/.familiar/host.lock`; when another host holds it the
//! boot phase is `error` ("another Familiar is already running") and nothing else starts.

mod cli;
mod embedded_db;
mod instance;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use familiar_core::Config;
use postgresql_embedded::PostgreSQL;
use serde::Serialize;
use sqlx::PgPool;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

pub use cli::{CliStatus, claude_status, codex_status, open_cli_terminal, open_folder};
pub use familiar_core::Signal;
pub use instance::InstanceLock;

/// Where startup is. Serialises as `starting | database | ready | error` (the web UI's `AppStatus.boot.phase`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Starting,
    /// Setting up / starting the built-in database (the first launch downloads it once).
    Database,
    /// API and daemon started.
    Ready,
    /// Startup stopped; `message` says why.
    Error,
}

/// Startup progress, shown by the first-run screen.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Boot {
    pub phase: Phase,
    pub message: String,
    pub database_url: Option<String>,
    /// The built-in database is in use (no `database_url` configured).
    pub embedded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub error: Option<String>,
    pub config_path: String,
}

/// Everything a first-run screen needs, in one call.
#[derive(Debug, Clone, Serialize)]
pub struct AppStatus {
    pub boot: Boot,
    pub daemon: DaemonStatus,
}

#[derive(Default)]
struct Daemon {
    token: Option<CancellationToken>,
    task: Option<tokio::task::JoinHandle<()>>,
    paused: bool,
    error: Option<String>,
    bots_dir: Option<PathBuf>,
}

struct Inner {
    rt: Handle,
    boot: watch::Sender<Boot>,
    daemon: Arc<Mutex<Daemon>>,
    signals: broadcast::Sender<Signal>,
    pg: tokio::sync::Mutex<Option<PostgreSQL>>,
    api_port: OnceLock<u16>,
    api_stop: CancellationToken,
    /// Shutdown has begun: nothing new starts.
    stopping: CancellationToken,
    /// Small pool for [`Host::local_token`], opened on first use.
    pool: tokio::sync::OnceCell<PgPool>,
    lock: Mutex<Option<InstanceLock>>,
}

/// A running local install. Cheap to clone (all clones are the same host).
#[derive(Clone)]
pub struct Host {
    inner: Arc<Inner>,
}

impl Host {
    /// Start booting in the background on `rt` and return at once; watch progress with [`Host::boot_status`] /
    /// [`Host::boot_watch`]. Boot: create `~/.familiar/config.toml` on first run, load it, start the built-in database
    /// when `database_url` is unset, serve the API, start the daemon. Every failure lands in the boot status.
    pub fn start(rt: Handle) -> Host {
        let host = Host {
            inner: Arc::new(Inner {
                rt,
                boot: watch::channel(Boot::default()).0,
                daemon: Arc::default(),
                signals: broadcast::channel(64).0,
                pg: Default::default(),
                api_port: OnceLock::new(),
                api_stop: CancellationToken::new(),
                stopping: CancellationToken::new(),
                pool: Default::default(),
                lock: Mutex::new(None),
            }),
        };
        let lock_path = Config::home_dir().join("host.lock");
        match InstanceLock::acquire(&lock_path) {
            Ok(Some(lock)) => *host.inner.lock.lock().unwrap() = Some(lock),
            Ok(None) => {
                host.set(Phase::Error, "another Familiar is already running");
                return host;
            }
            Err(e) => {
                host.set(Phase::Error, &format!("could not lock {}: {e}", lock_path.display()));
                return host;
            }
        }
        let h = host.clone();
        host.inner.rt.spawn(async move { h.boot().await });
        host
    }

    fn set(&self, phase: Phase, message: &str) {
        self.inner.boot.send_modify(|b| {
            b.phase = phase;
            b.message = message.into();
        });
    }

    async fn boot(&self) {
        if let Err(e) = Config::create_default() {
            return self.set(Phase::Error, &format!("could not create {}: {e:#}", Config::path().display()));
        }
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return self.set(Phase::Error, &format!("{e:#}")),
        };
        let url = match cfg.database_url.clone() {
            Some(url) => url,
            None => {
                self.set(Phase::Database, "Setting up the built-in database (first launch downloads it once)…");
                match embedded_db::start(&cfg).await {
                    Ok((pg, url)) => {
                        let mut slot = self.inner.pg.lock().await;
                        if self.inner.stopping.is_cancelled() {
                            let _ = tokio::time::timeout(Duration::from_secs(10), pg.stop()).await;
                            return;
                        }
                        *slot = Some(pg);
                        self.inner.boot.send_modify(|b| b.embedded = true);
                        url
                    }
                    Err(e) => return self.set(Phase::Error, &format!("{e:#}")),
                }
            }
        };
        if self.inner.stopping.is_cancelled() {
            return;
        }
        self.inner.boot.send_modify(|b| b.database_url = Some(url.clone()));
        self.start_api(&cfg, &url);
        self.resume_daemon();
        self.set(Phase::Ready, "");
    }

    /// The Familiar API on 127.0.0.1 (`local_api_port`, default 47080). It keeps serving while the daemon is paused.
    fn start_api(&self, cfg: &Config, url: &str) {
        let port = cfg.local_api_port.unwrap_or(47080);
        let _ = self.inner.api_port.set(port);
        let api = familiar_server::Config {
            database_url: url.to_owned(),
            host: [127, 0, 0, 1],
            port,
            secret_key: cfg.secret_key.clone(),
            public_url: Some(format!("http://localhost:{port}")),
            web_origins: vec![],
        };
        let stop = self.inner.api_stop.clone();
        self.inner.rt.spawn(async move {
            if let Err(e) = familiar_server::serve(api, stop.cancelled_owned()).await {
                tracing::error!("local API stopped: {e:#}");
            }
        });
    }

    /// Current startup progress.
    pub fn boot_status(&self) -> Boot {
        self.inner.boot.borrow().clone()
    }

    /// Startup progress as a stream of changes (for UIs that redraw on change instead of polling).
    pub fn boot_watch(&self) -> watch::Receiver<Boot> {
        self.inner.boot.subscribe()
    }

    /// Base URL of the local API (`http://127.0.0.1:<port>`) once boot has reached it; `None` before that.
    pub fn api_url(&self) -> Option<String> {
        self.inner.api_port.get().map(|p| format!("http://127.0.0.1:{p}"))
    }

    pub fn daemon_status(&self) -> DaemonStatus {
        let d = self.inner.daemon.lock().unwrap();
        DaemonStatus {
            running: d.token.is_some() && !d.paused && d.error.is_none(),
            error: d.error.clone(),
            config_path: Config::path().display().to_string(),
        }
    }

    pub fn app_status(&self) -> AppStatus {
        AppStatus { boot: self.boot_status(), daemon: self.daemon_status() }
    }

    /// Paused by [`Host::pause_daemon`] (and not resumed since).
    pub fn daemon_paused(&self) -> bool {
        self.inner.daemon.lock().unwrap().paused
    }

    /// Stop the daemon (in-flight runs drain in the background); the API keeps serving.
    pub fn pause_daemon(&self) {
        let mut d = self.inner.daemon.lock().unwrap();
        d.paused = true;
        if let Some(t) = d.token.take() {
            t.cancel();
        }
    }

    /// (Re)start the daemon with a freshly loaded config. A config or connection error is recorded in
    /// [`Host::daemon_status`], never fatal. Before the database is up this records "the database is not ready yet".
    pub fn resume_daemon(&self) {
        if self.inner.stopping.is_cancelled() {
            return;
        }
        let mut d = self.inner.daemon.lock().unwrap();
        d.paused = false;
        let mut cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                d.error = Some(format!("{e:#}"));
                return;
            }
        };
        let Some(url) = self.inner.boot.borrow().database_url.clone() else {
            d.error = Some("the database is not ready yet".into());
            return;
        };
        cfg.database_url = Some(url);
        d.error = None;
        d.bots_dir = Some(cfg.bots_dir());
        let token = CancellationToken::new();
        d.token = Some(token.clone());
        let previous = d.task.take(); // let a cancelled core finish draining before the new one starts
        let (daemon, signals) = (self.inner.daemon.clone(), self.inner.signals.clone());
        d.task = Some(self.inner.rt.spawn(async move {
            if let Some(prev) = previous {
                let _ = prev.await;
            }
            if token.is_cancelled() {
                return;
            }
            if let Err(e) = familiar_core::run_with_signals(cfg, token.clone(), signals).await {
                tracing::error!("familiar-core stopped: {e:#}");
                let mut d = daemon.lock().unwrap();
                if !token.is_cancelled() {
                    d.error = Some(format!("{e:#}"));
                    d.token = None;
                }
            }
        }));
    }

    /// Daemon events for desktop notifications (approval needed, run finished, `notify_user`).
    pub fn signals(&self) -> broadcast::Receiver<Signal> {
        self.inner.signals.subscribe()
    }

    /// The bots' folder (`bots_dir` from the config, default `~/.familiar/bots`), created if missing.
    pub fn bots_dir(&self) -> PathBuf {
        let dir = self.inner.daemon.lock().unwrap().bots_dir.clone().unwrap_or_else(|| Config::home_dir().join("bots"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// A fresh 30-day API session token for the single owner, without a password: for a UI in this process. `None`
    /// until boot is `ready` and an owner exists (first run: create one via `POST /api/auth/setup`), or when the database
    /// is not migrated yet (the API migrates it right after `ready`), so callers retry. Never logged.
    pub async fn local_token(&self) -> Option<String> {
        let url = {
            let b = self.inner.boot.borrow();
            if b.phase != Phase::Ready {
                return None;
            }
            b.database_url.clone()?
        };
        let pool = self
            .inner
            .pool
            .get_or_try_init(|| {
                sqlx::postgres::PgPoolOptions::new()
                    .max_connections(2)
                    .acquire_timeout(Duration::from_secs(15))
                    .connect(&url)
            })
            .await;
        let pool = match pool {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("local session: could not connect to the database: {e}");
                return None;
            }
        };
        match familiar_server::mint_owner_session(pool).await {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!("local session: {e:#}");
                None
            }
        }
    }

    /// Quit: stop the daemon and give in-flight runs up to 35 s to record their status, stop the API, shut the
    /// built-in database down cleanly (up to 10 s), release the instance lock. Idempotent; the host cannot be restarted.
    pub async fn shutdown(&self) {
        self.inner.stopping.cancel();
        let (token, task) = {
            let mut d = self.inner.daemon.lock().unwrap();
            (d.token.take(), d.task.take())
        };
        if let Some(t) = token {
            t.cancel();
        }
        if let Some(task) = task {
            let _ = tokio::time::timeout(Duration::from_secs(35), task).await;
        }
        self.inner.api_stop.cancel();
        if let Some(pool) = self.inner.pool.get() {
            pool.close().await;
        }
        if let Some(pg) = self.inner.pg.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), pg.stop()).await;
        }
        self.inner.lock.lock().unwrap().take();
    }
}

/// Log to `~/.familiar/logs/<name>.log` (started afresh past 10 MB), level from `RUST_LOG` (default `info`); stderr
/// if the file cannot be opened. Release GUI builds have no console. Does nothing if a subscriber is already set.
pub fn init_logging(name: &str) {
    let logs = Config::home_dir().join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let log_path = logs.join(format!("{name}.log"));
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
        let _ = std::fs::remove_file(&log_path);
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = match std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(file) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(file)).try_init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).try_init(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_serialises_like_the_web_ui_expects() {
        let v = serde_json::to_value(Boot { phase: Phase::Database, ..Default::default() }).unwrap();
        assert_eq!(v, serde_json::json!({"phase": "database", "message": "", "database_url": null, "embedded": false}));
    }
}
