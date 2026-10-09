//! A teammate's Settings → "Connectors" (the web's `pages/BotConnectors.tsx`): which installed connectors it may use,
//! one checkbox each (saved at once, the whole set: `PUT /api/bots/{id}/connectors`), with a way to Integrations to
//! install more. Live: a `connectors` notice refreshes the list.

use familiar_client::Connector;
use familiar_ui::components::{Button, ButtonSize, Skeleton, chip};
use familiar_ui::icons;
use familiar_ui::theme::{RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::approval::strip_hidden;
use crate::bot_settings::checkbox;
use crate::data::{AppData, DataEvent};

pub struct BotConnectors {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    /// Every installed connector (`None` until loaded).
    all: Option<Vec<Connector>>,
    /// The ones this teammate may use (`None` until loaded).
    linked: Option<Vec<Uuid>>,
    error: Option<String>,
    busy: bool,
}

impl BotConnectors {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.reload(cx),
            DataEvent::Changed(Some(n)) if n.t == "connectors" => this.reload(cx),
            _ => {}
        })
        .detach();
        let mut this = Self { data, toasts, bot, all: None, linked: None, error: None, busy: false };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { (client.connectors().await, client.bot_connectors(bot).await) });
        cx.spawn(async move |this, cx| {
            let Ok((all, linked)) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match (all, linked) {
                    (Ok(all), Ok(linked)) => {
                        p.all = Some(all);
                        // A save on its way decides what's ticked.
                        if !p.busy {
                            p.linked = Some(linked.into_iter().map(|c| c.id).collect());
                        }
                        p.error = None;
                    }
                    (Err(e), _) | (_, Err(e)) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Tick or untick one: shown at once, saved as the whole set; a failure puts it back.
    fn toggle(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(before) = self.linked.clone() else { return };
        let next = toggled(&before, id);
        self.linked = Some(next.clone());
        self.busy = true;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.set_bot_connectors(bot, &next).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(list) => p.linked = Some(list.into_iter().map(|c| c.id).collect()),
                    Err(e) => {
                        p.linked = Some(before);
                        p.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't change that", Some(e.into()), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// `set` with `id` added, or taken out if it was there.
pub fn toggled(set: &[Uuid], id: Uuid) -> Vec<Uuid> {
    if set.contains(&id) { set.iter().copied().filter(|x| *x != id).collect() } else { set.iter().copied().chain([id]).collect() }
}

impl Render for BotConnectors {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let name = self.data.read(cx).bot(self.bot).map(|b| strip_hidden(&b.name, false)).unwrap_or_else(|| "this teammate".into());
        let caption = |s: String| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s);
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Connectors"))
            .child(caption(format!(
                "The tools {name} may use (GitHub, Notion, your own servers…). It still asks you before using them, until you allow them."
            )));
        match (&self.all, &self.linked, &self.error) {
            (_, _, Some(e)) if self.all.is_none() || self.linked.is_none() => {
                col = col.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e.clone()));
            }
            (Some(all), Some(linked), _) => {
                if all.is_empty() {
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
                            .child("No connectors installed yet. Add one in Integrations, then tick it here."),
                    );
                }
                for c in all {
                    let id = c.id;
                    let on = linked.contains(&id);
                    let cname = strip_hidden(&c.name, false);
                    let this = cx.entity();
                    col = col.child(
                        div()
                            .id(SharedString::from(format!("bc-{id}")))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(12.0))
                            .py(px(9.0))
                            .rounded(px(8.0))
                            .bg(theme.sunken)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.hover))
                            .on_click(move |_, _, cx| this.update(cx, |p, cx| p.toggle(id, cx)))
                            .child(checkbox(on, &theme).mt(px(0.0)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(cname.clone()))
                                            .child(chip(Tone::Muted, if c.transport == "http" { "Online" } else { "On this PC" }, cx))
                                            .when(!c.enabled, |el| el.child(chip(Tone::Warn, "Off for everyone", cx))),
                                    )
                                    .child(
                                        div()
                                            .font_family(theme.font_mono.clone())
                                            .text_size(px(text::CAPTION))
                                            .text_color(theme.muted)
                                            .truncate()
                                            .child(format!("mcp__{cname}__…")),
                                    ),
                            ),
                    );
                }
                col = col.child(
                    div().flex().pt(px(2.0)).child(
                        Button::new("bc-integrations", if all.is_empty() { "Open Integrations" } else { "Add a connector" })
                            .size(ButtonSize::Small)
                            .icon(icons::PLUG)
                            .on_click(|_, _, cx| crate::shell::open_page("integrations", cx)),
                    ),
                );
            }
            _ => {
                col = col.child(div().rounded(px(RADIUS_CONTROL)).child(Skeleton::new(44.0)));
            }
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_a_connector() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(toggled(&[a], b), [a, b]);
        assert_eq!(toggled(&[a, b], a), [b]);
        assert!(toggled(&[a], a).is_empty());
    }
}
