//! The window's root: the [`Engine`]'s lifecycle screens (getting ready, first run, couldn't start, stopping) and,
//! once signed in, the app [`Shell`].

use familiar_ui::anim;
use familiar_ui::components::{Button, Skeleton};
use familiar_ui::icons;
use familiar_ui::mascot::{Mascot, MascotState, default_avatar};
use familiar_ui::notice::{NoticeChipIcon, notice_chip};
use familiar_ui::theme::{Theme, text};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;

use crate::data::AppData;
use crate::engine::{Engine, Phase, familiar_home};
use crate::perf;
use crate::shell::Shell;
use crate::text_input;
use crate::titlebar;

pub struct Root {
    engine: Entity<Engine>,
    shell: Option<Entity<Shell>>,
    open: Option<String>,
    setup: Option<SetupForm>,
}

/// First run: create the owner account (the web and phone sign in with it; this app signs itself in).
struct SetupForm {
    email: Entity<InputState>,
    password: Entity<InputState>,
    busy: bool,
    error: Option<String>,
}

impl Root {
    pub fn new(open: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        familiar_ui::observe_window(window, cx);
        let engine = cx.new(Engine::new);
        cx.observe_in(&engine, window, |this, engine, window, cx| {
            let phase = engine.read(cx).phase.clone();
            match phase {
                Phase::Ready(client) if this.shell.is_none() => {
                    let mode = engine.read(cx).mode;
                    if let Some(host) = crate::engine::HOSTED.lock().unwrap().clone().filter(|_| mode == crate::engine::Mode::Hosted) {
                        crate::notify::watch_host(host, cx);
                    }
                    let data = cx.new(|cx| AppData::new(client, mode, cx));
                    let open = this.open.take();
                    this.shell = Some(cx.new(|cx| Shell::new(data, open, window, cx)));
                    this.setup = None;
                }
                Phase::FirstRun(_) if this.setup.is_none() => this.setup = Some(SetupForm::new(window, cx)),
                // A newer Familiar ran on this data: look for that update right away.
                Phase::Failed(ref m) if m == familiar_host::NEWER_DATA => {
                    if let Some(updates) = crate::updates::entity(cx) {
                        updates.update(cx, |u, cx| u.check(cx));
                    }
                }
                _ => {}
            }
            crate::tray::sync(cx);
            cx.notify();
        })
        .detach();
        // The "newer data" screen shows where the update stands.
        if let Some(updates) = crate::updates::entity(cx) {
            cx.observe(&updates, |this, _, cx| {
                if this.shell.is_none() {
                    cx.notify();
                }
            })
            .detach();
        }
        // With the tray up, closing the window hides it and the engine keeps working; Quit in the tray quits.
        // Without one, closing quits (in host mode the engine drains first; the app quits when that is done).
        let quitting = engine.clone();
        window.on_window_should_close(cx, move |window, cx| {
            if crate::tray::installed(cx) {
                crate::desktop::hide_window(window, cx);
                return false;
            }
            quitting.update(cx, |e, cx| e.request_quit(cx))
        });
        Self { engine, shell: None, open, setup: None }
    }

    /// Quit for real (the tray's Quit). Host mode shows the window while in-flight work drains.
    pub fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine.update(cx, |e, cx| e.request_quit(cx)) {
            cx.quit();
        } else {
            crate::desktop::show_window(window, cx);
        }
    }

    pub fn shell(&self) -> Option<Entity<Shell>> {
        self.shell.clone()
    }

    fn submit_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Phase::FirstRun(client) = self.engine.read(cx).phase.clone() else { return };
        let Some(form) = self.setup.as_mut() else { return };
        if form.busy {
            return;
        }
        let email = form.email.read(cx).value().trim().to_owned();
        let password = form.password.read(cx).value().to_string();
        if !email.contains('@') {
            form.error = Some("Enter your email address.".into());
            form.email.update(cx, |s, cx| s.focus(window, cx));
            cx.notify();
            return;
        }
        if password.chars().count() < 10 {
            form.error = Some("Use at least 10 characters for the password.".into());
            form.password.update(cx, |s, cx| s.focus(window, cx));
            cx.notify();
            return;
        }
        form.busy = true;
        form.error = None;
        cx.notify();
        let signup = client.clone();
        let task = Tokio::spawn(cx, async move { signup.setup(&email, &password).await.map(|_| ()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Ok(())) => this.engine.update(cx, |e, cx| e.signed_up(client, cx)),
                Ok(Err(e)) => this.setup_failed(e.message(), cx),
                Err(e) => this.setup_failed(e.to_string(), cx),
            });
        })
        .detach();
    }

    fn setup_failed(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(form) = self.setup.as_mut() {
            form.busy = false;
            form.error = Some(message);
            cx.notify();
        }
    }

    fn boot(&self, message: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let message = if message.trim().is_empty() { "Starting up…" } else { message };
        screen(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .child(Mascot::new("boot", default_avatar("familiar"), MascotState::Working, 112.0))
                .child(title("Getting Familiar ready…", &theme).mt(px(8.0)))
                .child(
                    div()
                        .text_size(px(text::LEAD))
                        .text_color(theme.muted)
                        .text_center()
                        .max_w(px(420.0))
                        .child(SharedString::from(message.to_owned())),
                )
                .child(div().mt(px(14.0)).child(Skeleton::new(4.0).width(180.0).radius(2.0))),
            &theme,
        )
    }

    fn failed(&self, message: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let engine = self.engine.clone();
        screen(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .w(px(520.0))
                .child(Mascot::new("boot-failed", default_avatar("familiar"), MascotState::Paused, 96.0))
                .child(title("Familiar couldn't start", &theme).mt(px(8.0)))
                .child(
                    div()
                        .w_full()
                        .mt(px(6.0))
                        .child(notice_chip(&theme, false, "What happened", message.to_owned(), NoticeChipIcon::Tile)),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .mt(px(12.0))
                        .child(Button::new("boot-retry", "Try again").primary().icon(icons::REFRESH).on_click(
                            move |_, _, cx| engine.update(cx, |e, cx| e.connect(cx)),
                        ))
                        .child(Button::new("boot-logs", "Open logs").on_click(|_, _, _| {
                            let logs = familiar_home().join("logs");
                            let _ = std::fs::create_dir_all(&logs);
                            let _ = familiar_host::open_folder(&logs);
                        })),
                ),
            &theme,
        )
    }

    /// The data was migrated by a newer Familiar (see `familiar_host::NEWER_DATA`): say so plainly and offer the update.
    fn newer_data(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let updates = crate::updates::entity(cx);
        let state = updates.as_ref().map(|u| {
            let u = u.read(cx);
            (u.summary(), u.ready().is_some(), u.busy() || u.restarting, matches!(u.status, crate::updates::Status::Off(_)), u.apply_error.clone())
        });
        let (summary, ready, busy, off, error) = state.unwrap_or_else(|| (String::new(), false, false, true, None));
        let mut buttons = div().flex().gap(px(10.0)).mt(px(12.0));
        if let Some(updates) = updates.filter(|_| !off) {
            buttons = buttons.child(if ready {
                Button::new("newer-restart", "Restart to update")
                    .primary()
                    .icon(icons::REFRESH)
                    .disabled(busy)
                    .on_click(move |_, _, cx| updates.update(cx, |u, cx| u.restart(cx)))
            } else {
                Button::new("newer-check", if busy { "Checking…" } else { "Check for updates" })
                    .primary()
                    .icon(icons::REFRESH)
                    .disabled(busy)
                    .on_click(move |_, _, cx| updates.update(cx, |u, cx| u.check(cx)))
            });
        }
        buttons = buttons.child(Button::new("newer-logs", "Open logs").on_click(|_, _, _| {
            let logs = familiar_home().join("logs");
            let _ = std::fs::create_dir_all(&logs);
            let _ = familiar_host::open_folder(&logs);
        }));
        let status = match (error, off) {
            (Some(e), _) => Some((e, theme.bad)),
            (None, true) => Some((
                "Download the newest Familiar from github.com/dhruvkhator/familiar/releases and install it.".to_owned(),
                theme.muted,
            )),
            (None, false) => (!summary.is_empty()).then_some((summary, theme.muted)),
        };
        screen(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .w(px(520.0))
                .child(Mascot::new("newer-data", default_avatar("familiar"), MascotState::Paused, 96.0))
                .child(title("This data is from a newer Familiar", &theme).mt(px(8.0)))
                .child(div().text_size(px(text::LEAD)).text_color(theme.muted).text_center().child(format!(
                    "A newer version of Familiar has run on this computer's data, and this one ({}) can't open it safely. \
                     Update to the newest version; your data stays as it is.",
                    crate::updates::VERSION
                )))
                .when_some(status, |el, (s, color)| {
                    el.child(div().mt(px(6.0)).text_size(px(text::SMALL)).text_color(color).text_center().child(s))
                })
                .child(buttons),
            &theme,
        )
    }

    fn first_run(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if self.setup.is_none() {
            self.setup = Some(SetupForm::new(window, cx));
        }
        let form = self.setup.as_ref().unwrap();
        let (email, password, busy, error) = (form.email.clone(), form.password.clone(), form.busy, form.error.clone());
        let this = cx.entity();
        let label = |s: &'static str| {
            div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).text_color(theme.ink).child(s)
        };
        screen(
            anim::appear(
                "first-run",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .w(px(380.0))
                    .child(Mascot::new("first-run", default_avatar("familiar"), MascotState::Idle, 96.0))
                    .child(title("Welcome to Familiar", &theme).mt(px(8.0)))
                    .child(
                        div()
                            .text_size(px(text::LEAD))
                            .text_color(theme.muted)
                            .text_center()
                            .child("Create your account. It protects your Familiar, and signs you in from your phone or another computer."),
                    )
                    .child(
                        div()
                            .w_full()
                            .mt(px(14.0))
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label("Email"))
                            .child(text_input::field("setup-email", &email, 40.0, window, cx))
                            .child(label("Password").mt(px(8.0)))
                            .child(text_input::field("setup-password", &password, 40.0, window, cx)),
                    )
                    .when_some(error, |el, e| {
                        el.child(div().w_full().text_size(px(text::SMALL)).text_color(theme.bad).child(e))
                    })
                    .when(!familiar_host::claude_found(), |el| {
                        el.child(div().w_full().mt(px(8.0)).child(notice_chip(
                            &theme,
                            true,
                            "Claude Code isn't installed",
                            familiar_host::CLAUDE_MISSING,
                            NoticeChipIcon::Tile,
                        )))
                    })
                    .child(
                        div().w_full().mt(px(10.0)).child(
                            Button::new("setup-create", if busy { "Creating…" } else { "Create account" })
                                .primary()
                                .full_width()
                                .disabled(busy)
                                .on_click(move |_, window, cx| this.update(cx, |r, cx| r.submit_setup(window, cx))),
                        ),
                    ),
            )
            .into_any_element(),
            &theme,
        )
    }

    fn stopping(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        screen(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .child(Mascot::new("stopping", default_avatar("familiar"), MascotState::Paused, 96.0))
                .child(title("Stopping Familiar…", &theme).mt(px(8.0)))
                .child(
                    div()
                        .text_size(px(text::LEAD))
                        .text_color(theme.muted)
                        .child("Letting running work finish and closing the database."),
                ),
            &theme,
        )
    }
}

impl SetupForm {
    fn new(window: &mut Window, cx: &mut Context<Root>) -> Self {
        let email = text_input::new_line("you@example.com", false, window, cx);
        let password = text_input::new_line("At least 10 characters", true, window, cx);
        cx.subscribe_in(&email, window, |this: &mut Root, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => {
                if let Some(f) = this.setup.as_ref() {
                    f.password.update(cx, |s, cx| s.focus(window, cx));
                }
            }
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        cx.subscribe_in(&password, window, |this: &mut Root, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.submit_setup(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        email.update(cx, |s, cx| s.focus(window, cx));
        Self { email, password, busy: false, error: None }
    }
}

fn title(s: &'static str, theme: &Theme) -> gpui::Div {
    div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child(s)
}

/// A full-window, centred lifecycle screen.
fn screen(content: impl IntoElement, theme: &Theme) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.bg)
        .text_color(theme.ink)
        .text_size(px(text::BODY))
        .font_family(theme.font_sans.clone())
        .child(div().pb(px(48.0)).child(content))
        .into_any_element()
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = self.engine.read(cx).phase.clone();
        let with_sidebar = self.shell.is_some() && !matches!(phase, Phase::Stopping);
        let body = match phase {
            Phase::Stopping => self.stopping(cx),
            // Not cached: a cached view that redraws redraws every view inside it, so the shell's own children (the
            // sidebar, a teammate's page) are the cached ones.
            _ if self.shell.is_some() => self.shell.clone().unwrap().into_any_element(),
            Phase::Booting(message) => self.boot(&message, cx),
            Phase::FirstRun(_) => self.first_run(window, cx),
            Phase::Failed(message) => if message == familiar_host::NEWER_DATA { self.newer_data(cx) } else { self.failed(&message, cx) },
            Phase::Ready(_) => self.boot("Opening your workspace…", cx),
        };
        perf::milestone_painted("first_frame");
        window_frame(body, with_sidebar, window, cx)
    }
}

/// A window root's frame around `body`: the per-frame motion bookkeeping, the title bar (Windows) and the perf probe.
pub fn window_frame(body: AnyElement, with_sidebar: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    perf::frame_begin();
    perf::count("Root");
    anim::frame(window);
    if !titlebar::CUSTOM {
        return div().size_full().child(body).child(perf::frame_probe()).into_any_element();
    }
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(Theme::of(cx).bg)
        .child(titlebar::render(with_sidebar, window, cx))
        .child(div().flex_1().min_h_0().w_full().child(body))
        .child(perf::frame_probe())
        .into_any_element()
}
