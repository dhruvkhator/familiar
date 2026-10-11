//! Each bot's browser: a headless Chrome the daemon runs itself (persistent profile, logins survive), shared with
//! Playwright MCP over CDP.
//!
//! **Live view.** While someone watches (the desktop app's computer panel, through the API running in this process:
//! [`local`]), the page is screencast over CDP (`Page.startScreencast`): Chrome sends a JPEG only when the page changes,
//! each frame is acknowledged at most every [`SCREENCAST_MIN_GAP`] (~30 fps), and viewers always get the newest frame
//! (stale ones are dropped, never queued). Without a viewer the screencast stops. If the screencast fails, viewers get
//! the old screenshots (`Page.captureScreenshot` every [`FRAME_EVERY`]). While a run is active, or you have taken
//! over, a frame also goes into `live_frames` about every second (the web app, the `live_frames` notices, and "is it
//! browsing"). Your clicks and typing are replayed over CDP (take-over), in order.
//!
//! **Signing in.** [`Manager::open_window`] stops the headless browser and opens the same profile as a normal, visible
//! Chrome window (no DevTools port, no automation switches), so the owner signs in to sites in a real browser. While it
//! is open the teammate's runs wait ([`Manager::begin_run`]) and its headless browser stays off; once the window's
//! process ends, the next use starts headless again on the same profile (cookies kept). Two browsers never share a
//! profile: the gate, the PID file and the profile lock are all checked first.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::sync::{Mutex, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::db::Db;

/// The screenshot fallback's pace (and the `live_frames` row's).
const FRAME_EVERY: Duration = Duration::from_millis(900);
/// After you stop interacting, keep streaming this long (so the result of your last click shows up).
const TAKEOVER_LINGER: Duration = Duration::from_secs(120);
/// A screencast frame is acknowledged no sooner than this after the previous one; Chrome sends the next frame only
/// after the ack and its next compositor frame, so this caps the live view at ~30 fps (measured: frames 33 ms apart
/// on a page that changes every frame), and it costs nothing while the page doesn't change. The same delayed-ack
/// throttling as Playwright's screencast `FrameThrottler` (Apache-2.0).
pub const SCREENCAST_MIN_GAP: Duration = Duration::from_millis(30);
const SCREENCAST_QUALITY: u32 = 65;
/// How often a screencast checks which tab is in front (it follows a new one) and its address and title.
const TARGET_CHECK: Duration = Duration::from_secs(1);
/// The live view's picture size when the viewer doesn't say (the browser's window size).
const DEFAULT_SIZE: (u32, u32) = (1280, 800);

#[derive(Default)]
pub struct Manager {
    bots: Mutex<HashMap<Uuid, Arc<BotBrowser>>>,
    /// Live-view viewers and frames per bot; kept while the browser restarts, so a viewer stays subscribed.
    hubs: std::sync::Mutex<HashMap<Uuid, Arc<Hub>>>,
    gate: std::sync::Mutex<Gate>,
}

struct BotBrowser {
    port: u16,
    child: Mutex<Option<Child>>,
    /// The take-over connection (input, screenshots); the screencast has its own.
    cdp: Mutex<Option<Cdp>>,
    state: std::sync::Mutex<Activity>,
    hub: Arc<Hub>,
    /// Ends the screencast supervisor when this browser is stopped.
    stop: CancellationToken,
}

#[derive(Default)]
struct Activity {
    runs: usize,
    last_input: Option<Instant>,
    streaming: bool,
}

// ---- who uses a profile ---------------------------------------------------------------------------------------------

/// Who is using each teammate's browser profile: its runs (headless) or the owner's sign-in window. Never both.
#[derive(Default)]
struct Gate(HashMap<Uuid, Uses>);

#[derive(Default)]
struct Uses {
    runs: usize,
    window: bool,
    /// The window's process, once started.
    pid: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
enum Opening {
    Go,
    AlreadyOpen,
    Busy,
}

impl Gate {
    fn begin_run(&mut self, bot: Uuid) -> bool {
        let u = self.0.entry(bot).or_default();
        if u.window {
            return false;
        }
        u.runs += 1;
        true
    }

    fn end_run(&mut self, bot: Uuid) {
        if let Some(u) = self.0.get_mut(&bot) {
            u.runs = u.runs.saturating_sub(1);
        }
    }

    fn open_window(&mut self, bot: Uuid) -> Opening {
        let u = self.0.entry(bot).or_default();
        if u.window {
            Opening::AlreadyOpen
        } else if u.runs > 0 {
            Opening::Busy
        } else {
            u.window = true;
            Opening::Go
        }
    }

    fn window_closed(&mut self, bot: Uuid) {
        if let Some(u) = self.0.get_mut(&bot) {
            u.window = false;
            u.pid = None;
        }
    }

    fn window(&self, bot: Uuid) -> bool {
        self.0.get(&bot).is_some_and(|u| u.window)
    }
}

/// Why the sign-in window didn't open.
#[derive(Debug, PartialEq, Eq)]
pub enum WindowError {
    /// A run of this teammate is using its browser.
    Busy,
    /// No Chrome or Edge on this computer.
    NoBrowser,
    Failed(String),
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::Busy => f.write_str("it is using its browser right now; try again when its run is done"),
            WindowError::NoBrowser => f.write_str("install Google Chrome or Microsoft Edge to sign in in a window"),
            WindowError::Failed(e) => f.write_str(e),
        }
    }
}

// ---- live-view frames -----------------------------------------------------------------------------------------------

/// One picture of the live view.
#[derive(Debug)]
pub struct Frame {
    /// Increases with every frame of this bot's view (across browser restarts).
    pub seq: u64,
    pub jpeg: Vec<u8>,
    /// The page's viewport in CSS pixels; take-over coordinates are in these (the picture may be scaled).
    pub view_width: u32,
    pub view_height: u32,
    pub url: String,
    pub title: String,
    pub at: Instant,
}

/// What the viewers want: how many there are and the biggest picture any of them can show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Want {
    pub viewers: usize,
    pub width: u32,
    pub height: u32,
}

struct Hub {
    frame: watch::Sender<Option<Arc<Frame>>>,
    want: watch::Sender<Want>,
    viewers: std::sync::Mutex<(u64, HashMap<u64, (u32, u32)>)>,
    seq: AtomicU64,
    /// A screencast is running (its frames are fresh whenever the page changes).
    casting: AtomicBool,
}

impl Default for Hub {
    fn default() -> Self {
        Hub {
            frame: watch::channel(None).0,
            want: watch::channel(Want::default()).0,
            viewers: Default::default(),
            seq: AtomicU64::new(0),
            casting: AtomicBool::new(false),
        }
    }
}

impl Hub {
    fn subscribe(self: &Arc<Self>, width: u32, height: u32) -> Viewer {
        let size = clamp_size(width, height);
        let id = {
            let mut v = self.viewers.lock().unwrap();
            v.0 += 1;
            let id = v.0;
            v.1.insert(id, size);
            self.want.send_replace(want_of(&v.1));
            id
        };
        let mut frames = self.frame.subscribe();
        // The newest picture right away (a page that doesn't change sends nothing new).
        if frames.borrow().is_some() {
            frames.mark_changed();
        }
        Viewer { frames, hub: self.clone(), id }
    }

    fn publish(&self, jpeg: Vec<u8>, view: (u32, u32), url: &str, title: &str) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let f = Frame {
            seq,
            jpeg,
            view_width: view.0,
            view_height: view.1,
            url: url.to_owned(),
            title: title.to_owned(),
            at: Instant::now(),
        };
        self.frame.send_replace(Some(Arc::new(f)));
        seq
    }

    fn latest(&self) -> Option<Arc<Frame>> {
        self.frame.borrow().clone()
    }

    fn casting(self: &Arc<Self>) -> CastingGuard {
        self.casting.store(true, Ordering::SeqCst);
        CastingGuard(self.clone())
    }
}

struct CastingGuard(Arc<Hub>);

impl Drop for CastingGuard {
    fn drop(&mut self) {
        self.0.casting.store(false, Ordering::SeqCst);
    }
}

fn clamp_size(w: u32, h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return DEFAULT_SIZE;
    }
    (w.clamp(160, 2560), h.clamp(120, 1600))
}

fn want_of(viewers: &HashMap<u64, (u32, u32)>) -> Want {
    Want {
        viewers: viewers.len(),
        width: viewers.values().map(|s| s.0).max().unwrap_or(0),
        height: viewers.values().map(|s| s.1).max().unwrap_or(0),
    }
}

/// Someone watching a bot's live view: the newest frame (older ones are skipped, never queued). The screencast runs
/// while at least one viewer exists; dropping the viewer unsubscribes.
pub struct Viewer {
    pub frames: watch::Receiver<Option<Arc<Frame>>>,
    hub: Arc<Hub>,
    id: u64,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut v = self.hub.viewers.lock().unwrap();
        v.1.remove(&self.id);
        self.hub.want.send_replace(want_of(&v.1));
    }
}

/// Paces screencast acks: no sooner than `gap` after the previous one.
struct Pacer {
    gap: Duration,
    last: Option<Instant>,
}

impl Pacer {
    fn new(gap: Duration) -> Self {
        Pacer { gap, last: None }
    }

    /// How long to wait before acknowledging a frame that arrived at `now`.
    fn delay(&self, now: Instant) -> Duration {
        self.last.map_or(Duration::ZERO, |l| self.gap.saturating_sub(now.saturating_duration_since(l)))
    }

    fn acked(&mut self, at: Instant) {
        self.last = Some(at);
    }
}

/// A `Page.screencastFrame` event: (session id to ack, JPEG as base64, viewport in CSS pixels).
fn screencast_frame(v: &Value) -> Option<(i64, &str, (u32, u32))> {
    if v["method"] != "Page.screencastFrame" {
        return None;
    }
    let p = &v["params"];
    let meta = &p["metadata"];
    let view = (meta["deviceWidth"].as_f64().unwrap_or(0.0).round() as u32, meta["deviceHeight"].as_f64().unwrap_or(0.0).round() as u32);
    Some((p["sessionId"].as_i64()?, p["data"].as_str()?, view))
}

fn screencast_params(w: Want) -> Value {
    let (width, height) = clamp_size(w.width, w.height);
    json!({ "format": "jpeg", "quality": SCREENCAST_QUALITY, "maxWidth": width, "maxHeight": height, "everyNthFrame": 1 })
}

// ---- the in-process link ----------------------------------------------------------------------------------------------

/// The daemon running in this process, for the API in the same process (the desktop app): the live stream and the
/// sign-in window. A hosted API has none (its daemon runs on another computer).
#[derive(Clone)]
pub struct Local {
    db: Db,
    manager: Arc<Manager>,
    profiles: PathBuf,
    bin: Option<String>,
}

static LOCAL: std::sync::Mutex<Vec<Local>> = std::sync::Mutex::new(Vec::new());

/// The daemon of `owner` in this process, if any.
pub fn local(owner: Uuid) -> Option<Local> {
    LOCAL.lock().unwrap().iter().rev().find(|l| l.db.owner == owner).cloned()
}

/// Registered until dropped.
pub struct LocalGuard(Arc<Manager>);

impl Drop for LocalGuard {
    fn drop(&mut self) {
        LOCAL.lock().unwrap().retain(|l| !Arc::ptr_eq(&l.manager, &self.0));
        // Viewers still subscribed get no more frames from this daemon: end their streams.
        for hub in self.0.hubs.lock().unwrap().values() {
            hub.frame.send_replace(None);
        }
    }
}

/// Offer this daemon's browsers to the API in this process (see [`local`]).
pub fn register(db: Db, manager: Arc<Manager>, profiles: PathBuf, bin: Option<String>) -> LocalGuard {
    LOCAL.lock().unwrap().push(Local { db, manager: manager.clone(), profiles, bin });
    LocalGuard(manager)
}

impl Local {
    /// Watch a bot's live view; `width`×`height` is the biggest picture the viewer can show (0 = the browser's size).
    pub fn watch(&self, bot: Uuid, width: u32, height: u32) -> Viewer {
        self.manager.hub(bot).subscribe(width, height)
    }

    /// The owner's sign-in window for this bot is open.
    pub fn window_open(&self, bot: Uuid) -> bool {
        self.manager.signing_in(bot)
    }

    /// Open the bot's browser profile in a normal Chrome window at `url` (see [`Manager::open_window`]).
    pub async fn open_window(&self, bot: Uuid, url: Option<&str>) -> Result<(), WindowError> {
        let b = self.db.bot(bot).await.map_err(|_| WindowError::Failed("no such teammate".into()))?;
        self.manager.open_window(&self.db, bot, &b.slug, self.profiles.clone(), self.bin.as_deref(), url).await
    }

    /// Close the bot's sign-in window (see [`Manager::close_sign_in_window`]).
    pub async fn close_window(&self, bot: Uuid) -> bool {
        self.manager.close_sign_in_window(bot).await
    }
}

// ---- the manager ------------------------------------------------------------------------------------------------------

impl Manager {
    fn hub(&self, bot: Uuid) -> Arc<Hub> {
        self.hubs.lock().unwrap().entry(bot).or_default().clone()
    }

    /// May a run of this bot start? Not while the owner signs in in its window. Pair with [`Manager::end_run`].
    pub fn begin_run(&self, bot: Uuid) -> bool {
        self.gate.lock().unwrap().begin_run(bot)
    }

    pub fn end_run(&self, bot: Uuid) {
        self.gate.lock().unwrap().end_run(bot);
    }

    /// The owner's sign-in window for this bot is open (its runs wait, its headless browser stays off).
    pub fn signing_in(&self, bot: Uuid) -> bool {
        self.gate.lock().unwrap().window(bot)
    }

    /// Start (or reuse) the bot's Chrome. Returns the CDP endpoint for Playwright MCP, or None when no Chrome/Edge
    /// is installed (Playwright then launches its own headless browser and there is no live view) or while the owner's
    /// sign-in window has the profile.
    pub async fn ensure(&self, bot: Uuid, slug: &str, profiles: PathBuf, bin: Option<&str>) -> Option<String> {
        let mut bots = self.bots.lock().await;
        if self.signing_in(bot) {
            return None;
        }
        if let Some(b) = bots.get(&bot) {
            if b.alive().await {
                return Some(b.endpoint());
            }
            b.stop.cancel();
        }
        let exe = bin.map(PathBuf::from).or_else(find_chrome)?;
        let port = free_port().ok()?;
        let profile = profiles.join(slug);
        let _ = std::fs::create_dir_all(&profile);
        // A Chrome left over from a crash still holds this profile; a new one would just hand off to it and exit.
        kill_leftover(&profile).await;
        // Still held (the owner's sign-in window from a relaunched process, say): never share a profile between two
        // browsers. The run goes on without its browser (the runner's fallback) rather than risk the profile.
        if profile_in_use(&profile) {
            warn!(bot = %slug, "its browser profile is still in use; not starting a second browser on it");
            return None;
        }
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--headless=new");
        // Headless Chrome says "HeadlessChrome" in its user agent, and sites such as x.com refuse it outright (HTTP 403),
        // so the teammate could never sign in. Send what a normal Chrome of the same version sends.
        if let Some(ua) = normal_user_agent(&exe).await {
            cmd.arg(format!("--user-agent={ua}"));
        }
        cmd.arg(format!("--remote-debugging-port={port}"))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(["--window-size=1280,800", "--no-first-run", "--no-default-browser-check", "--disable-sync"])
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!("could not start {}: {e}", exe.display());
                return None;
            }
        };
        let b = Arc::new(BotBrowser {
            port,
            child: Mutex::new(Some(child)),
            cdp: Mutex::new(None),
            state: Default::default(),
            hub: self.hub(bot),
            stop: CancellationToken::new(),
        });
        if let Some(pid) = b.child.lock().await.as_ref().and_then(|c| c.id()) {
            let _ = std::fs::write(profile.join(PID_FILE), pid.to_string());
        }
        // Wait until the DevTools endpoint answers; if it never does, fall back rather than hand out a dead endpoint.
        let mut up = false;
        for _ in 0..50 {
            if b.alive().await {
                up = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !up {
            warn!(bot = %slug, "browser did not start; using Playwright's own browser (no live view)");
            b.close().await;
            return None;
        }
        info!(bot = %slug, port, "browser started");
        tokio::spawn(supervise(Arc::downgrade(&b), b.hub.clone(), b.stop.clone()));
        let endpoint = b.endpoint();
        bots.insert(bot, b);
        Some(endpoint)
    }

    /// A run started using this bot's browser: stream frames until it ends.
    pub async fn run_started(self: &Arc<Self>, db: &Db, bot: Uuid) {
        if let Some(b) = self.bots.lock().await.get(&bot).cloned() {
            b.state.lock().unwrap().runs += 1;
            self.stream(db.clone(), bot, b);
        }
    }

    pub async fn run_finished(&self, bot: Uuid) {
        if let Some(b) = self.bots.lock().await.get(&bot) {
            let mut s = b.state.lock().unwrap();
            s.runs = s.runs.saturating_sub(1);
        }
    }

    /// Owner input from the computer panel ("take over"). Works between runs too.
    pub async fn input(self: &Arc<Self>, db: &Db, bot: Uuid, ev: &Value) -> Result<()> {
        let b = self.bots.lock().await.get(&bot).cloned().context("this bot's browser is not running")?;
        b.state.lock().unwrap().last_input = Some(Instant::now());
        self.stream(db.clone(), bot, b.clone());
        let mut guard = b.cdp.lock().await;
        let result = Self::dispatch(b.connect(&mut guard).await?, ev).await;
        if result.is_err() {
            *guard = None; // a dead socket must not be reused for the next input
        }
        result
    }

    async fn dispatch(cdp: &mut Cdp, ev: &Value) -> Result<()> {
        let (x, y) = (ev["x"].as_f64().unwrap_or(0.0), ev["y"].as_f64().unwrap_or(0.0));
        match ev["type"].as_str().unwrap_or_default() {
            "click" => {
                for kind in ["mousePressed", "mouseReleased"] {
                    cdp.call("Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }))
                        .await?;
                }
            }
            "move" => {
                cdp.call("Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": x, "y": y })).await?;
            }
            "type" => {
                cdp.call("Input.insertText", json!({ "text": ev["text"].as_str().unwrap_or_default() })).await?;
            }
            "key" => {
                let key = ev["key"].as_str().unwrap_or("Enter");
                let (down, up) = key_events(key);
                cdp.call("Input.dispatchKeyEvent", down).await?;
                cdp.call("Input.dispatchKeyEvent", up).await?;
            }
            "scroll" => {
                let dy = ev["dy"].as_f64().unwrap_or(400.0);
                cdp.call("Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": dy }))
                    .await?;
            }
            "navigate" => {
                let url = ev["url"].as_str().unwrap_or_default();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    bail!("only http(s) URLs");
                }
                cdp.call("Page.navigate", json!({ "url": url })).await?;
            }
            other => bail!("unknown input `{other}`"),
        }
        Ok(())
    }

    /// Start the `live_frames` loop for a bot unless it is already running.
    fn stream(self: &Arc<Self>, db: Db, bot: Uuid, b: Arc<BotBrowser>) {
        {
            let mut s = b.state.lock().unwrap();
            if s.streaming {
                return;
            }
            s.streaming = true;
        }
        tokio::spawn(async move {
            let mut last_hash = 0u64;
            loop {
                {
                    // Decide and clear `streaming` under one lock, so a run starting right now can't be missed.
                    let mut s = b.state.lock().unwrap();
                    let keep = !b.stop.is_cancelled()
                        && (s.runs > 0 || s.last_input.is_some_and(|t| t.elapsed() < TAKEOVER_LINGER));
                    if !keep {
                        s.streaming = false;
                        break;
                    }
                }
                // While a screencast runs, its newest frame is the picture (no extra screenshot).
                let shot = match b.hub.latest().filter(|_| b.hub.casting.load(Ordering::SeqCst)) {
                    Some(f) => Ok((f.jpeg.clone(), f.url.clone(), f.title.clone(), f.view_width as i64, f.view_height as i64)),
                    None => b.frame().await,
                };
                match shot {
                    Ok((jpeg, url, title, w, h)) => {
                        let hash = hash_of(&jpeg);
                        if hash != last_hash {
                            last_hash = hash;
                            if let Err(e) = save_frame(&db, bot, &jpeg, &url, &title, w, h).await {
                                warn!("live frame save failed: {e:#}");
                            }
                        }
                    }
                    Err(e) => {
                        *b.cdp.lock().await = None; // reconnect next time (tab closed, target changed…)
                        debug!("live frame: {e:#}");
                    }
                }
                tokio::time::sleep(FRAME_EVERY).await;
            }
        });
    }

    /// "Sign in in a window": stop the bot's headless browser and open its profile as a normal, visible Chrome window
    /// at `url` (no DevTools port, no automation switches). Refused while a run of the bot is using the browser. While
    /// the window is open the bot's runs wait; when its process ends, the next use starts headless again on the same
    /// profile. With the window already open, `url` opens in a new tab of it.
    pub async fn open_window(
        self: &Arc<Self>,
        db: &Db,
        bot: Uuid,
        slug: &str,
        profiles: PathBuf,
        bin: Option<&str>,
        url: Option<&str>,
    ) -> Result<(), WindowError> {
        if let Some(u) = url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            return Err(WindowError::Failed("only http(s) pages".into()));
        }
        let profile = profiles.join(slug);
        let opening = self.gate.lock().unwrap().open_window(bot);
        if opening == Opening::Busy {
            return Err(WindowError::Busy);
        }
        let Some(exe) = bin.map(PathBuf::from).or_else(find_chrome).filter(|p| p.is_file()) else {
            if opening == Opening::Go {
                self.gate.lock().unwrap().window_closed(bot);
            }
            return Err(WindowError::NoBrowser);
        };
        match opening {
            Opening::Busy => unreachable!(),
            Opening::AlreadyOpen => {
                // A second start on the same profile hands the page to the open window (a new tab) and exits.
                if url.is_some() {
                    let mut cmd = window_command(&exe, &profile, url);
                    cmd.kill_on_drop(false);
                    if let Ok(mut c) = cmd.spawn() {
                        tokio::spawn(async move { c.wait().await });
                    }
                }
                return Ok(());
            }
            Opening::Go => {}
        }
        let started = async {
            // The headless browser lets go of the profile first.
            if let Some(b) = self.bots.lock().await.remove(&bot) {
                b.close().await;
            }
            let _ = std::fs::create_dir_all(&profile);
            kill_leftover(&profile).await;
            wait_until_free(&profile).await?;
            window_command(&exe, &profile, url).spawn().map_err(|e| WindowError::Failed(format!("could not start {}: {e}", exe.display())))
        }
        .await;
        let mut child = match started {
            Ok(c) => c,
            Err(e) => {
                self.gate.lock().unwrap().window_closed(bot);
                return Err(e);
            }
        };
        let pid = child.id();
        self.gate.lock().unwrap().0.entry(bot).or_default().pid = pid;
        if let Some(pid) = pid {
            let _ = std::fs::write(profile.join(PID_FILE), pid.to_string());
        }
        info!(bot = %slug, "sign-in window opened");
        notify_window(db, bot).await;
        let (me, db) = (self.clone(), db.clone());
        tokio::spawn(async move {
            let _ = child.wait().await;
            if pid.is_some_and(|p| std::fs::read_to_string(profile.join(PID_FILE)).is_ok_and(|s| s.trim() == p.to_string())) {
                let _ = std::fs::remove_file(profile.join(PID_FILE));
            }
            // The launched process can end while another Chrome process keeps the profile (a relaunch to drop admin
            // rights or to update, a crash restart): the window counts as open until nothing holds the profile, however
            // long that takes, so a headless start can never share it.
            while profile_in_use(&profile) {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            me.gate.lock().unwrap().window_closed(bot);
            info!(%bot, "sign-in window closed");
            notify_window(&db, bot).await;
        });
        Ok(())
    }

    /// "Done": close the bot's sign-in window the way a person would (Chrome saves cookies first). On macOS closing the
    /// last window leaves Chrome running, so this (or quitting Chrome) is how the owner hands the profile back there.
    /// False when no window is open.
    pub async fn close_sign_in_window(&self, bot: Uuid) -> bool {
        let pid = self.gate.lock().unwrap().0.get(&bot).filter(|u| u.window).and_then(|u| u.pid);
        match pid {
            Some(pid) => {
                close_window(pid).await;
                true
            }
            None => false,
        }
    }

    pub async fn shutdown(&self) {
        for (_, b) in self.bots.lock().await.drain() {
            b.close().await;
        }
        // Sign-in windows close the way a person closes them (Chrome saves the session), then for sure.
        let pids: Vec<u32> = self.gate.lock().unwrap().0.values().filter_map(|u| u.pid).collect();
        for pid in pids {
            close_window(pid).await;
        }
    }
}

/// A key press as CDP key events (down, up). Enter carries its text, so it submits forms like a real key does.
fn key_events(key: &str) -> (Value, Value) {
    let (code, vk, text) = match key {
        "Enter" => ("Enter", 13, Some("\r")),
        "Tab" => ("Tab", 9, None),
        "Backspace" => ("Backspace", 8, None),
        "Delete" => ("Delete", 46, None),
        "Escape" => ("Escape", 27, None),
        "ArrowDown" => ("ArrowDown", 40, None),
        "ArrowUp" => ("ArrowUp", 38, None),
        "ArrowLeft" => ("ArrowLeft", 37, None),
        "ArrowRight" => ("ArrowRight", 39, None),
        "Home" => ("Home", 36, None),
        "End" => ("End", 35, None),
        "PageUp" => ("PageUp", 33, None),
        "PageDown" => ("PageDown", 34, None),
        _ => (key, 0, None),
    };
    let mut down = json!({ "type": if text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": key, "code": code, "windowsVirtualKeyCode": vk });
    if let Some(t) = text {
        down["text"] = json!(t);
        down["unmodifiedText"] = json!(t);
    }
    (down, json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk }))
}

/// The sign-in window: a normal Chrome on the teammate's profile. No DevTools port, no headless or automation
/// switches and Chrome's own user agent, so sites see the browser a person uses.
fn window_args(profile: &Path, url: Option<&str>) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-sync".to_owned(),
        // Closing the window ends the browser (background apps would keep the profile busy).
        "--disable-background-mode".to_owned(),
        "--new-window".to_owned(),
    ];
    args.extend(url.map(str::to_owned));
    args
}

fn window_command(exe: &Path, profile: &Path, url: Option<&str>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(window_args(profile, url))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    cmd
}

/// Tell the app the sign-in window opened or closed (`live_window` notice), and wake the scheduler for waiting runs.
async fn notify_window(db: &Db, bot: Uuid) {
    let payload = json!({ "t": "live_window", "id": bot, "bot": bot, "owner": db.owner, "op": "UPDATE" });
    if let Err(e) = sqlx::query("select pg_notify('familiar', $1)").bind(payload.to_string()).execute(&db.pool).await {
        warn!("live_window notice failed: {e}");
    }
}

/// Close a sign-in window the way a person would (Chrome saves cookies and the session), then for sure after 30 s:
/// Chrome commits its cookie store on shutdown, and a hurried kill could lose the sign-in the window was opened for.
async fn close_window(pid: u32) {
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("taskkill");
        c.args(["/PID", &pid.to_string()]).creation_flags(0x0800_0000);
        let _ = c.output().await;
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::process::Command::new("kill").args(["-TERM", &pid.to_string()]).output().await;
    }
    for _ in 0..300 {
        if !pid_alive(pid).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("taskkill");
        c.args(["/T", "/F", "/PID", &pid.to_string()]).creation_flags(0x0800_0000);
        let _ = c.output().await;
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::process::Command::new("kill").args(["-KILL", &pid.to_string()]).output().await;
    }
}

async fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let mut q = tokio::process::Command::new("tasklist");
        q.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]).creation_flags(0x0800_0000);
        q.output().await.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }
    #[cfg(not(windows))]
    {
        tokio::process::Command::new("kill").args(["-0", &pid.to_string()]).output().await.is_ok_and(|o| o.status.success())
    }
}

/// Wait (up to 5 s) until no browser holds the profile.
async fn wait_until_free(profile: &Path) -> Result<(), WindowError> {
    for _ in 0..50 {
        if !profile_in_use(profile) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(WindowError::Failed("its browser is still closing; try again in a moment".into()))
}

/// Does a running browser hold this profile? Windows: Chrome keeps `lockfile` open without sharing (and deletes it as
/// it exits). Elsewhere: `SingletonLock` links to `<host>-<pid>` while that process lives.
fn profile_in_use(profile: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let lock = profile.join("lockfile");
        lock.exists() && std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&lock).is_err()
    }
    #[cfg(not(windows))]
    {
        let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else { return false };
        let target = target.to_string_lossy().into_owned();
        match target.rsplit_once('-').and_then(|(_, pid)| pid.parse::<i32>().ok()) {
            // Signal 0 checks the process exists without touching it.
            Some(pid) => std::process::Command::new("kill").args(["-0", &pid.to_string()]).output().is_ok_and(|o| o.status.success()),
            None => true,
        }
    }
}

// ---- the screencast ----------------------------------------------------------------------------------------------------

/// While anyone watches the bot's live view, screencast its page; fall back to screenshots if that fails.
async fn supervise(b: Weak<BotBrowser>, hub: Arc<Hub>, stop: CancellationToken) {
    let mut want = hub.want.subscribe();
    let mut failures = 0u32;
    loop {
        while want.borrow_and_update().viewers == 0 {
            tokio::select! {
                r = want.changed() => if r.is_err() { return },
                _ = stop.cancelled() => return,
            }
        }
        let Some(browser) = b.upgrade() else { return };
        let r = tokio::select! {
            r = screencast(&browser, &hub, &mut want) => r,
            _ = stop.cancelled() => return,
        };
        match r {
            Ok(()) => failures = 0,
            Err(e) => {
                failures += 1;
                debug!("screencast stopped: {e:#}");
                // The old way meanwhile: a screenshot when the page changed, about every second.
                let until = Instant::now() + if failures > 3 { Duration::from_secs(30) } else { Duration::from_secs(2) };
                let mut last = 0u64;
                while Instant::now() < until && want.borrow().viewers > 0 {
                    match browser.frame().await {
                        Ok((jpeg, url, title, w, h)) => {
                            let hash = hash_of(&jpeg);
                            if hash != last {
                                last = hash;
                                hub.publish(jpeg, (w as u32, h as u32), &url, &title);
                            }
                        }
                        Err(_) => *browser.cdp.lock().await = None,
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(FRAME_EVERY) => {}
                        _ = stop.cancelled() => return,
                    }
                }
            }
        }
    }
}

/// One screencast session on the tab in front, until nobody watches (Ok), another tab comes to the front (Ok: the
/// caller starts again on it) or something fails.
async fn screencast(b: &BotBrowser, hub: &Arc<Hub>, want: &mut watch::Receiver<Want>) -> Result<()> {
    let (ws_url, mut url, mut title) = b.page().await?;
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    let mut size = *want.borrow_and_update();
    if size.viewers == 0 {
        return Ok(());
    }
    let mut next = 1u64;
    cdp_send(&mut ws, &mut next, "Page.startScreencast", screencast_params(size)).await?;
    let _casting = hub.casting();
    let mut pacer = Pacer::new(SCREENCAST_MIN_GAP);
    let mut check = tokio::time::interval(TARGET_CHECK);
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    check.tick().await;
    loop {
        tokio::select! {
            msg = ws.next() => {
                let msg = msg.context("devtools connection closed")??;
                let Message::Text(text) = msg else { continue };
                let v: Value = serde_json::from_str(&text)?;
                if let Some(err) = v.get("error") {
                    bail!("{}", err["message"].as_str().unwrap_or("devtools error"));
                }
                let Some((session, data, view)) = screencast_frame(&v) else { continue };
                let jpeg = base64::engine::general_purpose::STANDARD.decode(data)?;
                hub.publish(jpeg, view, &url, &title);
                let wait = pacer.delay(Instant::now());
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                pacer.acked(Instant::now());
                cdp_send(&mut ws, &mut next, "Page.screencastFrameAck", json!({ "sessionId": session })).await?;
            }
            r = want.changed() => {
                r?;
                let w = *want.borrow_and_update();
                if w.viewers == 0 {
                    let _ = cdp_send(&mut ws, &mut next, "Page.stopScreencast", json!({})).await;
                    return Ok(());
                }
                if (w.width, w.height) != (size.width, size.height) {
                    size = w;
                    cdp_send(&mut ws, &mut next, "Page.startScreencast", screencast_params(size)).await?;
                }
            }
            _ = check.tick() => {
                let (now, u, t) = b.page().await?;
                if now != ws_url {
                    return Ok(());
                }
                (url, title) = (u, t);
            }
        }
    }
}

/// Send a CDP command without waiting for its answer (a failure comes back as a message with `error`).
async fn cdp_send(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>, next: &mut u64, method: &str, params: Value) -> Result<()> {
    ws.send(Message::text(json!({ "id": *next, "method": method, "params": params }).to_string())).await?;
    *next += 1;
    Ok(())
}

fn hash_of(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

impl BotBrowser {
    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn alive(&self) -> bool {
        reqwest::Client::new()
            .get(format!("{}/json/version", self.endpoint()))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// Stop this browser (it lets go of the profile) and its screencast.
    async fn close(&self) {
        self.stop.cancel();
        *self.cdp.lock().await = None;
        if let Some(mut c) = self.child.lock().await.take() {
            let _ = c.kill().await;
            let _ = c.wait().await;
        }
    }

    /// The tab the bot is looking at: the first page target (Chrome lists the most recently active first).
    async fn page(&self) -> Result<(String, String, String)> {
        let list: Value = reqwest::Client::new()
            .get(format!("{}/json/list", self.endpoint()))
            .timeout(Duration::from_secs(3))
            .send()
            .await?
            .json()
            .await?;
        let page = list
            .as_array()
            .and_then(|a| a.iter().find(|t| t["type"] == "page"))
            .context("no open tab")?;
        Ok((
            page["webSocketDebuggerUrl"].as_str().context("no debugger url")?.to_owned(),
            page["url"].as_str().unwrap_or_default().to_owned(),
            page["title"].as_str().unwrap_or_default().to_owned(),
        ))
    }

    /// The take-over connection to the tab in front. Which tab that is gets checked at most every second (not on every
    /// key press).
    async fn connect<'a>(&self, slot: &'a mut Option<Cdp>) -> Result<&'a mut Cdp> {
        if slot.as_ref().is_none_or(|c| c.checked.elapsed() >= TARGET_CHECK) {
            let (ws_url, _, _) = self.page().await?;
            match slot.as_mut() {
                Some(c) if c.url == ws_url => c.checked = Instant::now(),
                _ => {
                    let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
                    *slot = Some(Cdp { ws, url: ws_url, next: 1, checked: Instant::now() });
                }
            }
        }
        Ok(slot.as_mut().unwrap())
    }

    async fn frame(&self) -> Result<(Vec<u8>, String, String, i64, i64)> {
        let (_, url, title) = self.page().await?;
        let mut guard = self.cdp.lock().await;
        let cdp = self.connect(&mut guard).await?;
        let shot = cdp.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 60 })).await?;
        let jpeg = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().context("no image")?)?;
        let metrics = cdp.call("Page.getLayoutMetrics", json!({})).await.unwrap_or_default();
        let vp = &metrics["cssVisualViewport"];
        let (w, h) = (vp["clientWidth"].as_f64().unwrap_or(1280.0) as i64, vp["clientHeight"].as_f64().unwrap_or(800.0) as i64);
        Ok((jpeg, url, title, w, h))
    }
}

struct Cdp {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    url: String,
    next: u64,
    /// When `url` was last confirmed as the tab in front.
    checked: Instant,
}

impl Cdp {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        self.ws.send(Message::text(json!({ "id": id, "method": method, "params": params }).to_string())).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.ws.next())
                .await
                .map_err(|_| anyhow!("{method} timed out"))?
                .context("devtools connection closed")??;
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text)?;
            if v["id"].as_u64() == Some(id) {
                if let Some(err) = v.get("error") {
                    bail!("{method}: {}", err["message"].as_str().unwrap_or("error"));
                }
                return Ok(v["result"].clone());
            }
        }
    }
}

async fn save_frame(db: &Db, bot: Uuid, jpeg: &[u8], url: &str, title: &str, w: i64, h: i64) -> Result<()> {
    sqlx::query(
        "insert into live_frames (bot_id, owner_id, jpeg, url, title, width, height, updated_at)
         values ($1, $2, $3, $4, $5, $6, $7, now())
         on conflict (bot_id) do update set jpeg = excluded.jpeg, url = excluded.url, title = excluded.title,
           width = excluded.width, height = excluded.height, updated_at = now()",
    )
    .bind(bot)
    .bind(db.owner)
    .bind(jpeg)
    .bind(url)
    .bind(title)
    .bind(w as i32)
    .bind(h as i32)
    .execute(&db.pool)
    .await?;
    Ok(())
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn find_chrome() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(),
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe".into(),
            PathBuf::from(local).join(r"Google\Chrome\Application\chrome.exe"),
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into(),
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe".into(),
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into(),
            "/Applications/Chromium.app/Contents/MacOS/Chromium".into(),
        ]
    } else {
        ["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"]
            .iter()
            .map(PathBuf::from)
            .collect()
    };
    candidates.into_iter().find(|p| p.is_file())
}

/// The user agent a regular (not headless) Chrome or Edge of this binary's version sends, in Chrome's reduced form
/// (`Chrome/<major>.0.0.0`). None when the version can't be found; the browser then keeps its own.
async fn normal_user_agent(exe: &std::path::Path) -> Option<String> {
    let major = browser_major_version(exe).await?;
    let platform = if cfg!(windows) {
        "Windows NT 10.0; Win64; x64"
    } else if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else {
        "X11; Linux x86_64"
    };
    let edge = exe.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("edge"));
    Some(user_agent(platform, major, edge))
}

fn user_agent(platform: &str, major: u32, edge: bool) -> String {
    let mut ua =
        format!("Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36");
    if edge {
        ua.push_str(&format!(" Edg/{major}.0.0.0"));
    }
    ua
}

/// Windows installs keep a folder named after the full version next to the exe (`Application\154.0.8037.98\`), and
/// `chrome.exe --version` prints nothing there; elsewhere `--version` prints e.g. "Google Chrome 154.0.8037.98".
async fn browser_major_version(exe: &std::path::Path) -> Option<u32> {
    fn major(s: &str) -> Option<u32> {
        let mut parts = s.trim().split('.');
        let m = parts.next()?.parse().ok()?;
        (parts.count() == 3).then_some(m)
    }
    if cfg!(windows) {
        std::fs::read_dir(exe.parent()?)
            .ok()?
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| major(&e.file_name().to_string_lossy()))
            .max()
    } else {
        let out = tokio::process::Command::new(exe).arg("--version").output().await.ok()?;
        String::from_utf8_lossy(&out.stdout).split_whitespace().find_map(major)
    }
}

const PID_FILE: &str = "familiar-chrome.pid";

/// Kill the browser a previous daemon started on this profile, if it is still running. Checks the process really is a
/// browser first, so a reused pid never takes down something else.
async fn kill_leftover(profile: &std::path::Path) {
    let Some(pid) = std::fs::read_to_string(profile.join(PID_FILE)).ok().and_then(|s| s.trim().parse::<u32>().ok()) else {
        return;
    };
    let _ = std::fs::remove_file(profile.join(PID_FILE));
    #[cfg(windows)]
    {
        let mut q = tokio::process::Command::new("tasklist");
        q.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]).creation_flags(0x0800_0000);
        let Ok(out) = q.output().await else { return };
        let listing = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
        if listing.contains("chrome.exe") || listing.contains("msedge.exe") || listing.contains("chromium") {
            let mut k = tokio::process::Command::new("taskkill");
            k.args(["/T", "/F", "/PID", &pid.to_string()]).creation_flags(0x0800_0000);
            let _ = k.output().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    #[cfg(not(windows))]
    {
        let Ok(out) = tokio::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "args="]).output().await else {
            return;
        };
        if String::from_utf8_lossy(&out.stdout).contains(&profile.display().to_string()) {
            let _ = tokio::process::Command::new("kill").arg(pid.to_string()).output().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_looks_like_a_normal_browser() {
        let chrome = user_agent("Windows NT 10.0; Win64; x64", 154, false);
        assert_eq!(
            chrome,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36"
        );
        assert!(!chrome.contains("Headless"));
        assert!(user_agent("X11; Linux x86_64", 154, true).ends_with(" Edg/154.0.0.0"));
    }

    #[tokio::test]
    async fn finds_the_installed_version() {
        if let Some(exe) = find_chrome() {
            assert!(browser_major_version(&exe).await.is_some_and(|m| m >= 100), "no version for {}", exe.display());
        }
    }

    #[test]
    fn the_sign_in_window_is_a_normal_browser() {
        let profile = Path::new("/tmp/bots/.browsers/ada");
        let args = window_args(profile, Some("https://accounts.example.com/login"));
        assert_eq!(args.first().map(String::as_str), Some(format!("--user-data-dir={}", profile.display()).as_str()));
        assert_eq!(args.last().map(String::as_str), Some("https://accounts.example.com/login"), "the page goes last");
        for a in &args {
            // No keychain or password-store switches either: the headless browser uses the system's, so both read the
            // same encrypted cookies (Playwright's `--use-mock-keychain` would make the window's unreadable to it).
            for banned in [
                "--remote-debugging",
                "--headless",
                "--enable-automation",
                "--user-agent",
                "--remote-allow-origins",
                "--disable-blink-features",
                "--use-mock-keychain",
                "--password-store",
            ] {
                assert!(!a.starts_with(banned), "{a}");
            }
        }
        // Without a page Chrome opens its new-tab page.
        assert!(window_args(profile, None).iter().all(|a| a.starts_with("--")));
    }

    #[test]
    fn a_profile_is_used_by_runs_or_the_window_never_both() {
        let (ada, bo) = (Uuid::new_v4(), Uuid::new_v4());
        let mut g = Gate::default();
        assert!(g.begin_run(ada));
        assert_eq!(g.open_window(ada), Opening::Busy, "a run is using the browser");
        assert!(!g.window(ada));
        assert_eq!(g.open_window(bo), Opening::Go, "another teammate's profile is free");
        g.end_run(ada);
        assert_eq!(g.open_window(ada), Opening::Go);
        assert!(g.window(ada));
        assert_eq!(g.open_window(ada), Opening::AlreadyOpen);
        assert!(!g.begin_run(ada), "runs wait while the owner signs in");
        g.window_closed(ada);
        assert!(!g.window(ada));
        assert!(g.begin_run(ada), "the window closed: runs go again");
        g.end_run(ada);
        g.end_run(ada); // never below zero
        assert_eq!(g.open_window(ada), Opening::Go);
    }

    #[cfg(windows)]
    #[test]
    fn a_held_lockfile_means_in_use() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("familiar-lock-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!profile_in_use(&dir), "no lockfile");
        std::fs::write(dir.join("lockfile"), b"").unwrap();
        assert!(!profile_in_use(&dir), "a stale lockfile nobody holds");
        let held = std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(dir.join("lockfile")).unwrap();
        assert!(profile_in_use(&dir));
        drop(held);
        assert!(!profile_in_use(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn viewers_are_counted_and_the_biggest_size_wins() {
        let hub = Arc::new(Hub::default());
        let mut want = hub.want.subscribe();
        assert_eq!(*want.borrow_and_update(), Want::default());
        let a = hub.subscribe(800, 500);
        assert!(want.has_changed().unwrap());
        assert_eq!(*want.borrow_and_update(), Want { viewers: 1, width: 800, height: 500 });
        let b = hub.subscribe(0, 0);
        assert_eq!(*want.borrow(), Want { viewers: 2, width: 1280, height: 800 }, "no size = the browser's");
        drop(b);
        assert_eq!(*want.borrow(), Want { viewers: 1, width: 800, height: 500 });
        let c = hub.subscribe(99_999, 1);
        assert_eq!(*want.borrow(), Want { viewers: 2, width: 2560, height: 500 }, "clamped");
        drop(a);
        drop(c);
        assert_eq!(want.borrow().viewers, 0, "the screencast stops with the last viewer");
    }

    #[test]
    fn frames_are_numbered_and_viewers_see_only_the_newest() {
        let hub = Arc::new(Hub::default());
        let mut v = hub.subscribe(0, 0);
        assert!(!v.frames.has_changed().unwrap(), "nothing yet");
        assert_eq!(hub.publish(vec![1], (1280, 800), "https://a.example/", "A"), 1);
        assert_eq!(hub.publish(vec![2], (1280, 800), "https://a.example/", "A"), 2);
        assert_eq!(hub.publish(vec![3], (640, 400), "https://b.example/", "B"), 3);
        assert!(v.frames.has_changed().unwrap());
        let f = v.frames.borrow_and_update().clone().unwrap();
        assert_eq!((f.seq, f.jpeg.as_slice(), f.view_width, f.url.as_str()), (3, &[3u8][..], 640, "https://b.example/"));
        assert!(!v.frames.has_changed().unwrap(), "1 and 2 were dropped, not queued");
        // A new viewer gets the newest picture at once (a still page sends nothing new).
        let mut late = hub.subscribe(0, 0);
        assert!(late.frames.has_changed().unwrap());
        assert_eq!(late.frames.borrow_and_update().as_ref().unwrap().seq, 3);
    }

    #[test]
    fn acks_are_paced() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::from_millis(40));
        assert_eq!(p.delay(t0), Duration::ZERO, "the first frame is acked at once");
        p.acked(t0);
        assert_eq!(p.delay(t0 + Duration::from_millis(10)), Duration::from_millis(30));
        assert_eq!(p.delay(t0 + Duration::from_millis(40)), Duration::ZERO);
        assert_eq!(p.delay(t0 + Duration::from_secs(5)), Duration::ZERO, "a still page: the next change shows at once");
        p.acked(t0 + Duration::from_millis(40));
        assert_eq!(p.delay(t0 + Duration::from_millis(41)), Duration::from_millis(39));
    }

    #[test]
    fn screencast_events() {
        let ev = json!({ "method": "Page.screencastFrame", "params": { "data": "/9j/AA==", "sessionId": 7,
            "metadata": { "deviceWidth": 1280, "deviceHeight": 799.6, "offsetTop": 0, "pageScaleFactor": 1 } } });
        assert_eq!(screencast_frame(&ev), Some((7, "/9j/AA==", (1280, 800))));
        assert_eq!(screencast_frame(&json!({ "id": 1, "result": {} })), None);
        assert_eq!(screencast_frame(&json!({ "method": "Page.screencastVisibilityChanged", "params": { "visible": true } })), None);
        let p = screencast_params(Want { viewers: 1, width: 900, height: 600 });
        assert_eq!(p, json!({ "format": "jpeg", "quality": 65, "maxWidth": 900, "maxHeight": 600, "everyNthFrame": 1 }));
    }

    #[test]
    fn enter_submits_and_arrows_have_key_codes() {
        let (down, up) = key_events("Enter");
        assert_eq!((down["type"].as_str(), down["text"].as_str(), up["type"].as_str()), (Some("keyDown"), Some("\r"), Some("keyUp")));
        let (down, _) = key_events("ArrowLeft");
        assert_eq!((down["type"].as_str(), down["windowsVirtualKeyCode"].as_i64()), (Some("rawKeyDown"), Some(37)));
        assert!(down.get("text").is_none());
    }
}

/// With a real Chrome (`cargo test -p familiar-core live_view -- --ignored --nocapture`): a temporary profile, a local
/// test page, and the numbers behind the live view (frame rate, input to picture, the old screenshot path).
#[cfg(test)]
mod chrome_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const PAGE: &str = r#"<!doctype html><html><body style="margin:0;font:40px sans-serif">
<input id="t" autofocus style="font-size:40px;width:90%">
<div id="b" style="height:2400px;background:#fff" onclick="this.style.background=this.style.background=='rgb(255, 255, 255)'?'#000':'#fff'">click me</div>
</body></html>"#;
    const ANIM: &str = r#"<!doctype html><html><body style="margin:0"><div id="n" style="font:120px sans-serif"></div>
<script>let i=0;(function f(){document.getElementById('n').textContent=++i;document.body.style.background='hsl('+(i%360)+',60%,60%)';requestAnimationFrame(f)})()</script>
</body></html>"#;

    /// A tiny HTTP server: `/anim` changes every frame, anything else is a still page with a field and a big box.
    async fn serve() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let body = if req.starts_with("GET /anim") { ANIM } else { PAGE };
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(format!("{head}{body}").as_bytes()).await;
                });
            }
        });
        port
    }

    fn stats(name: &str, mut ms: Vec<f64>) -> (f64, f64) {
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p = |q: f64| ms[((ms.len() as f64 - 1.0) * q).round() as usize];
        let (p50, p95) = (p(0.5), p(0.95));
        println!("{name}: n={} p50={p50:.0} ms p95={p95:.0} ms max={:.0} ms", ms.len(), ms[ms.len() - 1]);
        (p50, p95)
    }

    /// The next frame newer than `after`, if one comes within `within`.
    async fn next_frame(v: &mut Viewer, after: u64, within: Duration) -> Option<Arc<Frame>> {
        tokio::time::timeout(within, async {
            loop {
                v.frames.changed().await.ok()?;
                let f = v.frames.borrow_and_update().clone()?;
                if f.seq > after {
                    return Some(f);
                }
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Wait until the page is quiet for 400 ms; the newest frame number.
    async fn settle(v: &mut Viewer) -> u64 {
        let mut seq = v.frames.borrow().as_ref().map_or(0, |f| f.seq);
        while let Some(f) = next_frame(v, seq, Duration::from_millis(400)).await {
            seq = f.seq;
        }
        seq
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "starts a real Chrome on a temporary profile"]
    async fn live_view_with_a_real_chrome() {
        if find_chrome().is_none() {
            eprintln!("SKIP: no Chrome or Edge");
            return;
        }
        let profiles = std::env::temp_dir().join(format!("familiar-live-{}", Uuid::new_v4()));
        // No database: the `live_frames` writes just fail (and are logged).
        let db = Db { pool: sqlx::PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap(), owner: Uuid::new_v4() };
        let m = Arc::new(Manager::default());
        let bot = Uuid::new_v4();
        let port = serve().await;
        m.ensure(bot, "bench", profiles.clone(), None).await.expect("browser starts");
        let page = format!("http://127.0.0.1:{port}/");
        m.input(&db, bot, &json!({ "type": "navigate", "url": format!("{page}anim") })).await.unwrap();

        let hub = m.hub(bot);
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!hub.casting.load(Ordering::SeqCst), "no viewer, no screencast");

        // Watching a page that changes every frame.
        let mut v = hub.subscribe(1280, 800);
        let first = next_frame(&mut v, 0, Duration::from_secs(10)).await.expect("a first frame");
        assert!(hub.casting.load(Ordering::SeqCst));
        // The page's viewport in CSS pixels (the 1280×800 window less its frame).
        assert!((600..=1280).contains(&first.view_width) && (400..=800).contains(&first.view_height), "{first:?}");
        assert_eq!(&first.jpeg[..2], &[0xFF, 0xD8], "a JPEG");
        let t0 = Instant::now();
        let mut seq = first.seq;
        let mut gaps = Vec::new();
        let mut last = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if let Some(f) = next_frame(&mut v, seq, Duration::from_secs(1)).await {
                seq = f.seq;
                gaps.push(last.elapsed().as_secs_f64() * 1000.0);
                last = Instant::now();
            }
        }
        let fps = gaps.len() as f64 / t0.elapsed().as_secs_f64();
        println!("screencast, animated page: {fps:.1} fps, ~{} KB a frame", first.jpeg.len() / 1024);
        stats("screencast frame interval", gaps);
        assert!(fps >= 20.0, "{fps:.1} fps");

        // A still page sends (almost) nothing.
        m.input(&db, bot, &json!({ "type": "navigate", "url": page })).await.unwrap();
        let shown = next_frame(&mut v, seq, Duration::from_secs(5)).await.expect("the new page shows");
        assert!(shown.url.ends_with(':') || !shown.jpeg.is_empty());
        let seq = settle(&mut v).await;
        let quiet = Instant::now();
        let mut idle = 0;
        while quiet.elapsed() < Duration::from_secs(2) {
            if next_frame(&mut v, seq + idle, Duration::from_millis(200)).await.is_some() {
                idle += 1;
            }
        }
        println!("screencast, still page: {idle} frames in 2 s");
        assert!(idle <= 2, "{idle} frames from a still page");

        // Input → picture: a click turns the page black/white, a key types a letter in the field.
        let (mut click, mut key) = (Vec::new(), Vec::new());
        for i in 0..40 {
            if i == 20 {
                // Into the field for the typing half.
                m.input(&db, bot, &json!({ "type": "click", "x": 100, "y": 25 })).await.unwrap();
            }
            let seq = settle(&mut v).await;
            let t = Instant::now();
            let ev = if i < 20 { json!({ "type": "click", "x": 600, "y": 500 }) } else { json!({ "type": "type", "text": "a" }) };
            m.input(&db, bot, &ev).await.unwrap();
            let Some(f) = next_frame(&mut v, seq, Duration::from_secs(3)).await else {
                let b = m.bots.lock().await.get(&bot).cloned().unwrap();
                let mut g = b.cdp.lock().await;
                let c = b.connect(&mut g).await.unwrap();
                let js = "location.href+' '+document.getElementById('b').style.background+' '+document.getElementById('t').value+' '+document.visibilityState";
                let r = c.call("Runtime.evaluate", json!({ "expression": js })).await;
                panic!("no frame after {ev}: casting={} page={r:?}", hub.casting.load(Ordering::SeqCst));
            };
            let ms = f.at.duration_since(t).as_secs_f64() * 1000.0;
            if i < 20 { click.push(ms) } else { key.push(ms) }
        }
        let (_, p95c) = stats("screencast, click to picture", click);
        let (_, p95k) = stats("screencast, key to picture", key);
        assert!(p95c < 150.0 && p95k < 150.0, "{p95c} {p95k}");

        // Scrolling with the wheel, as the panel sends it (every 16 ms).
        let seq = settle(&mut v).await;
        let t0 = Instant::now();
        let mut sent = 0;
        while t0.elapsed() < Duration::from_secs(2) {
            let dy = if sent < 60 { 30 } else { -30 };
            m.input(&db, bot, &json!({ "type": "scroll", "x": 600, "y": 500, "dy": dy })).await.unwrap();
            sent += 1;
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
        let scrolled = v.frames.borrow().as_ref().map_or(0, |f| f.seq) - seq;
        println!("screencast, scrolling: {:.1} fps", scrolled as f64 / t0.elapsed().as_secs_f64());

        drop(v);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!hub.casting.load(Ordering::SeqCst), "the last viewer left: the screencast stopped");

        // Before: a screenshot about every 900 ms, so a click shows on the next one.
        let b = m.bots.lock().await.get(&bot).cloned().unwrap();
        let (mut capture, mut before) = (Vec::new(), Vec::new());
        for i in 0..9u64 {
            let (shot, ..) = b.frame().await.unwrap();
            let base = hash_of(&shot);
            let into = Duration::from_millis(100 * i); // where between two screenshots the click lands
            tokio::time::sleep(into).await;
            let t = Instant::now();
            m.input(&db, bot, &json!({ "type": "click", "x": 600, "y": 500 })).await.unwrap();
            tokio::time::sleep(FRAME_EVERY.saturating_sub(into + t.elapsed())).await;
            let c = Instant::now();
            let (jpeg, ..) = b.frame().await.unwrap();
            capture.push(c.elapsed().as_secs_f64() * 1000.0);
            assert_ne!(hash_of(&jpeg), base);
            before.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        stats("before: one screenshot", capture);
        stats("before: click to picture in the daemon (then a DB write, a notice and two HTTP requests)", before);

        // No sign-in window while a run uses the browser.
        assert!(m.begin_run(bot));
        assert_eq!(m.open_window(&db, bot, "bench", profiles.clone(), None, Some(&page)).await, Err(WindowError::Busy));
        m.end_run(bot);

        // The sign-in window (a visible Chrome on the temporary profile): the headless browser stops, runs wait,
        // "Done" closes it, and the next use starts headless again on the same profile.
        m.open_window(&db, bot, "bench", profiles.clone(), None, Some(&page)).await.expect("the window opens");
        assert!(m.signing_in(bot) && !m.begin_run(bot));
        assert!(m.bots.lock().await.get(&bot).is_none(), "the headless browser let go of the profile");
        assert!(m.ensure(bot, "bench", profiles.clone(), None).await.is_none(), "no headless start meanwhile");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(m.close_sign_in_window(bot).await);
        for _ in 0..100 {
            if !m.signing_in(bot) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!m.signing_in(bot), "the window's end was noticed");
        assert!(m.ensure(bot, "bench", profiles.clone(), None).await.is_some(), "headless again on the same profile");

        m.shutdown().await;
        let _ = std::fs::remove_dir_all(&profiles);
    }
}
