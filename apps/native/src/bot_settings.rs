//! A teammate's Settings tab (the web's `pages/Settings.tsx`): name, look (the avatar builder), persona, engine and
//! model, paused, and delete. The same form, in create mode, is the New teammate flow (the web's `CreateBotDialog`):
//! name, a randomised look, what it should do, engine and a plan-checked model, and an optional hello.

use familiar_client::{Bot, BotEngine, BotPatch, NewBot};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Switch, card};
use familiar_ui::icons;
use familiar_ui::mascot::{Accessory, Avatar, EYE_COUNT, MOUTH_COUNT, Mascot, MascotState, PALETTE, SHAPE_COUNT};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, hex, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, avatar_of};
use crate::text_input;

/// The Claude aliases (always available, they run the newest version) and the version each runs today; shown until
/// `/api/models` answers.
const CLAUDE_ALIASES: [(&str, &str); 4] =
    [("sonnet", "Sonnet 5.5"), ("opus", "Opus 5.5"), ("haiku", "Haiku 4.5"), ("fable", "Fable 5.1")];

/// One model of the picker.
struct Choice {
    id: String,
    label: String,
    /// `None`: not verified against the plan yet.
    available: Option<bool>,
}

/// What the create form tells the shell.
pub enum CreateEvent {
    /// The teammate exists; `intro`: ask it to introduce itself.
    Created { bot: Bot, intro: bool },
    Cancelled,
}

/// The hello a new teammate gets when the owner asks for one.
pub const INTRO: &str = "Introduce yourself in two sentences.";

pub struct BotSettings {
    /// The New teammate form (nothing exists yet).
    create: bool,
    /// Create mode: send [`INTRO`] once it exists (off by default: it uses the plan).
    intro: bool,
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    slug: String,
    name: Entity<InputState>,
    persona: Entity<TextareaState>,
    /// Codex: any model the CLI accepts.
    codex_model: Entity<InputState>,
    codex: bool,
    claude_model: String,
    /// `/api/models`: what this computer's plan and Codex CLI offer (`None` until it answers).
    catalog: Option<serde_json::Value>,
    show_unavailable: bool,
    paused: bool,
    avatar: Avatar,
    busy: bool,
    error: Option<String>,
    confirm_delete: bool,
}

impl EventEmitter<CreateEvent> for BotSettings {}

impl BotSettings {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = data.read(cx).bot(bot).cloned().unwrap_or_default();
        Self::build(data, toasts, b, false, window, cx)
    }

    /// The New teammate form: a random look, Claude on Sonnet.
    pub fn create(data: Entity<AppData>, toasts: Entity<ToastStack>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = Bot { model: "sonnet".into(), engine: BotEngine::Claude, ..Default::default() };
        let mut this = Self::build(data, toasts, b, true, window, cx);
        this.randomize(cx);
        cx.defer_in(window, |this, window, cx| this.name.update(cx, |s, cx| s.focus(window, cx)));
        this
    }

    fn build(data: Entity<AppData>, toasts: Entity<ToastStack>, b: Bot, create: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bot = b.id;
        let codex = b.engine == BotEngine::Codex;
        let name = text_input::new_line("Name", false, window, cx);
        name.update(cx, |s, cx| s.set_value(b.name.clone(), window, cx));
        let persona = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(if create {
                    "What should it do? For example: You keep an eye on my inbox and draft short, friendly replies."
                } else {
                    "Who is this teammate, and how should it work?"
                })
                .auto_grow(5, 14)
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
        let mut this = Self {
            create,
            intro: false,
            avatar: avatar_of(&b),
            claude_model: if codex { "sonnet".into() } else { b.model.clone() },
            catalog: None,
            show_unavailable: false,
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
        };
        this.load_catalog(cx);
        // The plan check reports through the computer's minute heartbeat: look again while it is still running.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(20)).await;
                let Ok(()) = this.update(cx, |p, cx| {
                    if p.checking() {
                        p.load_catalog(cx);
                    }
                }) else {
                    break;
                };
            }
        })
        .detach();
        this
    }

    /// Claude Code reported signed out (no plan check will come until it signs in).
    fn signed_out(&self) -> bool {
        self.catalog.as_ref().is_some_and(|c| c["claude"]["signed_in"] == false)
    }

    /// Still waiting on the plan check (or for the first answer).
    fn checking(&self) -> bool {
        let Some(c) = &self.catalog else { return true };
        if self.signed_out() {
            return false;
        }
        let models = c["claude"]["models"].as_array();
        c["claude"]["plan"].is_null() || models.is_none_or(|m| m.is_empty() || m.iter().any(|m| m["available"].is_null()))
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
            self.claude_model.clone()
        };
        if self.create {
            return self.create_bot(name, model, cx);
        }
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

    /// Create mode: make the teammate (the server picks the slug; a taken one gets a short suffix).
    fn create_bot(&mut self, name: String, model: String, cx: &mut Context<Self>) {
        let avatar = serde_json::from_value(self.avatar.to_json()).ok();
        let new = NewBot {
            name: Some(name.clone()),
            slug: None,
            persona: Some(self.persona.read(cx).value().trim().to_string()),
            model: Some(model),
            engine: Some(if self.codex { "codex" } else { "claude" }.into()),
            avatar,
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let intro = self.intro;
        let task = Tokio::spawn(cx, async move {
            match client.create_bot(&new).await {
                Err(familiar_client::ApiError::Http { status: 409, .. }) => {
                    let base = kebab(&name);
                    let base = if base.is_empty() { "teammate".to_owned() } else { base.chars().take(34).collect() };
                    let suffix = &Uuid::new_v4().simple().to_string()[..4];
                    client.create_bot(&NewBot { slug: Some(format!("{base}-{suffix}")), ..new }).await
                }
                r => r,
            }
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(bot) => {
                        this.toasts.update(cx, |t, cx| t.push(Tone::Ok, format!("{} is ready", bot.name), None, cx));
                        cx.emit(CreateEvent::Created { bot, intro });
                    }
                    Err(e) => {
                        this.error = Some(e.clone());
                        this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't create it", Some(e.into()), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        crate::data::swr(self, &client, "/api/models".into(), cx, |this, v: serde_json::Value, _| this.catalog = Some(v));
    }

    /// The Claude choices: aliases first, then exact versions, with what the plan check said.
    fn claude_choices(&self) -> (Option<String>, Vec<Choice>) {
        let claude = self.catalog.as_ref().map(|c| &c["claude"]);
        let plan = claude.and_then(|c| c["plan"].as_str()).map(str::to_owned);
        let listed: Vec<Choice> = claude
            .and_then(|c| c["models"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let id = m["id"].as_str()?.to_owned();
                let label = m["label"].as_str().unwrap_or(&id).to_owned();
                let alias = m["alias"] == true;
                let label = if alias { format!("{label} · latest") } else { label };
                Some(Choice { id, label, available: if alias { Some(true) } else { m["available"].as_bool() } })
            })
            .collect();
        if listed.is_empty() {
            let aliases = CLAUDE_ALIASES
                .iter()
                .map(|(id, label)| Choice { id: (*id).into(), label: format!("{label} · latest"), available: Some(true) })
                .collect();
            return (plan, aliases);
        }
        (plan, listed)
    }

    /// A selectable model pill.
    fn pill(&self, id: String, label: String, selected: bool, enabled: bool, cx: &mut Context<Self>, pick: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        div()
            .id(SharedString::from(format!("bs-model-{id}")))
            .px(px(12.0))
            .py(px(6.0))
            .rounded(px(RADIUS_CONTROL))
            .border_1()
            .border_color(if selected { theme.accent } else { theme.line })
            .bg(if selected { theme.accent_soft } else { theme.surface })
            .text_size(px(text::SMALL))
            .font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL })
            .text_color(if !enabled { theme.muted } else if selected { theme.accent } else { theme.ink })
            .when(!enabled, |el| el.opacity(0.6))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| {
                        pick(p, window, cx);
                        cx.notify()
                    }))
            })
            .child(label)
            .into_any_element()
    }

    fn model_picker(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let caption = |s: String| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s);
        if self.codex {
            let listed: Vec<(String, String)> = self
                .catalog
                .as_ref()
                .and_then(|c| c["codex"]["models"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|m| Some((m["id"].as_str()?.to_owned(), m["label"].as_str().unwrap_or_default().to_owned())))
                .collect();
            let current = self.codex_model.read(cx).value().trim().to_owned();
            let mut pills = div().flex().flex_wrap().gap(px(6.0));
            for (id, label) in listed.iter() {
                let label = if label.is_empty() { id.clone() } else { label.clone() };
                let pick = id.clone();
                pills = pills.child(self.pill(id.clone(), label, *id == current, true, cx, move |p, window, cx| {
                    p.codex_model.update(cx, |s, cx| s.set_value(pick.clone(), window, cx));
                }));
            }
            return div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .when(!listed.is_empty(), |el| el.child(pills))
                .child(text_input::field("bs-codex-model", &self.codex_model, 38.0, window, cx))
                .child(caption(match (&self.catalog, listed.is_empty()) {
                    (None, _) => "Checking your Codex models…".into(),
                    (Some(_), true) => "Any model your Codex CLI accepts.".into(),
                    (Some(_), false) => "Pick one your Codex CLI lists, or type any model it accepts.".into(),
                }))
                .into_any_element();
        }
        let (plan, choices) = self.claude_choices();
        let current = self.claude_model.clone();
        let mut pills = div().flex().flex_wrap().gap(px(6.0));
        let mut shown_current = false;
        for c in choices.iter().filter(|c| c.available == Some(true)) {
            shown_current |= c.id == current;
            let pick = c.id.clone();
            pills = pills.child(self.pill(c.id.clone(), c.label.clone(), c.id == current, true, cx, move |p, _, _| p.claude_model = pick.clone()));
        }
        if !shown_current {
            // The saved model, even if the plan check hasn't vouched for it (yet).
            let label = choices.iter().find(|c| c.id == current).map(|c| c.label.clone()).unwrap_or_else(|| current.clone());
            pills = pills.child(self.pill(current.clone(), label, true, true, cx, |_, _, _| {}));
        }
        let checking = self.checking();
        let unavailable: Vec<&Choice> = choices.iter().filter(|c| c.available == Some(false)).collect();
        let this = cx.entity();
        let mut col = div().flex().flex_col().gap(px(8.0)).child(pills);
        let mut notes = div().flex().items_center().gap(px(12.0));
        if let Some(plan) = plan.as_deref().filter(|p| *p != "unknown") {
            let mut chars = plan.chars();
            let plan = chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default();
            notes = notes.child(caption(format!("Your plan: {plan}")));
        }
        if self.signed_out() {
            notes = notes.child(caption("Sign in to Claude to see your models".into()));
        } else if checking {
            notes = notes.child(caption("Checking your plan…".into()));
        }
        if !unavailable.is_empty() {
            let open = self.show_unavailable;
            notes = notes.child(
                div()
                    .id("bs-unavailable")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .text_size(px(text::CAPTION))
                    .text_color(theme.muted)
                    .hover(|s| s.text_color(theme.ink))
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| {
                        p.show_unavailable = !p.show_unavailable;
                        cx.notify()
                    }))
                    .child(SharedString::from(format!("Not on your plan ({})", unavailable.len())))
                    .child(
                        icons::icon(icons::ALT_ARROW_DOWN)
                            .size(px(12.0))
                            .text_color(theme.muted)
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(if open { std::f32::consts::PI } else { 0.0 }))),
                    ),
            );
        }
        col = col.child(notes);
        if self.show_unavailable && !unavailable.is_empty() {
            let mut off = div().flex().flex_wrap().gap(px(6.0));
            for c in unavailable {
                off = off.child(self.pill(c.id.clone(), c.label.clone(), false, false, cx, |_, _, _| {}));
            }
            col = col.child(anim::appear("bs-unavailable-list", off));
        }
        col.into_any_element()
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

/// The web's `kebab`: lowercase, runs of anything else become one dash, at most 40 chars.
fn kebab(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect()
}

/// A checkbox square (the row around it takes the click).
fn checkbox(on: bool, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .mt(px(2.0))
        .size(px(18.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(if on { theme.accent } else { theme.line })
        .bg(if on { theme.accent } else { theme.surface })
        .flex()
        .items_center()
        .justify_center()
        .when(on, |el| el.child(icons::icon(icons::CHECK).size(px(13.0)).text_color(theme.accent_ink)))
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
        let paused_this = cx.entity();
        let save_this = cx.entity();
        let del_this = cx.entity();
        let codex = self.codex;
        let model_control = self.model_picker(window, cx);
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
        let create = self.create;
        let intro_this = cx.entity();
        let cancel_this = cx.entity();
        let create_this = cx.entity();
        let intro = self.intro;
        div()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .when(create, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("New teammate"))
                        .child(
                            div()
                                .text_size(px(text::LEAD))
                                .text_color(theme.muted)
                                .child("Name it, give it a look and a job. You can change all of this later."),
                        ),
                )
            })
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Name")).child(text_input::field("bs-name", &self.name, 38.0, window, cx)))
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Look")).child(builder))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label(if create { "What it should do" } else { "Persona" }))
                    .child(text_input::field("bs-persona", &self.persona, 110.0, window, cx))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(if create {
                        "Who it is and how it should work. Becomes the start of its instructions."
                    } else {
                        "Written at the top of its instructions."
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .items_start()
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
                                                    // The first model the Codex CLI lists, else the long-standing default.
                                                    let first = p
                                                        .catalog
                                                        .as_ref()
                                                        .and_then(|c| c["codex"]["models"][0]["id"].as_str())
                                                        .unwrap_or("gpt-5-codex")
                                                        .to_owned();
                                                    p.codex_model.update(cx, |s, cx| s.set_value(first, window, cx));
                                                }
                                                if !to_codex && p.claude_model.trim().is_empty() {
                                                    p.claude_model = "sonnet".into();
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
                    .child(div().flex().flex_col().gap(px(6.0)).child(label("Model")).child(model_control)),
            )
            .when_some(self.error.clone().filter(|_| create), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
            .when(create, |el| {
                el.child(
                    div()
                        .id("bs-intro")
                        .flex()
                        .items_start()
                        .gap(px(12.0))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            intro_this.update(cx, |p, cx| {
                                p.intro = !p.intro;
                                cx.notify()
                            })
                        })
                        .child(checkbox(intro, &theme))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(FontWeight::MEDIUM).child("Say hello when it's ready"))
                                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!(
                                    "Opens its chat and asks: “{INTRO}” Uses a little of your plan."
                                ))),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            Button::new("bs-create", if self.busy { "Creating…" } else { "Create teammate" })
                                .primary()
                                .icon(icons::PLUS)
                                .disabled(self.busy || name.is_empty())
                                .on_click(move |_, window, cx| create_this.update(cx, |p, cx| p.save(window, cx))),
                        )
                        .child(Button::new("bs-cancel", "Cancel").ghost().on_click(move |_, _, cx| {
                            cancel_this.update(cx, |_, cx| cx.emit(CreateEvent::Cancelled))
                        })),
                )
            })
            .when(!create, |el| {
                el.child(
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
            })
    }
}
