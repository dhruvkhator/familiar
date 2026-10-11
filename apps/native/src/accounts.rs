//! Integrations → Accounts: the owner's Gmail (and Google Calendar) for the teammates they pick, through Familiar's own
//! gmail_* / calendar_* tools. Two ways in:
//!
//! - **Connect Google**: the API starts the sign-in (PKCE, a loopback listener on 127.0.0.1); the app opens Google's
//!   page in the system browser (`cx.open_url`: the default browser on Windows and macOS, never a teammate's browser
//!   or a view inside Familiar) and polls until the browser comes back: "Connected as …", what was granted, who may use
//!   it, Disconnect (revoked at Google). A build without Familiar's OAuth client explains the one-time setup instead.
//! - **Gmail (App Password)**: the address and the 16-letter app password (steps and a link to make one; it needs
//!   2-Step Verification and Workspace admins can turn it off). The password is write-only: a masked field, read into
//!   a [`Zeroizing`] string only to send it (the request body is wiped once it went), never shown again, never logged;
//!   the form is dropped when it is saved and when the page leaves the screen.
//!
//! Live: an `accounts` notice (connected, linked, needs reconnecting) refreshes the card.

use std::collections::HashSet;
use std::time::Duration;

use familiar_client::{Account, AccountsInfo, NewAppPassword};
use familiar_ui::components::{Button, ButtonSize, Skeleton, card, chip};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::approval::strip_hidden;
use crate::bot_settings::checkbox;
use crate::data::{AppData, DataEvent};
use crate::text_input;

/// A "Connect Google" waiting for the browser.
struct Flow {
    id: Uuid,
    url: String,
}

/// The app-password form, while open.
struct AppForm {
    email: Entity<InputState>,
    password: Entity<InputState>,
}

pub struct AccountsCard {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    info: Option<AccountsInfo>,
    error: Option<String>,
    flow: Option<Flow>,
    flow_error: Option<String>,
    form: Option<AppForm>,
    form_error: Option<String>,
    saving: bool,
    /// Accounts with a request in flight.
    busy: HashSet<Uuid>,
    confirm_disconnect: Option<Uuid>,
    shown: bool,
    stale: bool,
}

impl AccountsCard {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.changed(cx),
            DataEvent::Changed(Some(n)) if n.t == "accounts" => this.changed(cx),
            // the teammates' names in "Who may use it"
            DataEvent::Updated(crate::data::Part::Overview) => cx.notify(),
            _ => {}
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            info: None,
            error: None,
            flow: None,
            flow_error: None,
            form: None,
            form_error: None,
            saving: false,
            busy: HashSet::new(),
            confirm_disconnect: None,
            shown: true,
            stale: false,
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

    /// The page shows or hides the card. Hidden: the app-password form (and what was typed) is dropped.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if !shown {
            self.form = None;
            self.form_error = None;
            self.confirm_disconnect = None;
        } else if std::mem::take(&mut self.stale) {
            self.reload(cx);
        }
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        if self.shown {
            self.reload(cx);
        } else {
            self.stale = true;
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.accounts().await });
        cx.spawn(async move |this, cx| {
            let Ok(r) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(info) => {
                        p.info = Some(info);
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Connect Google ---------------------------------------------------------------------------------------------

    fn connect_google(&mut self, cx: &mut Context<Self>) {
        if self.flow.is_some() {
            return;
        }
        self.flow_error = None;
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.start_google_connect().await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(start) => {
                        // The owner's own default browser, never a teammate's or a view inside Familiar.
                        cx.open_url(&start.auth_url);
                        p.flow = Some(Flow { id: start.flow_id, url: start.auth_url });
                        p.poll(start.flow_id, cx);
                    }
                    Err(e) => p.flow_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask every 1.5 s how the sign-in is going, until it ends (or the card goes away).
    fn poll(&mut self, flow: Uuid, cx: &mut Context<Self>) {
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let still = this.update(cx, |p, _| p.flow.as_ref().is_some_and(|f| f.id == flow)).unwrap_or(false);
                if !still {
                    return;
                }
                let c = client.clone();
                let Ok(task) = this.update(cx, |_, cx| Tokio::spawn(cx, async move { c.google_connect_status(flow).await })) else { return };
                let Ok(r) = task.await else { return };
                let done = this
                    .update(cx, |p, cx| {
                        let status = match &r {
                            Ok(s) => s.clone(),
                            Err(e) if e.status() == 404 => familiar_client::ConnectStatus { status: "failed".into(), error: Some("The sign-in ended (Familiar restarted?). Try again.".into()), account_id: None },
                            Err(_) => return false, // a blip: ask again
                        };
                        match status.status.as_str() {
                            "waiting" => false,
                            "connected" => {
                                p.flow = None;
                                p.reload(cx);
                                p.toast(Tone::Ok, "Google is connected", Some("Tick the teammates that may use it.".into()), cx);
                                cx.notify();
                                true
                            }
                            _ => {
                                p.flow = None;
                                p.flow_error = status.error.or_else(|| (status.status != "cancelled").then(|| "The sign-in didn't finish.".to_owned()));
                                cx.notify();
                                true
                            }
                        }
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
        })
        .detach();
    }

    fn cancel_flow(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.flow.take() else { return };
        let client = self.client(cx);
        Tokio::spawn(cx, async move { client.cancel_google_connect(flow.id).await }).detach();
        cx.notify();
    }

    // ---- app password -----------------------------------------------------------------------------------------------

    fn open_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = text_input::new_line("you@gmail.com".to_owned(), false, window, cx);
        let password = text_input::new_line("abcd efgh ijkl mnop".to_owned(), true, window, cx);
        for input in [&email, &password] {
            cx.subscribe_in(input, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::PressEnter { .. } => this.save_app_password(cx),
                InputEvent::Change => {
                    this.form_error = None;
                    cx.notify()
                }
                _ => {}
            })
            .detach();
        }
        email.update(cx, |s, cx| s.focus(window, cx));
        self.form = Some(AppForm { email, password });
        self.form_error = None;
        cx.notify();
    }

    fn save_app_password(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        if self.saving {
            return;
        }
        let email = form.email.read(cx).value().trim().to_owned();
        let password = Zeroizing::new(form.password.read(cx).value().to_string());
        if email.is_empty() || password.trim().is_empty() {
            self.form_error = Some("Both: your Gmail address and the app password.".into());
            cx.notify();
            return;
        }
        self.saving = true;
        self.form_error = None;
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move {
            let mut body = NewAppPassword { email: Some(email), app_password: Some(password.to_string()) };
            let r = client.connect_app_password(&body).await;
            body.app_password.zeroize();
            r
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.saving = false;
                match r {
                    Ok(a) => {
                        p.form = None;
                        p.toast(Tone::Ok, format!("Gmail connected as {}", strip_hidden(&a.email, false)), Some("Tick the teammates that may use it.".into()), cx);
                        p.reload(cx);
                    }
                    Err(e) => p.form_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- both -------------------------------------------------------------------------------------------------------

    fn disconnect(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_disconnect = None;
        if !self.busy.insert(id) {
            return;
        }
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.delete_account(id).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&id);
                match r {
                    Ok(d) if d.kind == "google" && !d.revoked => p.toast(
                        Tone::Warn,
                        "Disconnected here",
                        Some("Google didn't confirm the access was removed: check Third-party access in your Google account.".into()),
                        cx,
                    ),
                    Ok(d) if d.kind == "google" => p.toast(Tone::Ok, "Google disconnected", Some("Access removed at Google too.".into()), cx),
                    Ok(_) => p.toast(Tone::Ok, "Gmail disconnected", Some("Delete the app password in your Google account to cut it off there too.".into()), cx),
                    Err(e) => p.toast(Tone::Bad, "Couldn't disconnect", Some(e), cx),
                }
                p.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Tick or untick a teammate: shown at once, saved as the whole set; a failure puts it back.
    fn toggle_bot(&mut self, account: Uuid, bot: Uuid, cx: &mut Context<Self>) {
        if self.busy.contains(&account) {
            return;
        }
        let Some(a) = self.info.as_mut().and_then(|i| i.accounts.iter_mut().find(|a| a.id == account)) else { return };
        let before = a.bot_ids.clone();
        let next = crate::bot_connectors::toggled(&before, bot);
        a.bot_ids = next.clone();
        self.busy.insert(account);
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.set_account_bots(account, &next).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&account);
                if let Err(e) = r {
                    if let Some(a) = p.info.as_mut().and_then(|i| i.accounts.iter_mut().find(|a| a.id == account)) {
                        a.bot_ids = before;
                    }
                    p.toast(Tone::Bad, "Couldn't change that", Some(e), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    fn head(&self, glyph: &'static str, title: &str, sub: &str, status: Option<AnyElement>, cx: &App) -> gpui::Div {
        let theme = Theme::of(cx).clone();
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
                    .child(icon(glyph).size(px(18.0)).text_color(theme.accent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child(title.to_owned()))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(sub.to_owned())),
            )
            .children(status)
    }

    /// "Connected as …", what it may do, who may use it, Disconnect.
    fn connected(&self, a: &Account, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = a.id;
        let key = id.as_u128() as u64;
        let busy = self.busy.contains(&id);
        let mut col = div().flex().flex_col().gap(px(12.0));
        col = col.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .child(div().text_size(px(text::SMALL)).child(format!("Connected as {}", strip_hidden(&a.email, false))))
                .children(a.scope_words().into_iter().map(|w| chip(Tone::Muted, w, cx))),
        );
        if a.status == "reconnect" {
            let again = cx.entity();
            let oauth = a.is_oauth();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(theme.sunken)
                    .child(icon(icons::DANGER_TRIANGLE).size(px(16.0)).text_color(theme.warn))
                    .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).child(if oauth {
                        "Google stopped accepting this connection (access was removed or it expired). Connect again."
                    } else {
                        "Gmail stopped accepting this app password. Connect again with a new one."
                    }))
                    .child(Button::new(("acct-again", key), "Connect again").size(ButtonSize::Small).on_click(move |_, window, cx| {
                        again.update(cx, |p, cx| if oauth { p.connect_google(cx) } else { p.open_form(window, cx) })
                    })),
            );
        }
        // who may use it
        let bots: Vec<(Uuid, String)> = self.data.read(cx).bots().iter().map(|b| (b.id, strip_hidden(&b.name, false))).collect();
        let mut who = div().flex().flex_wrap().gap(px(8.0));
        for (bot, name) in &bots {
            let on = a.bot_ids.contains(bot);
            let (this, bot) = (cx.entity(), *bot);
            who = who.child(
                div()
                    .id(SharedString::from(format!("acct-{id}-{bot}")))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .bg(theme.sunken)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.toggle_bot(id, bot, cx)))
                    .child(checkbox(on, &theme).mt(px(0.0)))
                    .child(div().text_size(px(text::SMALL)).child(name.clone())),
            );
        }
        col = col.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Who may use it"))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                    "Ticked teammates read this mail with Familiar's Gmail tools. Every email or invitation they send still asks you first (an approved draft's one send doesn't).",
                ))
                .child(if bots.is_empty() {
                    div().text_size(px(text::SMALL)).text_color(theme.muted).child("No teammates yet.").into_any_element()
                } else {
                    who.into_any_element()
                }),
        );
        let actions = if self.confirm_disconnect == Some(id) {
            let (yes, no) = (cx.entity(), cx.entity());
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).text_color(theme.bad).child(if a.is_oauth() {
                    "Disconnect Google? Familiar removes its access at Google and forgets the token; teammates lose these tools."
                } else {
                    "Disconnect Gmail? Familiar forgets the app password; teammates lose these tools."
                }))
                .child(Button::new(("acct-disc-no", key), "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                    no.update(cx, |p, cx| {
                        p.confirm_disconnect = None;
                        cx.notify()
                    })
                }))
                .child(Button::new(("acct-disc-yes", key), "Disconnect").danger().size(ButtonSize::Small).on_click(move |_, _, cx| yes.update(cx, |p, cx| p.disconnect(id, cx))))
        } else {
            let disc = cx.entity();
            div().flex().items_center().child(div().flex_1()).child(
                Button::new(("acct-disconnect", key), if busy { "Working…" } else { "Disconnect" })
                    .ghost()
                    .size(ButtonSize::Small)
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        disc.update(cx, |p, cx| {
                            p.confirm_disconnect = Some(id);
                            cx.notify()
                        })
                    }),
            )
        };
        col.child(div().pt(px(4.0)).border_t_1().border_color(theme.line.opacity(0.6)).child(div().pt(px(10.0)).child(actions))).into_any_element()
    }

    fn google(&self, info: &AccountsInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let account = info.oauth().cloned();
        let status = account.as_ref().map(|a| {
            if a.status == "reconnect" { chip(Tone::Warn, "Needs you", cx) } else { chip(Tone::Ok, "Connected", cx) }.into_any_element()
        });
        let mut col = card(cx).p(px(18.0)).flex().flex_col().gap(px(14.0)).child(self.head(
            icons::CALENDAR,
            "Google: Gmail and Calendar",
            "Sign in with Google in your browser. The token stays encrypted on this computer.",
            status,
            cx,
        ));
        if let Some(a) = account {
            return col.child(self.connected(&a, cx)).into_any_element();
        }
        let caption = |s: String| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s);
        if !info.google.configured {
            let url = info.google.setup_url.clone();
            return col
                .child(div().text_size(px(text::SMALL)).child(
                    "This copy of Familiar has no Google sign-in set up yet: a one-time setup by whoever builds it (about ten minutes in Google Cloud). Until then, use Gmail with an app password below.",
                ))
                .when(crate::crm_model::safe_url(&url).is_some(), |el| {
                    el.child(div().flex().child(
                        Button::new("acct-setup", "Setup guide").size(ButtonSize::Small).icon(icons::BOOK).tooltip(url.clone()).on_click(move |_, _, cx| cx.open_url(&url)),
                    ))
                })
                .into_any_element();
        }
        if let Some(flow) = &self.flow {
            let cancel = cx.entity();
            let url = flow.url.clone();
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .p(px(14.0))
                    .rounded(px(RADIUS_CARD))
                    .bg(theme.sunken)
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Finish in your browser"))
                    .child(caption(
                        "Google's page opened in your browser. Pick your account and allow Familiar; if Google says it hasn't verified the app, choose Advanced, then Go to Familiar. This card updates by itself.".into(),
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(Button::new("acct-reopen", "Open the page again").size(ButtonSize::Small).icon(icons::LINK).on_click(move |_, _, cx| cx.open_url(&url)))
                            .child(Button::new("acct-cancel", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| cancel.update(cx, |p, cx| p.cancel_flow(cx)))),
                    ),
            );
            return col.into_any_element();
        }
        let go = cx.entity();
        col.child(caption(
            "Familiar asks Google for: read mail, send mail, save drafts, and calendar events. Teammates you tick can then search and read your mail; anything they send or invite asks you first.".into(),
        ))
        .when_some(self.flow_error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
        .child(div().flex().child(Button::new("acct-connect-google", "Connect Google").primary().icon(icons::LINK).on_click(move |_, _, cx| go.update(cx, |p, cx| p.connect_google(cx)))))
        .into_any_element()
    }

    fn app_password(&self, info: &AccountsInfo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let account = info.app_password_account().cloned();
        let status = account.as_ref().map(|a| {
            if a.status == "reconnect" { chip(Tone::Warn, "Needs you", cx) } else { chip(Tone::Ok, "Connected", cx) }.into_any_element()
        });
        let col = card(cx).p(px(18.0)).flex().flex_col().gap(px(14.0)).child(self.head(
            icons::LOCK,
            "Gmail (App Password)",
            "No Google Cloud project needed. Mail only: search, read, drafts and sends (no Calendar).",
            status,
            cx,
        ));
        if let (Some(a), None) = (&account, &self.form) {
            return col.child(self.connected(a, cx)).into_any_element();
        }
        let caption = |s: &str| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s.to_owned());
        let Some(form) = &self.form else {
            let open = cx.entity();
            return col
                .child(caption("Uses IMAP and SMTP over TLS with a 16-letter app password from your Google account."))
                .child(div().flex().child(
                    Button::new("acct-app-open", "Use an app password").size(ButtonSize::Small).icon(icons::LOCK).on_click(move |_, window, cx| open.update(cx, |p, cx| p.open_form(window, cx))),
                ))
                .into_any_element();
        };
        let url = info.app_password.url.clone();
        let typed = !form.email.read(cx).value().trim().is_empty() && !form.password.read(cx).value().trim().is_empty();
        let (go, cancel) = (cx.entity(), cx.entity());
        col.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(div().text_size(px(text::SMALL)).child("1. Turn on 2-Step Verification for your Google account, if it isn't."))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().text_size(px(text::SMALL)).child("2. Make an app password named Familiar and copy its 16 letters."))
                        .when(crate::crm_model::safe_url(&url).is_some(), |el| {
                            el.child(Button::new("acct-app-make", "Make one").size(ButtonSize::Small).icon(icons::LINK).tooltip(url.clone()).on_click(move |_, _, cx| cx.open_url(&url)))
                        }),
                )
                .child(div().text_size(px(text::SMALL)).child("3. Paste it here with your Gmail address."))
                .child(caption(
                    "No \"App passwords\" page? 2-Step Verification is off, the account uses only security keys, or a Google Workspace admin turned app passwords off (then use Connect Google).",
                )),
        )
        .child(
            div()
                .flex()
                .gap(px(10.0))
                .child(div().flex_1().child(text_input::field("acct-app-email", &form.email, 38.0, window, cx)))
                .child(div().flex_1().font_family(theme.font_mono.clone()).child(text_input::field("acct-app-pass", &form.password, 38.0, window, cx))),
        )
        .child(caption("Familiar checks it with Gmail, keeps it encrypted on this computer and never shows it again."))
        .when_some(self.form_error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .child(
                    Button::new("acct-app-save", if self.saving { "Checking with Gmail…" } else { "Connect" })
                        .primary()
                        .size(ButtonSize::Small)
                        .disabled(self.saving || !typed)
                        .on_click(move |_, _, cx| go.update(cx, |p, cx| p.save_app_password(cx))),
                )
                .child(Button::new("acct-app-cancel", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                    cancel.update(cx, |p, cx| {
                        p.form = None;
                        p.form_error = None;
                        cx.notify()
                    })
                })),
        )
        .into_any_element()
    }
}

impl Render for AccountsCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        match self.info.clone() {
            None => match self.error.clone() {
                Some(e) => div().text_size(px(text::SMALL)).text_color(theme.bad).child(e).into_any_element(),
                None => Skeleton::new(120.0).radius(RADIUS_CARD).into_any_element(),
            },
            Some(info) => div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(self.google(&info, cx))
                .child(self.app_password(&info, window, cx))
                .into_any_element(),
        }
    }
}
