//! A teammate's Settings tab (the web's `pages/Settings.tsx`): name, look (the avatar builder), persona, engine and
//! model, paused, and delete.

use familiar_client::{BotEngine, BotPatch};
use familiar_ui::components::{Button, ButtonSize, Segmented, Switch, card};
use familiar_ui::icons;
use familiar_ui::mascot::{Accessory, Avatar, EYE_COUNT, MOUTH_COUNT, Mascot, MascotState, PALETTE, SHAPE_COUNT};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, hex, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, avatar_of};
use crate::text_input;

/// The web's `CLAUDE_MODELS`.
const CLAUDE_MODELS: [&str; 4] = ["sonnet", "opus", "haiku", "fable"];

pub struct BotSettings {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    slug: String,
    name: Entity<InputState>,
    persona: Entity<TextareaState>,
    /// Codex: any model the CLI accepts.
    codex_model: Entity<InputState>,
    codex: bool,
    claude_model: usize,
    paused: bool,
    avatar: Avatar,
    busy: bool,
    error: Option<String>,
    confirm_delete: bool,
}

impl BotSettings {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = data.read(cx).bot(bot).cloned().unwrap_or_default();
        let codex = b.engine == BotEngine::Codex;
        let name = text_input::new_line("Name", false, window, cx);
        name.update(cx, |s, cx| s.set_value(b.name.clone(), window, cx));
        let persona = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("Who is this teammate, and how should it work?").auto_grow(5, 14)
        });
        persona.update(cx, |s, cx| s.set_value(b.persona.clone().unwrap_or_default(), window, cx));
        let codex_model = text_input::new_line("gpt-5-codex", false, window, cx);
        if codex {
            codex_model.update(cx, |s, cx| s.set_value(b.model.clone(), window, cx));
        }
        cx.subscribe_in(&name, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.save(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        for sub in [&codex_model] {
            cx.subscribe_in(sub, window, |_: &mut Self, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify()
                }
            })
            .detach();
        }
        cx.subscribe_in(&persona, window, |_: &mut Self, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify()
            }
        })
        .detach();
        Self {
            avatar: avatar_of(&b),
            claude_model: CLAUDE_MODELS.iter().position(|m| *m == b.model).unwrap_or(0),
            data,
            toasts,
            bot,
            slug: b.slug,
            name,
            persona,
            codex_model,
            codex,
            paused: b.paused,
            busy: false,
            error: None,
            confirm_delete: false,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error = Some("Give your teammate a name.".into());
            self.name.update(cx, |s, cx| s.focus(window, cx));
            cx.notify();
            return;
        }
        let model = if self.codex {
            match self.codex_model.read(cx).value().trim() {
                "" => "gpt-5-codex".to_owned(),
                m => m.to_owned(),
            }
        } else {
            CLAUDE_MODELS[self.claude_model].to_owned()
        };
        let patch = BotPatch {
            name: Some(name),
            persona: Some(self.persona.read(cx).value().to_string()),
            model: Some(model),
            paused: Some(self.paused),
            engine: Some(if self.codex { "codex" } else { "claude" }.into()),
            avatar: Some(self.avatar.to_json()),
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.update_bot(bot, &patch).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(_) => {
                        this.toasts.update(cx, |t, cx| t.push(Tone::Ok, "Saved", None, cx));
                        this.data.update(cx, |d, cx| d.reload_overview(cx));
                    }
                    Err(e) => {
                        this.error = Some(e.clone());
                        this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't save", Some(e.into()), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let name = self.name.read(cx).value().trim().to_owned();
        let task = Tokio::spawn(cx, async move { client.delete_bot(bot).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.confirm_delete = false;
                match r {
                    // The shell leaves this page once the overview no longer lists the teammate.
                    Ok(()) => {
                        this.toasts.update(cx, |t, cx| t.push(Tone::Ok, format!("Deleted {name}"), None, cx));
                        this.data.update(cx, |d, cx| d.reload_overview(cx));
                    }
                    Err(e) => {
                        this.error = Some(e.clone());
                        this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't delete", Some(e.into()), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn randomize(&mut self, cx: &mut Context<Self>) {
        let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7)
            | 1;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n) as usize
        };
        self.avatar = Avatar {
            shape: next(SHAPE_COUNT as u64) as u8,
            color: PALETTE[next(PALETTE.len() as u64)],
            eyes: next(EYE_COUNT as u64) as u8,
            mouth: next(MOUTH_COUNT as u64) as u8,
            accessory: Accessory::ALL[next(Accessory::ALL.len() as u64)],
        };
        cx.notify();
    }

    fn set_avatar(&mut self, change: impl Fn(&mut Avatar), cx: &mut Context<Self>) {
        change(&mut self.avatar);
        cx.notify();
    }

    /// One option tile of the avatar builder: a small mascot wearing that option.
    fn tile(&self, id: String, avatar: Avatar, selected: bool, change: impl Fn(&mut Avatar) + 'static, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        div()
            .id(SharedString::from(id.clone()))
            .size(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(RADIUS_CONTROL))
            .border_2()
            .border_color(if selected { theme.accent } else { gpui::transparent_black() })
            .bg(if selected { theme.accent_soft } else { theme.sunken })
            .cursor_pointer()
            .hover(|s| s.bg(theme.hover))
            .on_click(move |_, _, cx| this.update(cx, |p, cx| p.set_avatar(&change, cx)))
            .child(Mascot::new(id, avatar, MascotState::Idle, 34.0))
            .into_any_element()
    }

    fn builder(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let a = self.avatar;
        let row = |label: &'static str, items: Vec<AnyElement>| {
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div().w(px(76.0)).flex_none().text_size(px(text::SMALL)).text_color(theme.muted).child(label),
                )
                .child(div().flex().flex_wrap().gap(px(6.0)).children(items))
        };
        let shapes = (0..SHAPE_COUNT)
            .map(|i| self.tile(format!("ab-shape-{i}"), Avatar { shape: i, ..a }, a.shape == i, move |v| v.shape = i, cx))
            .collect();
        let eyes = (0..EYE_COUNT)
            .map(|i| self.tile(format!("ab-eyes-{i}"), Avatar { eyes: i, ..a }, a.eyes == i, move |v| v.eyes = i, cx))
            .collect();
        let mouths = (0..MOUTH_COUNT)
            .map(|i| self.tile(format!("ab-mouth-{i}"), Avatar { mouth: i, ..a }, a.mouth == i, move |v| v.mouth = i, cx))
            .collect();
        let accessories = Accessory::ALL
            .iter()
            .map(|&acc| {
                self.tile(format!("ab-acc-{}", acc.as_str()), Avatar { accessory: acc, ..a }, a.accessory == acc, move |v| v.accessory = acc, cx)
            })
            .collect();
        let this = cx.entity();
        let colors = PALETTE
            .iter()
            .map(|&c| {
                let this = this.clone();
                let selected = a.color == c;
                div()
                    .id(SharedString::from(format!("ab-color-{c:06x}")))
                    .size(px(30.0))
                    .rounded_full()
                    .bg(hex(c))
                    .border_2()
                    .border_color(if selected { theme.ink } else { gpui::transparent_black() })
                    .cursor_pointer()
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.set_avatar(move |v| v.color = c, cx)))
                    .into_any_element()
            })
            .collect();
        let this_random = cx.entity();
        card(cx)
            .p(px(16.0))
            .flex()
            .gap(px(24.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.0))
                    .flex_none()
                    .w(px(150.0))
                    .child(
                        div()
                            .size(px(140.0))
                            .rounded(px(RADIUS_CARD))
                            .bg(theme.sunken)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Mascot::new(format!("ab-preview-{}", self.bot), a, MascotState::Idle, 104.0)),
                    )
                    .child(
                        Button::new("ab-random", "Randomize")
                            .size(ButtonSize::Small)
                            .icon(icons::MAGIC_STICK_3)
                            .on_click(move |_, _, cx| this_random.update(cx, |p, cx| p.randomize(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(10.0))
                    .child(row("Shape", shapes))
                    .child(row("Colour", colors))
                    .child(row("Eyes", eyes))
                    .child(row("Mouth", mouths))
                    .child(row("Accessory", accessories)),
            )
            .into_any_element()
    }
}

fn label(s: &'static str) -> gpui::Div {
    div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(s)
}

impl Render for BotSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let name = self.name.read(cx).value().trim().to_owned();
        let shown_name = if name.is_empty() { "this teammate".to_owned() } else { name.clone() };
        let this = cx.entity();
        let builder = self.builder(cx);
        let engine_this = cx.entity();
        let model_this = cx.entity();
        let paused_this = cx.entity();
        let save_this = cx.entity();
        let del_this = cx.entity();
        let codex = self.codex;
        let model_control: AnyElement = if codex {
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(text_input::field("bs-codex-model", &self.codex_model, 38.0, window, cx))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Any model your Codex CLI accepts."))
                .into_any_element()
        } else {
            Segmented::new(
                "bs-model",
                CLAUDE_MODELS.iter().map(|m| (SharedString::from(*m), None)).collect(),
                self.claude_model,
            )
            .segment_width(80.0)
            .on_select(move |i, _, cx| {
                model_this.update(cx, |p, cx| {
                    p.claude_model = i;
                    cx.notify()
                })
            })
            .into_any_element()
        };
        let danger = if self.confirm_delete {
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(format!("Delete {shown_name} for good?")))
                .child(
                    Button::new("bs-delete-yes", if self.busy { "Deleting…" } else { "Delete" })
                        .danger()
                        .disabled(self.busy)
                        .on_click(move |_, _, cx| del_this.update(cx, |p, cx| p.delete(cx))),
                )
                .child(Button::new("bs-delete-no", "Cancel").ghost().on_click(move |_, _, cx| {
                    this.update(cx, |p, cx| {
                        p.confirm_delete = false;
                        cx.notify()
                    })
                }))
        } else {
            div().flex().child(Button::new("bs-delete", format!("Delete {shown_name}")).danger().on_click(move |_, _, cx| {
                this.update(cx, |p, cx| {
                    p.confirm_delete = true;
                    cx.notify()
                })
            }))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Name")).child(text_input::field("bs-name", &self.name, 38.0, window, cx)))
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Look")).child(builder))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("Persona"))
                    .child(text_input::field("bs-persona", &self.persona, 110.0, window, cx))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Written at the top of its instructions.")),
            )
            .child(
                div()
                    .flex()
                    .gap(px(28.0))
                    .flex_wrap()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label("Engine"))
                            .child(
                                Segmented::new("bs-engine", vec![("Claude".into(), None), ("Codex".into(), None)], codex as usize)
                                    .segment_width(90.0)
                                    .on_select(move |i, window, cx| {
                                        engine_this.update(cx, |p, cx| {
                                            let to_codex = i == 1;
                                            if to_codex != p.codex {
                                                p.codex = to_codex;
                                                if to_codex && p.codex_model.read(cx).value().trim().is_empty() {
                                                    p.codex_model.update(cx, |s, cx| s.set_value("gpt-5-codex", window, cx));
                                                }
                                                if !to_codex {
                                                    p.claude_model = 0;
                                                }
                                            }
                                            cx.notify()
                                        })
                                    }),
                            )
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(if codex {
                                "Runs on your Codex CLI sign-in."
                            } else {
                                "Runs on your Claude Code sign-in."
                            })),
                    )
                    .child(div().flex().flex_col().gap(px(6.0)).min_w(px(240.0)).child(label("Model")).child(model_control)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(Switch::new("bs-paused", self.paused).on_toggle(move |on, _, cx| {
                        paused_this.update(cx, |p, cx| {
                            p.paused = on;
                            cx.notify()
                        })
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::MEDIUM).child("Paused"))
                            .child(
                                div()
                                    .text_size(px(text::CAPTION))
                                    .text_color(theme.muted)
                                    .child("Queued runs wait until you resume this teammate."),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(
                        Button::new("bs-save", if self.busy && !self.confirm_delete { "Saving…" } else { "Save changes" })
                            .primary()
                            .disabled(self.busy || name.is_empty())
                            .on_click(move |_, window, cx| save_this.update(cx, |p, cx| p.save(window, cx))),
                    )
                    .child(
                        div()
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .child(format!("Slug: {} (fixed, it names the workspace folder)", self.slug)),
                    ),
            )
            .child(
                div()
                    .mt(px(8.0))
                    .p(px(16.0))
                    .rounded(px(RADIUS_CARD))
                    .border_1()
                    .border_color(theme.bad.opacity(0.5))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().font_weight(FontWeight::MEDIUM).child("Delete this teammate"))
                            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(
                                "This removes its history from the database. The workspace folder on your PC is left alone.",
                            )),
                    )
                    .child(danger),
            )
    }
}
