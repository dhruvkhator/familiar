//! Where the app's data comes from, and the app's lifecycle around it.
//!
//! **Host mode** (the default): this process is the whole local install. [`familiar_host::Host`] boots the built-in
//! database (or the configured Postgres), serves the Familiar API on 127.0.0.1 and runs the daemon; the app signs in
//! with a token the host mints for the owner, no password. On quit the host drains in-flight runs and stops the
//! database before the process exits.
//!
//! **Attached mode**: another Familiar (the Tauri app) already holds this machine's host lock, or (an older build)
//! already serves the API port, and must keep running. This window then uses that engine's API
//! (`127.0.0.1:<local_api_port>`, default 47080, or `FAMILIAR_API_URL`) and signs in with the owner password
//! (`FAMILIAR_PASSWORD`, else `owner_password.txt` in the Familiar folder).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use familiar_client::Client;
use familiar_host::{Host, Phase as BootPhase};
use gpui::{App, Context, Task};
use gpui_tokio::Tokio;

/// The host's boot message when another host holds the machine's lock (see `familiar_host::Host::start`).
const ANOTHER_HOST: &str = "another Familiar is already running";

/// The running host, for the last-chance drain in `main` after the event loop ends.
pub static HOSTED: Mutex<Option<Host>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// This app runs the engine.
    Hosted,
    /// The Familiar desktop app runs the engine; this window uses its API.
    Attached,
}

#[derive(Clone)]
pub enum Phase {
    /// Starting up; the message says what is happening.
    Booting(String),
    /// Host mode, no owner account yet: the first-run screen creates it with this (signed-out) client.
    FirstRun(Client),
    Ready(Client),
    Failed(String),
    /// Quitting: in-flight runs are draining and the database is stopping.
    Stopping,
}

pub struct Engine {
    pub phase: Phase,
    pub mode: Mode,
    host: Option<Host>,
    _task: Option<Task<()>>,
    _attach: Option<Task<()>>,
}

impl Engine {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self { phase: Phase::Booting(String::new()), mode: Mode::Hosted, host: None, _task: None, _attach: None };
        this.connect(cx);
        this
    }

    /// (Re)start from scratch: stop a previous host (a failed boot may hold the lock or a running database), then
    /// try to host; fall back to attaching when another Familiar holds the lock.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Stopping) {
            return;
        }
        self.phase = Phase::Booting(String::new());
        self.mode = Mode::Hosted;
        self._attach = None;
        let previous = self.host.take();
        HOSTED.lock().unwrap().take();
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            if let Some(old) = previous {
                let _ = Tokio::spawn(cx, async move { old.shutdown().await }).await;
            }
            // A Familiar from before the host lock (an older desktop app) runs without taking it: if something already
            // serves our API port, attach instead of starting a second daemon on the same database.
            let served = Tokio::spawn(cx, async {
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], api_port()));
                tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(addr)).await.is_ok_and(|c| c.is_ok())
            });
            if served.await.unwrap_or(false) {
                let _ = this.update(cx, |e, cx| e.attach(cx));
                return;
            }
            let handle = cx.update(|cx| Tokio::handle(cx));
            let host = Host::start(handle);
            let mut boot = host.boot_watch();
            let _ = this.update(cx, |e, _| {
                e.host = Some(host.clone());
                *HOSTED.lock().unwrap() = Some(host.clone());
            });
            // Follow the boot (tokio's watch needs no runtime to await).
            loop {
                let b = boot.borrow_and_update().clone();
                crate::perf::note(&format!("boot: {}", b.message));
                match b.phase {
                    BootPhase::Ready => {
                        crate::perf::milestone("engine_ready");
                        break;
                    }
                    BootPhase::Error if b.message == ANOTHER_HOST => {
                        let _ = this.update(cx, |e, cx| {
                            e.host = None;
                            HOSTED.lock().unwrap().take();
                            e.attach(cx);
                        });
                        return;
                    }
                    BootPhase::Error => {
                        let _ = this.update(cx, |e, cx| e.set(Phase::Failed(b.message), cx));
                        return;
                    }
                    BootPhase::Starting | BootPhase::Database => {
                        let _ = this.update(cx, |e, cx| e.set(Phase::Booting(b.message), cx));
                    }
                }
                if boot.changed().await.is_err() {
                    let _ = this.update(cx, |e, cx| e.set(Phase::Failed("Startup stopped unexpectedly.".into()), cx));
                    return;
                }
            }
            let _ = this.update(cx, |e, cx| e.set(Phase::Booting("Opening your workspace…".into()), cx));
            let client = Client::new(host.api_url().unwrap_or_else(local_api), None);
            let signing = Tokio::spawn(cx, hosted_sign_in(host, client.clone()));
            let phase = match signing.await {
                Ok(Ok(Some(token))) => {
                    crate::perf::milestone("signed_in");
                    client.set_token(Some(token));
                    Phase::Ready(client)
                }
                Ok(Ok(None)) => Phase::FirstRun(client),
                Ok(Err(e)) => Phase::Failed(e),
                Err(e) => Phase::Failed(e.to_string()),
            };
            let _ = this.update(cx, |e, cx| e.set(phase, cx));
        }));
    }

    /// Another Familiar runs the engine: use its API and sign in with the owner password.
    fn attach(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Attached;
        self.set(Phase::Booting("Connecting to the Familiar app…".into()), cx);
        let base = std::env::var("FAMILIAR_API_URL").unwrap_or_else(|_| local_api());
        let client = Client::new(base, None);
        let task = Tokio::spawn(cx, attached_sign_in(client.clone()));
        self._attach = Some(cx.spawn(async move |this, cx| {
            let phase = match task.await {
                Ok(Ok(())) => Phase::Ready(client),
                Ok(Err(e)) => Phase::Failed(e),
                Err(e) => Phase::Failed(e.to_string()),
            };
            let _ = this.update(cx, |e, cx| e.set(phase, cx));
        }));
    }

    fn set(&mut self, phase: Phase, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Stopping) {
            self.phase = phase;
            cx.notify();
        }
    }

    /// The first-run account was created; its session signs this app in.
    pub fn signed_up(&mut self, client: Client, cx: &mut Context<Self>) {
        self.set(Phase::Ready(client), cx);
    }

    /// The window wants to close. In host mode the engine drains first (the window shows it) and the app quits when
    /// that is done; returns whether the window may close right now.
    pub fn request_quit(&mut self, cx: &mut Context<Self>) -> bool {
        if matches!(self.phase, Phase::Stopping) {
            return false;
        }
        let Some(host) = self.host.take() else { return true };
        self.phase = Phase::Stopping;
        cx.notify();
        self._task = Some(cx.spawn(async move |_, cx| {
            let _ = Tokio::spawn(cx, async move { host.shutdown().await }).await;
            HOSTED.lock().unwrap().take();
            cx.update(|cx: &mut App| cx.quit());
        }));
        false
    }
}

/// Host mode: a token minted by the host, or `None` when no owner exists yet (first run). The API comes up and
/// migrates right after the host reports ready, so both are retried for a while.
async fn hosted_sign_in(host: Host, client: Client) -> Result<Option<String>, String> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(token) = host.local_token().await {
            return Ok(Some(token));
        }
        if let Ok(state) = client.auth_state().await
            && state.setup_needed
        {
            return Ok(None);
        }
        if Instant::now() > deadline {
            return Err("Familiar's engine started but didn't answer in time.".into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Attached mode: the owner password from `FAMILIAR_PASSWORD` or `owner_password.txt`.
async fn attached_sign_in(client: Client) -> Result<(), String> {
    let file = familiar_home().join("owner_password.txt");
    let password = std::env::var("FAMILIAR_PASSWORD").or_else(|_| std::fs::read_to_string(&file)).map_err(|_| {
        format!(
            "The Familiar app is already running on this computer, and signing in to it needs the owner password \
             (set FAMILIAR_PASSWORD or save it in {}). Or quit the Familiar app and try again.",
            file.display()
        )
    })?;
    let email = std::env::var("FAMILIAR_EMAIL").unwrap_or_else(|_| "owner@familiar.local".into());
    client.login(&email, password.trim()).await.map(|_| ()).map_err(|e| e.message())
}

/// `local_api_port` from the Familiar config (default 47080), where a running Familiar serves its API.
fn api_port() -> u16 {
    let text = std::fs::read_to_string(familiar_home().join("config.toml")).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "local_api_port")
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(47080)
}

fn local_api() -> String {
    format!("http://127.0.0.1:{}", api_port())
}

/// `~/.familiar`, or `FAMILIAR_HOME` (the same rule as familiar-core's `Config::home_dir`).
pub fn familiar_home() -> PathBuf {
    if let Some(home) = std::env::var_os("FAMILIAR_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(home).join(".familiar")
}
