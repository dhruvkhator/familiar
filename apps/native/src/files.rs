//! A teammate's Files tab (the web's `pages/Files.tsx`): the screenshots and files it saved while working
//! (`/api/bots/{id}/artifacts`), grouped by the run that made them with that run's chat and request, newest first.
//! Pictures show as thumbnails; every file can be saved through the system's Save dialog, and the kinds that can't
//! run anything (pictures, PDFs, plain text) can be opened with their usual app.
//!
//! Files are the teammate's output, so they are handled as untrusted: names are shown with hidden characters left out
//! and turned into safe Windows file names (no folders, reserved names or odd characters) before anything is written;
//! nothing else is ever opened (programs, scripts, web pages, shortcuts and office files are Save only); what is
//! written is marked as downloaded from the internet (Windows' "mark of the web"), so Windows and Office treat it
//! with care; and thumbnails are decoded off the UI thread, only from PNG, JPEG, GIF or WebP, within size limits.
//! Live: an `artifacts` notice for the teammate refreshes the list.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use familiar_client::{Artifact, Run, Thread};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, SectionHeader, Skeleton, card, divider, empty};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, ImageSource, IntoElement,
    ObjectFit, ParentElement as _, Render, RenderImage, SharedString, Styled as _,
    StyledImage as _, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::activity::OpenThread;
use crate::approval::strip_hidden;
use crate::data::{AppData, DataEvent, ago, excerpt};

/// Pictures larger than this aren't fetched for a thumbnail.
const THUMB_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// At most this many thumbnails are fetched (the newest pictures); the rest show an icon.
const THUMBS: usize = 30;
/// A thumbnail's longest side, in pixels.
const THUMB_SIDE: u32 = 480;
/// How many runs get their request and chat looked up.
const RUN_HEADS: usize = 30;

/// A thumbnail: decoded, or not shown (too big, an unknown kind, failed).
#[derive(Clone)]
enum Thumb {
    Loading,
    Ready(Arc<RenderImage>),
    None,
}

pub struct FilesTab {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    list: Option<Vec<Artifact>>,
    error: Option<String>,
    /// The runs the files came from (their request and chat), and the chats' titles.
    runs: HashMap<Uuid, Run>,
    threads: HashMap<Uuid, String>,
    thumbs: HashMap<Uuid, Thumb>,
    /// Files being fetched to save or open.
    busy: HashSet<Uuid>,
}

impl EventEmitter<OpenThread> for FilesTab {}

impl FilesTab {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.reload(cx),
            DataEvent::Changed(Some(n)) if n.t == "artifacts" && n.bot.as_deref().is_none_or(|b| b == this.bot.to_string()) => {
                this.reload(cx)
            }
            _ => {}
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            bot,
            list: None,
            error: None,
            runs: HashMap::new(),
            threads: HashMap::new(),
            thumbs: HashMap::new(),
            busy: HashSet::new(),
        };
        this.reload(cx);
        this
    }

    fn client(&self, cx: &App) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let bot = self.bot;
        let known: HashSet<Uuid> = self.runs.keys().copied().collect();
        let task = Tokio::spawn(cx, async move {
            let list = client.bot_artifacts(bot).await?;
            // The runs not looked up yet (the newest first), and the chats' titles.
            let mut wanted: Vec<Uuid> = Vec::new();
            for a in &list {
                if !known.contains(&a.run_id) && !wanted.contains(&a.run_id) && wanted.len() < RUN_HEADS {
                    wanted.push(a.run_id);
                }
            }
            let runs: Vec<Run> =
                futures::future::join_all(wanted.iter().map(|id| client.run(*id))).await.into_iter().filter_map(Result::ok).collect();
            let threads: Vec<Thread> = client.threads(bot).await.unwrap_or_default();
            Ok::<_, familiar_client::ApiError>((list, runs, threads))
        });
        cx.spawn(async move |this, cx| {
            let Ok(r) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok((list, runs, threads)) => {
                        p.runs.extend(runs.into_iter().map(|r| (r.id, r)));
                        p.threads = threads.into_iter().map(|t| (t.id, t.title.unwrap_or_default())).collect();
                        p.list = Some(list);
                        p.error = None;
                        p.load_thumbs(cx);
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetch and decode the pictures' thumbnails not tried yet (off the UI thread).
    fn load_thumbs(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<Artifact> = self
            .list
            .iter()
            .flatten()
            .filter(|a| is_picture(&a.mime))
            .take(THUMBS)
            .filter(|a| !self.thumbs.contains_key(&a.id))
            .cloned()
            .collect();
        for a in wanted {
            if a.bytes > THUMB_MAX_BYTES {
                self.thumbs.insert(a.id, Thumb::None);
                continue;
            }
            self.thumbs.insert(a.id, Thumb::Loading);
            let client = self.client(cx);
            let id = a.id;
            let task = Tokio::spawn(cx, async move { client.download_artifact(id).await });
            cx.spawn(async move |this, cx| {
                let bytes = match task.await {
                    Ok(Ok(b)) if b.len() as u64 <= THUMB_MAX_BYTES => b,
                    _ => {
                        let _ = this.update(cx, |p, cx| {
                            p.thumbs.insert(id, Thumb::None);
                            cx.notify()
                        });
                        return;
                    }
                };
                let decoded = cx.background_executor().spawn(async move { thumbnail(&bytes) }).await;
                let _ = this.update(cx, |p, cx| {
                    p.thumbs.insert(id, decoded.map(Thumb::Ready).unwrap_or(Thumb::None));
                    cx.notify()
                });
            })
            .detach();
        }
    }

    /// Fetch a file and write it where the owner picked (Save) or to a private temporary folder to open it.
    fn fetch_to(&mut self, a: &Artifact, open: bool, cx: &mut Context<Self>) {
        if self.busy.contains(&a.id) {
            return;
        }
        let name = safe_file_name(&a.name, &a.mime);
        if open && !can_open(&name) {
            return;
        }
        let target = if open {
            let dir = std::env::temp_dir().join("familiar-files").join(a.id.to_string());
            Some(dir.join(&name))
        } else {
            None
        };
        let rx = match &target {
            Some(_) => None,
            None => {
                let dir = std::env::var_os("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("Downloads"))
                    .filter(|d| d.is_dir())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                Some(cx.prompt_for_new_path(&dir, Some(&name)))
            }
        };
        let id = a.id;
        let client = self.client(cx);
        self.busy.insert(id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = match (target, rx) {
                (Some(p), _) => p,
                (None, Some(rx)) => match rx.await {
                    Ok(Ok(Some(p))) => p,
                    _ => {
                        let _ = this.update(cx, |p, cx| {
                            p.busy.remove(&id);
                            cx.notify()
                        });
                        return;
                    }
                },
                (None, None) => return,
            };
            let write_to = path.clone();
            let task = cx.update(|cx| {
                Tokio::spawn(cx, async move {
                    let bytes = client.download_artifact(id).await.map_err(|e| e.message())?;
                    if let Some(dir) = write_to.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make the folder: {e}"))?;
                    }
                    std::fs::write(&write_to, &bytes).map_err(|e| format!("Couldn't write the file: {e}"))?;
                    mark_downloaded(&write_to);
                    Ok::<_, String>(())
                })
            });
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r);
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&id);
                let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                match r {
                    Ok(()) if open => cx.open_with_system(&path),
                    Ok(()) => p.toast(Tone::Ok, format!("Saved {file}"), Some(path.display().to_string()), cx),
                    Err(e) => p.toast(Tone::Bad, if open { "Couldn't open it" } else { "Couldn't save it" }, Some(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    fn actions(&self, a: &Artifact, cx: &mut Context<Self>) -> gpui::Div {
        let key = a.id.as_u128() as u64;
        let busy = self.busy.contains(&a.id);
        let openable = can_open(&safe_file_name(&a.name, &a.mime));
        let (open, save) = (cx.entity(), cx.entity());
        let (a1, a2) = (a.clone(), a.clone());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .when(openable, |el| {
                el.child(
                    Button::new(("file-open", key), "Open")
                        .size(ButtonSize::Small)
                        .ghost()
                        .disabled(busy)
                        .tooltip("Open it with its usual app")
                        .on_click(move |_, _, cx| open.update(cx, |p, cx| p.fetch_to(&a1, true, cx))),
                )
            })
            .child(
                Button::new(("file-save", key), if busy { "Saving…" } else { "Save…" })
                    .size(ButtonSize::Small)
                    .icon(icons::DOWNLOAD)
                    .disabled(busy)
                    .tooltip(if openable { "Save a copy where you choose" } else { "Save a copy where you choose (Familiar doesn't open this kind of file)" })
                    .on_click(move |_, _, cx| save.update(cx, |p, cx| p.fetch_to(&a2, false, cx))),
            )
    }

    fn picture(&self, a: &Artifact, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let name = strip_hidden(&a.name, false);
        // Not asked for (past the newest [`THUMBS`]): an icon.
        let thumb = self.thumbs.get(&a.id).cloned().unwrap_or(Thumb::None);
        let preview = div()
            .h(px(150.0))
            .w_full()
            .rounded(px(RADIUS_CONTROL))
            .bg(theme.sunken)
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .child(match thumb {
                Thumb::Ready(image) => img(ImageSource::Render(image)).size_full().object_fit(ObjectFit::Contain).into_any_element(),
                Thumb::Loading => Skeleton::new(150.0).into_any_element(),
                Thumb::None => icon(icons::FILE).size(px(28.0)).text_color(theme.muted).into_any_element(),
            });
        card(cx)
            .flex_1()
            .min_w_0()
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(preview)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(text::SMALL)).truncate().child(name))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).truncate().child(format!("{} · {}", size_words(a.bytes), ago(Some(a.created_at))))),
                    )
                    .child(self.compact_actions(a, cx)),
            )
            .into_any_element()
    }

    /// A picture card's Open and Save, as icons.
    fn compact_actions(&self, a: &Artifact, cx: &mut Context<Self>) -> gpui::Div {
        let key = a.id.as_u128() as u64;
        let busy = self.busy.contains(&a.id);
        let (open, save) = (cx.entity(), cx.entity());
        let (a1, a2) = (a.clone(), a.clone());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .when(can_open(&safe_file_name(&a.name, &a.mime)), |el| {
                el.child(
                    Button::icon_only(("pic-open", key), icons::EYE)
                        .size(ButtonSize::Small)
                        .disabled(busy)
                        .tooltip("Open it with its usual app")
                        .on_click(move |_, _, cx| open.update(cx, |p, cx| p.fetch_to(&a1, true, cx))),
                )
            })
            .child(
                Button::icon_only(("pic-save", key), icons::DOWNLOAD)
                    .size(ButtonSize::Small)
                    .disabled(busy)
                    .tooltip("Save a copy where you choose")
                    .on_click(move |_, _, cx| save.update(cx, |p, cx| p.fetch_to(&a2, false, cx))),
            )
    }

    fn file_row(&self, a: &Artifact, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let name = strip_hidden(&a.name, false);
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(10.0))
            .child(
                div()
                    .size(px(32.0))
                    .flex_none()
                    .rounded(px(8.0))
                    .bg(theme.sunken)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(icons::FILE).size(px(16.0)).text_color(theme.muted)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).truncate().child(name))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).truncate().child(format!(
                        "{} · {} · {}",
                        kind_words(&a.mime),
                        size_words(a.bytes),
                        ago(Some(a.created_at))
                    ))),
            )
            .child(self.actions(a, cx))
            .into_any_element()
    }

    /// One run's files: where they came from, then the pictures (three across) and the other files.
    fn group(&self, run: Uuid, files: &[&Artifact], i: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let r = self.runs.get(&run);
        let when = files.first().map(|a| ago(Some(a.created_at))).unwrap_or_default();
        let chat = r.and_then(|r| self.threads.get(&r.thread_id)).map(|t| strip_hidden(t, false)).filter(|t| !t.trim().is_empty());
        let asked = r.and_then(|r| r.prompt.as_deref()).map(|p| excerpt(&strip_hidden(p, false), 110)).filter(|p| !p.is_empty());
        let thread = r.map(|r| r.thread_id);
        let this = cx.entity();
        let head = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(chat.unwrap_or_else(|| "A run".into())))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).truncate().child(match asked {
                        Some(p) => format!("{when} · “{p}”"),
                        None => when.clone(),
                    })),
            )
            .when_some(thread, |el, t| {
                el.child(
                    Button::new(("files-chat", run.as_u128() as u64), "Open chat")
                        .ghost()
                        .size(ButtonSize::Small)
                        .icon(icons::CHAT_ROUND_LINE)
                        .on_click(move |_, _, cx| this.update(cx, |_, cx| cx.emit(OpenThread(t)))),
                )
            });
        let pictures: Vec<&Artifact> = files.iter().copied().filter(|a| is_picture(&a.mime)).collect();
        let others: Vec<&Artifact> = files.iter().copied().filter(|a| !is_picture(&a.mime)).collect();
        let mut col = div().flex().flex_col().gap(px(10.0)).child(head);
        for row in pictures.chunks(3) {
            let mut line = div().flex().gap(px(10.0));
            for a in row {
                line = line.child(self.picture(a, cx));
            }
            for _ in row.len()..3 {
                line = line.child(div().flex_1());
            }
            col = col.child(line);
        }
        if !others.is_empty() {
            let mut c = card(cx).flex().flex_col().overflow_hidden();
            for (k, a) in others.iter().enumerate() {
                if k > 0 {
                    c = c.child(divider(cx));
                }
                c = c.child(self.file_row(a, cx));
            }
            col = col.child(c);
        }
        anim::stagger(SharedString::from(format!("files-run-{run}")), i, col).into_any_element()
    }
}

impl Render for FilesTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let name = self.data.read(cx).bot(self.bot).map(|b| strip_hidden(&b.name, false)).unwrap_or_else(|| "It".into());
        let count = self.list.as_ref().map(|l| l.len()).unwrap_or(0);
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(SectionHeader::new("Files").count(count))
                    .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(format!(
                        "Screenshots and files {name} saved while working, by the task that made them. Save any of them; pictures, PDFs and text also open with their usual app."
                    ))),
            );
        match (self.list.clone(), self.error.clone()) {
            (None, Some(e)) => page = page.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)),
            (None, None) => page = page.child(div().flex().flex_col().gap(px(10.0)).children((0..2).map(|_| Skeleton::new(120.0).radius(RADIUS_CARD)))),
            (Some(list), _) if list.is_empty() => {
                page = page.child(anim::appear(
                    "files-empty",
                    empty(
                        "No files yet",
                        Some(format!("Ask {name} to save a result as a file, or let it take screenshots while it browses.").into()),
                        cx,
                    ),
                ))
            }
            (Some(list), _) => {
                for (i, (run, files)) in group_by_run(&list).into_iter().enumerate() {
                    page = page.child(self.group(run, &files, i, cx));
                }
            }
        }
        page
    }
}

// ---- pure ---------------------------------------------------------------------------------------------------------

/// The files by the run that made them, in the list's order (newest first), each run once.
pub fn group_by_run(list: &[Artifact]) -> Vec<(Uuid, Vec<&Artifact>)> {
    let mut out: Vec<(Uuid, Vec<&Artifact>)> = Vec::new();
    for a in list {
        match out.iter_mut().find(|(r, _)| *r == a.run_id) {
            Some((_, v)) => v.push(a),
            None => out.push((a.run_id, vec![a])),
        }
    }
    out
}

/// A picture the tab can show a thumbnail of.
pub fn is_picture(mime: &str) -> bool {
    matches!(mime.trim().to_ascii_lowercase().as_str(), "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp")
}

/// "12 KB", "3.4 MB".
pub fn size_words(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.1} GB", b / KB / KB / KB)
    }
}

/// The kind of file, in words.
pub fn kind_words(mime: &str) -> String {
    let m = mime.trim().to_ascii_lowercase();
    match m.as_str() {
        "application/pdf" => "PDF".into(),
        "text/plain" => "Text".into(),
        "text/markdown" => "Markdown".into(),
        "text/csv" => "CSV".into(),
        "application/json" => "JSON".into(),
        "text/html" => "Web page".into(),
        "application/zip" => "Zip".into(),
        m if m.starts_with("image/") => "Picture".into(),
        m if m.is_empty() => "File".into(),
        m => strip_hidden(m, false),
    }
}

/// A safe Windows file name for an artifact: hidden characters, folders and characters Windows refuses left out,
/// reserved device names and trailing dots avoided, at most 120 characters (keeping the extension). A name with no
/// extension gets the usual one for its kind.
pub fn safe_file_name(name: &str, mime: &str) -> String {
    let flat = strip_hidden(name, false);
    let last = flat.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = last.chars().map(|c| if "<>:\"|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    s = s.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').to_owned();
    if s.is_empty() {
        s = "file".into();
    }
    if !s.contains('.')
        && let Some(ext) = ext_for(mime)
    {
        s = format!("{s}.{ext}");
    }
    let (stem, ext) = match s.rsplit_once('.') {
        Some((a, b)) if !a.is_empty() => (a.to_owned(), Some(b.to_owned())),
        _ => (s.clone(), None),
    };
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    let base = stem.split('.').next().unwrap_or("").to_ascii_uppercase();
    let stem = if reserved.contains(&base.as_str()) { format!("_{stem}") } else { stem };
    let ext = ext.map(|e| e.chars().take(16).collect::<String>());
    let room = 120usize.saturating_sub(ext.as_ref().map(|e| e.chars().count() + 1).unwrap_or(0));
    let stem: String = stem.chars().take(room.max(1)).collect();
    match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem,
    }
}

fn ext_for(mime: &str) -> Option<&'static str> {
    Some(match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "application/json" => "json",
        _ => return None,
    })
}

/// Whether Familiar opens a file of this (safe) name with its usual app: pictures, PDFs and plain text only. Anything
/// that could run something (programs, scripts, shortcuts, web pages, office files with macros) is Save only.
pub fn can_open(safe_name: &str) -> bool {
    let ext = safe_name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "pdf" | "txt" | "md" | "log")
}

/// Mark a written file as downloaded from the internet (an NTFS `Zone.Identifier` stream, as browsers write), so
/// Windows and Office warn before running or editing it. Best effort.
fn mark_downloaded(path: &Path) {
    if cfg!(windows) {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
}

/// A picture's thumbnail (BGRA for gpui), from PNG, JPEG, GIF or WebP only, within size limits; `None` otherwise.
fn thumbnail(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    use image::ImageFormat as F;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    if !matches!(reader.format(), Some(F::Png | F::Jpeg | F::Gif | F::WebP)) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let picture = reader.decode().ok()?;
    let mut rgba = picture.thumbnail(THUMB_SIDE, THUMB_SIDE).to_rgba8();
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_keeps_newest_first() {
        let (r1, r2) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let a = |n: u128, run| Artifact { id: Uuid::from_u128(100 + n), run_id: run, ..Default::default() };
        let list = vec![a(1, r1), a(2, r2), a(3, r1)];
        let g = group_by_run(&list);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, r1);
        assert_eq!(g[0].1.len(), 2);
        assert_eq!(g[1].0, r2);
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("report.pdf", "application/pdf"), "report.pdf");
        assert_eq!(safe_file_name(r"..\..\Windows\evil.exe", ""), "evil.exe");
        assert_eq!(safe_file_name("a/b/c.txt", "text/plain"), "c.txt");
        assert_eq!(safe_file_name("what?<now>.md", ""), "what__now_.md");
        assert_eq!(safe_file_name("CON.txt", ""), "_CON.txt");
        assert_eq!(safe_file_name("nul", "text/plain"), "_nul.txt");
        assert_eq!(safe_file_name("screenshot", "image/png"), "screenshot.png");
        assert_eq!(safe_file_name("...", ""), "file");
        assert_eq!(safe_file_name("trailing. . ", ""), "trailing");
        // The right-to-left override that would make "gpj.exe" read as "exe.jpg" is left out.
        assert_eq!(safe_file_name("photo\u{202E}gpj.exe", "image/jpeg"), "photogpj.exe");
        let long = format!("{}.png", "x".repeat(300));
        let safe = safe_file_name(&long, "");
        assert_eq!(safe.chars().count(), 120);
        assert!(safe.ends_with(".png"));
    }

    #[test]
    fn only_harmless_kinds_open() {
        for ok in ["a.png", "a.JPG", "a.pdf", "notes.txt", "README.md"] {
            assert!(can_open(ok), "{ok}");
        }
        for no in ["a.exe", "a.bat", "a.ps1", "a.html", "a.svg", "a.lnk", "a.docm", "a.js", "a.csv", "noext", "a.png.exe"] {
            assert!(!can_open(no), "{no}");
        }
        assert!(is_picture("image/PNG"));
        assert!(!is_picture("image/svg+xml"));
        assert_eq!(size_words(512), "512 B");
        assert_eq!(size_words(2048), "2 KB");
        assert_eq!(size_words(3 * 1024 * 1024 + 400_000), "3.4 MB");
        assert_eq!(kind_words("application/pdf"), "PDF");
    }

    #[cfg(windows)]
    #[test]
    fn written_files_carry_the_mark_of_the_web() {
        let dir = std::env::temp_dir().join(format!("familiar-files-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("report.pdf");
        std::fs::write(&file, b"%PDF").unwrap();
        mark_downloaded(&file);
        let mut stream = file.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let mark = std::fs::read_to_string(PathBuf::from(stream)).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(mark.contains("ZoneId=3"), "{mark:?}");
    }

    #[test]
    fn thumbnails_only_from_known_pictures() {
        assert!(thumbnail(b"not a picture").is_none());
        assert!(thumbnail(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_none());
        // A 1x1 PNG.
        let mut png = Vec::new();
        image::RgbaImage::new(1, 1).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert!(thumbnail(&png).is_some());
    }
}
