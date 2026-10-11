//! The teammate's computer (the web's `components/ComputerPanel.tsx` browser tab): the live view of its browser.
//!
//! While the panel shows, frames stream from the app's own API as the page changes (`/live/stream`, a CDP screencast
//! in the engine): only the newest one is decoded (off the UI thread) and swapped in once decoded, so a slow decode
//! skips frames instead of falling behind. Without the stream (it failed, or an API on another computer) frames are
//! fetched on `live_frames` notices as before. Drawn aspect-fit. "Take over" forwards clicks, pointer moves (at most
//! ~30 a second), scrolling and typing (in the page's CSS pixels) through `POST /live/input`, in order; what piles up
//! while one is on its way is merged (`coalesce_inputs`).
//!
//! "Sign in in a window" opens the teammate's browser profile as a normal Chrome window on this computer, so you sign
//! in to sites in a real browser (never typing passwords through the picture); the teammate waits until it closes.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use familiar_client::{ApiError, Client, LiveFrame, LiveInfo, LiveInput, LiveWindow};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Led, LedStatus};
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AppContext as _, Focusable as _, Bounds, Context, Entity, EventEmitter, FocusHandle, FontWeight, ImageSource, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement as _, Pixels, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, SharedString, Styled as _, StyledImage as _, Task, Window, canvas, div, img,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, DataEvent, avatar_of};

/// The panel saw the teammate open a real web page (the teammate page opens the panel unless you closed it).
pub struct Browsing;

/// Pointer moves go out at most this often (noVNC sends one per ~17 ms; hover menus need no more).
const MOVE_EVERY: Duration = Duration::from_millis(33);
/// After the stream ends unexpectedly, connect again this soon; after the API says it has none, this late.
const STREAM_RETRY: Duration = Duration::from_secs(1);
const STREAM_UNAVAILABLE: Duration = Duration::from_secs(30);

pub struct ComputerPanel {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    info: Option<LiveInfo>,
    loaded: bool,
    /// The decoded frame on screen and its size in frame pixels.
    frame: Option<(Arc<RenderImage>, u32, u32)>,
    /// Frames replaced since the last draw: removed from the sprite atlas on the next render.
    stale: Vec<Arc<RenderImage>>,
    fetching: bool,
    /// A notice arrived mid-fetch: fetch again when it lands.
    dirty: bool,
    /// Frames are only fetched while the panel shows (info alone otherwise, to notice browsing).
    shown: bool,
    browsing: bool,
    control: bool,
    focus: FocusHandle,
    /// The frame area in window coordinates and the window's scale, measured each paint (clicks map through it).
    area: Rc<Cell<Bounds<Pixels>>>,
    scale: Rc<Cell<f32>>,
    /// Inputs go out one at a time, in order.
    input: futures::channel::mpsc::UnboundedSender<LiveInput>,
    last_move: Option<Instant>,
    /// The address bar (Enter goes there); follows the page while you aren't editing it.
    address: Entity<InputState>,
    shown_url: String,
    /// A page you asked for before its browser showed anything.
    opening: Option<String>,
    /// The frame stream while the panel shows (dropping it disconnects); the picture size it asked for.
    stream: Option<Task<()>>,
    stream_size: (u32, u32),
    /// The stream has delivered a frame since it connected: the polled pictures are not needed.
    streaming: bool,
    /// Don't try the stream again before this (it failed, or this API has none).
    stream_after: Option<Instant>,
    /// The page's viewport in CSS pixels, from the newest streamed frame (input coordinates use it).
    view: Option<(u32, u32)>,
    /// The sign-in window (a real Chrome on this computer).
    window: LiveWindow,
    window_busy: bool,
    /// `window` came back from the API at least once.
    window_loaded: bool,
}

impl EventEmitter<Browsing> for ComputerPanel {}

impl ComputerPanel {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = crate::text_input::new_line("https://", false, window, cx);
        cx.subscribe_in(&address, window, |this: &mut Self, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.go(window, cx);
            }
        })
        .detach();
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => {
                this.refresh(cx);
                this.load_window(cx);
            }
            DataEvent::Changed(Some(n)) if n.bot.as_deref() == Some(&this.bot.to_string()) => match n.t.as_str() {
                "live_frames" => this.refresh(cx),
                "live_window" => this.load_window(cx),
                _ => {}
            },
            _ => {}
        })
        .detach();
        let client = data.read(cx).client.clone();
        let (tx, rx) = futures::channel::mpsc::unbounded::<LiveInput>();
        let (err_tx, mut err_rx) = futures::channel::mpsc::unbounded::<String>();
        Tokio::spawn(cx, send_inputs(client, bot, rx, err_tx)).detach();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(e) = err_rx.next().await {
                if this.update(cx, |p, cx| p.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't reach the page", Some(e.into()), cx))).is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            bot,
            info: None,
            loaded: false,
            frame: None,
            stale: Vec::new(),
            fetching: false,
            dirty: false,
            shown: false,
            browsing: false,
            control: false,
            focus: cx.focus_handle(),
            area: Rc::new(Cell::new(Bounds::default())),
            scale: Rc::new(Cell::new(1.0)),
            input: tx,
            last_move: None,
            address,
            shown_url: String::new(),
            opening: None,
            stream: None,
            stream_size: (0, 0),
            streaming: false,
            stream_after: None,
            view: None,
            window: LiveWindow::default(),
            window_busy: false,
            window_loaded: false,
        };
        this.refresh(cx);
        this.load_window(cx);
        this
    }

    /// The panel is (not) on screen: frames load (and stream) only while it is.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown != self.shown {
            self.shown = shown;
            if !shown {
                self.control = false;
                self.stop_stream();
            } else {
                self.start_stream(cx);
            }
            if shown && self.frame.is_none() {
                self.refresh(cx);
            }
        }
    }

    pub fn browsing(&self) -> bool {
        self.browsing
    }

    // ---- the frame stream ---------------------------------------------------------------------------------------

    /// The biggest picture the panel can show, in device pixels (0×0 before the first paint: the browser's size).
    fn wanted_size(&self) -> (u32, u32) {
        let a = self.area.get();
        let s = self.scale.get().max(1.0);
        let (w, h) = (f32::from(a.size.width) * s, f32::from(a.size.height) * s);
        if w < 1.0 || h < 1.0 { (0, 0) } else { (w.ceil() as u32, h.ceil() as u32) }
    }

    fn start_stream(&mut self, cx: &mut Context<Self>) {
        if self.stream.is_some() || !self.shown || self.stream_after.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let client = self.data.read(cx).client.clone();
        let (bot, (w, h)) = (self.bot, self.wanted_size());
        self.stream_size = (w, h);
        // The reader keeps only the newest frame; the decoder below takes it when it's free (older ones are skipped).
        let (tx, mut rx) = tokio::sync::watch::channel::<Option<LiveFrame>>(None);
        let reader = Tokio::spawn(cx, async move {
            use futures::StreamExt as _;
            let frames = client.live_stream(bot, w, h).await?;
            futures::pin_mut!(frames);
            while let Some(f) = frames.next().await {
                tx.send_replace(Some(f?));
            }
            Ok::<(), ApiError>(())
        });
        self.stream = Some(cx.spawn(async move |this, cx| {
            while rx.changed().await.is_ok() {
                let Some(f) = rx.borrow_and_update().clone() else { continue };
                let jpeg = f.jpeg.clone();
                let decoded = cx.background_spawn(async move { decode(&jpeg) }).await;
                if this.update(cx, |p, cx| p.streamed(f, decoded, cx)).is_err() {
                    return;
                }
            }
            let ended = match reader.await {
                Ok(Err(ApiError::Http { status, .. })) if matches!(status, 404 | 503) => STREAM_UNAVAILABLE,
                Ok(Err(e)) => {
                    tracing::debug!("live stream: {}", e.message());
                    STREAM_RETRY
                }
                _ => STREAM_RETRY,
            };
            let _ = this.update(cx, |p, _| {
                p.stream = None;
                p.streaming = false;
                p.stream_after = Some(Instant::now() + ended);
            });
            cx.background_executor().timer(ended).await;
            let _ = this.update(cx, |p, cx| {
                p.stream_after = None;
                p.start_stream(cx);
                p.refresh(cx);
            });
        }));
    }

    fn stop_stream(&mut self) {
        self.stream = None;
        self.streaming = false;
    }

    fn streamed(&mut self, f: LiveFrame, decoded: Option<(Arc<RenderImage>, u32, u32)>, cx: &mut Context<Self>) {
        let Some(d) = decoded else {
            tracing::warn!("live frame: not a JPEG it could decode");
            return;
        };
        self.streaming = true;
        if let Some((old, _, _)) = self.frame.replace(d) {
            self.stale.push(old);
        }
        if f.view_width > 0 && f.view_height > 0 {
            self.view = Some((f.view_width, f.view_height));
        }
        self.info = Some(LiveInfo { url: f.url, title: f.title, width: f.view_width, height: f.view_height, updated_at: Utc::now() });
        self.loaded = true;
        self.opening = None;
        self.note_browsing(cx);
        cx.notify();
    }

    // ---- polled frames (fallback) and page info ---------------------------------------------------------------

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.fetching {
            self.dirty = true;
            return;
        }
        self.fetching = true;
        self.dirty = false;
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let want_frame = self.shown && !self.streaming;
        let task = Tokio::spawn(cx, async move {
            let info = match client.live_info(bot).await {
                Ok(i) => Some(i),
                Err(ApiError::Http { status: 404, .. }) => None,
                Err(e) => return Err(e),
            };
            let jpeg = match &info {
                Some(_) if want_frame => match client.live_frame(bot).await {
                    Ok(b) => Some(b),
                    Err(e) => {
                        tracing::warn!("live frame: {e}");
                        None
                    }
                },
                _ => None,
            };
            Ok((info, jpeg))
        });
        cx.spawn(async move |this, cx| {
            let fetched = task.await;
            let (info, jpeg) = match fetched {
                Ok(Ok(v)) => v,
                _ => {
                    let _ = this.update(cx, |p, cx| p.fetched(None, None, false, cx));
                    return;
                }
            };
            // Decode on a background thread; the old frame stays up until this one is ready.
            let decoded = match jpeg {
                Some(bytes) => {
                    let d = cx.background_spawn(async move { decode(&bytes) }).await;
                    if d.is_none() {
                        tracing::warn!("live frame: not a JPEG it could decode");
                    }
                    d
                }
                None => None,
            };
            let _ = this.update(cx, |p, cx| p.fetched(info, decoded, true, cx));
        })
        .detach();
    }

    fn fetched(&mut self, info: Option<LiveInfo>, frame: Option<(Arc<RenderImage>, u32, u32)>, ok: bool, cx: &mut Context<Self>) {
        self.fetching = false;
        if ok {
            self.loaded = true;
            // The stream's own page info is newer than the database's while it delivers.
            let newer = match (&self.info, &info) {
                (Some(mine), Some(theirs)) if self.streaming => theirs.updated_at > mine.updated_at,
                (Some(_), None) if self.streaming => false,
                _ => true,
            };
            if newer {
                self.info = info;
            }
            if let Some(f) = frame.filter(|_| !self.streaming) {
                if let Some((old, _, _)) = self.frame.replace(f) {
                    self.stale.push(old);
                }
            }
            if self.info.is_some() {
                self.opening = None;
            }
            self.note_browsing(cx);
        }
        if self.dirty {
            self.refresh(cx);
        }
        cx.notify();
    }

    fn note_browsing(&mut self, cx: &mut Context<Self>) {
        let browsing = self.info.as_ref().is_some_and(real_page);
        if browsing && !self.browsing {
            cx.emit(Browsing);
        }
        self.browsing = browsing;
    }

    // ---- the sign-in window -----------------------------------------------------------------------------------

    fn load_window(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.live_window(bot).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(w)) = task.await {
                let _ = this.update(cx, |p, cx| {
                    p.window_loaded = true;
                    if p.window != w {
                        p.window = w;
                        if !w.open {
                            // Back to its headless browser: the stream picks the view up again.
                            p.stream_after = None;
                            p.start_stream(cx);
                            p.refresh(cx);
                        }
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Open the teammate's browser profile in a real Chrome window at `url` (the Set up checklist's "Log in", or
    /// this panel's "Sign in in a window"). Where the API can't (it runs on another computer), take over here instead.
    pub fn sign_in_at(&mut self, url: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.window_loaded && !self.window.available {
            if let Some(url) = url {
                self.take_over_at(url, window, cx);
            }
            return;
        }
        if self.window_busy {
            return;
        }
        self.window_busy = true;
        self.control = false;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.open_live_window(bot, url.as_deref()).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.window_busy = false;
                match r {
                    Ok(()) => {
                        p.window = LiveWindow { available: true, open: true };
                        p.stop_stream();
                    }
                    Err(e) => p.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't open the window", Some(e.into()), cx)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Done": close the sign-in window (Chrome saves the session first).
    fn done_signing_in(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        self.window_busy = true;
        cx.notify();
        let task = Tokio::spawn(cx, async move { client.close_live_window(bot).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |p, cx| {
                p.window_busy = false;
                if !matches!(r, Ok(Ok(_))) {
                    p.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't close the window", Some("Close it yourself.".into()), cx));
                }
                p.load_window(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // ---- take over ------------------------------------------------------------------------------------------------

    /// The address bar's Enter: open that page (a bare host gets https://).
    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.address.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            return;
        }
        let url = if raw.starts_with("http://") || raw.starts_with("https://") { raw } else { format!("https://{raw}") };
        self.address.update(cx, |s, cx| s.set_value(url.clone(), window, cx));
        self.send(LiveInput::navigate(url));
        self.focus.focus(window, cx);
    }

    /// Open `url` in the teammate's browser (the engine starts it if needed) and hand you the controls.
    pub fn take_over_at(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.address.update(cx, |s, cx| s.set_value(url.clone(), window, cx));
        self.send(LiveInput::navigate(url.clone()));
        self.opening = Some(url);
        self.set_control(true, window, cx);
    }

    fn send(&self, input: LiveInput) {
        let _ = self.input.unbounded_send(input);
    }

    fn set_control(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.control = on;
        if on {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// A point in the window → the page's CSS pixels, through the aspect-fit rectangle; `None` outside the picture.
    fn to_frame(&self, pos: gpui::Point<Pixels>) -> Option<(i64, i64)> {
        let (_, fw, fh) = self.frame.as_ref()?;
        let from_info = self.info.as_ref().map(|i| (i.width, i.height)).filter(|(w, h)| *w > 0 && *h > 0);
        let (fw, fh) = self.view.filter(|_| self.streaming).or(from_info).unwrap_or((*fw, *fh));
        let a = self.area.get();
        let (aw, ah) = (f32::from(a.size.width), f32::from(a.size.height));
        if aw <= 0.0 || ah <= 0.0 {
            return None;
        }
        let scale = (aw / fw as f32).min(ah / fh as f32);
        let (w, h) = (fw as f32 * scale, fh as f32 * scale);
        let (x0, y0) = (f32::from(a.origin.x) + (aw - w) / 2.0, f32::from(a.origin.y) + (ah - h) / 2.0);
        let (x, y) = ((f32::from(pos.x) - x0) / scale, (f32::from(pos.y) - y0) / scale);
        (x >= 0.0 && y >= 0.0 && x <= fw as f32 && y <= fh as f32).then(|| (x.round() as i64, y.round() as i64))
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.control {
            return;
        }
        self.focus.focus(window, cx);
        if let Some((x, y)) = self.to_frame(ev.position) {
            self.send(LiveInput::click(x, y));
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.control || self.last_move.is_some_and(|t| t.elapsed() < MOVE_EVERY) {
            return;
        }
        if let Some((x, y)) = self.to_frame(ev.position) {
            self.last_move = Some(Instant::now());
            self.send(LiveInput::moved(x, y));
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.control {
            return;
        }
        let dy = match ev.delta {
            ScrollDelta::Pixels(p) => -f32::from(p.y),
            ScrollDelta::Lines(l) => -l.y * 40.0,
        };
        if dy.abs() >= 1.0 {
            // At the pointer, so the part of the page under it scrolls.
            let (x, y) = self.to_frame(ev.position).unwrap_or((0, 0));
            self.send(LiveInput { x: Some(x), y: Some(y), ..LiveInput::scroll(dy.round() as i64) });
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.control {
            return;
        }
        let k = &ev.keystroke;
        let m = k.modifiers;
        if (m.control || m.platform) && k.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()).filter(|t| !t.is_empty()) {
                self.send(LiveInput::type_text(text));
            }
            cx.stop_propagation();
            return;
        }
        let named = match k.key.as_str() {
            "enter" => Some("Enter"),
            "backspace" => Some("Backspace"),
            "tab" => Some("Tab"),
            "escape" => Some("Escape"),
            "delete" => Some("Delete"),
            "up" => Some("ArrowUp"),
            "down" => Some("ArrowDown"),
            "left" => Some("ArrowLeft"),
            "right" => Some("ArrowRight"),
            "home" => Some("Home"),
            "end" => Some("End"),
            "pageup" => Some("PageUp"),
            "pagedown" => Some("PageDown"),
            _ => None,
        };
        if let Some(key) = named {
            self.send(LiveInput::key(key));
        } else if let Some(ch) = k.key_char.clone().filter(|c| !c.is_empty() && !m.control && !m.alt && !m.platform) {
            self.send(LiveInput::type_text(ch));
        } else {
            return;
        }
        cx.stop_propagation();
    }

    /// "Signing in…" in place of the picture while the window is open.
    fn signing_in(&self, name: &str, avatar: familiar_ui::mascot::Avatar, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let this = cx.entity();
        let close_hint = if cfg!(target_os = "macos") {
            "Quit Chrome (⌘Q) or press Done when you're finished."
        } else {
            "Close the Chrome window when you're done."
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .px(px(24.0))
            .child(Mascot::new(format!("computer-signing-in-{}", self.bot), avatar, MascotState::NeedsYou, 88.0))
            .child(div().font_weight(FontWeight::MEDIUM).child("Signing in…"))
            .child(
                div()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .text_center()
                    .child(format!("{close_hint} {name} keeps the sign-in and waits until then.")),
            )
            .child(
                Button::new("computer-window-done", if self.window_busy { "Closing…" } else { "Done" })
                    .size(ButtonSize::Small)
                    .primary()
                    .icon(icons::CHECK)
                    .disabled(self.window_busy)
                    .tooltip("Closes the Chrome window (it saves your sign-in first)")
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.done_signing_in(cx))),
            )
    }

    fn sign_in_button(&self, cx: &mut Context<Self>) -> Button {
        let this = cx.entity();
        Button::new("computer-sign-in", if self.window_busy { "Opening…" } else { "Sign in in a window" })
            .size(ButtonSize::Small)
            .icon(icons::LOCK)
            .disabled(self.window_busy)
            .tooltip("Opens this page in a real Chrome window with its browser profile. Sign in there, then close the window.")
            .on_click(move |_, window, cx| {
                this.update(cx, |p, cx| {
                    let url = p.info.as_ref().map(|i| i.url.clone()).filter(|u| u.starts_with("http://") || u.starts_with("https://"));
                    p.sign_in_at(url, window, cx)
                })
            })
    }
}

/// An actual web page, seen in the last two minutes.
fn real_page(i: &LiveInfo) -> bool {
    let url = i.url.trim();
    (url.starts_with("http://") || url.starts_with("https://")) && Utc::now() - i.updated_at < chrono::Duration::minutes(2)
}

/// JPEG → BGRA frame for gpui's atlas.
fn decode(bytes: &[u8]) -> Option<(Arc<RenderImage>, u32, u32)> {
    let mut rgba = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?.to_rgba8();
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let (w, h) = rgba.dimensions();
    Some((Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])), w, h))
}

/// Send inputs in order, one request at a time; whatever piled up meanwhile goes out merged.
async fn send_inputs(
    client: Client,
    bot: Uuid,
    mut rx: futures::channel::mpsc::UnboundedReceiver<LiveInput>,
    errors: futures::channel::mpsc::UnboundedSender<String>,
) {
    use futures::StreamExt as _;
    while let Some(first) = rx.next().await {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        for input in familiar_client::coalesce_inputs(batch) {
            if let Err(e) = client.live_input(bot, &input).await {
                let _ = errors.unbounded_send(e.message());
            }
        }
    }
}

impl Render for ComputerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for old in self.stale.drain(..) {
            let _ = window.drop_image(old);
        }
        // A much bigger (or smaller) panel than the stream was asked for: ask again for that size.
        let want = self.wanted_size();
        let (sw, sh) = self.stream_size;
        if self.stream.is_some() && want.0 > 0 && (sw == 0 || want.0.abs_diff(sw) * 4 > sw || want.1.abs_diff(sh) * 4 > sh) {
            self.stop_stream();
            self.start_stream(cx);
        }
        let theme = Theme::of(cx).clone();
        let (name, avatar) = {
            let d = self.data.read(cx);
            match d.bot(self.bot) {
                Some(b) => (b.name.clone(), avatar_of(b)),
                None => ("Your teammate".to_owned(), familiar_ui::mascot::default_avatar(&self.bot.to_string())),
            }
        };
        let live = self.browsing;
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(14.0))
            .h(px(44.0))
            .flex_none()
            .border_b_1()
            .border_color(theme.line)
            .child(icon(icons::MONITOR).size(px(16.0)).text_color(theme.muted))
            .child(div().font_weight(FontWeight::MEDIUM).child("Computer"))
            .child(Led::new(if live { LedStatus::Online } else { LedStatus::Offline }).size(7.0))
            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(if self.window.open {
                "signing in"
            } else if live {
                "live"
            } else {
                "idle"
            }));

        if self.window.open {
            let body = self.signing_in(&name, avatar, &theme, cx);
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(head)
                .child(div().flex_1().flex().items_center().justify_center().child(anim::appear("computer-signing-in", body)));
        }

        let Some(info) = self.info.clone() else {
            // No session yet (or still loading).
            let body = if let Some(url) = self.opening.clone() {
                let host = reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or(url);
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(24.0))
                    .child(Mascot::new(format!("computer-opening-{}", self.bot), avatar, MascotState::Working, 88.0))
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Opening {host}…")))
                    .child(
                        div()
                            .text_size(px(text::SMALL))
                            .text_color(theme.muted)
                            .text_center()
                            .child(format!("Starting {name}'s browser. You'll have the controls as soon as the page shows.")),
                    )
            } else if self.loaded {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(24.0))
                    .child(Mascot::new(format!("computer-none-{}", self.bot), avatar, MascotState::Idle, 88.0))
                    .child(div().font_weight(FontWeight::MEDIUM).child("No browser session yet"))
                    .child(
                        div()
                            .text_size(px(text::SMALL))
                            .text_color(theme.muted)
                            .text_center()
                            .child(format!("When {name} opens a web page, you can watch it here and take over.")),
                    )
                    .when(self.window.available, |el| el.child(div().pt(px(4.0)).child(self.sign_in_button(cx))))
            } else {
                div().text_size(px(text::SMALL)).text_color(theme.muted).child("Looking for its browser…")
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(head)
                .child(div().flex_1().flex().items_center().justify_center().child(anim::appear("computer-empty", body)));
        };

        // The address follows the page unless you're typing in it.
        if info.url != self.shown_url {
            self.shown_url = info.url.clone();
            if !self.address.read(cx).focus_handle(cx).is_focused(window) {
                let url = info.url.clone();
                self.address.update(cx, |s, cx| s.set_value(url, window, cx));
            }
        }
        let this_go = cx.entity();
        let bar = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .mx(px(12.0))
            .mt(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().child(crate::text_input::field("computer-address", &self.address, 34.0, window, cx)))
                    .child(
                        Button::new("computer-go", "Go")
                            .size(ButtonSize::Small)
                            .icon(icons::ARROW_RIGHT)
                            .on_click(move |_, window, cx| this_go.update(cx, |p, cx| p.go(window, cx))),
                    ),
            )
            .child(
                div()
                    .px(px(2.0))
                    .truncate()
                    .text_size(px(text::CAPTION))
                    .text_color(theme.muted)
                    .child(SharedString::from(if info.title.trim().is_empty() { "Untitled page".to_owned() } else { info.title.clone() })),
            );

        let (area, scale) = (self.area.clone(), self.scale.clone());
        let control = self.control;
        let picture = div()
            .id("computer-frame")
            .relative()
            .flex_1()
            .min_h_0()
            .mx(px(12.0))
            .my(px(10.0))
            .rounded(px(RADIUS_CARD))
            .overflow_hidden()
            .border_1()
            .border_color(if control { theme.accent } else { theme.line })
            .bg(theme.sunken)
            .track_focus(&self.focus)
            .when(control, |el| el.cursor_crosshair())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_key_down(cx.listener(Self::on_key))
            .child(
                canvas(
                    move |bounds, window, _| {
                        area.set(bounds);
                        scale.set(window.scale_factor());
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .map(|el| match self.frame.as_ref().map(|(f, _, _)| f.clone()) {
                Some(f) => el.child(img(ImageSource::Render(f)).size_full().object_fit(ObjectFit::Contain)),
                None => el.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(text::SMALL))
                        .text_color(theme.muted)
                        .child("Loading the view…"),
                ),
            })
            .when(control, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(10.0))
                        .left(px(10.0))
                        .child(anim::appear(
                            "computer-in-control",
                            div()
                                .px(px(10.0))
                                .py(px(3.0))
                                .rounded_full()
                                .bg(theme.accent)
                                .text_color(theme.accent_ink)
                                .text_size(px(text::CAPTION))
                                .font_weight(FontWeight::MEDIUM)
                                .child("You're in control"),
                        )),
                )
            });

        let this = cx.entity();
        let foot = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .pb(px(12.0))
            .flex_none()
            .child(
                Button::new("computer-control", if control { "Return control" } else { "Take over" })
                    .size(ButtonSize::Small)
                    .icon(if control { icons::ARROW_RIGHT } else { icons::EYE })
                    .when(control, |b| b.primary())
                    .tooltip("Click, scroll and type on its page right here (for quick clicks; sign in to sites in a window)")
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.set_control(!p.control, window, cx))),
            )
            .when(self.window.available, |el| el.child(self.sign_in_button(cx)))
            .child(div().flex_1().min_w_0().truncate().text_size(px(text::CAPTION)).text_color(theme.muted).child(if control {
                format!("Click, scroll and type on the page. {} pastes.", if cfg!(target_os = "macos") { "⌘V" } else { "Ctrl+V" })
            } else {
                format!("{name} is driving.")
            }));

        div().size_full().flex().flex_col().child(head).child(bar).child(picture).child(foot)
    }
}
