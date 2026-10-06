//! Teammate templates in the New teammate flow: the picker (category chips over a grid of ready-made teammates, plus
//! "Blank teammate") and the "What it sets up" summary the form shows for a picked template. The form itself is
//! [`crate::bot_settings::BotSettings`]; creating goes through `POST /api/templates/{id}/create`.

use std::collections::HashMap;

use familiar_client::{ConnectorPreset, Template};
use familiar_ui::anim;
use familiar_ui::components::{HoverCard, Skeleton, card, chip};
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Avatar, Mascot, MascotState, resolve_avatar};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::data::{AppData, swr};

/// The catalog's categories, in the order the chips show them.
const CATEGORIES: [&str; 6] = ["Growth & marketing", "Sales", "Social", "Research", "Desk work", "Personal"];

/// The picker's answer: a template, or `None` for a blank teammate.
pub struct Picked(pub Option<Template>);

pub struct TemplatePicker {
    templates: Option<Vec<Template>>,
    /// Index into the chips: 0 is "All".
    category: usize,
}

impl EventEmitter<Picked> for TemplatePicker {}

impl TemplatePicker {
    pub fn new(data: Entity<AppData>, cx: &mut Context<Self>) -> Self {
        let mut this = Self { templates: None, category: 0 };
        let client = data.read(cx).client.clone();
        swr(&mut this, &client, "/api/templates".into(), cx, |this, list: Vec<Template>, _| this.templates = Some(list));
        this
    }

    fn chips(&self) -> Vec<&'static str> {
        let present = |c: &&str| self.templates.as_ref().is_none_or(|l| l.iter().any(|t| t.category == *c));
        std::iter::once("All").chain(CATEGORIES.iter().copied().filter(present)).collect()
    }

    fn chip_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let mut row = div().flex().flex_wrap().gap(px(6.0));
        for (i, label) in self.chips().into_iter().enumerate() {
            let selected = i == self.category;
            let this = cx.entity();
            row = row.child(
                div()
                    .id(SharedString::from(format!("tpl-cat-{i}")))
                    .px(px(12.0))
                    .py(px(5.0))
                    .rounded_full()
                    .border_1()
                    .border_color(if selected { theme.accent } else { theme.line })
                    .bg(if selected { theme.accent_soft } else { theme.surface })
                    .text_size(px(text::SMALL))
                    .font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                    .text_color(if selected { theme.accent } else { theme.ink })
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .on_click(move |_, _, cx| {
                        this.update(cx, |p, cx| {
                            p.category = i;
                            cx.notify()
                        })
                    })
                    .child(label),
            );
        }
        row.into_any_element()
    }

    /// One template tile: its mascot, name and one-line pitch. The text block has a fixed height so rows line up.
    fn tile(&self, i: usize, t: &Template, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let pick = t.clone();
        anim::stagger(
            SharedString::from(format!("tpl-in-{}", t.id)),
            i,
            div().child(
                HoverCard::new(SharedString::from(format!("tpl-{}", t.id)))
                    .padding(14.0)
                    .on_click(move |_, _, cx| this.update(cx, |_, cx| cx.emit(Picked(Some(pick.clone())))))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(12.0))
                            .child(mascot_tile(format!("tpl-m-{}", t.id), template_avatar(t), 48.0, &theme))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(62.0))
                                    .gap(px(3.0))
                                    .child(div().font_weight(FontWeight::MEDIUM).text_color(theme.ink).truncate().child(t.name.clone()))
                                    .child(
                                        div()
                                            .text_size(px(text::SMALL))
                                            .text_color(theme.muted)
                                            .line_clamp(2)
                                            .text_ellipsis()
                                            .child(t.summary.clone()),
                                    ),
                            ),
                    ),
            ),
        )
        .into_any_element()
    }

    fn blank_tile(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        anim::stagger(
            "tpl-in-blank",
            0,
            div().child(
                HoverCard::new("tpl-blank")
                    .padding(14.0)
                    .on_click(move |_, _, cx| this.update(cx, |_, cx| cx.emit(Picked(None))))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .size(px(48.0))
                                    .flex_none()
                                    .rounded(px(RADIUS_CONTROL))
                                    .border_2()
                                    .border_dashed()
                                    .border_color(theme.line)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon(icons::PLUS).size(px(20.0)).text_color(theme.muted)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(62.0))
                                    .gap(px(3.0))
                                    .child(div().font_weight(FontWeight::MEDIUM).child("Blank teammate"))
                                    .child(
                                        div()
                                            .text_size(px(text::SMALL))
                                            .text_color(theme.muted)
                                            .line_clamp(2)
                                            .text_ellipsis()
                                            .child("Start from scratch: name it, give it a look and write its job yourself."),
                                    ),
                            ),
                    ),
            ),
        )
        .into_any_element()
    }
}

impl Render for TemplatePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let chips = self.chip_row(cx);
        let mut grid = div().grid().grid_cols(2).gap(px(12.0));
        match self.templates.clone() {
            None => {
                for _ in 0..6 {
                    grid = grid.child(Skeleton::new(92.0).radius(RADIUS_CARD));
                }
            }
            Some(list) => {
                let want = self.chips().get(self.category).copied().unwrap_or("All");
                let shown: Vec<&Template> = list.iter().filter(|t| want == "All" || t.category == want).collect();
                let mut i = 0;
                if want == "All" {
                    grid = grid.child(self.blank_tile(cx));
                    i += 1;
                }
                for t in shown {
                    grid = grid.child(self.tile(i, t, cx));
                    i += 1;
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("New teammate"))
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(
                        "Hire a ready-made teammate and answer a few questions, or start from a blank one.",
                    )),
            )
            .child(chips)
            // Keyed by category so a filter change re-plays the reveal.
            .child(div().id(SharedString::from(format!("tpl-grid-{}", self.category))).child(grid))
    }
}

/// The template's look, resolved like a stored avatar (keyed by the template id).
pub fn template_avatar(t: &Template) -> Avatar {
    let stored = t.avatar.as_ref().and_then(|a| serde_json::to_value(a).ok());
    resolve_avatar(&t.id, stored.as_ref())
}

/// A mascot on a soft tile.
pub fn mascot_tile(key: String, avatar: Avatar, size: f32, theme: &Theme) -> gpui::Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(RADIUS_CONTROL))
        .bg(theme.sunken)
        .flex()
        .items_center()
        .justify_center()
        .child(Mascot::new(key, avatar, MascotState::Idle, size * 0.78))
}

/// What a picked template will set up: its schedules (created off), the sites to sign in to, connectors that help.
pub fn setup_summary(t: &Template, presets: &[ConnectorPreset], cx: &App) -> AnyElement {
    let theme = Theme::of(cx).clone();
    let section = |glyph: &'static str, title: &'static str, body: AnyElement| {
        div()
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(div().pt(px(1.0)).child(icon(glyph).size(px(16.0)).text_color(theme.muted)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(title))
                    .child(body),
            )
    };
    let caption = |s: String| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s);
    let mut col = card(cx).p(px(16.0)).flex().flex_col().gap(px(14.0));
    if !t.schedules.is_empty() {
        let mut rows = div().flex().flex_col().gap(px(3.0));
        for s in &t.schedules {
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::SMALL))
                    .child(div().text_color(theme.ink).child(s.label.clone()))
                    .child(div().text_color(theme.muted).child(describe_cron(&s.cron))),
            );
        }
        let body = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(rows)
            .child(div().flex().child(chip(Tone::Muted, "Off until you turn them on", cx)));
        col = col.child(section(icons::CALENDAR, "Schedules", body.into_any_element()));
    }
    if !t.logins.is_empty() {
        let sites = t.logins.iter().map(|l| l.site.as_str()).collect::<Vec<_>>();
        let body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().text_size(px(text::SMALL)).child(join_and(&sites)))
            .child(caption("You sign in yourself, in its own browser, from a checklist on its page.".into()));
        col = col.child(section(icons::MONITOR, "Sites to sign in to", body.into_any_element()));
    }
    if !t.connectors.is_empty() {
        let names: HashMap<&str, &str> = presets.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
        let list = t.connectors.iter().map(|c| *names.get(c.as_str()).unwrap_or(&c.as_str())).collect::<Vec<_>>();
        let body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().text_size(px(text::SMALL)).child(join_and(&list)))
            .child(caption("Optional. Ones you already set up in Settings are linked; add the others there.".into()));
        col = col.child(section(icons::WIDGET, "Connectors that help", body.into_any_element()));
    }
    col.into_any_element()
}

/// "a", "a and b", "a, b and c".
fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Fill `{{key}}` from the answers for a preview (unanswered ones read "…").
pub fn preview(s: &str, answers: &[(String, String)]) -> String {
    let mut out = s.to_owned();
    for (k, v) in answers {
        let v = v.trim();
        out = out.replace(&format!("{{{{{k}}}}}"), if v.is_empty() { "…" } else { v });
    }
    out
}

/// A 5-field cron in plain words for the common shapes ("Weekdays at 9:30", "Tue and Thu at 8:00"); anything else
/// stays as the expression.
pub fn describe_cron(cron: &str) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const PLURAL: [&str; 7] = ["Sundays", "Mondays", "Tuesdays", "Wednesdays", "Thursdays", "Fridays", "Saturdays"];
    let f: Vec<&str> = cron.split_whitespace().collect();
    let (Ok(min), Ok(hour)) = (f.first().unwrap_or(&"").parse::<u32>(), f.get(1).unwrap_or(&"").parse::<u32>()) else {
        return cron.to_owned();
    };
    if f.len() != 5 || f[2] != "*" || f[3] != "*" || min > 59 || hour > 23 {
        return cron.to_owned();
    }
    let at = format!("{hour}:{min:02}");
    let day = |s: &str| s.parse::<usize>().ok().map(|d| d % 7);
    let when = match f[4] {
        "*" => "Every day".to_owned(),
        "1-5" | "MON-FRI" | "mon-fri" => "Weekdays".to_owned(),
        "0,6" | "6,0" | "6-7" => "Weekends".to_owned(),
        d if day(d).is_some() => PLURAL[day(d).unwrap_or(0)].to_owned(),
        list => {
            let days: Option<Vec<&str>> = list.split(',').map(|d| day(d).map(|d| DAYS[d])).collect();
            match days {
                Some(d) if !d.is_empty() => join_and(&d),
                _ => return cron.to_owned(),
            }
        }
    };
    format!("{when} at {at}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_words() {
        assert_eq!(describe_cron("30 9 * * 1-5"), "Weekdays at 9:30");
        assert_eq!(describe_cron("0 9 * * 1"), "Mondays at 9:00");
        assert_eq!(describe_cron("0 8 * * 2,4"), "Tue and Thu at 8:00");
        assert_eq!(describe_cron("30 8 * * *"), "Every day at 8:30");
        assert_eq!(describe_cron("*/5 * * * *"), "*/5 * * * *");
        assert_eq!(describe_cron("0 9 1 * *"), "0 9 1 * *");
    }

    #[test]
    fn preview_fills_answers() {
        let a = vec![("product".to_owned(), "Familiar".to_owned()), ("voice".to_owned(), " ".to_owned())];
        assert_eq!(preview("Post about {{product}} in {{voice}}", &a), "Post about Familiar in …");
    }
}
