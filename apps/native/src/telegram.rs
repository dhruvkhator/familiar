//! Integrations → Telegram (the web's Telegram wizard and status, `pages/Integrations.tsx`): talk to your teammates
//! and answer what they ask from your phone. Not connected: three steps (make a bot with @BotFather, paste its token,
//! pick who answers). Connected: whether the chat is paired (else the `/start <code>` to send, with Copy), who answers,
//! on/off, replace the token, and disconnect with a confirmation.
//!
//! The bot token is write-only: a masked field, read into a [`Zeroizing`] string only to send it (the request body is
//! wiped once it went), never shown again and never logged. The field, with what was typed, is dropped once it is
//! saved and when the page leaves the screen. Live: a `channels` notice (the chat pairing) refreshes the card.

use familiar_client::{Channel, ChannelPatch, NewChannel};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Skeleton, Switch, card, chip};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, FontWeight, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::approval::strip_hidden;
use crate::data::{AppData, DataEvent};
use crate::menu::{self, MenuItem};
use crate::text_input;

/// Where a new bot is made.
const BOTFATHER: &str = "https://t.me/BotFather";

pub struct TelegramCard {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    /// `None` until loaded; `Some(None)`: not connected.
    channel: Option<Option<Channel>>,
    error: Option<String>,
    /// The token field (the wizard's, or "Replace the token"), while it is open.
    token: Option<Entity<InputState>>,
    replacing: bool,
    /// The wizard's choice of who answers (`None`: the first teammate).
    pick: Option<Uuid>,
    /// The "Who answers" menu, with its highlighted row.
    menu: Option<usize>,
    menu_focus: FocusHandle,
    busy: bool,
    confirm_disconnect: bool,
    copied: bool,
    shown: bool,
    stale: bool,
}

impl TelegramCard {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.changed(cx),
            DataEvent::Changed(Some(n)) if n.t == "channels" => this.changed(cx),
            // The teammates' names in "Who answers".
            DataEvent::Updated(crate::data::Part::Overview) => cx.notify(),
            _ => {}
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            channel: None,
            error: None,
            token: None,
            replacing: false,
            pick: None,
            menu: None,
            menu_focus: cx.focus_handle(),
            busy: false,
            confirm_disconnect: false,
            copied: false,
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

    /// The page shows or hides the card. Hidden: the token field (and what was typed) is dropped.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if !shown {
            self.token = None;
            self.replacing = false;
            self.confirm_disconnect = false;
            self.menu = None;
            self.copied = false;
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
        let task = Tokio::spawn(cx, async move { client.channels().await });
        cx.spawn(async move |this, cx| {
            let Ok(r) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(list) => {
                        let tg = list.into_iter().find(|c| c.kind == "telegram");
                        let was_waiting = matches!(&p.channel, Some(Some(c)) if !c.bound);
                        if was_waiting && tg.as_ref().is_some_and(|c| c.bound) {
                            p.toast(Tone::Ok, "Telegram is connected", Some("Say hello to your teammate there.".into()), cx);
                        }
                        p.channel = Some(tg);
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The masked token field, made when first needed.
    fn token_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(t) = &self.token {
            return t.clone();
        }
        let input = text_input::new_line("123456789:AA…", true, window, cx);
        cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.save_token(cx),
            InputEvent::Change => {
                this.error = None;
                cx.notify()
            }
            _ => {}
        })
        .detach();
        self.token = Some(input.clone());
        input
    }

    fn teammates(&self, cx: &App) -> Vec<(Uuid, String)> {
        self.data.read(cx).bots().iter().map(|b| (b.id, strip_hidden(&b.name, false))).collect()
    }

    /// Who answers now: the channel's teammate, the wizard's pick, else the first teammate.
    fn answering(&self, cx: &App) -> Option<Uuid> {
        let list = self.teammates(cx);
        let want = match &self.channel {
            Some(Some(c)) => c.default_bot_id,
            _ => self.pick,
        };
        want.filter(|id| list.iter().any(|(b, _)| b == id)).or_else(|| list.first().map(|(b, _)| *b))
    }

    /// Connect (the wizard) or replace the token. The API checks it with Telegram first.
    fn save_token(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.token.clone() else { return };
        let token = Zeroizing::new(input.read(cx).value().trim().to_owned());
        if token.is_empty() || self.busy {
            return;
        }
        let existing = self.channel.clone().flatten();
        let bot = self.answering(cx);
        if existing.is_none() && bot.is_none() {
            self.error = Some("Make a teammate first: Telegram messages go to one.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move {
            match existing {
                Some(c) => {
                    let mut patch = ChannelPatch { token: Some(token.to_string()), ..Default::default() };
                    let r = client.update_channel(c.id, &patch).await;
                    patch.token.zeroize();
                    r
                }
                None => {
                    let mut body = NewChannel { kind: Some("telegram".into()), token: Some(token.to_string()), default_bot_id: bot };
                    let r = client.create_channel(&body).await;
                    body.token.zeroize();
                    r
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(c) => {
                        let replaced = p.replacing;
                        p.token = None;
                        p.replacing = false;
                        p.channel = Some(Some(c));
                        if replaced {
                            p.toast(Tone::Ok, "Token replaced", None, cx);
                        }
                        p.reload(cx);
                    }
                    Err(e) => p.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Change the channel (who answers, on/off), shown at once; a failure puts it back.
    fn patch(&mut self, patch: ChannelPatch, cx: &mut Context<Self>) {
        let Some(Some(c)) = self.channel.clone() else { return };
        if self.busy {
            return;
        }
        let before = c.clone();
        let mut shown = c.clone();
        if let Some(b) = patch.default_bot_id {
            shown.default_bot_id = Some(b);
        }
        if let Some(on) = patch.enabled {
            shown.enabled = on;
        }
        self.channel = Some(Some(shown));
        self.busy = true;
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.update_channel(c.id, &patch).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(c) => p.channel = Some(Some(c)),
                    Err(e) => {
                        p.channel = Some(Some(before));
                        p.toast(Tone::Bad, "Couldn't change that", Some(e), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        let Some(Some(c)) = self.channel.clone() else { return };
        self.confirm_disconnect = false;
        self.busy = true;
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.delete_channel(c.id).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(()) => {
                        p.channel = Some(None);
                        p.toast(Tone::Ok, "Telegram disconnected", None, cx);
                    }
                    Err(e) => p.toast(Tone::Bad, "Couldn't disconnect", Some(e), cx),
                }
                p.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_teammate(&mut self, i: usize, cx: &mut Context<Self>) {
        self.menu = None;
        let Some((id, _)) = self.teammates(cx).get(i).cloned() else { return };
        if matches!(&self.channel, Some(Some(_))) {
            self.patch(ChannelPatch { default_bot_id: Some(id), ..Default::default() }, cx);
        } else {
            self.pick = Some(id);
        }
        cx.notify();
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    /// "Who answers": a select of the teammates.
    fn who_answers(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = self.teammates(cx);
        let current = self.answering(cx);
        let label = current.and_then(|id| list.iter().find(|(b, _)| *b == id)).map(|(_, n)| n.clone()).unwrap_or_else(|| "No teammates yet".into());
        let items: Vec<MenuItem> = list.iter().map(|(id, n)| MenuItem::new(n.clone()).checked(Some(*id) == current)).collect();
        let this = cx.entity();
        let popover = self.menu.map(|cursor| {
            let (pick, cur, close) = (cx.entity(), cx.entity(), cx.entity());
            menu::popover(
                "tg-who",
                &self.menu_focus,
                items,
                cursor,
                240.0,
                false,
                move |i, _, cx| pick.update(cx, |p, cx| p.pick_teammate(i, cx)),
                move |i, _, cx| {
                    cur.update(cx, |p, cx| {
                        p.menu = Some(i);
                        cx.notify()
                    })
                },
                move |_, cx| {
                    close.update(cx, |p, cx| {
                        p.menu = None;
                        cx.notify()
                    })
                },
                cx,
            )
        });
        let empty = list.is_empty();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(menu::trigger("tg-who", label, false, cx).when(!empty, |el| {
                el.on_click(move |_, window, cx| {
                    this.update(cx, |p, cx| {
                        if menu::closed_just_now("tg-who") {
                            return;
                        }
                        if p.menu.is_some() {
                            p.menu = None;
                        } else {
                            let at = p.answering(cx).and_then(|id| p.teammates(cx).iter().position(|(b, _)| *b == id)).unwrap_or(0);
                            p.menu = Some(at);
                            p.menu_focus.focus(window, cx);
                        }
                        cx.notify()
                    })
                })
            }))
            .children(popover)
            .into_any_element()
    }

    fn head(&self, status: Option<AnyElement>, cx: &mut Context<Self>) -> gpui::Div {
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
                    .child(icon(icons::SEND).size(px(18.0)).text_color(theme.accent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Telegram"))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Message your teammates and answer what they ask, from your phone.")),
            )
            .children(status)
    }

    /// A numbered step of the wizard.
    fn step(n: usize, title: &str, body: impl IntoElement, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.sunken)
                    .border_1()
                    .border_color(theme.line)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(text::CAPTION))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted)
                    .child(n.to_string()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .pt(px(2.0))
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(title.to_owned()))
                    .child(body),
            )
    }

    fn wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let token = self.token_field(window, cx);
        let typed = !token.read(cx).value().trim().is_empty();
        let has_bots = !self.teammates(cx).is_empty();
        let who = self.who_answers(cx);
        let go = cx.entity();
        let caption = |s: &str| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s.to_owned());
        card(cx)
            .p(px(18.0))
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(self.head(None, cx))
            .child(Self::step(
                1,
                "Make a bot for yourself",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().text_size(px(text::SMALL)).child("In Telegram, open @BotFather, send /newbot and answer its two questions. It replies with a token."))
                    .child(div().flex().child(
                        Button::new("tg-botfather", "Open @BotFather").size(ButtonSize::Small).icon(icons::LINK).on_click(|_, _, cx| cx.open_url(BOTFATHER)),
                    )),
                &theme,
            ))
            .child(Self::step(
                2,
                "Paste its token",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().font_family(theme.font_mono.clone()).child(text_input::field("tg-token", &token, 38.0, window, cx)))
                    .child(caption("Familiar checks it with Telegram, keeps it encrypted on this computer and never shows it again.")),
                &theme,
            ))
            .child(Self::step(
                3,
                "Choose who answers",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().flex().child(who))
                    .child(caption("Plain messages go to this teammate. To reach another, start with /bot and its short name (/bots lists them): /bot milo draft a post.")),
                &theme,
            ))
            .when_some(self.error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
            .child(
                div().flex().child(
                    Button::new("tg-connect", if self.busy { "Checking the token…" } else { "Connect and get the pairing code" })
                        .primary()
                        .icon(icons::SEND)
                        .disabled(self.busy || !typed || !has_bots)
                        .on_click(move |_, _, cx| go.update(cx, |p, cx| p.save_token(cx))),
                ),
            )
            .into_any_element()
    }

    fn status(&mut self, c: &Channel, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let state = if c.bound { chip(Tone::Ok, "Connected", cx) } else { chip(Tone::Warn, "Waiting to pair", cx) };
        let this = cx.entity();
        let toggle = Switch::new("tg-on", c.enabled).on_toggle(move |on, _, cx| {
            this.update(cx, |p, cx| p.patch(ChannelPatch { enabled: Some(on), ..Default::default() }, cx))
        });
        let status = div().flex().items_center().gap(px(10.0)).child(state).when(!c.enabled, |el| el.child(chip(Tone::Muted, "Off", cx))).child(toggle);
        let mut col = card(cx).p(px(18.0)).flex().flex_col().gap(px(16.0)).child(self.head(Some(status.into_any_element()), cx));
        if !c.bound {
            let code = c.pair_code.clone().unwrap_or_default();
            let command = format!("/start {code}");
            let (copy, copied) = (cx.entity(), self.copied);
            let clip = command.clone();
            col = col.child(anim::appear(
                "tg-pair",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .p(px(16.0))
                    .rounded(px(RADIUS_CARD))
                    .bg(theme.sunken)
                    .child(div().text_size(px(text::SMALL)).child("Open your new bot in Telegram and send it this message:"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .px(px(14.0))
                                    .py(px(10.0))
                                    .rounded(px(RADIUS_CONTROL))
                                    .bg(theme.surface)
                                    .border_1()
                                    .border_color(theme.line)
                                    .font_family(theme.font_mono.clone())
                                    .text_size(px(text::TITLE))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(if code.is_empty() { "/start …".to_owned() } else { command }),
                            )
                            .child(
                                Button::new("tg-copy", if copied { "Copied" } else { "Copy" })
                                    .icon(if copied { icons::CHECK } else { icons::COPY })
                                    .disabled(code.is_empty())
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(clip.clone()));
                                        copy.update(cx, |p, cx| {
                                            p.copied = true;
                                            cx.notify()
                                        })
                                    }),
                            ),
                    )
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                        "This card updates by itself once it's paired. From then on only that chat can talk to your teammates.",
                    )),
            ));
        }
        let who = self.who_answers(cx);
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Who answers"))
                        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Plain messages go to this teammate; /bot <short name> … reaches another, /bots lists them.")),
                )
                .child(who),
        );
        if self.replacing {
            let token = self.token_field(window, cx);
            let typed = !token.read(cx).value().trim().is_empty();
            let (go, cancel) = (cx.entity(), cx.entity());
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("New token"))
                    .child(div().font_family(theme.font_mono.clone()).child(text_input::field("tg-new-token", &token, 38.0, window, cx)))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("The pairing stays. The old token stops working here once this one is saved."))
                    .when_some(self.error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(
                                Button::new("tg-token-save", if self.busy { "Checking…" } else { "Save token" })
                                    .primary()
                                    .size(ButtonSize::Small)
                                    .disabled(self.busy || !typed)
                                    .on_click(move |_, _, cx| go.update(cx, |p, cx| p.save_token(cx))),
                            )
                            .child(Button::new("tg-token-cancel", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                                cancel.update(cx, |p, cx| {
                                    p.replacing = false;
                                    p.token = None;
                                    p.error = None;
                                    cx.notify()
                                })
                            })),
                    ),
            );
        }
        let actions = if self.confirm_disconnect {
            let (yes, no) = (cx.entity(), cx.entity());
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).text_color(theme.bad).child(
                    "Disconnect Telegram? Your teammates stop answering there; you can connect again any time.",
                ))
                .child(Button::new("tg-disc-no", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                    no.update(cx, |p, cx| {
                        p.confirm_disconnect = false;
                        cx.notify()
                    })
                }))
                .child(Button::new("tg-disc-yes", "Disconnect").danger().size(ButtonSize::Small).on_click(move |_, _, cx| yes.update(cx, |p, cx| p.disconnect(cx))))
        } else {
            let (rep, disc) = (cx.entity(), cx.entity());
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .when(!self.replacing, |el| {
                    el.child(Button::new("tg-replace", "Replace the token").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        rep.update(cx, |p, cx| {
                            p.replacing = true;
                            p.error = None;
                            cx.notify()
                        })
                    }))
                })
                .child(div().flex_1())
                .child(Button::new("tg-disconnect", "Disconnect").ghost().size(ButtonSize::Small).disabled(self.busy).on_click(move |_, _, cx| {
                    disc.update(cx, |p, cx| {
                        p.confirm_disconnect = true;
                        cx.notify()
                    })
                }))
        };
        col.child(div().pt(px(4.0)).border_t_1().border_color(theme.line.opacity(0.6)).child(div().pt(px(12.0)).child(actions))).into_any_element()
    }
}

impl Render for TelegramCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        match self.channel.clone() {
            None => match self.error.clone() {
                Some(e) => div().text_size(px(text::SMALL)).text_color(theme.bad).child(e).into_any_element(),
                None => Skeleton::new(120.0).radius(RADIUS_CARD).into_any_element(),
            },
            Some(None) => self.wizard(window, cx),
            Some(Some(c)) => self.status(&c, window, cx),
        }
    }
}
