//! A teammate's Learned tab → "Skills it wrote" (the web's `pages/Skills.tsx`): the know-how it saved for itself as
//! `SKILL.md` files in its workspace (the daemon mirrors them to `/api/bots/{id}/skills`). Each shows its name, when it
//! is used (its description) and when it last changed; opening one shows the file read-only, as plain text with hidden
//! characters written out (it is the teammate's own writing). Live: a `skills` notice refreshes the list.

use std::collections::HashSet;

use familiar_client::Skill;
use familiar_ui::anim::{self, Expand};
use familiar_ui::components::{Button, ButtonSize, SectionHeader, Skeleton, card, divider};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, text};
use gpui::{
    ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use uuid::Uuid;

use crate::approval::{reveal, strip_hidden};
use crate::data::{AppData, DataEvent, ago, swr};

/// A skill file longer than this shows its start and how much more there is.
const SHOWN_CHARS: usize = 60_000;

pub struct SkillsSection {
    data: Entity<AppData>,
    bot: Uuid,
    list: Option<Vec<Skill>>,
    /// The skills open, with their unfolding.
    open: std::collections::HashMap<Uuid, Expand>,
    copied: HashSet<Uuid>,
}

impl SkillsSection {
    pub fn new(data: Entity<AppData>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.reload(cx),
            DataEvent::Changed(Some(n)) if n.t == "skills" && n.bot.as_deref().is_none_or(|b| b == this.bot.to_string()) => {
                this.reload(cx)
            }
            _ => {}
        })
        .detach();
        let mut this = Self { data, bot, list: None, open: Default::default(), copied: HashSet::new() };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        swr(self, &client, format!("/api/bots/{}/skills", self.bot), cx, |this, list: Vec<Skill>, _| this.list = Some(list));
    }

    fn toggle(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.open.entry(id).or_insert_with(|| Expand::new(false)).toggle();
        cx.notify();
    }

    /// Open a skill (the bench's shot).
    pub fn open_first(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.list.as_ref().and_then(|l| l.first()).map(|s| s.id) {
            self.open.entry(id).or_insert_with(|| Expand::new(false)).set_open(true);
            cx.notify();
        }
    }

    fn row(&mut self, s: &Skill, i: usize, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        let id = s.id;
        let key = id.as_u128() as u64;
        let exp = self.open.entry(id).or_insert_with(|| Expand::new(false));
        let openness = exp.openness();
        let name = strip_hidden(&s.name, false);
        let when = s.description.as_deref().map(|d| strip_hidden(d, true)).filter(|d| !d.trim().is_empty());
        let (body, hidden) = reveal(&shown_body(&s.body), true);
        let this = cx.entity();
        let copy = cx.entity();
        let copied = self.copied.contains(&id);
        let raw = s.body.clone();
        let head = div()
            .id(("skill", key))
            .flex()
            .items_start()
            .gap(px(12.0))
            .px(px(16.0))
            .py(px(12.0))
            .cursor_pointer()
            .hover(|st| st.bg(theme.hover))
            .on_click(move |_, _, cx| this.update(cx, |p, cx| p.toggle(id, cx)))
            .child(
                div()
                    .size(px(32.0))
                    .flex_none()
                    .rounded(px(RADIUS_CONTROL))
                    .bg(theme.accent_soft)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(icons::BOOK).size(px(16.0)).text_color(theme.accent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().font_family(theme.font_mono.clone()).text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).truncate().child(name))
                            .child(div().flex_none().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("updated {}", ago(Some(s.updated_at))))),
                    )
                    .child(div().text_size(px(text::SMALL)).text_color(theme.muted).line_clamp(2).child(match when {
                        Some(d) => format!("Used when: {d}"),
                        None => "No description: it decides from the name when to use it.".to_owned(),
                    })),
            )
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(14.0))
                    .mt(px(4.0))
                    .text_color(theme.muted)
                    .with_transformation(gpui::Transformation::rotate(gpui::radians(openness * std::f32::consts::PI))),
            );
        let detail = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(16.0))
            .pb(px(14.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().text_size(px(text::CAPTION)).text_color(if hidden { theme.bad } else { theme.muted }).child(if hidden {
                        "SKILL.md, read only. It has hidden characters, written out as ⟨U+…⟩."
                    } else {
                        "SKILL.md, read only. To change it, ask the teammate in chat."
                    }))
                    .child(
                        Button::new(("skill-copy", key), if copied { "Copied" } else { "Copy" })
                            .ghost()
                            .size(ButtonSize::Small)
                            .icon(if copied { icons::CHECK } else { icons::COPY })
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(strip_hidden(&raw, true)));
                                copy.update(cx, |p, cx| {
                                    p.copied.insert(id);
                                    cx.notify()
                                })
                            }),
                    ),
            )
            .child(
                div()
                    .id(("skill-body", key))
                    .max_h(px(360.0))
                    .overflow_y_scroll()
                    .px(px(14.0))
                    .py(px(12.0))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(theme.sunken)
                    .font_family(theme.font_mono.clone())
                    .text_size(px(text::CAPTION))
                    .text_color(theme.ink)
                    .child(body),
            );
        let exp = self.open.get_mut(&id).expect("inserted above");
        let detail = exp.render(SharedString::from(format!("skill-detail-{id}")), window, cx, detail);
        anim::stagger(SharedString::from(format!("skill-in-{id}")), i, div().flex().flex_col().child(head).child(detail)).into_any_element()
    }
}

/// A skill's text as shown: the whole file, or its start and how much more there is.
pub fn shown_body(body: &str) -> String {
    let n = body.chars().count();
    if n <= SHOWN_CHARS {
        return body.to_owned();
    }
    let start: String = body.chars().take(SHOWN_CHARS).collect();
    format!("{start}\n\n… {} more characters (Copy has the whole file)", n - SHOWN_CHARS)
}

impl Render for SkillsSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let name = self.data.read(cx).bot(self.bot).map(|b| strip_hidden(&b.name, false)).unwrap_or_else(|| "It".into());
        let mut col = div().flex().flex_col().gap(px(10.0));
        let count = self.list.as_ref().map(|l| l.len()).unwrap_or(0);
        col = col
            .child(SectionHeader::new("Skills it wrote").count(count))
            .child(div().mt(px(-4.0)).text_size(px(text::SMALL)).text_color(theme.muted).child(format!(
                "Step-by-step know-how {name} saved for itself, used whenever a task matches. Ask it in chat to learn one: “Walk me through filing an expense, then save it as a skill.”"
            )));
        match self.list.clone() {
            None => col = col.child(Skeleton::new(56.0).radius(RADIUS_CARD)),
            Some(list) if list.is_empty() => {
                col = col.child(
                    div()
                        .px(px(16.0))
                        .py(px(14.0))
                        .rounded(px(RADIUS_CARD))
                        .border_1()
                        .border_dashed()
                        .border_color(theme.line)
                        .text_size(px(text::SMALL))
                        .text_color(theme.muted)
                        .child("None yet. Skills it saves to its workspace show up here after its next run."),
                )
            }
            Some(list) => {
                let mut c = card(cx).flex().flex_col().overflow_hidden();
                for (i, s) in list.iter().enumerate() {
                    if i > 0 {
                        c = c.child(divider(cx));
                    }
                    c = c.child(self.row(s, i, window, cx));
                }
                col = col.child(c);
            }
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_skills_are_cut_with_a_count() {
        assert_eq!(shown_body("short"), "short");
        let long = "x".repeat(SHOWN_CHARS + 5);
        let shown = shown_body(&long);
        assert!(shown.ends_with("… 5 more characters (Copy has the whole file)"));
        assert_eq!(shown.chars().filter(|c| *c == 'x').count(), SHOWN_CHARS);
    }
}
