//! The always-on loop: heartbeat, LISTEN/NOTIFY, run scheduling, schedule fire times.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use serde::Deserialize;
use sqlx::postgres::{PgListener, PgPoolOptions};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::db::Db;
use crate::{mcp, runner};

/// Hold scheduled/proactive runs above this five-hour subscription utilization.
const THROTTLE_AT: f64 = 0.9;

#[derive(Debug, Clone, Deserialize)]
pub struct Notice {
    pub t: String,
    pub id: Uuid,
    #[serde(default)]
    pub owner: Option<Uuid>,
    /// INSERT, UPDATE or DELETE.
    #[serde(default)]
    pub op: Option<String>,
}

impl Notice {
    /// "Something may have been missed" (listener reconnected): everyone re-checks.
    fn all() -> Self {
        Notice { t: "*".into(), id: Uuid::nil(), owner: None, op: None }
    }
}

/// Coarse events for a host app (desktop notifications); ignored by the headless daemon.
#[derive(Debug, Clone)]
pub enum Signal {
    ApprovalPending { bot: String, tool: String },
    RunFinished { bot: String, status: String },
    /// The bot used `notify_user` to tell its owner something.
    Notify { bot: String, message: String, thread: Uuid },
    /// A teammate started using this PC's desktop (`Some(name)`), or nobody uses it any more (`None`).
    Desktop { bot: Option<String> },
}

#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub notices: broadcast::Sender<Notice>,
    /// Latest five-hour utilization and when that window resets (unix seconds).
    utilization: Arc<Mutex<(f64, Option<i64>)>>,
    signals: broadcast::Sender<Signal>,
    /// The Familiar MCP server bots call back into, and the run tokens it accepts.
    pub mcp_url: String,
    pub registry: mcp::Registry,
    /// Each bot's headless Chrome, its live view and take-over input.
    pub browsers: Arc<crate::browser::Manager>,
    /// Decrypts connector and channel secrets (None when `secret_key` is not configured).
    pub secrets: Option<Arc<familiar_crypto::SecretBox>>,
    /// Which teammate is using this PC's desktop (one at a time).
    pub desktop: Arc<crate::desktop::Control>,
    /// Desktop control is offered only when the daemon runs inside the app ([`run_with_signals`]), which shows who is
    /// using the desktop and can stop it from the tray.
    pub desktop_ui: bool,
}

impl Ctx {
    pub fn subscribe_signals(&self) -> broadcast::Receiver<Signal> {
        self.signals.subscribe()
    }

    pub fn signal(&self, s: Signal) {
        let _ = self.signals.send(s);
    }

    pub fn set_utilization(&self, u: f64, resets_at: Option<i64>) {
        *self.utilization.lock().unwrap() = (u, resets_at);
    }

    pub fn throttled(&self) -> bool {
        let (u, resets_at) = *self.utilization.lock().unwrap();
        u >= THROTTLE_AT && resets_at.is_none_or(|t| Utc::now().timestamp() < t)
    }
}

struct Active {
    bot: Uuid,
    cancel: CancellationToken,
}

type ActiveMap = Arc<Mutex<HashMap<Uuid, Active>>>;

/// Run the daemon until `shutdown` is cancelled.
pub async fn run(cfg: Config, shutdown: CancellationToken) -> Result<()> {
    serve(cfg, shutdown, broadcast::channel(16).0, false).await
}

/// Like [`run`], but publishes [`Signal`]s to `signals`: for the app, which shows them (approvals, who is using the
/// desktop). Only this offers desktop control.
pub async fn run_with_signals(cfg: Config, shutdown: CancellationToken, signals: broadcast::Sender<Signal>) -> Result<()> {
    serve(cfg, shutdown, signals, true).await
}

async fn serve(cfg: Config, shutdown: CancellationToken, signals: broadcast::Sender<Signal>, desktop_ui: bool) -> Result<()> {
    let url = cfg.database_url.as_deref().context("database_url is not set")?;
    let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    let owner = match cfg.owner_id {
        Some(id) => id,
        None => wait_for_owner(&pool, &shutdown).await?,
    };
    let secrets = match cfg.secret_key.as_deref() {
        Some(k) => Some(Arc::new(familiar_crypto::SecretBox::from_base64(k)?)),
        None => None,
    };
    let (mcp_listener, mcp_url) = mcp::bind().await?;
    let ctx = Ctx {
        db: Db { pool: pool.clone(), owner },
        cfg: Arc::new(cfg),
        notices: broadcast::channel(256).0,
        utilization: Arc::default(),
        signals,
        mcp_url,
        registry: mcp::Registry::default(),
        browsers: Arc::default(),
        secrets,
        desktop: Arc::default(),
        desktop_ui,
    };
    mcp::serve(mcp_listener, ctx.clone(), shutdown.clone());
    tokio::spawn(crate::tools::ensure_playwright());
    tokio::spawn(crate::telegram::run(ctx.clone(), shutdown.clone()));
    let models: crate::models::Shared = Arc::default();
    tokio::spawn(crate::models::run(ctx.cfg.clone(), models.clone(), shutdown.clone()));
    let stale = ctx.db.fail_stale_runs().await?;
    if stale > 0 {
        warn!("marked {stale} interrupted run(s) as failed");
    }

    let wake = Arc::new(Notify::new());
    let active: ActiveMap = Arc::default();
    // CRM webhook deliveries: their own loop, so a slow receiver never holds up runs.
    let hooks = Arc::new(Notify::new());
    tokio::spawn(crate::crm::webhooks::run(ctx.db.clone(), ctx.secrets.clone(), hooks.clone(), shutdown.clone()));

    let device = Config::device_id();
    let device_name = ctx.cfg.device_name.clone().unwrap_or_else(|| {
        hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|_| "familiar".into())
    });
    tokio::spawn({
        let (ctx, db, shutdown, active, models) = (ctx.clone(), ctx.db.clone(), shutdown.clone(), active.clone(), models.clone());
        let health = ctx.cfg.server_url.as_deref().map(|u| format!("{}/healthz", u.trim_end_matches('/')));
        async move {
            // Minute heartbeat also keeps free-tier databases that pause when idle awake.
            let http = reqwest::Client::new();
            let claude_version = claude_version(&ctx.cfg.claude_bin).await;
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            for n in 0u64.. {
                tokio::select! { _ = tick.tick() => {}, _ = shutdown.cancelled() => return }
                if let Err(e) = db.heartbeat(device, &device_name).await {
                    warn!("heartbeat failed: {e:#}");
                }
                let (utilization, resets_at) = *ctx.utilization.lock().unwrap();
                let mut info = serde_json::json!({
                    "utilization": utilization, "resets_at": resets_at, "throttled": ctx.throttled(),
                    "active_runs": active.lock().unwrap().len(), "claude_version": claude_version,
                    "daemon_version": env!("CARGO_PKG_VERSION"),
                });
                let models = models.lock().unwrap().clone();
                if !models.is_null() {
                    info["models"] = models;
                }
                let _ = db.set_device_info(device, &info).await;
                if let Some(url) = health.as_deref().filter(|_| n % 10 == 0) {
                    let _ = http.get(url).timeout(Duration::from_secs(90)).send().await;
                }
                if n % 1440 == 0 {
                    match db.prune_events().await {
                        Ok(0) => {}
                        Ok(k) => info!("pruned {k} old events"),
                        Err(e) => warn!("event pruning failed: {e:#}"),
                    }
                }
            }
        }
    });

    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen_all(["familiar", "familiar_input"]).await?;
    tokio::spawn(listen(ctx.clone(), listener, wake.clone(), hooks, active.clone(), shutdown.clone()));

    info!(owner = %ctx.db.owner, max_parallel = ctx.cfg.max_parallel, "Familiar daemon running");
    loop {
        if let Err(e) = tick(&ctx, &active, &wake).await {
            warn!("scheduler tick failed: {e:#}");
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = shutdown.cancelled() => break,
        }
    }
    ctx.browsers.shutdown().await;
    for a in active.lock().unwrap().values() {
        a.cancel.cancel();
    }
    // Give runs a moment to interrupt cleanly and record their status.
    // Cancelled runs need up to ~20 s (interrupt grace + exit grace) to record their status.
    for _ in 0..60 {
        if active.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

async fn listen(
    ctx: Ctx,
    mut listener: PgListener,
    wake: Arc<Notify>,
    hooks: Arc<Notify>,
    active: ActiveMap,
    shutdown: CancellationToken,
) {
    loop {
        let next = tokio::select! { n = listener.try_recv() => n, _ = shutdown.cancelled() => return };
        match next {
            Ok(Some(n)) => {
                if n.channel() == "familiar_input" {
                    // Take-over input from the computer panel, already validated and owner-scoped by the API.
                    let Ok(ev) = serde_json::from_str::<serde_json::Value>(n.payload()) else { continue };
                    let owner = ev["owner"].as_str().and_then(|o| o.parse::<Uuid>().ok());
                    // "Stop desktop control" (the tray, `POST /api/desktop/stop`).
                    if ev["type"] == "desktop_stop" {
                        if owner == Some(ctx.db.owner) {
                            stop_desktop(&ctx).await;
                        }
                        continue;
                    }
                    let bot = ev["bot"].as_str().and_then(|b| b.parse::<Uuid>().ok());
                    if let (Some(owner), Some(bot)) = (owner, bot) {
                        if owner == ctx.db.owner {
                            let ctx = ctx.clone();
                            tokio::spawn(async move {
                                // Opening a page (the Set up checklist's "Log in to …") starts the bot's browser even
                                // before its first run, so you can sign in to sites in its own profile.
                                if ev["type"] == "navigate"
                                    && let Ok(b) = ctx.db.bot(bot).await
                                {
                                    let profiles = ctx.cfg.bots_dir().join(".browsers");
                                    ctx.browsers.ensure(b.id, &b.slug, profiles, ctx.cfg.browser_bin.as_deref()).await;
                                }
                                if let Err(e) = ctx.browsers.input(&ctx.db, bot, &ev).await {
                                    warn!("take-over input failed: {e:#}");
                                }
                            });
                        }
                    }
                    continue;
                }
                let Ok(notice) = serde_json::from_str::<Notice>(n.payload()) else { continue };
                if notice.owner.is_some_and(|o| o != ctx.db.owner) {
                    continue;
                }
                if notice.t == "runs" && active.lock().unwrap().contains_key(&notice.id) {
                    check_cancel(&ctx, &active, notice.id).await;
                }
                // A decided draft (approvals) gets its follow-up run queued on the next tick.
                if matches!(notice.t.as_str(), "runs" | "messages" | "schedules" | "approvals") {
                    wake.notify_one();
                }
                if notice.t == "crm_webhook_deliveries" && notice.op.as_deref() == Some("INSERT") {
                    hooks.notify_one();
                }
                let _ = ctx.notices.send(notice);
            }
            Ok(None) => {
                // Connection dropped and sqlx reconnected: notifications in the gap are lost, so re-sweep.
                info!("listener reconnected");
                let _ = ctx.notices.send(Notice::all());
                wake.notify_one();
                hooks.notify_one();
            }
            Err(e) => {
                warn!("listener error: {e:#}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// "Stop desktop control": the teammate using the desktop loses it at once, its waiting desktop requests are denied and
/// its run is cancelled.
pub async fn stop_desktop(ctx: &Ctx) {
    if let Some((run, bot, cancel)) = ctx.desktop.stop() {
        info!(%run, %bot, "desktop control stopped by the owner");
        // Denied first (so the teammate is told it was stopped), then the run ends.
        if let Err(e) = ctx.db.deny_desktop_approvals(Some(run)).await {
            warn!("denying desktop requests failed: {e:#}");
        }
        cancel.cancel();
        ctx.signal(Signal::Desktop { bot: None });
    }
}

async fn check_cancel(ctx: &Ctx, active: &ActiveMap, run: Uuid) {
    if let Ok(Some(status)) = ctx.db.run_status(run).await {
        if status == "cancelled" {
            if let Some(a) = active.lock().unwrap().get(&run) {
                a.cancel.cancel();
            }
        }
    }
}

async fn tick(ctx: &Ctx, active: &ActiveMap, wake: &Arc<Notify>) -> Result<()> {
    ctx.db.enqueue_due_schedules().await?;
    // Nightly dreams: after 03:00 local, every bot that worked since its last dream reviews the day.
    let now = Local::now();
    if let Some(three) = now.date_naive().and_hms_opt(3, 0, 0).and_then(|t| t.and_local_timezone(Local).single()) {
        if now >= three {
            for bot in ctx.db.bots_due_for_dream(three.with_timezone(&Utc)).await? {
                ctx.db.queue_dream(bot).await?;
                info!(%bot, "queued nightly dream");
            }
        }
    }
    for (id, expr) in ctx.db.schedules_without_next().await? {
        match croner::Cron::from_str(&expr).and_then(|c| c.find_next_occurrence(&Local::now(), false)) {
            Ok(next) => ctx.db.set_next_run(id, Some(next.with_timezone(&Utc))).await?,
            Err(e) => warn!(schedule = %id, "invalid cron `{expr}`: {e}"),
        }
    }

    // Windows-MCP is installed once, when a teammate first gets desktop control (never at run time).
    if ctx.desktop_ui
        && matches!(crate::tools::desktop_status(), crate::tools::Desktop::Missing)
        && ctx.db.any_desktop_bot().await.unwrap_or(false)
    {
        tokio::spawn(crate::tools::ensure_windows_mcp());
    }

    // Decided drafts → follow-up runs (queued behind whatever the teammate is doing); old undecided ones expire.
    if let Err(e) = crate::drafts::sweep(ctx).await {
        warn!("draft queue sweep failed: {e:#}");
    }

    // Safety net for missed cancel notifications.
    let ids: Vec<Uuid> = active.lock().unwrap().keys().copied().collect();
    for id in ids {
        check_cancel(ctx, active, id).await;
    }

    let free = ctx.cfg.max_parallel.saturating_sub(active.lock().unwrap().len());
    if free == 0 {
        return Ok(());
    }
    let throttled = ctx.throttled();
    let mut started = 0;
    for run in ctx.db.eligible_runs(free as i64 + 8).await? {
        if started == free {
            break;
        }
        // The owner's own actions (a message, a draft decision) still go through.
        if throttled && !matches!(run.kind.as_str(), "chat" | "followup") {
            continue;
        }
        if active.lock().unwrap().values().any(|a| a.bot == run.bot_id) {
            continue;
        }
        if !ctx.db.claim_run(run.id).await? {
            continue;
        }
        let cancel = CancellationToken::new();
        active.lock().unwrap().insert(run.id, Active { bot: run.bot_id, cancel: cancel.clone() });
        started += 1;
        let (ctx, active, wake) = (ctx.clone(), active.clone(), wake.clone());
        tokio::spawn(async move {
            let id = run.id;
            runner::execute(ctx, run, cancel).await;
            active.lock().unwrap().remove(&id);
            wake.notify_one();
        });
    }
    Ok(())
}

async fn claude_version(bin: &str) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version");
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = cmd.output().await.ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Single-owner install without `owner_id` configured: the account created in the app's first-run screen.
async fn wait_for_owner(pool: &sqlx::PgPool, shutdown: &CancellationToken) -> Result<Uuid> {
    let mut announced = false;
    loop {
        let ids: Vec<Uuid> = sqlx::query_scalar("select id from users order by created_at limit 2").fetch_all(pool).await?;
        match ids.as_slice() {
            [id] => return Ok(*id),
            [] => {}
            _ => anyhow::bail!("several accounts exist: set owner_id in the config"),
        }
        if !announced {
            info!("waiting for the owner account to be created in the app");
            announced = true;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = shutdown.cancelled() => anyhow::bail!("stopped before an account was created"),
        }
    }
}
