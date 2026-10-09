//! A teammate's Settings → "Webhooks that wake it" (the web's `pages/Triggers.tsx`): addresses that start a run when
//! something calls them (a deploy finished, a form was sent, an alert fired). Each has a name, what to do (the request's
//! body is added after it) and whether it may act or only research; on/off, a new address (rotate, confirmed), delete
//! (confirmed), and when it last woke the teammate.
//!
//! The address carries its own key: the API returns it only when it is made or rotated, so it is shown once (with
//! Copy, an example request, and what calling it does), held in a [`Zeroizing`] string, and wiped at "I've saved it"
//! or when the page leaves the screen. Prompts are shown with hidden characters written out.

use std::collections::HashSet;

use familiar_client::{NewTrigger, Trigger, TriggerPatch};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Skeleton, Switch, chip};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::approval::{reveal, strip_hidden};
use crate::data::{AppData, ago};
use crate::text_input;

/// The New trigger form.
struct Adding {
    name: Entity<InputState>,
    prompt: Entity<TextareaState>,
    research: bool,
    error: Option<String>,
    saving: bool,
}

/// What a row is asking to confirm.
#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    Rotate,
    Delete,
}

pub struct BotTriggers {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    list: Option<Vec<Trigger>>,
    error: Option<String>,
    adding: Option<Adding>,
    /// A new or rotated trigger's address (id, name, address): shown until "I've saved it" or the page is hidden.
    shown_once: Option<(Uuid, String, Zeroizing<String>)>,
    copied: bool,
    example: bool,
    confirm: Option<(Uuid, Confirm)>,
    busy: HashSet<Uuid>,
}

impl BotTriggers {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &crate::data::DataEvent, cx| {
            // Triggers send no notices of their own; a run they started (or a resync) may have moved "last woke it".
            let mine = |n: &familiar_client::Notice| n.bot.as_deref().is_none_or(|b| b == this.bot.to_string());
            match ev {
                crate::data::DataEvent::Changed(None) => this.reload(cx),
                crate::data::DataEvent::Changed(Some(n)) if n.t == "runs" && mine(n) => this.reload(cx),
                _ => {}
            }
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            bot,
            list: None,
            error: None,
            adding: None,
            shown_once: None,
            copied: false,
            example: false,
            confirm: None,
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

    /// The page left the screen: the address shown once is wiped.
    pub fn hidden(&mut self, cx: &mut Context<Self>) {
        if self.shown_once.take().is_some() {
            self.copied = false;
            self.example = false;
            cx.notify();
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.triggers(bot).await });
        cx.spawn(async move |this, cx| {
            let Ok(r) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(list) => {
                        p.list = Some(list);
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = text_input::new_line("Build failed", false, window, cx);
        cx.subscribe_in(&name, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.create(cx),
            InputEvent::Change => {
                if let Some(a) = this.adding.as_mut() {
                    a.error = None;
                }
                cx.notify()
            }
            _ => {}
        })
        .detach();
        let prompt = text_input::new_field(
            "A build failed. Read the details below, find the cause and tell me what to fix.",
            false,
            8,
            window,
            cx,
        );
        cx.subscribe(&prompt, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        name.update(cx, |s, cx| s.focus(window, cx));
        self.adding = Some(Adding { name, prompt, research: false, error: None, saving: false });
        cx.notify();
    }

    /// Open the New trigger form filled in (`submit`: and create it). For the bench.
    pub fn fill(&mut self, name: &str, prompt: &str, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.start_add(window, cx);
        if let Some(a) = self.adding.as_ref() {
            a.name.update(cx, |s, cx| s.set_value(name.to_owned(), window, cx));
            a.prompt.update(cx, |s, cx| s.set_value(prompt.to_owned(), window, cx));
        }
        if submit {
            self.create(cx);
        }
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(a) = self.adding.as_mut() else { return };
        if a.saving {
            return;
        }
        let name = a.name.read(cx).value().trim().to_owned();
        let prompt = a.prompt.read(cx).value().trim().to_owned();
        if name.is_empty() || prompt.is_empty() {
            a.error = Some("Give it a name and say what to do.".into());
            cx.notify();
            return;
        }
        a.saving = true;
        a.error = None;
        cx.notify();
        let body = NewTrigger { name: Some(name), prompt: Some(prompt), kind: Some(kind_of(a.research).into()) };
        let client = self.client(cx);
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.create_trigger(bot, &body).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(t) => {
                        p.adding = None;
                        p.show_once(&t);
                        p.toast(Tone::Ok, format!("Made “{}”", strip_hidden(&t.name, false)), None, cx);
                        p.reload(cx);
                    }
                    Err(e) => {
                        if let Some(a) = p.adding.as_mut() {
                            a.saving = false;
                            a.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keep a fresh address to show once (and the answer's copy goes with the row).
    fn show_once(&mut self, t: &Trigger) {
        self.copied = false;
        self.example = false;
        self.shown_once = t.url.clone().filter(|u| !u.is_empty()).map(|u| (t.id, strip_hidden(&t.name, false), Zeroizing::new(u)));
    }

    /// Run a call for one trigger: busy meanwhile, a toast on failure, the list reloaded after.
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

    fn toggle(&mut self, id: Uuid, on: bool, cx: &mut Context<Self>) {
        if self.busy.contains(&id) {
            return;
        }
        if let Some(t) = self.list.as_mut().and_then(|l| l.iter_mut().find(|t| t.id == id)) {
            t.enabled = on;
        }
        let client = self.client(cx);
        let patch = TriggerPatch { enabled: Some(on), ..Default::default() };
        self.act(id, async move { client.update_trigger(id, &patch).await }, "Couldn't change it", cx, |_, _, _| {});
    }

    fn rotate(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm = None;
        let client = self.client(cx);
        self.act(id, async move { client.rotate_trigger(id).await }, "Couldn't make a new address", cx, |p, r, cx| {
            if let Ok(t) = r {
                p.show_once(&t);
                p.toast(Tone::Ok, "New address made", Some("The old one no longer works.".into()), cx);
            }
        });
    }

    fn delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm = None;
        if self.shown_once.as_ref().is_some_and(|(t, _, _)| *t == id) {
            self.shown_once = None;
        }
        let client = self.client(cx);
        self.act(id, async move { client.delete_trigger(id).await }, "Couldn't delete it", cx, move |p, r, cx| {
            if r.is_ok() {
                p.list.iter_mut().for_each(|l| l.retain(|t| t.id != id));
                p.toast(Tone::Ok, "Trigger deleted", Some("Its address stopped working.".into()), cx);
            }
        });
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    fn once_view(&self, name: &str, url: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let (copy, done, ex) = (cx.entity(), cx.entity(), cx.entity());
        let url_copy = Zeroizing::new(url.to_owned());
        // The example carries the address too: built only while it is shown, and wiped with it.
        let example = Zeroizing::new(if self.example { example_request(url) } else { String::new() });
        let example_copy = example.clone();
        let local = local_only(url);
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
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Copy the address of “{name}” now: it's shown only once"))),
            )
            .child(div().text_size(px(text::SMALL)).text_color(theme.ink).child(
                "Anyone who has it can wake this teammate, so keep it private. Lost it, or it leaked? Make a new one (the old one stops working).",
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
                            .child(url.to_owned()),
                    )
                    .child(
                        Button::new("trig-copy", if self.copied { "Copied" } else { "Copy" })
                            .size(ButtonSize::Small)
                            .icon(if self.copied { icons::CHECK } else { icons::COPY })
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url_copy.to_string()));
                                copy.update(cx, |p, cx| {
                                    p.copied = true;
                                    cx.notify()
                                })
                            }),
                    ),
            )
            .when(local, |el| {
                el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                    "It works on this PC: a script, a scheduled task or a local tool can call it. Services on the internet can't reach it unless you set up a tunnel to this computer.",
                ))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(Button::new("trig-once-done", "I've saved it").primary().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        done.update(cx, |p, cx| {
                            p.shown_once = None;
                            p.example = false;
                            cx.notify()
                        })
                    }))
                    .child(
                        Button::new("trig-example", if self.example { "Hide the example" } else { "Show an example request" })
                            .ghost()
                            .size(ButtonSize::Small)
                            .on_click(move |_, _, cx| {
                                ex.update(cx, |p, cx| {
                                    p.example = !p.example;
                                    cx.notify()
                                })
                            }),
                    ),
            )
            .when(self.example, |el| {
                el.child(anim::appear(
                    "trig-example-in",
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .px(px(12.0))
                                        .py(px(10.0))
                                        .rounded(px(RADIUS_CONTROL))
                                        .bg(theme.surface)
                                        .border_1()
                                        .border_color(theme.line)
                                        .font_family(theme.font_mono.clone())
                                        .text_size(px(text::CAPTION))
                                        .child(example.to_string()),
                                )
                                .child(Button::icon_only("trig-example-copy", icons::COPY).size(ButtonSize::Small).tooltip("Copy the example").on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(example_copy.to_string()))
                                })),
                        )
                        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                            "The request's body (any kind, up to 64 KB) is added after the instructions. At most 30 calls a minute.",
                        )),
                ))
            })
            .into_any_element()
    }

    fn add_view(&self, a: &Adding, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let label = |s: &'static str| div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(s);
        let caption = |s: &'static str| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s);
        let this = cx.entity();
        let (go, cancel) = (cx.entity(), cx.entity());
        let ready = !a.name.read(cx).value().trim().is_empty() && !a.prompt.read(cx).value().trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(16.0))
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(theme.line)
            .bg(theme.surface)
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Name")).child(text_input::field("trig-name", &a.name, 38.0, window, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("What to do when it's called"))
                    .child(text_input::field("trig-prompt", &a.prompt, 80.0, window, cx))
                    .child(caption("Whatever the call sends is added after this.")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("What it may do"))
                    .child(div().flex().child(
                        Segmented::new("trig-kind", vec![("Act on it".into(), None), ("Research only".into(), None)], a.research as usize)
                            .segment_width(130.0)
                            .on_select(move |i, _, cx| {
                                this.update(cx, |p, cx| {
                                    if let Some(a) = p.adding.as_mut() {
                                        a.research = i == 1;
                                    }
                                    cx.notify()
                                })
                            }),
                    ))
                    .child(caption(if a.research {
                        "It can read and browse, and changes nothing."
                    } else {
                        "Like a message from you: it can act, and asks you first wherever your rules say so."
                    })),
            )
            .when_some(a.error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        Button::new("trig-create", if a.saving { "Making it…" } else { "Make the address" })
                            .primary()
                            .size(ButtonSize::Small)
                            .disabled(a.saving || !ready)
                            .on_click(move |_, _, cx| go.update(cx, |p, cx| p.create(cx))),
                    )
                    .child(Button::new("trig-cancel", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        cancel.update(cx, |p, cx| {
                            p.adding = None;
                            cx.notify()
                        })
                    })),
            )
            .into_any_element()
    }

    fn row(&self, t: &Trigger, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = t.id;
        let key = id.as_u128() as u64;
        let busy = self.busy.contains(&id);
        let name = strip_hidden(&t.name, false);
        let (prompt, hidden) = reveal(&t.prompt, true);
        let research = t.kind == "proactive";
        let (t_on, t_rot, t_del) = (cx.entity(), cx.entity(), cx.entity());
        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(8.0))
            .bg(theme.sunken)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(icons::BOLT).size(px(15.0)).text_color(if t.enabled { theme.accent } else { theme.muted }))
                    .child(div().flex_1().min_w_0().flex().items_center().gap(px(6.0)).child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).truncate().child(name.clone())).when(research, |el| el.child(chip(Tone::Accent, "Research only", cx))).when(!t.enabled, |el| el.child(chip(Tone::Muted, "Off", cx))))
                    .child(Switch::new(("trig-on", key), t.enabled).on_toggle(move |on, _, cx| t_on.update(cx, |p, cx| p.toggle(id, on, cx))))
                    .child(
                        Button::icon_only(("trig-rotate", key), icons::REFRESH)
                            .size(ButtonSize::Small)
                            .disabled(busy)
                            .tooltip("Make a new address (the old one stops working)")
                            .on_click(move |_, _, cx| {
                                t_rot.update(cx, |p, cx| {
                                    p.confirm = Some((id, Confirm::Rotate));
                                    cx.notify()
                                })
                            }),
                    )
                    .child(
                        Button::icon_only(("trig-del", key), icons::TRASH)
                            .size(ButtonSize::Small)
                            .disabled(busy)
                            .tooltip("Delete")
                            .on_click(move |_, _, cx| {
                                t_del.update(cx, |p, cx| {
                                    p.confirm = Some((id, Confirm::Delete));
                                    cx.notify()
                                })
                            }),
                    ),
            )
            .child(div().pl(px(23.0)).text_size(px(text::SMALL)).text_color(if hidden { theme.bad } else { theme.ink }).line_clamp(3).child(prompt))
            .child(div().pl(px(23.0)).text_size(px(text::CAPTION)).text_color(theme.muted).child(match t.last_fired_at {
                Some(at) => format!("Last woke it {}", ago(Some(at))),
                None => "Not called yet".to_owned(),
            }));
        if let Some((_, what)) = self.confirm.filter(|(c, _)| *c == id) {
            let (yes, no) = (cx.entity(), cx.entity());
            let (words, action) = match what {
                Confirm::Rotate => ("Make a new address? The old one stops working at once: update whatever calls it.".to_owned(), "New address"),
                Confirm::Delete => (format!("Delete “{name}”? Its address stops working."), "Delete"),
            };
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pl(px(23.0))
                    .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).text_color(theme.bad).child(words))
                    .child(Button::new(("trig-no", key), "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        no.update(cx, |p, cx| {
                            p.confirm = None;
                            cx.notify()
                        })
                    }))
                    .child(Button::new(("trig-yes", key), action).danger().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        yes.update(cx, |p, cx| match what {
                            Confirm::Rotate => p.rotate(id, cx),
                            Confirm::Delete => p.delete(id, cx),
                        })
                    })),
            );
        }
        row.into_any_element()
    }
}

impl Render for BotTriggers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let name = self.data.read(cx).bot(self.bot).map(|b| strip_hidden(&b.name, false)).unwrap_or_else(|| "this teammate".into());
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Webhooks that wake it"))
            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!(
                "Let other tools wake {name}: a deploy finished, a form was sent, an alert fired. Calling the address (a POST) starts a run with your instructions, followed by whatever the call sent."
            )));
        if let Some((_, tname, url)) = self.shown_once.as_ref() {
            let (tname, url) = (tname.clone(), url.clone());
            col = col.child(anim::appear("trig-once", div().child(self.once_view(&tname, &url, cx))));
        }
        match (&self.list, &self.error) {
            (None, Some(e)) => col = col.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e.clone())),
            (None, None) => col = col.child(Skeleton::new(56.0).radius(8.0)),
            (Some(list), _) => {
                if list.is_empty() && self.adding.is_none() {
                    col = col.child(
                        div()
                            .px(px(12.0))
                            .py(px(10.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_dashed()
                            .border_color(theme.line)
                            .text_size(px(text::SMALL))
                            .text_color(theme.muted)
                            .child("None yet. Make one to get a private address that wakes it."),
                    );
                }
                for t in list {
                    col = col.child(self.row(t, cx));
                }
            }
        }
        match self.adding.as_ref() {
            Some(a) => col = col.child(self.add_view(a, window, cx)),
            None => {
                let this = cx.entity();
                col = col.child(div().flex().pt(px(2.0)).child(
                    Button::new("trig-new", "New webhook")
                        .size(ButtonSize::Small)
                        .icon(icons::BOLT)
                        .on_click(move |_, window, cx| this.update(cx, |p, cx| p.start_add(window, cx))),
                ));
            }
        }
        col
    }
}

/// The API's kind for a trigger: `proactive` (research only) or `scheduled` (a normal run that may act).
pub fn kind_of(research: bool) -> &'static str {
    if research { "proactive" } else { "scheduled" }
}

/// The address only reaches this computer (Familiar's own API on localhost).
pub fn local_only(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| h == "localhost" || h.ends_with(".localhost") || h.starts_with("127.") || h == "[::1]" || h == "::1")
}

/// An example call, ready to paste into a terminal.
pub fn example_request(url: &str) -> String {
    format!("curl -X POST '{url}' \\\n  -H 'Content-Type: application/json' \\\n  -d '{{\"event\":\"deploy_finished\",\"status\":\"ok\"}}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_addresses() {
        assert_eq!(kind_of(true), "proactive");
        assert_eq!(kind_of(false), "scheduled");
        assert!(local_only("http://localhost:7766/hooks/abc"));
        assert!(local_only("http://127.0.0.1:7766/hooks/abc"));
        assert!(local_only("http://[::1]:7766/hooks/abc"));
        assert!(!local_only("https://familiar.example.com/hooks/abc"));
        assert!(!local_only("not a url"));
        let ex = example_request("http://localhost:1/hooks/k");
        assert!(ex.starts_with("curl -X POST 'http://localhost:1/hooks/k'"));
        assert!(ex.contains("deploy_finished"));
    }
}
