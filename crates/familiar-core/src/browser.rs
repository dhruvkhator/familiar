//! Each bot's browser: a headless Chrome the daemon runs itself (persistent profile, logins survive), shared with
//! Playwright MCP over CDP. While the bot works — or you have taken over — the daemon streams JPEG frames into
//! `live_frames` and replays your clicks and typing, which is the app's live "computer" view.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};
use uuid::Uuid;

use crate::db::Db;

const FRAME_EVERY: Duration = Duration::from_millis(900);
/// After you stop interacting, keep streaming this long (so the result of your last click shows up).
const TAKEOVER_LINGER: Duration = Duration::from_secs(120);

#[derive(Default)]
pub struct Manager {
    bots: Mutex<HashMap<Uuid, Arc<BotBrowser>>>,
}

struct BotBrowser {
    port: u16,
    child: Mutex<Option<Child>>,
    cdp: Mutex<Option<Cdp>>,
    state: std::sync::Mutex<Activity>,
}

#[derive(Default)]
struct Activity {
    runs: usize,
    last_input: Option<Instant>,
    streaming: bool,
}

impl Manager {
    /// Start (or reuse) the bot's Chrome. Returns the CDP endpoint for Playwright MCP, or None when no Chrome/Edge
    /// is installed (Playwright then launches its own headless browser and there is no live view).
    pub async fn ensure(&self, bot: Uuid, slug: &str, profiles: PathBuf, bin: Option<&str>) -> Option<String> {
        let mut bots = self.bots.lock().await;
        if let Some(b) = bots.get(&bot) {
            if b.alive().await {
                return Some(b.endpoint());
            }
        }
        let exe = bin.map(PathBuf::from).or_else(find_chrome)?;
        let port = free_port().ok()?;
        let profile = profiles.join(slug);
        let _ = std::fs::create_dir_all(&profile);
        // A Chrome left over from a crash still holds this profile; a new one would just hand off to it and exit.
        kill_leftover(&profile).await;
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--headless=new")
            .arg(format!("--remote-debugging-port={port}"))
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
            if let Some(mut c) = b.child.lock().await.take() {
                let _ = c.kill().await;
            }
            return None;
        }
        info!(bot = %slug, port, "browser started");
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
            "type" => {
                cdp.call("Input.insertText", json!({ "text": ev["text"].as_str().unwrap_or_default() })).await?;
            }
            "key" => {
                let key = ev["key"].as_str().unwrap_or("Enter");
                let (code, vk) = match key {
                    "Enter" => ("Enter", 13),
                    "Tab" => ("Tab", 9),
                    "Backspace" => ("Backspace", 8),
                    "Escape" => ("Escape", 27),
                    "ArrowDown" => ("ArrowDown", 40),
                    "ArrowUp" => ("ArrowUp", 38),
                    _ => (key, 0),
                };
                for kind in ["keyDown", "keyUp"] {
                    cdp.call(
                        "Input.dispatchKeyEvent",
                        json!({ "type": kind, "key": key, "code": code, "windowsVirtualKeyCode": vk }),
                    )
                    .await?;
                }
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

    /// Start the frame loop for a bot unless it is already running.
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
                    let keep = s.runs > 0 || s.last_input.is_some_and(|t| t.elapsed() < TAKEOVER_LINGER);
                    if !keep {
                        s.streaming = false;
                        break;
                    }
                }
                match b.frame().await {
                    Ok((jpeg, url, title, w, h)) => {
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        jpeg.hash(&mut hasher);
                        let hash = hasher.finish();
                        if hash != last_hash {
                            last_hash = hash;
                            if let Err(e) = save_frame(&db, bot, &jpeg, &url, &title, w, h).await {
                                warn!("live frame save failed: {e:#}");
                            }
                        }
                    }
                    Err(e) => {
                        *b.cdp.lock().await = None; // reconnect next time (tab closed, target changed…)
                        tracing::debug!("live frame: {e:#}");
                    }
                }
                tokio::time::sleep(FRAME_EVERY).await;
            }
        });
    }

    pub async fn shutdown(&self) {
        for (_, b) in self.bots.lock().await.drain() {
            if let Some(mut c) = b.child.lock().await.take() {
                let _ = c.kill().await;
            }
        }
    }
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

    async fn connect<'a>(&self, slot: &'a mut Option<Cdp>) -> Result<&'a mut Cdp> {
        let (ws_url, _, _) = self.page().await?;
        if slot.as_ref().map(|c| c.url.as_str()) != Some(ws_url.as_str()) {
            let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
            *slot = Some(Cdp { ws, url: ws_url, next: 1 });
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
