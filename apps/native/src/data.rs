//! App-wide live data: one [`AppData`] entity on the local Familiar API through a signed-in `familiar-client`.
//!
//! It holds what the shell shows everywhere (bots with their status, the overview, pending approvals, recent runs
//! across bots, schedules) plus the live text deltas of running runs. Reads go through the client's SWR cache (the
//! cached value is applied at once, the fresh one when it lands). One SSE connection (`client.stream()`) runs on the
//! gpui_tokio runtime; change notices are coalesced per table+scope over 150 ms (the client's [`Coalescer`]) and then
//! refresh only the affected pieces. Views observe the entity, and subscribe to [`DataEvent`] for their own data
//! (a thread's messages, a run's events).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use familiar_client::{
    Approval, ApprovalStatus, Bot, BotStatus, Client, Coalescer, Delta, DeltaKind, LiveEvent, Notice, Overview, Run,
    Schedule, resync_wins,
};
use familiar_ui::mascot::{Accessory, Avatar, MascotState, resolve_avatar};
use futures::StreamExt as _;
use gpui::{Context, EventEmitter, SharedString, Task};
use gpui_tokio::Tokio;
use serde::de::DeserializeOwned;
use uuid::Uuid;

pub use crate::engine::Mode;

/// How many bots Home reads runs and schedules for (the web's `slice(0, 10)`).
const HOME_BOTS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Ready,
    Failed(String),
}

/// Live (never persisted) output of a run, built from SSE deltas.
#[derive(Debug, Clone, Default)]
pub struct LiveBuf {
    pub text: String,
    pub thinking: String,
}

/// Emitted after the entity applied a live update, for views that keep their own data.
#[derive(Debug, Clone)]
pub enum DataEvent {
    /// A coalesced change notice; `None` is a resync (refetch everything you show).
    Changed(Option<Notice>),
    /// New delta text for this run.
    Delta(Uuid),
}

enum LiveMsg {
    Changed(Option<Notice>),
    Delta(Delta),
}

pub struct AppData {
    pub client: Client,
    /// Who runs the engine. Attached: Settings notes "Using the Familiar app's engine", and notifications come from
    /// this data (host mode has the daemon's signals).
    pub mode: Mode,
    pub status: Status,
    pub overview: Option<Overview>,
    pub pending: Vec<Approval>,
    /// Recent runs across the first bots, newest first.
    pub runs: Vec<Run>,
    pub runs_loaded: bool,
    pub schedules: Vec<Schedule>,
    pub live: HashMap<Uuid, LiveBuf>,
    /// Approvals already seen (`None` before the first load): a new one is a notification in attached mode.
    seen_approvals: Option<HashSet<Uuid>>,
    /// Runs active at the last load: one that has finished since is a notification in attached mode.
    active_runs: Option<HashSet<Uuid>>,
    _stream: Option<Task<Result<(), gpui_tokio::JoinError>>>,
    _pump: Option<Task<()>>,
}

impl EventEmitter<DataEvent> for AppData {}

impl AppData {
    /// `client` is already signed in (the [`crate::engine::Engine`] did that).
    pub fn new(client: Client, mode: Mode, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            client,
            mode,
            status: Status::Connecting,
            overview: None,
            pending: Vec::new(),
            runs: Vec::new(),
            runs_loaded: false,
            schedules: Vec::new(),
            live: HashMap::new(),
            seen_approvals: None,
            active_runs: None,
            _stream: None,
            _pump: None,
        };
        this.reload_all(cx);
        this.start_stream(cx);
        // Device heartbeats don't send change notices: look at the overview again shortly after boot (the daemon's
        // first heartbeat) and then every minute, so "Computer online" and the plan check stay current.
        cx.spawn(async move |this, cx| {
            for secs in [3u64, 10].into_iter().chain(std::iter::repeat(60)) {
                cx.background_executor().timer(Duration::from_secs(secs)).await;
                if this.update(cx, |d, cx| d.reload_overview(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        this
    }

    // ---- reads ------------------------------------------------------------------------------------------------

    pub fn bots(&self) -> &[Bot] {
        self.overview.as_ref().map(|o| o.bots.as_slice()).unwrap_or(&[])
    }

    pub fn bot(&self, id: Uuid) -> Option<&Bot> {
        self.bots().iter().find(|b| b.id == id)
    }

    /// In host mode the computer is this process: online while the hosted daemon runs, without waiting for its first
    /// heartbeat to reach the overview (a stale `last_seen_at` from the last session would read "offline").
    pub fn pc_online(&self) -> bool {
        if self.mode == Mode::Hosted
            && crate::engine::HOSTED.lock().unwrap().as_ref().is_some_and(|h| h.daemon_status().running)
        {
            return true;
        }
        self.overview.as_ref().is_some_and(|o| o.pc_online(Utc::now()))
    }

    /// A teammate just created here: list it at once (the overview reload brings its status).
    pub fn add_bot(&mut self, bot: Bot, cx: &mut Context<Self>) {
        if let Some(o) = self.overview.as_mut()
            && !o.bots.iter().any(|b| b.id == bot.id)
        {
            o.bots.push(bot);
        }
        // The cached overview predates it: don't let the stale copy un-list it while the fresh one loads.
        self.client.invalidate("/api/overview");
        self.reload_overview(cx);
        cx.notify();
    }

    /// A fresh row of a known teammate (an API answer): shown at once, ahead of the overview reload.
    pub fn put_bot(&mut self, bot: Bot, cx: &mut Context<Self>) {
        if let Some(b) = self.overview.as_mut().and_then(|o| o.bots.iter_mut().find(|b| b.id == bot.id)) {
            *b = bot;
            self.client.invalidate("/api/overview");
            cx.notify();
        }
    }

    /// The web's `stateOf(bot)`.
    pub fn state_of(&self, bot: &Bot) -> MascotState {
        if bot.effective_status() == BotStatus::Paused {
            MascotState::Paused
        } else if self.pending.iter().any(|a| a.bot_id == bot.id) {
            MascotState::NeedsYou
        } else if bot.status == Some(BotStatus::Running) {
            MascotState::Working
        } else if bot.last_run_at.is_some_and(|t| Utc::now() - t < chrono::Duration::minutes(10)) {
            MascotState::Done
        } else {
            MascotState::Idle
        }
    }

    pub fn teammate(&self, bot: &Bot) -> Teammate {
        Teammate {
            uuid: bot.id,
            id: bot.id.to_string().into(),
            name: bot.name.clone().into(),
            avatar: avatar_of(bot),
            state: self.state_of(bot),
            model: (!bot.model.is_empty()).then(|| bot.model.clone().into()),
        }
    }

    pub fn teammates(&self) -> Vec<Teammate> {
        self.bots().iter().map(|b| self.teammate(b)).collect()
    }

    // ---- loading ----------------------------------------------------------------------------------------------

    /// SWR read: apply the cached value now (if any), the fresh one when it lands.
    fn fetch<T>(&mut self, path: String, cx: &mut Context<Self>, apply: fn(&mut Self, T, &mut Context<Self>))
    where
        T: DeserializeOwned + Send + 'static,
    {
        if let Some(v) = self.client.peek::<T>(&path) {
            apply(self, v, cx);
        }
        let client = self.client.clone();
        let task = Tokio::spawn(cx, async move { client.get::<T>(&path).await });
        cx.spawn(async move |this, cx| match task.await {
            Ok(Ok(v)) => {
                let _ = this.update(cx, |this, cx| {
                    apply(this, v, cx);
                    cx.notify();
                });
            }
            Ok(Err(e)) => {
                tracing::warn!("fetch failed: {e}");
                let _ = this.update(cx, |this, cx| {
                    if this.overview.is_none() {
                        this.status = Status::Failed(e.message());
                        cx.notify();
                    }
                });
            }
            Err(_) => {}
        })
        .detach();
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        self.reload_overview(cx);
        self.reload_pending(cx);
    }

    pub fn reload_overview(&mut self, cx: &mut Context<Self>) {
        self.fetch("/api/overview".into(), cx, |this, o: Overview, cx| {
            let first = this.overview.is_none();
            let bots_changed = this.overview.as_ref().map(|old| ids(&old.bots)) != Some(ids(&o.bots));
            this.overview = Some(o);
            this.status = Status::Ready;
            if first || bots_changed {
                this.reload_runs(cx);
                this.reload_schedules(cx);
            }
        });
    }

    pub fn reload_pending(&mut self, cx: &mut Context<Self>) {
        self.fetch("/api/approvals?status=pending".into(), cx, |this, list: Vec<Approval>, cx| {
            this.pending = list.into_iter().filter(|a| a.status == ApprovalStatus::Pending).collect();
            this.alert_approvals(cx);
        });
    }

    fn home_paths(&self, f: impl Fn(Uuid) -> String) -> Vec<String> {
        self.bots().iter().take(HOME_BOTS).map(|b| f(b.id)).collect()
    }

    pub fn reload_runs(&mut self, cx: &mut Context<Self>) {
        let paths = self.home_paths(|id| format!("/api/bots/{id}/runs?limit=6"));
        if !self.runs_loaded {
            let cached: Vec<Run> = paths.iter().filter_map(|p| self.client.peek::<Vec<Run>>(p)).flatten().collect();
            if !cached.is_empty() {
                self.set_runs(cached);
            }
        }
        let client = self.client.clone();
        let task = Tokio::spawn(cx, async move {
            let all = futures::future::join_all(paths.iter().map(|p| client.get::<Vec<Run>>(p))).await;
            all.into_iter().flat_map(|r| r.unwrap_or_default()).collect::<Vec<Run>>()
        });
        cx.spawn(async move |this, cx| {
            if let Ok(runs) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.alert_runs(&runs, cx);
                    this.set_runs(runs);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn bot_name(&self, id: Uuid) -> String {
        self.bot(id).map(|b| b.name.clone()).unwrap_or_else(|| "A teammate".into())
    }

    fn alert_approvals(&mut self, cx: &mut Context<Self>) {
        let attached = self.mode == Mode::Attached;
        if let (true, Some(seen)) = (attached, self.seen_approvals.as_ref()) {
            for a in self.pending.iter().filter(|a| !seen.contains(&a.id)) {
                let bot = a.bot_name.clone().unwrap_or_else(|| self.bot_name(a.bot_id));
                crate::notify::show("needs", "approval", "Approval needed", format!("{bot} wants to use {}", a.tool_name), cx);
            }
        }
        self.seen_approvals.get_or_insert_default().extend(self.pending.iter().map(|a| a.id));
    }

    fn alert_runs(&mut self, runs: &[Run], cx: &mut Context<Self>) {
        if let (Mode::Attached, Some(before)) = (self.mode, self.active_runs.as_ref()) {
            for r in runs.iter().filter(|r| before.contains(&r.id) && !r.status.is_active()) {
                let (title, body) = crate::notify::finished(&self.bot_name(r.bot_id), r.status.as_str());
                crate::notify::show(&format!("bot:{}", r.bot_id), "finished", title, body, cx);
            }
        }
        self.active_runs = Some(runs.iter().filter(|r| r.status.is_active()).map(|r| r.id).collect());
    }

    fn set_runs(&mut self, mut runs: Vec<Run>) {
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        // Live buffers of runs that are known to have finished are dropped.
        for r in &runs {
            if !r.status.is_active() {
                self.live.remove(&r.id);
            }
        }
        self.runs = runs;
        self.runs_loaded = true;
    }

    pub fn reload_schedules(&mut self, cx: &mut Context<Self>) {
        let paths = self.home_paths(|id| format!("/api/bots/{id}/schedules"));
        let client = self.client.clone();
        let task = Tokio::spawn(cx, async move {
            let all = futures::future::join_all(paths.iter().map(|p| client.get::<Vec<Schedule>>(p))).await;
            all.into_iter().flat_map(|r| r.unwrap_or_default()).collect::<Vec<Schedule>>()
        });
        cx.spawn(async move |this, cx| {
            if let Ok(s) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.schedules = s;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Decide an approval (`response`: the answer to an `ask_user`). Refreshes pending + overview after.
    pub fn decide(
        &mut self,
        id: Uuid,
        approve: bool,
        response: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        // Hide it at once; a failure brings it back on the reload.
        self.pending.retain(|a| a.id != id);
        cx.notify();
        let client = self.client.clone();
        let task = Tokio::spawn(cx, async move { client.decide_approval(id, approve, response.as_deref()).await });
        cx.spawn(async move |this, cx| {
            let r = match task.await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(e.message()),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                this.reload_pending(cx);
                this.reload_overview(cx);
            });
            r
        })
    }

    // ---- live ---------------------------------------------------------------------------------------------------

    fn start_stream(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<LiveMsg>();
        let client = self.client.clone();
        self._stream = Some(Tokio::spawn(cx, async move {
            let notices = tx.clone();
            let coalescer = Coalescer::new(Coalescer::<String, Option<Notice>>::DEFAULT_WINDOW, resync_wins, move |_, n| {
                let _ = notices.unbounded_send(LiveMsg::Changed(n));
            });
            let mut stream = std::pin::pin!(client.stream());
            while let Some(ev) = stream.next().await {
                match ev {
                    LiveEvent::Delta(d) => {
                        let _ = tx.unbounded_send(LiveMsg::Delta(d));
                    }
                    LiveEvent::Notice(n) => {
                        let scope = n.run.clone().or_else(|| n.bot.clone()).unwrap_or_default();
                        coalescer.push(format!("{}|{scope}", n.t), Some(n));
                    }
                    LiveEvent::Resync => coalescer.push("*".into(), None),
                }
            }
            // Keep the coalescer alive until its last window closes.
            tokio::time::sleep(Duration::from_millis(200)).await;
        }));
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                if this.update(cx, |this, cx| this.on_live(msg, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn on_live(&mut self, msg: LiveMsg, cx: &mut Context<Self>) {
        match msg {
            LiveMsg::Delta(d) => {
                let Ok(run) = d.run.parse::<Uuid>() else { return };
                let buf = self.live.entry(run).or_default();
                match d.kind {
                    DeltaKind::Thinking => buf.thinking.push_str(&d.text),
                    _ => buf.text.push_str(&d.text),
                }
                cx.emit(DataEvent::Delta(run));
                cx.notify();
            }
            LiveMsg::Changed(n) => {
                match n.as_ref().map(|n| n.t.as_str()) {
                    None => {
                        self.reload_all(cx);
                        self.reload_runs(cx);
                        self.reload_schedules(cx);
                    }
                    Some("runs") => {
                        self.reload_runs(cx);
                        self.reload_overview(cx);
                    }
                    Some("approvals") => {
                        self.reload_pending(cx);
                        self.reload_overview(cx);
                    }
                    Some("bots") => self.reload_overview(cx),
                    Some("schedules") => self.reload_schedules(cx),
                    _ => {}
                }
                cx.emit(DataEvent::Changed(n));
            }
        }
    }
}

fn ids(bots: &[Bot]) -> Vec<Uuid> {
    bots.iter().map(|b| b.id).collect()
}

pub fn avatar_of(bot: &Bot) -> Avatar {
    let stored = bot.avatar.as_ref().and_then(|a| serde_json::to_value(a).ok());
    resolve_avatar(&bot.id.to_string(), stored.as_ref())
}

// ---- formatting (the web's lib/util.ts) ----------------------------------------------------------------------------

/// `ago(iso)`: "just now", "5m ago", "3h ago", "2d ago"; `None` is "never".
pub fn ago(t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else { return "never".into() };
    let s = (Utc::now() - t).num_milliseconds().max(0) as f64 / 1000.0;
    let s = s.round();
    if s < 45.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{}m ago", (s / 60.0).round())
    } else if s < 86400.0 {
        format!("{}h ago", (s / 3600.0).round())
    } else {
        format!("{}d ago", (s / 86400.0).round())
    }
}

/// `until(iso)`: "due", "in 5m", "in 3h", "in 2d"; `None` is "pending".
pub fn until(t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else { return "pending".into() };
    let s = ((t - Utc::now()).num_milliseconds() as f64 / 1000.0).round();
    if s <= 0.0 {
        "due".into()
    } else if s < 3600.0 {
        format!("in {}m", (s / 60.0).round().max(1.0))
    } else if s < 86400.0 {
        format!("in {}h", (s / 3600.0).round())
    } else {
        format!("in {}d", (s / 86400.0).round())
    }
}

/// `excerpt(s, n)`: whitespace collapsed, trimmed, cut to `n` chars with an ellipsis.
pub fn excerpt(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n { format!("{}…", flat.chars().take(n.saturating_sub(1)).collect::<String>()) } else { flat }
}

/// The last `n` chars of `s`.
pub fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

pub fn run_status(s: familiar_client::RunStatus) -> familiar_ui::components::RunStatus {
    use familiar_client::RunStatus as C;
    use familiar_ui::components::RunStatus as U;
    match s {
        C::Queued => U::Queued,
        C::Running => U::Running,
        C::WaitingApproval => U::WaitingApproval,
        C::Succeeded => U::Succeeded,
        C::Failed => U::Failed,
        C::Cancelled | C::Unknown => U::Cancelled,
    }
}

// ---- teammates -------------------------------------------------------------------------------------------------------

/// A bot as the shell draws it.
#[derive(Debug, Clone)]
pub struct Teammate {
    pub uuid: Uuid,
    pub id: SharedString,
    pub name: SharedString,
    pub avatar: Avatar,
    pub state: MascotState,
    pub model: Option<SharedString>,
}

/// Sample teammates for the gallery: one in every state, a spread of shapes and accessories.
pub fn sample_teammates() -> Vec<Teammate> {
    let mk = |id: &str, name: &str, shape, color, eyes, mouth, accessory, state| Teammate {
        uuid: Uuid::nil(),
        id: id.to_owned().into(),
        name: name.to_owned().into(),
        avatar: Avatar { shape, color, eyes, mouth, accessory },
        state,
        model: Some("sonnet".into()),
    };
    vec![
        mk("s-ada", "Ada", 0, 0x7285d5, 1, 0, Accessory::Headphones, MascotState::Working),
        mk("s-milo", "Milo", 2, 0xeda84b, 0, 2, Accessory::None, MascotState::NeedsYou),
        mk("s-juniper", "Juniper", 1, 0x4fb98a, 3, 3, Accessory::Bow, MascotState::Done),
        mk("s-pip", "Pip", 4, 0xa283d8, 2, 1, Accessory::Antenna, MascotState::Idle),
        mk("s-otto", "Otto", 3, 0x4fa9cf, 4, 0, Accessory::Glasses, MascotState::Paused),
        mk("s-rue", "Rue", 0, 0xe58fa4, 0, 3, Accessory::Crown, MascotState::Idle),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_collapses_and_cuts() {
        assert_eq!(excerpt("  a \n b  ", 90), "a b");
        assert_eq!(excerpt("abcdef", 4), "abc…");
        assert_eq!(tail("hello", 3), "llo");
    }
}

/// Stale-while-revalidate for any view: apply the cached value of `path` now (if any), the fresh one when it lands
/// (then notify). Errors are logged and leave the current value.
pub fn swr<V: 'static, T>(
    this: &mut V,
    client: &Client,
    path: String,
    cx: &mut Context<V>,
    apply: impl Fn(&mut V, T, &mut Context<V>) + 'static,
) where
    T: DeserializeOwned + Send + 'static,
{
    if let Some(v) = client.peek::<T>(&path) {
        apply(this, v, cx);
    }
    let client = client.clone();
    let task = Tokio::spawn(cx, async move { client.get::<T>(&path).await });
    cx.spawn(async move |this, cx| match task.await {
        Ok(Ok(v)) => {
            let _ = this.update(cx, |this, cx| {
                apply(this, v, cx);
                cx.notify();
            });
        }
        Ok(Err(e)) => tracing::warn!("fetch failed: {e}"),
        Err(_) => {}
    })
    .detach();
}
