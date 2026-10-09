//! Settings → "Send CRM updates to another app": the CRM's outgoing webhooks (`/api/crm/webhooks`). A list (address,
//! events, on/off, the last delivery), Add (an address checked as you type against the same rules the API applies,
//! and the events to send), the signing secret shown once with a Copy button, Test (a `ping`, answered at once),
//! recent deliveries (status, attempts, last error), and delete with a confirmation. Live: a delivery's notice
//! (`crm_webhook_deliveries`) refreshes the list and the open deliveries.

use std::collections::{HashMap, HashSet};

use familiar_client::{CrmWebhook, CrmWebhookDelivery, CrmWebhookPatch, NewCrmWebhook};
use familiar_ui::components::{Button, ButtonSize, Skeleton, Switch, card, chip, divider};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use zeroize::Zeroizing;
use uuid::Uuid;

use crate::bot_settings::checkbox;
use crate::crm_model::{WEBHOOK_EVENTS, webhook_url_problem};
use crate::data::{AppData, DataEvent, ago, excerpt};
use crate::text_input;

/// The Add form.
struct Adding {
    url: Entity<InputState>,
    events: [bool; WEBHOOK_EVENTS.len()],
    error: Option<String>,
}

pub struct CrmWebhooks {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    hooks: Option<Vec<CrmWebhook>>,
    error: Option<String>,
    adding: Option<Adding>,
    /// The new webhook's signing secret: shown until "I've saved it" or Settings is left (wiped from memory then).
    secret: Option<(Uuid, Zeroizing<String>)>,
    copied: bool,
    /// The webhook whose deliveries are open.
    open: Option<Uuid>,
    deliveries: HashMap<Uuid, Vec<CrmWebhookDelivery>>,
    /// The last Test of each webhook.
    tested: HashMap<Uuid, Result<CrmWebhookDelivery, String>>,
    confirm_delete: Option<Uuid>,
    busy: HashSet<Uuid>,
    saving: bool,
    /// Settings is on screen: delivery notices refresh the list (else they mark it stale).
    shown: bool,
    stale: bool,
    /// A refresh waiting out [`NOTICE_GAP`].
    reload_wait: Option<Task<()>>,
}

/// Delivery notices refresh the list at most this often.
const NOTICE_GAP: std::time::Duration = std::time::Duration::from_secs(2);

impl CrmWebhooks {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.changed(cx),
            DataEvent::Changed(Some(n)) if n.t == "crm_webhook_deliveries" => this.changed(cx),
            _ => {}
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            hooks: None,
            error: None,
            adding: None,
            secret: None,
            copied: false,
            open: None,
            deliveries: HashMap::new(),
            tested: HashMap::new(),
            confirm_delete: None,
            busy: HashSet::new(),
            saving: false,
            shown: true,
            stale: false,
            reload_wait: None,
        };
        this.reload(cx);
        this
    }

    /// Settings shows or hides this section: hidden, notices only mark it stale and the shown-once secret is wiped.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        if !shown {
            self.secret = None;
            self.copied = false;
            self.reload_wait = None;
        } else if std::mem::take(&mut self.stale) {
            self.reload(cx);
        }
        cx.notify();
    }

    /// A delivery changed: refresh once the notices pause ([`NOTICE_GAP`]).
    fn changed(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            self.stale = true;
            return;
        }
        if self.reload_wait.is_none() {
            self.reload_wait = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTICE_GAP).await;
                let _ = this.update(cx, |p, cx| {
                    p.reload_wait = None;
                    p.reload(cx);
                });
            }));
        }
    }

    fn client(&self, cx: &gpui::App) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let open = self.open;
        let task = Tokio::spawn(cx, async move {
            let hooks = client.crm_webhooks().await;
            let deliveries = match open {
                Some(id) => Some((id, client.crm_webhook_deliveries(id, Some(20)).await)),
                None => None,
            };
            (hooks, deliveries)
        });
        cx.spawn(async move |this, cx| {
            let Ok((hooks, deliveries)) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match hooks {
                    Ok(h) => {
                        p.hooks = Some(h);
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                if let Some((id, Ok(d))) = deliveries {
                    p.deliveries.insert(id, d);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = text_input::new_line("https://hooks.zapier.com/hooks/catch/…", false, window, cx);
        url.update(cx, |s, cx| s.focus(window, cx));
        cx.subscribe_in(&url, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.add(cx),
            InputEvent::Change => {
                if let Some(a) = this.adding.as_mut() {
                    a.error = None;
                }
                cx.notify()
            }
            _ => {}
        })
        .detach();
        self.adding = Some(Adding { url, events: [true; WEBHOOK_EVENTS.len()], error: None });
        cx.notify();
    }

    /// Open the Add form with `url` filled in (`submit`: and add it). For the bench's screenshots.
    pub fn fill_add(&mut self, url: &str, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.start_add(window, cx);
        if let Some(a) = self.adding.as_ref() {
            a.url.update(cx, |s, cx| s.set_value(url.to_owned(), window, cx));
        }
        if submit {
            self.add(cx);
        }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let Some(a) = self.adding.as_mut() else { return };
        if self.saving {
            return;
        }
        let url = a.url.read(cx).value().trim().to_owned();
        if let Err(why) = webhook_url_problem(&url) {
            a.error = Some(why.to_owned());
            cx.notify();
            return;
        }
        let events: Vec<String> = WEBHOOK_EVENTS.iter().zip(a.events).filter(|(_, on)| *on).map(|((e, _), _)| (*e).to_owned()).collect();
        if events.is_empty() {
            a.error = Some("Pick at least one kind of update to send.".into());
            cx.notify();
            return;
        }
        self.saving = true;
        cx.notify();
        let client = self.client(cx);
        let body = NewCrmWebhook { url: Some(url), events: Some(events), enabled: Some(true) };
        let task = Tokio::spawn(cx, async move { client.create_crm_webhook(&body).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.saving = false;
                match r {
                    Ok(w) => {
                        p.adding = None;
                        p.copied = false;
                        if let Some(s) = w.secret.clone() {
                            p.secret = Some((w.id, Zeroizing::new(s)));
                        }
                        p.toast(Tone::Ok, "Webhook added", None, cx);
                        p.reload(cx);
                    }
                    Err(e) => {
                        if let Some(a) = p.adding.as_mut() {
                            a.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run a call for one webhook: busy meanwhile, a toast on failure, the list reloaded after.
    fn act<T: Send + 'static>(
        &mut self,
        id: Uuid,
        call: impl std::future::Future<Output = Result<T, familiar_client::ApiError>> + Send + 'static,
        failed: &'static str,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    ) {
        if !self.busy.insert(id) {
            return;
        }
        cx.notify();
        let task = Tokio::spawn(cx, call);
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&id);
                if let Err(e) = &r {
                    p.toast(Tone::Bad, failed, Some(e.clone()), cx);
                }
                done(p, r, cx);
                p.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle(&mut self, w: &CrmWebhook, on: bool, cx: &mut Context<Self>) {
        // A call for it is on its way: flipping the switch now would show a state never sent.
        if self.busy.contains(&w.id) {
            return;
        }
        let client = self.client(cx);
        let id = w.id;
        if let Some(h) = self.hooks.as_mut().and_then(|l| l.iter_mut().find(|x| x.id == id)) {
            h.enabled = on;
        }
        let patch = CrmWebhookPatch { enabled: Some(on), ..Default::default() };
        self.act(id, async move { client.update_crm_webhook(id, &patch).await }, "Couldn't change it", cx, |_, _, _| {});
    }

    fn test(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let client = self.client(cx);
        self.tested.remove(&id);
        self.act(id, async move { client.test_crm_webhook(id).await }, "Couldn't send the test", cx, move |p, r, _| {
            p.tested.insert(id, r);
        });
    }

    fn delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = None;
        let client = self.client(cx);
        self.act(id, async move { client.delete_crm_webhook(id).await }, "Couldn't delete it", cx, move |p, r, cx| {
            if r.is_ok() {
                p.hooks.iter_mut().for_each(|l| l.retain(|w| w.id != id));
                p.toast(Tone::Ok, "Webhook deleted", None, cx);
            }
        });
    }

    fn toggle_open(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.open = if self.open == Some(id) { None } else { Some(id) };
        if self.open.is_some() {
            self.reload(cx);
        }
        cx.notify();
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    fn status_of(d: &CrmWebhookDelivery) -> (Tone, String) {
        match d.status.as_str() {
            "delivered" => (Tone::Ok, format!("Delivered {}", ago(d.delivered_at.or(d.created_at)))),
            "failed" => (Tone::Bad, format!("Failed after {} attempt{}", d.attempts, if d.attempts == 1 { "" } else { "s" })),
            _ if d.attempts > 0 => (Tone::Warn, format!("Retrying · {} attempt{} so far", d.attempts, if d.attempts == 1 { "" } else { "s" })),
            _ => (Tone::Muted, "Waiting to send".to_owned()),
        }
    }

    fn secret_view(&self, secret: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let (copy, done) = (cx.entity(), cx.entity());
        let s = Zeroizing::new(secret.to_owned());
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(16.0))
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(theme.warn.opacity(0.5))
            .bg(theme.warn_soft)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(icons::DANGER_TRIANGLE).size(px(16.0)).text_color(theme.warn))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Copy the signing secret now: it won't be shown again")),
            )
            .child(div().text_size(px(text::SMALL)).text_color(theme.ink).child(
                "Paste it into the other app. It checks that each update really came from Familiar: the Familiar-Signature header is t=<time>,v1=<HMAC-SHA256 of \"<time>.<body>\" with this secret>.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(12.0))
                            .py(px(8.0))
                            .rounded(px(RADIUS_CONTROL))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.line)
                            .font_family(theme.font_mono.clone())
                            .text_size(px(text::CAPTION))
                            .truncate()
                            .child(secret.to_owned()),
                    )
                    .child(
                        Button::new("hook-copy", if self.copied { "Copied" } else { "Copy" })
                            .size(ButtonSize::Small)
                            .icon(if self.copied { icons::CHECK } else { icons::COPY })
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(s.to_string()));
                                copy.update(cx, |p, cx| {
                                    p.copied = true;
                                    cx.notify()
                                })
                            }),
                    ),
            )
            .child(
                div().flex().child(Button::new("hook-secret-done", "I've saved it").primary().size(ButtonSize::Small).on_click(move |_, _, cx| {
                    done.update(cx, |p, cx| {
                        p.secret = None;
                        cx.notify()
                    })
                })),
            )
            .into_any_element()
    }

    fn add_view(&self, a: &Adding, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let url = a.url.read(cx).value().trim().to_owned();
        // As you type: a hint once there is something to judge, else the rules.
        let (hint, bad) = match (&a.error, url.is_empty()) {
            (Some(e), _) => (e.clone(), true),
            (None, true) => ("https:// to a public address, or http://localhost on this PC. Addresses on your local network (192.168…, 10…) aren't allowed: the updates carry customer data.".to_owned(), false),
            (None, false) => match webhook_url_problem(&url) {
                Ok(()) => ("Looks good.".to_owned(), false),
                Err(e) => (e.to_owned(), true),
            },
        };
        let all = a.events.iter().all(|e| *e);
        let mut events = div().flex().flex_col().gap(px(2.0));
        let this_all = cx.entity();
        events = events.child(
            div()
                .id("hook-ev-all")
                .flex()
                .items_center()
                .gap(px(10.0))
                .py(px(4.0))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    this_all.update(cx, |p, cx| {
                        if let Some(a) = p.adding.as_mut() {
                            let on = !a.events.iter().all(|e| *e);
                            a.events = [on; WEBHOOK_EVENTS.len()];
                        }
                        cx.notify()
                    })
                })
                .child(checkbox(all, &theme).mt(px(0.0)))
                .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Every change")),
        );
        for (i, (ev, what)) in WEBHOOK_EVENTS.iter().enumerate() {
            let this = cx.entity();
            events = events.child(
                div()
                    .id(("hook-ev", i))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .pl(px(28.0))
                    .py(px(3.0))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        this.update(cx, |p, cx| {
                            if let Some(a) = p.adding.as_mut() {
                                a.events[i] = !a.events[i];
                            }
                            cx.notify()
                        })
                    })
                    .child(checkbox(a.events[i], &theme).mt(px(0.0)))
                    .child(div().flex_1().text_size(px(text::SMALL)).child(*what))
                    .child(div().text_size(px(text::CAPTION)).font_family(theme.font_mono.clone()).text_color(theme.muted).child(*ev)),
            );
        }
        let (go, cancel) = (cx.entity(), cx.entity());
        card(cx)
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Address"))
                    .child(text_input::field("hook-url", &a.url, 38.0, window, cx))
                    .child(div().text_size(px(text::CAPTION)).text_color(if bad { theme.bad } else { theme.muted }).child(hint)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("What to send"))
                    .child(events),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        Button::new("hook-add-go", if self.saving { "Adding…" } else { "Add webhook" })
                            .primary()
                            .size(ButtonSize::Small)
                            .disabled(self.saving || url.is_empty())
                            .on_click(move |_, _, cx| go.update(cx, |p, cx| p.add(cx))),
                    )
                    .child(Button::new("hook-add-cancel", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        cancel.update(cx, |p, cx| {
                            p.adding = None;
                            cx.notify()
                        })
                    })),
            )
            .into_any_element()
    }

    fn hook_row(&self, w: &CrmWebhook, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = w.id;
        let key = id.as_u128() as u64;
        let busy = self.busy.contains(&id);
        let events = if w.events.len() == WEBHOOK_EVENTS.len() {
            "Every change".to_owned()
        } else {
            format!("{} kind{} of update: {}", w.events.len(), if w.events.len() == 1 { "" } else { "s" }, excerpt(&w.events.join(", "), 80))
        };
        let last = match &w.last_delivery {
            Some(d) => {
                let (tone, words) = Self::status_of(d);
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(chip(tone, words, cx))
                    .when_some(d.last_error.clone().filter(|e| !e.trim().is_empty() && d.status != "delivered"), |el, e| {
                        el.child(div().min_w_0().truncate().text_size(px(text::CAPTION)).text_color(theme.bad).child(excerpt(&e, 80)))
                    })
            }
            None => div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Nothing sent yet"),
        };
        let (w1, t, o, del) = (w.clone(), cx.entity(), cx.entity(), cx.entity());
        let this = cx.entity();
        let open = self.open == Some(id);
        let mut row = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(16.0))
                .py(px(12.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.0))
                        .child(div().truncate().font_family(theme.font_mono.clone()).text_size(px(text::SMALL)).text_color(if w.enabled { theme.ink } else { theme.muted }).child(w.url.clone()))
                        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).truncate().child(events))
                        .child(last),
                )
                .child(Switch::new(("hook-on", key), w.enabled).on_toggle(move |on, _, cx| this.update(cx, |p, cx| p.toggle(&w1, on, cx))))
                .child(
                    Button::new(("hook-test", key), if busy { "Testing…" } else { "Test" })
                        .size(ButtonSize::Small)
                        .disabled(busy || !w.enabled)
                        .tooltip(if w.enabled { "Send a ping now and see what the other app answers" } else { "Turn it on to test it" })
                        .on_click(move |_, _, cx| t.update(cx, |p, cx| p.test(id, cx))),
                )
                .child(
                    Button::icon_only(("hook-log", key), icons::LIST)
                        .size(ButtonSize::Small)
                        .when(open, |b| b.secondary())
                        .tooltip("Recent deliveries")
                        .on_click(move |_, _, cx| o.update(cx, |p, cx| p.toggle_open(id, cx))),
                )
                .child(
                    Button::icon_only(("hook-del", key), icons::TRASH)
                        .size(ButtonSize::Small)
                        .tooltip("Delete")
                        .on_click(move |_, _, cx| {
                            del.update(cx, |p, cx| {
                                p.confirm_delete = Some(id);
                                cx.notify()
                            })
                        }),
                ),
        );
        if let Some(r) = self.tested.get(&id) {
            let (tone, words) = match r {
                Ok(d) if d.status == "delivered" => (Tone::Ok, "Test delivered: the other app answered OK.".to_owned()),
                Ok(d) => (Tone::Bad, format!("Test failed: {}", d.last_error.clone().unwrap_or_else(|| "no answer".into()))),
                Err(e) => (Tone::Bad, format!("Test failed: {e}")),
            };
            let (fg, bg) = theme.tone(tone);
            row = row.child(
                div()
                    .mx(px(16.0))
                    .mb(px(12.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(bg)
                    .text_color(fg)
                    .text_size(px(text::SMALL))
                    .child(words),
            );
        }
        if self.confirm_delete == Some(id) {
            let (yes, no) = (cx.entity(), cx.entity());
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .pb(px(12.0))
                    .child(div().flex_1().text_size(px(text::SMALL)).text_color(theme.bad).child("Delete this webhook? Updates stop going to it, and its waiting deliveries are dropped."))
                    .child(Button::new(("hook-del-no", key), "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        no.update(cx, |p, cx| {
                            p.confirm_delete = None;
                            cx.notify()
                        })
                    }))
                    .child(Button::new(("hook-del-yes", key), "Delete").danger().size(ButtonSize::Small).on_click(move |_, _, cx| yes.update(cx, |p, cx| p.delete(id, cx)))),
            );
        }
        if open {
            let mut log = div().flex().flex_col().mx(px(16.0)).mb(px(12.0)).rounded(px(RADIUS_CONTROL)).border_1().border_color(theme.line).overflow_hidden();
            match self.deliveries.get(&id) {
                None => log = log.child(div().p(px(10.0)).child(Skeleton::new(28.0))),
                Some(list) if list.is_empty() => {
                    log = log.child(div().px(px(12.0)).py(px(10.0)).text_size(px(text::SMALL)).text_color(theme.muted).child("No deliveries yet."))
                }
                Some(list) => {
                    for (i, d) in list.iter().enumerate() {
                        let (tone, words) = Self::status_of(d);
                        log = log.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .px(px(12.0))
                                .py(px(7.0))
                                .when(i > 0, |el| el.border_t_1().border_color(theme.line.opacity(0.6)))
                                .text_size(px(text::CAPTION))
                                .child(div().w(px(150.0)).flex_none().font_family(theme.font_mono.clone()).truncate().child(d.event.clone()))
                                .child(chip(tone, words, cx))
                                .child(div().flex_1().min_w_0().truncate().text_color(theme.bad).child(d.last_error.clone().filter(|_| d.status != "delivered").unwrap_or_default()))
                                .child(div().flex_none().text_color(theme.muted).child(ago(d.created_at))),
                        );
                    }
                }
            }
            row = row.child(log);
        }
        row.into_any_element()
    }
}

impl Render for CrmWebhooks {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut col = div().flex().flex_col().gap(px(12.0));
        if let Some((_, s)) = self.secret.clone() {
            col = col.child(self.secret_view(&s, cx));
        }
        match (&self.hooks, &self.error) {
            (None, Some(e)) => col = col.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e.clone())),
            (None, None) => col = col.child(Skeleton::new(64.0).radius(RADIUS_CARD)),
            (Some(list), _) if list.is_empty() && self.adding.is_none() => {
                col = col.child(
                    div()
                        .px(px(16.0))
                        .py(px(16.0))
                        .rounded(px(RADIUS_CARD))
                        .border_1()
                        .border_dashed()
                        .border_color(theme.line)
                        .text_size(px(text::SMALL))
                        .text_color(theme.muted)
                        .child("No webhooks yet. Add one to keep another CRM (HubSpot through Zapier, Attio, your own) in step with this one."),
                )
            }
            (Some(list), _) if !list.is_empty() => {
                let mut c = card(cx).flex().flex_col();
                for (i, w) in list.iter().enumerate() {
                    if i > 0 {
                        c = c.child(divider(cx));
                    }
                    c = c.child(self.hook_row(w, cx));
                }
                col = col.child(c);
            }
            _ => {}
        }
        match self.adding.as_ref() {
            Some(a) => col = col.child(self.add_view(a, window, cx)),
            None => {
                let this = cx.entity();
                let full = self.hooks.as_ref().is_some_and(|l| l.len() >= 20);
                col = col.child(
                    div().flex().child(
                        Button::new("hook-add", "Add a webhook")
                            .size(ButtonSize::Small)
                            .icon(icons::PLUS)
                            .disabled(full)
                            .tooltip(if full { "You have the most there can be (20)" } else { "Send CRM changes to an address" })
                            .on_click(move |_, window, cx| this.update(cx, |p, cx| p.start_add(window, cx))),
                    ),
                );
            }
        }
        col
    }
}
