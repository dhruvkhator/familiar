//! The window's root: the [`Engine`]'s lifecycle screens (getting ready, first run, couldn't start, stopping) and,
//! once signed in, the app [`Shell`].

use familiar_ui::anim;
use familiar_ui::components::{Button, Skeleton};
use familiar_ui::icons;
use familiar_ui::mascot::{Mascot, MascotState, default_avatar};
use familiar_ui::notice::{NoticeChipIcon, notice_chip};
use familiar_ui::theme::{Theme, text};
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;

use crate::data::AppData;
use crate::engine::{Engine, Phase, familiar_home};
use crate::shell::Shell;
use crate::text_input;

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
                _ => {}
            }
            crate::tray::sync(cx);
            cx.notify();
        })
        .detach();
        // With the tray up, closing the window hides it and the engine keeps working; Quit in the tray quits.
        // Without one, closing quits (in host mode the engine drains first; the app quits when that is done).
        let quitting = engine.clone();
        window.on_window_should_close(cx, move |window, cx| {
            if crate::tray::installed(cx) {
                crate::desktop::hide_window(window);
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
            crate::desktop::show_window(window);
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
                            .child("Create your account. You'll use it to sign in from the web or your phone."),
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
        anim::frame(window);
        let phase = self.engine.read(cx).phase.clone();
        match phase {
            Phase::Stopping => self.stopping(cx),
            _ if self.shell.is_some() => self.shell.clone().unwrap().into_any_element(),
            Phase::Booting(message) => self.boot(&message, cx),
            Phase::FirstRun(_) => self.first_run(window, cx),
            Phase::Failed(message) => self.failed(&message, cx),
            Phase::Ready(_) => self.boot("Opening your workspace…", cx),
        }
    }
}
