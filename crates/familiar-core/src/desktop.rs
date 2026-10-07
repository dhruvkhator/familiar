//! Desktop control: a teammate may use this PC's desktop (mouse, keyboard, screen) through Windows-MCP
//! (<https://github.com/CursorTouch/Windows-MCP>, MIT), installed once by [`crate::tools::ensure_windows_mcp`]. Off by
//! default, per teammate, and only when Familiar runs inside the app (which shows who is using the desktop and can stop
//! it from the tray).
//!
//! - Only the tools in [`TOOLS`] are served; [`EXCLUDED`] (commands, files, processes, the registry, the clipboard,
//!   web fetches, toasts) are switched off at the server and refused here too, as is any tool this list doesn't know.
//! - Every call asks the owner, screenshots and screen reads included (they show the owner's screen): no rule, no
//!   auto-review, no "Always allow" ([`classify`] runs before them in `runner::decide_tool`). The card says in plain words
//!   what will happen ([`describe`]).
//! - One teammate at a time ([`Control`]): the first desktop call of a run takes the desktop until the run ends;
//!   another teammate is told it is busy. The app's tray shows who has it and "Stop desktop control" cancels that run
//!   and denies its waiting desktop requests.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The MCP server's name in a run's config: its tools are `mcp__desktop__<Tool>`.
pub const SERVER: &str = "desktop";

/// The Windows-MCP tools a teammate gets (each asks the owner).
pub const TOOLS: [&str; 13] = [
    "App", "Click", "Type", "Scroll", "Move", "Shortcut", "MultiSelect", "MultiEdit", "Wait", "WaitFor", "Screenshot", "Snapshot",
    "DisplayInventory",
];

/// Windows-MCP tools that are never served: they run commands (PowerShell, and Process ends programs), read or change
/// files (FileSystem) or the registry (Registry), read the owner's clipboard (Clipboard, passwords live there), fetch web
/// pages or read the owner's own browser tab from outside the teammate's browser (Scrape), or show toasts that can pose
/// as any app (Notification).
pub const EXCLUDED: [&str; 7] = ["PowerShell", "FileSystem", "Process", "Registry", "Clipboard", "Scrape", "Notification"];

/// What [`classify`] says about a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Ask the owner; the plain words go on the card.
    Ask(String),
    /// Never: why, for the teammate.
    Refuse(String),
}

/// Whether `tool` is a desktop tool and what to do with it. None = not a desktop tool.
pub fn classify(tool: &str, input: &Value) -> Option<Verdict> {
    let name = tool.strip_prefix("mcp__")?.strip_prefix(SERVER)?.strip_prefix("__")?;
    if !TOOLS.contains(&name) {
        return Some(Verdict::Refuse(format!(
            "`{name}` is not one of the desktop tools Familiar allows (no commands, files, processes, registry, \
             clipboard, web fetches or notifications)."
        )));
    }
    if name == "App" {
        let mode = input["mode"].as_str().unwrap_or("launch");
        let by_path = ["executable", "args", "cwd"].iter().any(|k| !input[*k].is_null());
        if !matches!(mode, "launch" | "switch" | "resize") || by_path {
            return Some(Verdict::Refuse(
                "Starting programs by path (with arguments) is not allowed: that runs commands. Open apps by their \
                 Start Menu name (mode launch) instead."
                    .into(),
            ));
        }
    }
    Some(Verdict::Ask(describe(name, input)))
}

/// A call in plain words for the approval card: the app or window, the element, the coordinates, the text to type.
pub fn describe(tool: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().map(str::to_owned);
    let quoted = |k: &str| s(k).map(|v| format!("“{v}”")).unwrap_or_else(|| "(nothing)".into());
    let truthy = |k: &str| input[k] == true || input[k].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let target = || match (point(&input["loc"]), input["label"].as_i64()) {
        (Some(p), _) => format!("at {p}"),
        (None, Some(l)) => format!("on screen element #{l} (from its last look at the screen)"),
        (None, None) => "where the mouse is".to_owned(),
    };
    let area = || {
        if let Some(r) = rect(&input["region"]) {
            format!("the area {r} of your screen")
        } else if let Some(d) = input["display"].as_array().filter(|d| !d.is_empty()) {
            let list: Vec<String> = d.iter().map(|v| v.to_string()).collect();
            format!("display {} (everything on it)", list.join(" and "))
        } else {
            "your whole screen (every window on it)".to_owned()
        }
    };
    match tool {
        "App" => match input["mode"].as_str().unwrap_or("launch") {
            "switch" => format!("Bring the window {} to the front", quoted("name")),
            "resize" => {
                let mut out = format!("Move or resize the window {}", if s("name").is_some() { quoted("name") } else { "in front".into() });
                if let Some(p) = point(&input["window_loc"]) {
                    out.push_str(&format!(" to {p}"));
                }
                if let Some([w, h]) = pair(&input["window_size"]) {
                    out.push_str(&format!(", size {w}×{h}"));
                }
                out
            }
            _ => format!("Open the app {} (the closest match in your Start Menu)", quoted("name")),
        },
        "Click" => {
            let button = input["button"].as_str().unwrap_or("left");
            let what = match input["clicks"].as_i64().unwrap_or(1) {
                0 => "Hover the mouse".to_owned(),
                2 => format!("Double-click ({button} button)"),
                1 if button == "left" => "Click".to_owned(),
                1 => format!("Click the {button} button"),
                n => format!("Click {n} times ({button} button)"),
            };
            format!("{what} {}", target())
        }
        "Type" => {
            let mut out = format!("Type {} {}", quoted("text"), target());
            if truthy("clear") {
                out.push_str(", replacing the text that is there");
            }
            if truthy("press_enter") {
                out.push_str(", then press Enter");
            }
            out
        }
        "Scroll" => {
            let n = input["wheel_times"].as_i64().unwrap_or(1);
            format!("Scroll {} {n} notch{} {}", input["direction"].as_str().unwrap_or("down"), if n == 1 { "" } else { "es" }, target())
        }
        "Move" => {
            let to = target();
            if truthy("drag") || !input["from_loc"].is_null() {
                match point(&input["from_loc"]) {
                    Some(from) => format!("Drag from {from} to {}", to.trim_start_matches("at ")),
                    None => format!("Drag from where the mouse is to {}", to.trim_start_matches("at ")),
                }
            } else {
                format!("Move the mouse {}", if to.starts_with("at ") { to.replacen("at ", "to ", 1) } else { to })
            }
        }
        "Shortcut" => format!("Press {}", quoted("shortcut")),
        "MultiSelect" => {
            let n = input["locs"].as_array().map(Vec::len).or_else(|| input["labels"].as_array().map(Vec::len)).unwrap_or(0);
            let places: Vec<String> = input["locs"].as_array().into_iter().flatten().filter_map(point).collect();
            let ctrl = input["press_ctrl"] != false && input["press_ctrl"] != "false";
            format!(
                "Click {n} place{}{}{}",
                if n == 1 { "" } else { "s" },
                if ctrl { " holding Ctrl" } else { "" },
                if places.is_empty() { String::new() } else { format!(": {}", places.join(", ")) }
            )
        }
        "MultiEdit" => {
            let fields: Vec<String> = input["locs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| match f.as_array().map(Vec::as_slice) {
                    Some([x, y, text, ..]) => format!("“{}” at ({x}, {y})", text.as_str().map(str::to_owned).unwrap_or_else(|| text.to_string())),
                    _ => f.to_string(),
                })
                .chain(input["labels"].as_array().into_iter().flatten().map(|f| match f.as_array().map(Vec::as_slice) {
                    Some([label, text, ..]) => format!("“{}” in element #{label}", text.as_str().map(str::to_owned).unwrap_or_else(|| text.to_string())),
                    _ => f.to_string(),
                }))
                .collect();
            format!("Type into {} field{}: {}", fields.len(), if fields.len() == 1 { "" } else { "s" }, fields.join("; "))
        }
        "Wait" => format!("Wait {} s", input["duration"].as_i64().unwrap_or(0)),
        "WaitFor" => {
            let mut out = format!("Watch your screen until {}", input["condition"].as_str().unwrap_or("something").replace('_', " "));
            if s("text").is_some() {
                out.push_str(&format!(" {}", quoted("text")));
            }
            if s("window_name").is_some() {
                out.push_str(&format!(" in the window {}", quoted("window_name")));
            }
            out.push_str(&format!(" (up to {} s)", input["timeout"].as_f64().unwrap_or(10.0)));
            out
        }
        "Screenshot" => format!("Take a screenshot of {} and look at it", area()),
        "Snapshot" => {
            let mut out = format!("Read what is on {}: window names, buttons and their text", area());
            if truthy("use_vision") {
                out.push_str(", with a screenshot");
            }
            if truthy("use_dom") {
                out.push_str(", including the page open in your browser");
            }
            out
        }
        "DisplayInventory" => "Read your display layout (screen sizes and scaling); nothing on screen".to_owned(),
        other => format!("Use the desktop tool {other}"),
    }
}

/// `[x, y]` (or "x,y") as "(x, y)".
fn point(v: &Value) -> Option<String> {
    pair(v).map(|[x, y]| format!("({x}, {y})"))
}

fn pair(v: &Value) -> Option<[i64; 2]> {
    match v {
        Value::Array(a) if a.len() >= 2 => Some([a[0].as_i64()?, a[1].as_i64()?]),
        Value::String(s) => {
            let mut it = s.trim_matches(['[', ']', '(', ')']).split(',').map(|p| p.trim().parse::<i64>());
            Some([it.next()?.ok()?, it.next()?.ok()?])
        }
        _ => None,
    }
}

/// `[left, top, right, bottom]` as "from (l, t) to (r, b)".
fn rect(v: &Value) -> Option<String> {
    let nums: Vec<i64> = match v {
        Value::Array(a) => a.iter().filter_map(Value::as_i64).collect(),
        Value::String(s) => s.trim_matches(['[', ']']).split(',').filter_map(|p| p.trim().parse().ok()).collect(),
        _ => return None,
    };
    match nums.as_slice() {
        [l, t, r, b] => Some(format!("from ({l}, {t}) to ({r}, {b})")),
        _ => None,
    }
}

/// Half the size of the approval card's picture of the screen around a desktop step's target.
const PREVIEW_HALF: (i32, i32) = (180, 110);

/// Where a desktop step lands (physical virtual-desktop pixels, as Windows-MCP uses them), when it says.
pub fn target(tool: &str, input: &Value) -> Option<(i32, i32)> {
    let name = tool.strip_prefix("mcp__desktop__")?;
    let first = |k: &str| input[k].as_array().and_then(|a| a.first()).cloned();
    let loc = match name {
        "Click" | "Type" | "Scroll" | "Move" => input["loc"].clone(),
        "MultiSelect" => first("locs")?,
        "MultiEdit" => first("locs").map(|f| match f.as_array() {
            Some(a) if a.len() >= 2 => json!([a[0], a[1]]),
            _ => Value::Null,
        })?,
        _ => return None,
    };
    let [x, y] = pair(&loc)?;
    Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

/// A small picture (PNG) of the owner's screen around (x, y), the spot marked: for the approval card only, never given
/// to the teammate, and dropped once the request is decided. None off Windows or when the screen can't be read.
pub fn preview(x: i32, y: i32) -> Option<Vec<u8>> {
    let (left, top, w, h, mut bgra) = capture(x - PREVIEW_HALF.0, y - PREVIEW_HALF.1, PREVIEW_HALF.0 * 2, PREVIEW_HALF.1 * 2)?;
    mark(&mut bgra, w, h, x - left, y - top);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for p in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[p[2], p[1], p[0]]);
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().ok()?.write_image_data(&rgb).ok()?;
    Some(out)
}

/// A red crosshair with a white outline at (cx, cy) of a BGRA image.
fn mark(bgra: &mut [u8], w: i32, h: i32, cx: i32, cy: i32) {
    let mut put = |x: i32, y: i32, c: [u8; 3]| {
        if (0..w).contains(&x) && (0..h).contains(&y) {
            let i = ((y * w + x) * 4) as usize;
            bgra[i..i + 3].copy_from_slice(&[c[2], c[1], c[0]]);
        }
    };
    for pass in [([0xff, 0xff, 0xff], 2), ([0xe5, 0x48, 0x4d], 1)] {
        let (c, r) = pass;
        for d in -14i32..=14 {
            if d.abs() < 4 {
                continue;
            }
            for t in -r..=r {
                put(cx + d, cy + t, c);
                put(cx + t, cy + d, c);
            }
        }
    }
}

/// The screen's pixels in (left, top, w, h), clipped to the virtual desktop, read in physical pixels (per-monitor DPI
/// aware, as Windows-MCP is). Returns the clipped (left, top, w, h, BGRA).
#[cfg(windows)]
fn capture(left: i32, top: i32, w: i32, h: i32) -> Option<(i32, i32, i32, i32, Vec<u8>)> {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    // SAFETY: plain GDI calls on handles created and released here; buffers are sized for the bitmap read.
    unsafe {
        let old = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let (l, t) = (left.max(vx), top.max(vy));
        let (r, b) = ((left + w).min(vx + vw), (top + h).min(vy + vh));
        let result = (|| {
            let (w, h) = (r - l, b - t);
            if w <= 0 || h <= 0 {
                return None;
            }
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let prev = SelectObject(mem, bmp);
            let copied = BitBlt(mem, 0, 0, w, h, screen, l, t, SRCCOPY | CAPTUREBLT) != 0;
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            info.bmiHeader.biHeight = -h; // top-down rows
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let rows = if copied { GetDIBits(mem, bmp, 0, h as u32, buf.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS) } else { 0 };
            SelectObject(mem, prev);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            (rows == h).then_some((l, t, w, h, buf))
        })();
        SetThreadDpiAwarenessContext(old);
        result
    }
}

#[cfg(not(windows))]
fn capture(_: i32, _: i32, _: i32, _: i32) -> Option<(i32, i32, i32, i32, Vec<u8>)> {
    None
}

/// The `desktop` MCP server for a run's config: the installed Windows-MCP over stdio, only [`TOOLS`] served and
/// [`EXCLUDED`] excluded, its own (empty) config file instead of `~/.windows-mcp/config.toml`, telemetry off.
pub fn server_config(exe: &Path, config: &Path) -> Value {
    json!({
        "type": "stdio",
        "command": exe.display().to_string(),
        "args": [
            "serve", "--transport", "stdio", "--config", config.display().to_string(),
            "--tools", TOOLS.join(","), "--exclude-tools", EXCLUDED.join(","),
        ],
        "env": {
            // Telemetry is on by default (PostHog): off, and no client at all.
            "ANONYMIZED_TELEMETRY": "false",
            "POSTHOG_API_KEY": "",
            // The command line decides which tools exist, never the environment.
            "WINDOWS_MCP_TOOLS": "",
            "WINDOWS_MCP_EXCLUDE_TOOLS": "",
            "WINDOWS_MCP_DEBUG": "false",
        },
    })
}

/// Who may use the desktop right now: one teammate's run at a time.
#[derive(Default)]
pub struct Control(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    /// Runs of teammates with desktop control on, while they run.
    runs: HashMap<Uuid, Entry>,
    /// The run using the desktop.
    holder: Option<Uuid>,
}

struct Entry {
    bot: String,
    cancel: CancellationToken,
    /// Stopped from the tray: no more desktop actions in this run.
    stopped: bool,
}

/// Why a run may not use the desktop now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Busy {
    /// Another teammate has it.
    Other(String),
    /// The owner stopped desktop control for this run.
    Stopped,
    /// The run was not started with desktop control.
    Off,
}

impl Busy {
    pub fn message(&self) -> String {
        match self {
            Busy::Other(bot) => format!(
                "{bot} is using the desktop right now; only one teammate can at a time. Do something else, or try again \
                 when it has finished."
            ),
            Busy::Stopped => "Your owner stopped desktop control for this run. Don't use the desktop again in it.".into(),
            Busy::Off => "Desktop control is not on for this run.".into(),
        }
    }
}

impl Control {
    /// A run of a teammate with desktop control on started (`cancel`: the run's own token).
    pub fn run_started(&self, run: Uuid, bot: &str, cancel: CancellationToken) {
        self.0.lock().unwrap().runs.insert(run, Entry { bot: bot.to_owned(), cancel, stopped: false });
    }

    /// The run ended. Returns true when it was using the desktop (now free).
    pub fn run_finished(&self, run: Uuid) -> bool {
        let mut g = self.0.lock().unwrap();
        g.runs.remove(&run);
        if g.holder == Some(run) {
            g.holder = None;
            return true;
        }
        false
    }

    /// Take the desktop for this run (kept until the run ends). Ok(true) when it was just taken.
    pub fn acquire(&self, run: Uuid) -> Result<bool, Busy> {
        let mut g = self.0.lock().unwrap();
        match g.runs.get(&run) {
            None => return Err(Busy::Off),
            Some(e) if e.stopped => return Err(Busy::Stopped),
            Some(_) => {}
        }
        match g.holder {
            Some(h) if h == run => Ok(false),
            Some(h) => Err(Busy::Other(g.runs.get(&h).map(|e| e.bot.clone()).unwrap_or_else(|| "Another teammate".into()))),
            None => {
                g.holder = Some(run);
                Ok(true)
            }
        }
    }

    /// The run using the desktop and its teammate's name.
    pub fn holder(&self) -> Option<(Uuid, String)> {
        let g = self.0.lock().unwrap();
        g.holder.and_then(|h| g.runs.get(&h).map(|e| (h, e.bot.clone())))
    }

    /// "Stop desktop control": the run using the desktop gives it up and may not use it again. Returns (run, teammate,
    /// the run's token) when one had it; the caller denies its waiting requests, then cancels it.
    pub fn stop(&self) -> Option<(Uuid, String, CancellationToken)> {
        let mut g = self.0.lock().unwrap();
        let run = g.holder.take()?;
        let e = g.runs.get_mut(&run)?;
        e.stopped = true;
        Some((run, e.bot.clone(), e.cancel.clone()))
    }
}

/// The instructions note of a teammate with desktop control on: ready, or why not.
pub fn note(ready: Result<(), String>) -> String {
    match ready {
        Ok(()) => "## This PC's desktop\nYou can use this PC's desktop through the `desktop` tools. Every step (each \
            click, keystroke, screenshot or look at the screen) asks your owner first, and only one teammate can use \
            the desktop at a time. Look at only what you need (a `region` around one window, not the whole screen), \
            never type passwords, one-time codes or payment details, and stop at any dialog or window you didn't \
            expect. If your owner stops desktop control, don't use it again in this run.\n"
            .to_owned(),
        Err(why) => format!(
            "## This PC's desktop\nDesktop control is on for you but not ready ({why}). Don't try to use the desktop; \
             tell your owner if a task needs it.\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> String {
        format!("mcp__desktop__{name}")
    }

    /// Every tool Windows-MCP 0.8.7 serves, as its `tools/list` reports them.
    const ALL: [&str; 20] = [
        "App", "DisplayInventory", "PowerShell", "FileSystem", "Snapshot", "Screenshot", "Click", "Type", "Scroll", "Move",
        "Shortcut", "Wait", "WaitFor", "Scrape", "MultiSelect", "MultiEdit", "Clipboard", "Process", "Notification",
        "Registry",
    ];

    #[test]
    fn every_windows_mcp_tool_is_classified() {
        for name in ALL {
            let v = classify(&tool(name), &json!({ "mode": "launch", "name": "notepad", "loc": [1, 2], "text": "x" })).unwrap();
            match (EXCLUDED.contains(&name), TOOLS.contains(&name), &v) {
                (true, false, Verdict::Refuse(why)) => assert!(why.contains("not one of the desktop tools"), "{name}: {why}"),
                (false, true, Verdict::Ask(words)) => assert!(!words.is_empty(), "{name}"),
                other => panic!("{name}: {other:?}"),
            }
        }
        // The two lists cover everything, without overlap.
        assert_eq!(TOOLS.len() + EXCLUDED.len(), ALL.len());
        assert!(TOOLS.iter().all(|t| !EXCLUDED.contains(t)));
        // A tool this list doesn't know (a newer Windows-MCP) is refused.
        assert!(matches!(classify(&tool("ControlStatus"), &json!({})), Some(Verdict::Refuse(_))));
        assert!(matches!(classify(&tool("click"), &json!({})), Some(Verdict::Refuse(_))), "names are exact");
        // Not desktop tools.
        for t in ["Click", "mcp__browser__browser_click", "mcp__desktopx__Click", "mcp__familiar__notify_user", "Bash", "mcp__desktop"] {
            assert_eq!(classify(t, &json!({})), None, "{t}");
        }
    }

    #[test]
    fn commands_and_paths_are_never_started() {
        for input in [
            json!({ "mode": "launch_executable", "executable": "C:\\Windows\\System32\\cmd.exe", "args": ["/c", "del x"] }),
            json!({ "mode": "launch", "name": "notepad", "executable": "powershell.exe" }),
            json!({ "mode": "launch", "name": "x", "args": "-c calc" }),
            json!({ "mode": "switch", "name": "x", "cwd": "C:\\" }),
            json!({ "mode": "exec" }),
        ] {
            assert!(matches!(classify(&tool("App"), &input), Some(Verdict::Refuse(_))), "{input}");
        }
        for input in [json!({ "name": "notepad" }), json!({ "mode": "switch", "name": "Notepad" }), json!({ "mode": "resize", "window_size": [800, 600] })] {
            assert!(matches!(classify(&tool("App"), &input), Some(Verdict::Ask(_))), "{input}");
        }
    }

    #[test]
    fn plain_words() {
        let d = |t: &str, i: Value| describe(t, &i);
        assert_eq!(d("App", json!({ "name": "notepad" })), "Open the app “notepad” (the closest match in your Start Menu)");
        assert_eq!(d("App", json!({ "mode": "switch", "name": "Untitled - Notepad" })), "Bring the window “Untitled - Notepad” to the front");
        assert_eq!(d("App", json!({ "mode": "resize", "name": "Notepad", "window_loc": [0, 0], "window_size": [800, 600] })), "Move or resize the window “Notepad” to (0, 0), size 800×600");
        assert_eq!(d("Click", json!({ "loc": [120, 340] })), "Click at (120, 340)");
        assert_eq!(d("Click", json!({ "loc": [1, 2], "clicks": 2 })), "Double-click (left button) at (1, 2)");
        assert_eq!(d("Click", json!({ "loc": "5, 6", "button": "right" })), "Click the right button at (5, 6)");
        assert_eq!(d("Click", json!({ "label": 7, "clicks": 0 })), "Hover the mouse on screen element #7 (from its last look at the screen)");
        assert_eq!(
            d("Type", json!({ "text": "hello", "loc": [10, 20], "clear": "true", "press_enter": true })),
            "Type “hello” at (10, 20), replacing the text that is there, then press Enter"
        );
        assert_eq!(d("Scroll", json!({ "direction": "up", "wheel_times": 3 })), "Scroll up 3 notches where the mouse is");
        assert_eq!(d("Move", json!({ "loc": [5, 5] })), "Move the mouse to (5, 5)");
        assert_eq!(d("Move", json!({ "loc": [5, 5], "from_loc": [1, 1], "drag": true })), "Drag from (1, 1) to (5, 5)");
        assert_eq!(d("Shortcut", json!({ "shortcut": "ctrl+s" })), "Press “ctrl+s”");
        assert_eq!(d("MultiSelect", json!({ "locs": [[1, 2], [3, 4]] })), "Click 2 places holding Ctrl: (1, 2), (3, 4)");
        assert_eq!(d("MultiEdit", json!({ "locs": [[1, 2, "Ada"], [3, 4, "Lovelace"]] })), "Type into 2 fields: “Ada” at (1, 2); “Lovelace” at (3, 4)");
        assert_eq!(d("Wait", json!({ "duration": 2 })), "Wait 2 s");
        assert_eq!(d("WaitFor", json!({ "condition": "active_window", "window_name": "Notepad", "timeout": 5 })), "Watch your screen until active window in the window “Notepad” (up to 5 s)");
        assert_eq!(d("Screenshot", json!({})), "Take a screenshot of your whole screen (every window on it) and look at it");
        assert_eq!(d("Screenshot", json!({ "region": [0, 0, 400, 300] })), "Take a screenshot of the area from (0, 0) to (400, 300) of your screen and look at it");
        assert_eq!(d("Snapshot", json!({ "display": [0], "use_vision": true })), "Read what is on display 0 (everything on it): window names, buttons and their text, with a screenshot");
        assert!(d("DisplayInventory", json!({})).contains("nothing on screen"));
    }

    #[test]
    fn preview_targets() {
        // (The capture itself is never run in tests: it would read the screen.)
        assert_eq!(target("mcp__desktop__Click", &json!({ "loc": [120, 340] })), Some((120, 340)));
        assert_eq!(target("mcp__desktop__Type", &json!({ "loc": "10, 20", "text": "x" })), Some((10, 20)));
        assert_eq!(target("mcp__desktop__MultiEdit", &json!({ "locs": [[5, 6, "a"], [7, 8, "b"]] })), Some((5, 6)));
        assert_eq!(target("mcp__desktop__MultiSelect", &json!({ "locs": [[1, 2]] })), Some((1, 2)));
        assert_eq!(target("mcp__desktop__Click", &json!({ "label": 3 })), None, "an element id has no spot");
        assert_eq!(target("mcp__desktop__Screenshot", &json!({ "region": [0, 0, 9, 9] })), None);
        assert_eq!(target("mcp__browser__browser_click", &json!({ "loc": [1, 2] })), None);
        let mut img = vec![0u8; 40 * 40 * 4];
        mark(&mut img, 40, 40, 20, 20);
        let at = |x: usize, y: usize| &img[(y * 40 + x) * 4..][..3];
        assert_eq!(at(30, 20), [0x4d, 0x48, 0xe5], "red line (BGR)");
        assert_eq!(at(20, 20), [0, 0, 0], "the spot itself stays visible");
        mark(&mut img, 40, 40, -5, 100); // off the picture: nothing breaks
    }

    #[test]
    fn excluded_at_the_server_and_telemetry_off() {
        let v = server_config(Path::new(r"C:\tools\windows-mcp.exe"), Path::new(r"C:\tools\config.toml"));
        let args: Vec<&str> = v["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        let flag = |f: &str| args.iter().position(|a| *a == f).map(|i| args[i + 1]).unwrap();
        assert_eq!(flag("--transport"), "stdio");
        assert_eq!(flag("--config"), r"C:\tools\config.toml");
        let served: Vec<&str> = flag("--tools").split(',').collect();
        let excluded: Vec<&str> = flag("--exclude-tools").split(',').collect();
        for t in ["PowerShell", "Registry", "FileSystem", "Process", "Clipboard", "Scrape", "Notification"] {
            assert!(excluded.contains(&t) && !served.contains(&t), "{t}");
        }
        assert_eq!(served, TOOLS.to_vec());
        assert_eq!(v["env"]["ANONYMIZED_TELEMETRY"], "false");
        assert_eq!(v["env"]["POSTHOG_API_KEY"], "");
        assert_eq!(v["env"]["WINDOWS_MCP_TOOLS"], "");
        assert_eq!(v["type"], "stdio");
    }

    #[test]
    fn one_teammate_at_a_time() {
        let c = Control::default();
        let (a, b, x) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (ca, cb) = (CancellationToken::new(), CancellationToken::new());
        c.run_started(a, "Ada", ca.clone());
        c.run_started(b, "Bo", cb.clone());
        assert_eq!(c.acquire(x), Err(Busy::Off), "a run without desktop control");
        assert_eq!(c.acquire(a), Ok(true));
        assert_eq!(c.acquire(a), Ok(false), "kept for the run");
        assert_eq!(c.acquire(b), Err(Busy::Other("Ada".into())));
        assert!(Busy::Other("Ada".into()).message().starts_with("Ada is using the desktop"));
        assert_eq!(c.holder(), Some((a, "Ada".into())));
        assert!(c.run_finished(a), "freed when the run ends");
        assert_eq!(c.holder(), None);
        assert_eq!(c.acquire(b), Ok(true));
        // Stop from the tray: the run gives it up (its token is handed back to cancel) and can't take it again.
        let (run, bot, token) = c.stop().unwrap();
        assert_eq!((run, bot.as_str()), (b, "Bo"));
        token.cancel();
        assert!(cb.is_cancelled() && !ca.is_cancelled());
        assert_eq!(c.acquire(b), Err(Busy::Stopped));
        assert_eq!(c.holder(), None);
        assert!(c.stop().is_none(), "nothing to stop");
        c.run_started(a, "Ada", ca.clone());
        assert_eq!(c.acquire(a), Ok(true));
        assert!(!c.run_finished(b), "b no longer holds it");
        assert_eq!(c.holder(), Some((a, "Ada".into())));
    }
}
