//! In-app updates: Familiar finds, verifies, downloads and installs its new versions itself.
//!
//! - [`verify`]: what a release must prove (versions, the signed manifest, signed artifacts, allowed hosts).
//! - [`source`]: GitHub's release list and the verified download into `~/.familiar/updates/<version>/`.
//! - [`apply`]: the detached helper that installs a staged update after the app quits and starts it again.
//!
//! [`Updates`] is the app's one update state (a global entity): it checks 20 s after start and every 6 hours (later
//! when GitHub rate-limits), downloads in the background, and offers "Restart to update" in Settings, with a dot on
//! Settings in the sidebar. Restarting is always the owner's click; Settings holds it back while a teammate works or an
//! approval waits. Developer builds (no `FAMILIAR_VERSION` baked in) never check or install.

pub mod apply;
pub mod source;
pub mod verify;

#[cfg(test)]
mod e2e_tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use gpui_tokio::Tokio;

use source::{CheckError, Found, ListCache, Source, Staged};
use verify::Platform;

/// This build's version: the release's (CI sets `FAMILIAR_VERSION` from the tag), else the crate's.
pub const VERSION: &str = match option_env!("FAMILIAR_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
/// A release build from CI (it has a real version to update from).
pub const OFFICIAL: bool = option_env!("FAMILIAR_VERSION").is_some();
/// When this build's source was committed (Unix seconds; CI sets `FAMILIAR_BUILT_AT`): a newer release can't have been
/// signed before it, so an older signed manifest is refused even if its version number is higher.
pub const BUILT_AT: Option<u64> = match option_env!("FAMILIAR_BUILT_AT") {
    Some(s) => parse_secs(s),
    None => None,
};

const fn parse_secs(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 12 {
        return None;
    }
    let mut n = 0u64;
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            return None;
        }
        n = n * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    Some(n)
}

/// A Unix time as a local date ("12 Oct 2026").
pub fn date_of(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%-d %b %Y").to_string())
        .unwrap_or_default()
}

const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(6 * 3600);
const RETRY: Duration = Duration::from_secs(3600);
const TICK: Duration = Duration::from_secs(60);

/// `~/.familiar/updates`.
pub fn updates_dir() -> PathBuf {
    crate::engine::familiar_home().join("updates")
}

/// Pre-releases are offered by default while this build is one (the alpha).
fn default_early() -> bool {
    verify::parse_version(VERSION).is_some_and(|v| !v.pre.is_empty())
}

#[derive(Clone)]
pub enum Status {
    /// Updates are off here (developer build, or a system without updates); the reason.
    Off(&'static str),
    Idle,
    Checking,
    Downloading { version: String, done: u64, total: u64 },
    UpToDate,
    Ready(Staged),
    Failed(String),
}

pub struct Updates {
    pub status: Status,
    pub checked_at: Option<DateTime<Utc>>,
    /// What the last update attempt did (read once at start from the helper's report).
    pub last: Option<apply::Outcome>,
    /// "Restart to update" couldn't start the updater: why.
    pub apply_error: Option<String>,
    /// The helper is started and the app is quitting.
    pub restarting: bool,
    early: bool,
    next_at: Instant,
    cache: Arc<Mutex<ListCache>>,
    checking: Option<Task<()>>,
    _schedule: Option<Task<()>>,
}

struct UpdatesGlobal(Entity<Updates>);

impl Global for UpdatesGlobal {}

/// Create the app's update state and start checking. Call once, for the main window.
pub fn init(cx: &mut App) {
    let updates = cx.new(Updates::new);
    cx.set_global(UpdatesGlobal(updates));
}

/// The app's update state (`None` in the gallery and the bench).
pub fn entity(cx: &App) -> Option<Entity<Updates>> {
    cx.try_global::<UpdatesGlobal>().map(|g| g.0.clone())
}

impl Updates {
    fn new(cx: &mut Context<Self>) -> Self {
        let dir = updates_dir();
        let last = apply::take_outcome(&dir);
        if let Some(o) = &last {
            tracing::info!("last update attempt ({}): {}", o.version, o.message);
        }
        let status = if !OFFICIAL {
            Status::Off("This is a developer build: it doesn't update itself. Installed releases do.")
        } else if Platform::current().is_none() {
            Status::Off("Familiar doesn't update itself on this system yet.")
        } else {
            Status::Idle
        };
        let schedule = matches!(status, Status::Idle).then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TICK).await;
                    let alive = this.update(cx, |u, cx| {
                        if Instant::now() >= u.next_at {
                            u.check(cx);
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            status,
            checked_at: None,
            last,
            apply_error: None,
            restarting: false,
            early: crate::prefs::load().early_updates.unwrap_or_else(default_early),
            next_at: Instant::now() + FIRST_CHECK,
            cache: Arc::default(),
            checking: None,
            _schedule: schedule,
        }
    }

    pub fn early(&self) -> bool {
        self.early
    }

    pub fn busy(&self) -> bool {
        self.checking.is_some()
    }

    pub fn ready(&self) -> Option<&Staged> {
        match &self.status {
            Status::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// Worth a dot on Settings: an update is ready, or the last one failed.
    pub fn attention(&self) -> bool {
        self.ready().is_some() || self.last.as_ref().is_some_and(|o| !o.ok)
    }

    pub fn set_early(&mut self, on: bool, cx: &mut Context<Self>) {
        crate::prefs::update(|p| p.early_updates = Some(on));
        self.early = on;
        // A staged pre-release isn't wanted any more.
        if !on && self.ready().is_some_and(|s| !s.version.pre.is_empty()) {
            self.status = Status::Idle;
        }
        self.check(cx);
    }

    /// Check now (and download what's found). Does nothing while a check runs or when updates are off.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Off(_)) || self.checking.is_some() || self.restarting {
            return;
        }
        let Some(platform) = Platform::current() else { return };
        let Some(current) = verify::parse_version(VERSION) else {
            self.status = Status::Off("This build's version isn't a release version.");
            return;
        };
        let staged_before = self.ready().cloned();
        if staged_before.is_none() {
            self.status = Status::Checking;
        }
        self.apply_error = None;
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<(String, u64, u64)>();
        let (cache, early) = (self.cache.clone(), self.early);
        let task = Tokio::spawn(cx, async move {
            let source = Source::github();
            let http = source.client().map_err(CheckError::Failed)?;
            let progress = move |v: &semver::Version, done: u64, total: u64| {
                let _ = tx.unbounded_send((v.to_string(), done, total));
            };
            let trust = source::Trust { keys: verify::release_keys(), not_before: BUILT_AT };
            source::check(&source, &http, &cache, &trust, &current, early, platform, &updates_dir(), &progress).await
        });
        self.checking = Some(cx.spawn(async move |this, cx| {
            let mut pump_cx = cx.clone();
            let pump_this = this.clone();
            let pump = async move {
                use futures::StreamExt as _;
                while let Some((version, done, total)) = rx.next().await {
                    let _ = pump_this.update(&mut pump_cx, |u, cx| {
                        if u.ready().is_none() {
                            u.status = Status::Downloading { version, done, total };
                            cx.notify();
                        }
                    });
                }
            };
            let (_, result) = futures::join!(pump, task);
            let _ = this.update(cx, |u, cx| {
                u.checking = None;
                u.checked_at = Some(Utc::now());
                let keep = |u: &mut Updates, fallback: Status| {
                    u.status = match staged_before.clone() {
                        Some(s) => Status::Ready(s),
                        None => fallback,
                    };
                };
                match result {
                    Ok(Ok(Found::UpToDate)) => {
                        u.status = Status::UpToDate;
                        u.next_at = Instant::now() + EVERY;
                    }
                    Ok(Ok(Found::Ready(staged))) => {
                        tracing::info!("update {} is ready", staged.version);
                        u.status = Status::Ready(staged);
                        u.next_at = Instant::now() + EVERY;
                    }
                    Ok(Err(CheckError::RateLimited(wait))) => {
                        keep(u, Status::Failed("GitHub asked Familiar to slow down; it will check again later.".into()));
                        u.next_at = Instant::now() + wait.max(RETRY);
                    }
                    Ok(Err(CheckError::Failed(e))) => {
                        tracing::warn!("update check: {e}");
                        keep(u, Status::Failed(e));
                        u.next_at = Instant::now() + RETRY;
                    }
                    Err(e) => {
                        keep(u, Status::Failed(e.to_string()));
                        u.next_at = Instant::now() + RETRY;
                    }
                }
                cx.notify();
            });
        }));
    }

    /// "Restart to update": start the helper, then quit (gracefully; the helper waits for that).
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        let Some(staged) = self.ready().cloned() else { return };
        if self.restarting || self.checking.is_some() {
            return;
        }
        match apply::launch(&staged) {
            Ok(()) => {
                tracing::info!("restarting to update to {}", staged.version);
                self.restarting = true;
                self.apply_error = None;
                // After this event: quitting updates the window, which may be the one that was clicked.
                cx.defer(crate::desktop::quit_app);
            }
            Err(e) => {
                tracing::error!("restart to update: {e}");
                self.apply_error = Some(e);
            }
        }
        cx.notify();
    }

    /// One line about where updates stand.
    pub fn summary(&self) -> String {
        match &self.status {
            Status::Off(why) => (*why).to_owned(),
            Status::Idle => "Familiar checks for updates shortly after it starts, then every 6 hours.".into(),
            Status::Checking => "Checking for updates…".into(),
            Status::Downloading { version, done, total } => {
                let pct = if *total > 0 { done * 100 / total } else { 0 };
                format!("Downloading {version}… {pct}%")
            }
            Status::UpToDate => "Up to date.".into(),
            Status::Ready(s) => match s.released_at {
                Some(t) => format!("Version {} (released {}) is ready.", s.version, date_of(t)),
                None => format!("Version {} is ready.", s.version),
            },
            Status::Failed(e) => format!("Couldn't check for updates: {e}"),
        }
    }

    /// "Last checked 5m ago" once a check has run.
    pub fn last_checked(&self) -> Option<String> {
        self.checked_at.map(|t| format!("Last checked {}.", crate::data::ago(Some(t))))
    }
}

/// Why "Restart to update" should wait, if it should: a teammate is mid-run, or an approval is waiting (restarting
/// would cut the run short, or leave the teammate waiting longer).
pub fn hold_reason(data: &crate::data::AppData) -> Option<String> {
    if !data.pending.is_empty() {
        return Some("An approval is waiting for you. Answer it, then restart.".into());
    }
    let working: Vec<&str> = data
        .bots()
        .iter()
        .filter(|b| b.status == Some(familiar_client::BotStatus::Running))
        .map(|b| b.name.as_str())
        .collect();
    match working.as_slice() {
        [] => None,
        [one] => Some(format!("{one} is working. Restart when it's done.")),
        [first, rest @ ..] => Some(format!("{first} and {} more are working. Restart when they're done.", rest.len())),
    }
}
