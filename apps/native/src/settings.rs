//! App Settings (the web's `pages/AppSettings.tsx`): the Claude Code / Codex sign-ins on this computer, the owner
//! account, this computer (who runs the engine, data folder, start at login, appearance, motion), the CRM's webhooks
//! ([`crate::crm_webhooks`]) and about.

use std::time::{Duration, Instant};

use familiar_client::AccountUpdate;
use familiar_host::CliStatus;
use familiar_ui::appearance::{self, AppearanceMode};
use familiar_ui::components::{Button, ButtonSize, Led, LedStatus, Segmented, Switch, card, chip, divider};
use familiar_ui::icons::{self, icon};
use familiar_ui::motion::{self, ReduceMotion};
use familiar_ui::theme::{RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;

use crate::data::{AppData, Mode};
use crate::engine::{HOSTED, familiar_home};
use crate::{desktop, prefs, text_input};

#[derive(Clone, Copy, PartialEq)]
enum Cli {
    Claude,
    Codex,
}

impl Cli {
    fn name(self) -> &'static str {
        match self {
            Cli::Claude => "Claude Code",
            Cli::Codex => "Codex",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
        }
    }
    fn blurb(self) -> &'static str {
        match self {
            Cli::Claude => "Your Claude subscription runs the teammates set to Claude.",
            Cli::Codex => "Your ChatGPT plan runs the teammates set to Codex.",
        }
    }
    fn install(self) -> &'static str {
        match self {
            Cli::Claude => "npm i -g @anthropic-ai/claude-code",
            Cli::Codex => "npm i -g @openai/codex@latest",
        }
    }
}

pub struct AppSettings {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    claude: Option<CliStatus>,
    codex: Option<CliStatus>,
    checking: bool,
    checked_at: Option<Instant>,
    /// The page is on screen (Shell sets it): window focus re-checks the sign-ins only then.
    shown: bool,
    email: Entity<InputState>,
    orig_email: String,
    new_pw: Entity<InputState>,
    again_pw: Entity<InputState>,
    current_pw: Entity<InputState>,
    saving: bool,
    error: Option<String>,
    autostart: bool,
    /// "Send CRM updates to another app".
    webhooks: Entity<crate::crm_webhooks::CrmWebhooks>,
}

impl AppSettings {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let email = text_input::new_line("you@example.com", false, window, cx);
        let new_pw = text_input::new_line("New password", true, window, cx);
        let again_pw = text_input::new_line("Type it again", true, window, cx);
        let current_pw = text_input::new_line("Current password", true, window, cx);
        for input in [&email, &new_pw, &again_pw, &current_pw] {
            cx.subscribe_in(input, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.save_account(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            })
            .detach();
        }
        // The web re-checks on window focus: the sign-in finishes in a terminal and the browser.
        cx.observe_window_activation(window, |this, window, cx| {
            let stale = this.checked_at.is_none_or(|t| t.elapsed() > Duration::from_secs(5));
            if window.is_window_active() && this.shown && stale {
                this.check(cx);
            }
        })
        .detach();
        let webhooks = cx.new(|cx| crate::crm_webhooks::CrmWebhooks::new(data.clone(), toasts.clone(), cx));
        let mut this = Self {
            data,
            toasts,
            webhooks,
            claude: None,
            codex: None,
            checking: false,
            checked_at: None,
            shown: true,
            email,
            orig_email: String::new(),
            new_pw,
            again_pw,
            current_pw,
            saving: false,
            error: None,
            autostart: desktop::autostart_enabled(),
        };
        this.check(cx);
        this.load_me(window, cx);
        this
    }

    pub fn webhooks(&self) -> Entity<crate::crm_webhooks::CrmWebhooks> {
        self.webhooks.clone()
    }

    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        let was = self.shown;
        self.shown = shown;
        if shown && !was {
            self.autostart = desktop::autostart_enabled();
            self.check(cx);
        }
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        cx.notify();
        let task = Tokio::spawn(cx, async { tokio::join!(familiar_host::claude_status(), familiar_host::codex_status()) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok((claude, codex)) = r {
                    this.claude = Some(claude);
                    this.codex = Some(codex);
                }
                this.checking = false;
                this.checked_at = Some(Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    fn load_me(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let task = Tokio::spawn(cx, async move { client.me().await });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(me)) = task.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.orig_email = me.email.clone();
                    this.email.update(cx, |s, cx| s.set_value(me.email, window, cx));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut App) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    fn save_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let email = self.email.read(cx).value().trim().to_lowercase();
        let next = self.new_pw.read(cx).value().to_string();
        let again = self.again_pw.read(cx).value().to_string();
        let current = self.current_pw.read(cx).value().to_string();
        let email_changed = !email.is_empty() && email != self.orig_email.to_lowercase();
        let fail = |this: &mut Self, msg: &str, cx: &mut Context<Self>| {
            this.error = Some(msg.into());
            cx.notify();
        };
        if !email_changed && next.is_empty() {
            return fail(self, "Change the email or the password first.", cx);
        }
        if email_changed && !email.contains('@') {
            return fail(self, "Enter a valid email address.", cx);
        }
        if !next.is_empty() && next != again {
            return fail(self, "The new passwords don't match.", cx);
        }
        if !next.is_empty() && next.chars().count() < 10 {
            return fail(self, "New password must be at least 10 characters.", cx);
        }
        if current.is_empty() {
            self.current_pw.update(cx, |s, cx| s.focus(window, cx));
            return fail(self, "Enter your current password to save.", cx);
        }
        self.saving = true;
        self.error = None;
        cx.notify();
        let update = AccountUpdate {
            current_password: Some(current),
            email: email_changed.then_some(email),
            new_password: (!next.is_empty()).then(|| next.clone()),
        };
        let client = self.data.read(cx).client.clone();
        let attached = self.data.read(cx).mode == Mode::Attached;
        let task = Tokio::spawn(cx, async move { client.update_account(&update).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match r {
                    Ok(user) => {
                        this.orig_email = user.email.clone();
                        this.email.update(cx, |s, cx| s.set_value(user.email, window, cx));
                        for input in [&this.new_pw, &this.again_pw, &this.current_pw] {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        // Attached mode signs in with the saved owner password: keep that file in step.
                        if attached && !next.is_empty() {
                            let file = familiar_home().join("owner_password.txt");
                            if file.exists() {
                                let _ = std::fs::write(&file, &next);
                            }
                        }
                        let msg = if next.is_empty() { "Account updated" } else { "Account updated. Other devices were signed out." };
                        this.toast(Tone::Ok, msg, None, cx);
                    }
                    Err(e) => {
                        this.error = Some(e.clone());
                        this.toast(Tone::Bad, "Couldn't save your account", Some(e), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- sections ---------------------------------------------------------------------------------------------

    fn cli_card(&self, which: Cli, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let st = match which {
            Cli::Claude => self.claude.clone(),
            Cli::Codex => self.codex.clone(),
        };
        let (tone, label) = match &st {
            None => (Tone::Muted, "Checking…".to_owned()),
            Some(s) if !s.installed => (Tone::Warn, "Not installed".to_owned()),
            Some(s) if s.needs_update => (Tone::Warn, "Update needed".to_owned()),
            Some(s) if !s.logged_in => (Tone::Warn, "Not signed in".to_owned()),
            Some(s) => {
                let mut l = "Signed in".to_owned();
                if let Some(m) = &s.auth_method {
                    l.push_str(&format!(" via {m}"));
                }
                if let Some(v) = &s.version {
                    l.push_str(&format!(" · {}", v.split_whitespace().rev().find(|t| t.chars().any(|c| c.is_ascii_digit())).unwrap_or(v)));
                }
                (Tone::Ok, l)
            }
        };
        let key = which.key();
        let run = move |action: String| {
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut App| {
                if let Err(e) = familiar_host::open_cli_terminal(&action) {
                    tracing::warn!("open terminal: {e}");
                }
            }
        };
        let installed = st.as_ref().is_some_and(|s| s.installed);
        let logged_in = st.as_ref().is_some_and(|s| s.logged_in);
        let needs_update = st.as_ref().is_some_and(|s| s.needs_update);
        let this = cx.entity();
        let mut buttons = div().flex().flex_wrap().gap(px(8.0));
        if st.is_some() && !installed {
            buttons = buttons.child(
                Button::new(SharedString::from(format!("{key}-install")), "Install")
                    .primary()
                    .size(ButtonSize::Small)
                    .on_click(run(format!("{key}_install"))),
            );
        } else {
            let b = Button::new(SharedString::from(format!("{key}-login")), if logged_in { "Switch account" } else { "Sign in" })
                .size(ButtonSize::Small)
                .disabled(st.is_none())
                .on_click(run(format!("{key}_login")));
            buttons = buttons.child(if logged_in { b } else { b.primary() });
        }
        if which == Cli::Codex && needs_update {
            buttons = buttons.child(
                Button::new("codex-update", "Update Codex").primary().size(ButtonSize::Small).on_click(run("codex_update".into())),
            );
        }
        buttons = buttons.child(
            Button::new(SharedString::from(format!("{key}-check")), if self.checking { "Checking…" } else { "Check again" })
                .ghost()
                .size(ButtonSize::Small)
                .icon(icons::REFRESH)
                .disabled(self.checking)
                .on_click(move |_, _, cx| this.update(cx, |p, cx| p.check(cx))),
        );
        card(cx)
            .flex_1()
            .min_w(px(260.0))
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .size(px(36.0))
                            .flex_none()
                            .rounded(px(RADIUS_CONTROL))
                            .bg(theme.accent_soft)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                icon(if which == Cli::Claude { icons::MAGIC_STICK_3 } else { icons::WIDGET })
                                    .size(px(18.0))
                                    .text_color(theme.accent),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::MEDIUM).child(which.name()))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(which.blurb())),
                    ),
            )
            .child(div().flex().child(chip(tone, label, cx)))
            .when(st.is_some() && !installed, |el| {
                el.child(
                    div()
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(RADIUS_CONTROL))
                        .bg(theme.sunken)
                        .font_family(theme.font_mono.clone())
                        .text_size(px(text::CAPTION))
                        .child(which.install()),
                )
            })
            .child(buttons)
            .into_any_element()
    }

    fn account(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let next = self.new_pw.read(cx).value().to_string();
        let again = self.again_pw.read(cx).value().to_string();
        let mismatch = !next.is_empty() && !again.is_empty() && next != again;
        let email_changed = {
            let e = self.email.read(cx).value().trim().to_lowercase();
            !e.is_empty() && e != self.orig_email.to_lowercase()
        };
        let can_save = !self.saving
            && !self.current_pw.read(cx).value().is_empty()
            && (email_changed || !next.is_empty())
            && !mismatch;
        let this = cx.entity();
        let mut form = card(cx).p(px(16.0)).flex().flex_col().gap(px(14.0));
        form = form
            .child(field("Email", None, text_input::field("acct-email", &self.email, 38.0, window, cx), &theme))
            .child(field(
                "New password",
                Some("At least 10 characters. Leave blank to keep the current one.".into()),
                text_input::field("acct-new", &self.new_pw, 38.0, window, cx),
                &theme,
            ));
        if !next.is_empty() {
            form = form.child(field(
                "Confirm new password",
                mismatch.then(|| "Doesn't match.".into()),
                text_input::field("acct-again", &self.again_pw, 38.0, window, cx),
                &theme,
            ));
        }
        form.child(field(
            "Current password",
            Some("Needed to save any change.".into()),
            text_input::field("acct-current", &self.current_pw, 38.0, window, cx),
            &theme,
        ))
        .when_some(self.error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
        .child(
            div().flex().child(
                Button::new("acct-save", if self.saving { "Saving…" } else { "Save changes" })
                    .primary()
                    .disabled(!can_save)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.save_account(window, cx))),
            ),
        )
        .into_any_element()
    }

    fn computer(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let mode = self.data.read(cx).mode;
        let (engine_label, engine_hint) = match mode {
            Mode::Hosted => (
                "Running Familiar's engine",
                "This app runs the database, the API and your teammates' work. It keeps running in the tray when you close the window.",
            ),
            Mode::Attached => (
                "Using the Familiar app's engine",
                "The Familiar desktop app already runs the engine on this computer; this window uses it.",
            ),
        };
        let this = cx.entity();
        let toasts = self.toasts.clone();
        let theme_mode = appearance::mode(cx);
        let selected = AppearanceMode::ALL.iter().position(|m| *m == theme_mode).unwrap_or(0);
        let reduce = motion::preference(cx) == ReduceMotion::On;
        let this_motion = cx.entity();
        let this_theme = cx.entity();
        let rows: Vec<AnyElement> = vec![
            setting_row(
                engine_label,
                engine_hint,
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .child(Led::new(LedStatus::Online))
                    .child(if mode == Mode::Hosted { "This app" } else { "Familiar app" })
                    .into_any_element(),
                &theme,
            ),
            setting_row(
                "Data folder",
                "Your teammates' workspaces, the database and logs.",
                Button::new("open-data", "Open data folder")
                    .size(ButtonSize::Small)
                    .on_click(move |_, _, cx| {
                        let dir = HOSTED.lock().unwrap().as_ref().map(|h| h.bots_dir()).unwrap_or_else(familiar_home);
                        if let Err(e) = familiar_host::open_folder(&dir) {
                            toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't open the folder", Some(e.into()), cx));
                        }
                    })
                    .into_any_element(),
                &theme,
            ),
            setting_row(
                "Start at login",
                "Open Familiar in the tray when you sign in to Windows.",
                Switch::new("autostart", self.autostart)
                    .on_toggle(move |on, _, cx| {
                        this.update(cx, |p, cx| {
                            match desktop::set_autostart(on) {
                                Ok(()) => p.autostart = on,
                                Err(e) => p.toast(Tone::Bad, "Couldn't change start at login", Some(e), cx),
                            }
                            cx.notify();
                        })
                    })
                    .into_any_element(),
                &theme,
            ),
            setting_row(
                "Appearance",
                "Follow Windows, or pin light or dark.",
                Segmented::new(
                    "settings-theme",
                    AppearanceMode::ALL.iter().map(|m| (SharedString::from(m.label()), Some(m.icon()))).collect(),
                    selected,
                )
                .segment_width(84.0)
                .on_select(move |i, _, cx| {
                    let m = AppearanceMode::ALL[i];
                    appearance::set_mode(m, cx);
                    prefs::update(|p| p.theme = m);
                    this_theme.update(cx, |_, cx| cx.notify());
                })
                .into_any_element(),
                &theme,
            ),
            setting_row(
                "Reduce motion",
                "Fewer animations. Windows' own setting is followed when this is off.",
                Switch::new("settings-motion", reduce)
                    .on_toggle(move |on, _, cx| {
                        motion::set_preference(if on { ReduceMotion::On } else { ReduceMotion::System }, cx);
                        prefs::update(|p| p.reduce_motion = on);
                        this_motion.update(cx, |_, cx| cx.notify());
                    })
                    .into_any_element(),
                &theme,
            ),
        ];
        let mut list = card(cx).flex().flex_col();
        for (i, row) in rows.into_iter().enumerate() {
            if i > 0 {
                list = list.child(divider(cx));
            }
            list = list.child(row);
        }
        list.into_any_element()
    }
}

/// A labelled form field.
fn field(label: &'static str, hint: Option<SharedString>, input: impl IntoElement, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(label))
        .child(input)
        .when_some(hint, |el, h| {
            let bad = h.as_ref() == "Doesn't match.";
            el.child(div().text_size(px(text::CAPTION)).text_color(if bad { theme.bad } else { theme.muted }).child(h))
        })
}

/// A settings line: label + hint on the left, the control on the right.
fn setting_row(label: &'static str, hint: &'static str, control: AnyElement, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(16.0))
        .px(px(16.0))
        .py(px(14.0))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(hint)),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// A page section: title, optional hint, content.
pub fn section(title: &'static str, hint: Option<&'static str>, content: impl IntoElement, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child(title))
                .when_some(hint, |el, h| el.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(h))),
        )
        .child(content)
}

impl Render for AppSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let claude = self.cli_card(Cli::Claude, cx);
        let codex = self.cli_card(Cli::Codex, cx);
        let account = self.account(window, cx);
        let computer = self.computer(cx);
        div()
            .flex()
            .flex_col()
            .gap(px(36.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().text_size(px(text::DISPLAY)).font_weight(FontWeight::SEMIBOLD).child("Settings"))
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child("Accounts, this computer, CRM webhooks, and about.")),
            )
            .child(section(
                "AI accounts",
                Some("A terminal window opens and your browser finishes the sign-in. Familiar checks again when you come back."),
                div().flex().flex_wrap().gap(px(12.0)).child(claude).child(codex),
                &theme,
            ))
            .child(section("Your account", Some("You sign in with this from the web or your phone."), account, &theme))
            .child(section("This computer", None, computer, &theme))
            .child(section(
                "Send CRM updates to another app",
                Some("Zapier, HubSpot (through Zapier), Attio or your own app: every change you pick is sent there as it happens, signed so it can tell it came from Familiar."),
                self.webhooks.clone(),
                &theme,
            ))
            .child(section(
                "About",
                None,
                card(cx)
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .text_size(px(text::SMALL))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child("Familiar")
                            .child(
                                div()
                                    .font_family(theme.font_mono.clone())
                                    .text_color(theme.muted)
                                    .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                            ),
                    )
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child("How it all fits together is written up in docs/ARCHITECTURE.md in the Familiar folder."),
                    ),
                &theme,
            ))
    }
}
