//! A teammate's Settings tab (the web's `pages/Settings.tsx`): name, look (the avatar builder), persona, engine and
//! model, the connectors it may use ([`crate::bot_connectors`]), what it may do without asking, shared folders, the
//! desktop, paused, and delete. The same form, in create mode, is the New teammate flow (the web's `CreateBotDialog`):
//! name, a randomised look, what it should do, engine and a plan-checked model, and an optional hello. Hired from a
//! template ([`crate::templates`]), it starts with the template's questions, look, instructions and model, shows what
//! the template sets up, and offers its first task instead of the hello.

use familiar_client::{Bot, BotEngine, BotPatch, ConnectorPreset, FromTemplate, NewBot, Template};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Switch, card};
use familiar_ui::icons;
use familiar_ui::mascot::{Accessory, Avatar, EYE_COUNT, MOUTH_COUNT, Mascot, MascotState, PALETTE, SHAPE_COUNT};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, hex, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, avatar_of, excerpt};
use crate::templates;
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
    /// The teammate exists; `post`: a first message to send it (the hello, or the template's first task).
    Created { bot: Bot, post: Option<String> },
    Cancelled,
    /// Back to the template picker.
    Back,
}

/// A template question's field.
enum Answer {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Answer {
    fn value(&self, cx: &App) -> String {
        match self {
            Self::Line(s) => s.read(cx).value().to_string(),
            Self::Area(s) => s.read(cx).value().to_string(),
        }
    }
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
    /// Create mode: the template it is hired from, with its questions' fields (key, label, field).
    template: Option<Template>,
    answers: Vec<(String, String, Answer)>,
    /// Connector names for the template's suggestions.
    presets: Vec<ConnectorPreset>,
    /// Edit mode: this teammate's own allow rules ("Always allow" on an approval adds them), to see and remove.
    allowed: Option<Vec<familiar_client::Rule>>,
    /// Edit mode: the folders on this PC shared with it (`None` until loaded).
    folders: Option<Vec<familiar_client::Folder>>,
    /// Edit mode: which connectors it may use.
    connectors: Option<Entity<crate::bot_connectors::BotConnectors>>,
    /// Edit mode: the webhooks that wake it.
    triggers: Option<Entity<crate::triggers::BotTriggers>>,
    /// "Can use this PC's desktop" (saved at once when switched).
    desktop: bool,
}

impl EventEmitter<CreateEvent> for BotSettings {}

impl BotSettings {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = data.read(cx).bot(bot).cloned().unwrap_or_default();
        Self::build(data, toasts, b, false, window, cx)
    }

    /// The New teammate form: a random look, Claude on Sonnet; or, from a template, its name, look, instructions
    /// and model, with its questions on top.
    pub fn create(
        data: Entity<AppData>,
        toasts: Entity<ToastStack>,
        template: Option<Template>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Some(t) = template else {
            let b = Bot { model: "sonnet".into(), engine: BotEngine::Claude, ..Default::default() };
            let mut this = Self::build(data, toasts, b, true, window, cx);
            this.randomize(cx);
            cx.defer_in(window, |this, window, cx| this.name.update(cx, |s, cx| s.focus(window, cx)));
            return this;
        };
        let b = Bot {
            name: t.name.clone(),
            persona: Some(t.instructions.clone()),
            model: t.model.clone(),
            engine: BotEngine::Claude,
            ..Default::default()
        };
        let mut this = Self::build(data, toasts, b, true, window, cx);
        this.avatar = templates::template_avatar(&t);
        for q in &t.questions {
            // "e.g." so a sample answer never reads as one already given.
            let placeholder = format!("e.g. {}", q.placeholder);
            let field = if q.multiline {
                Answer::Area(cx.new(|cx| TextareaState::new(window, cx).placeholder(placeholder).auto_grow(2, 6)))
            } else {
                Answer::Line(text_input::new_line(placeholder, false, window, cx))
            };
            match &field {
                Answer::Line(s) => cx.subscribe_in(s, window, Self::on_answer).detach(),
                Answer::Area(s) => cx.subscribe_in(s, window, Self::on_answer).detach(),
            }
            this.answers.push((q.key.clone(), q.label.clone(), field));
        }
        let client = this.data.read(cx).client.clone();
        crate::data::swr(&mut this, &client, "/api/connectors/presets".into(), cx, |this, p: Vec<ConnectorPreset>, _| {
            this.presets = p
        });
        this.template = Some(t);
        cx.defer_in(window, |this, window, cx| match this.answers.first() {
            Some((_, _, Answer::Line(s))) => s.update(cx, |s, cx| s.focus(window, cx)),
            Some((_, _, Answer::Area(s))) => s.update(cx, |s, cx| s.focus(window, cx)),
            None => this.name.update(cx, |s, cx| s.focus(window, cx)),
        });
        this
    }

    fn on_answer<E>(&mut self, _: &Entity<E>, ev: &InputEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(ev, InputEvent::Change) {
            cx.notify()
        }
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
        let connectors = (!create).then(|| cx.new(|cx| crate::bot_connectors::BotConnectors::new(data.clone(), toasts.clone(), bot, cx)));
        let triggers = (!create).then(|| cx.new(|cx| crate::triggers::BotTriggers::new(data.clone(), toasts.clone(), bot, cx)));
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
            template: None,
            answers: Vec::new(),
            presets: Vec::new(),
            allowed: None,
            folders: None,
            connectors,
            triggers,
            desktop: b.desktop,
        };
        this.load_catalog(cx);
        if !create {
            this.load_allowed(cx);
            this.load_folders(cx);
            let data = this.data.clone();
            cx.subscribe(&data, |this: &mut Self, _, ev: &crate::data::DataEvent, cx| match ev {
                crate::data::DataEvent::Changed(None) => this.load_folders(cx),
                crate::data::DataEvent::Changed(Some(n)) if n.t == "bot_folders" => this.load_folders(cx),
                _ => {}
            })
            .detach();
        }
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

    /// Its webhooks section (edit mode).
    pub fn triggers_view(&self) -> Option<Entity<crate::triggers::BotTriggers>> {
        self.triggers.clone()
    }

    /// The page left the screen: what was shown only once (a trigger's new address) is wiped.
    pub fn hidden(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.triggers.clone() {
            t.update(cx, |t, cx| t.hidden(cx));
        }
    }

    /// The page is on screen again: what went stale meanwhile is read afresh.
    pub fn shown_again(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.triggers.clone() {
            t.update(cx, |t, cx| t.shown_again(cx));
        }
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
            desktop: None,
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
        if let Some(t) = self.template.clone() {
            return self.hire(t, name, model, avatar, cx);
        }
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
                        cx.emit(CreateEvent::Created { bot, post: intro.then(|| INTRO.to_owned()) });
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

    /// Create mode from a template: one call makes the teammate, its schedules (off) and its Set up checklist.
    fn hire(&mut self, t: Template, name: String, model: String, avatar: Option<familiar_client::Avatar>, cx: &mut Context<Self>) {
        let hire = FromTemplate {
            answers: Some(self.answers.iter().map(|(k, _, f)| (k.clone(), f.value(cx))).collect()),
            name: Some(name),
            instructions: Some(self.persona.read(cx).value().trim().to_string()),
            engine: Some(if self.codex { "codex" } else { "claude" }.into()),
            model: Some(model),
            avatar,
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let first = self.intro;
        let task = Tokio::spawn(cx, async move { client.create_from_template(&t.id, &hire).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(hired) => {
                        this.toasts.update(cx, |t, cx| t.push(Tone::Ok, format!("{} is ready", hired.bot.name), None, cx));
                        let post = hired.first_task.filter(|_| first);
                        cx.emit(CreateEvent::Created { bot: hired.bot, post });
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

    /// The template's questions, each with its own field.
    fn questions(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.answers.is_empty() {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let mut col = div().flex().flex_col().gap(px(14.0));
        for (i, (key, label, field)) in self.answers.iter().enumerate() {
            let id = SharedString::from(format!("bs-q-{key}"));
            let input = match field {
                Answer::Line(s) => text_input::field(id, s, 38.0, window, cx).into_any_element(),
                Answer::Area(s) => text_input::field(id, s, 60.0, window, cx).into_any_element(),
            };
            col = col.child(anim::stagger(
                SharedString::from(format!("bs-q-in-{key}")),
                i,
                div().flex().flex_col().gap(px(6.0)).child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(label.clone())).child(input),
            ));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("A few questions"))
                        .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(
                            "Your answers go into its instructions. Skip any you like: it will ask you when it matters.",
                        )),
                )
                .child(col)
                .into_any_element(),
        )
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
pub(crate) fn checkbox(on: bool, theme: &Theme) -> gpui::Div {
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

impl BotSettings {
    fn load_allowed(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        crate::data::swr(self, &client, format!("/api/rules?bot_id={}", self.bot), cx, |this, list: Vec<familiar_client::Rule>, _| {
            this.allowed = Some(list.into_iter().filter(|r| r.decision == familiar_client::RuleDecision::Allow).collect());
        });
    }

    fn remove_rule(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        if let Some(list) = self.allowed.as_mut() {
            list.retain(|r| r.id != id);
        }
        cx.notify();
        let task = Tokio::spawn(cx, async move { client.delete_rule(id).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(()) => this.toasts.update(cx, |t, cx| t.push(Tone::Ok, "It asks you again from now on", None, cx)),
                    Err(e) => this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't remove that", Some(e.into()), cx)),
                }
                this.data.read(cx).client.invalidate("/api/rules");
                this.load_allowed(cx);
            });
        })
        .detach();
    }

    /// "Allowed without asking": the teammate's allow rules in plain words, each with Remove.
    fn allowed_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let list = self.allowed.as_ref()?;
        let mut col = div().flex().flex_col().gap(px(6.0)).child(
            div().flex().items_center().justify_between().child(label("Allowed without asking")).child(
                Button::new("bs-all-rules", "All rules")
                    .ghost()
                    .size(ButtonSize::Small)
                    .icon(icons::SHIELD)
                    .tooltip("Every rule, for every teammate")
                    .on_click(|_, _, cx| crate::shell::open_page("rules", cx)),
            ),
        );
        if list.is_empty() {
            col = col.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                "Nothing yet. \"Always allow\" on an approval adds a rule here; it takes effect at once and you can remove it.",
            ));
        }
        for r in list {
            let id = r.id;
            let this = cx.entity();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(theme.sunken)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(text::SMALL)).text_color(theme.ink).child(crate::approval::rule_words(&r.pattern)))
                            .child(
                                div()
                                    .font_family(theme.font_mono.clone())
                                    .text_size(px(text::CAPTION))
                                    .text_color(theme.muted)
                                    .truncate()
                                    .child(crate::approval::reveal(&r.pattern, false).0),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("bs-rule-{id}")), "Remove")
                            .size(ButtonSize::Small)
                            .ghost()
                            .on_click(move |_, _, cx| this.update(cx, |p, cx| p.remove_rule(id, cx))),
                    ),
            );
        }
        Some(col.into_any_element())
    }
}

impl BotSettings {
    fn load_folders(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        crate::data::swr(self, &client, format!("/api/bots/{}/folders", self.bot), cx, |this, list: Vec<familiar_client::Folder>, _| {
            this.folders = Some(list)
        });
    }

    fn folders_changed(&mut self, cx: &mut Context<Self>) {
        self.data.read(cx).client.invalidate(&format!("/api/bots/{}/folders", self.bot));
        self.load_folders(cx);
    }

    /// "Add folder": the system picker, then share it read only.
    fn add_folder(&mut self, cx: &mut Context<Self>) {
        crate::folders::pick(cx, |this: &mut Self, path, cx| {
            let client = this.data.read(cx).client.clone();
            crate::folders::share(client, this.bot, path, cx, |this: &mut Self, r, cx| {
                match r {
                    Ok(f) => this.toasts.update(cx, |t, cx| {
                        t.push(Tone::Ok, format!("Shared {} (read only)", crate::folders::short_name(&f)), None, cx)
                    }),
                    Err(e) => this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't share that folder", Some(e.into()), cx)),
                }
                this.folders_changed(cx);
            });
        });
    }

    fn set_folder_mode(&mut self, id: Uuid, write: bool, cx: &mut Context<Self>) {
        if let Some(f) = self.folders.as_mut().and_then(|l| l.iter_mut().find(|f| f.id == id)) {
            f.mode = if write { "write" } else { "read" }.into();
        }
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let task = Tokio::spawn(cx, async move { client.set_folder_mode(id, if write { "write" } else { "read" }).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = r {
                    this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't change that", Some(e.into()), cx));
                }
                this.folders_changed(cx);
            });
        })
        .detach();
    }

    fn remove_folder(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if let Some(list) = self.folders.as_mut() {
            list.retain(|f| f.id != id);
        }
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let task = Tokio::spawn(cx, async move { client.delete_folder(id).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(()) => this.toasts.update(cx, |t, cx| t.push(Tone::Ok, "It can't use that folder any more", None, cx)),
                    Err(e) => this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't remove that", Some(e.into()), cx)),
                }
                this.folders_changed(cx);
            });
        })
        .detach();
    }

    /// Switch "Can use this PC's desktop" (saved at once).
    fn set_desktop(&mut self, on: bool, cx: &mut Context<Self>) {
        self.desktop = on;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let patch = BotPatch { desktop: Some(on), ..Default::default() };
        let task = Tokio::spawn(cx, async move { client.update_bot(bot, &patch).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(b) => {
                        this.desktop = b.desktop;
                        let msg = if b.desktop { "It can use your desktop, one approved step at a time" } else { "It can't use your desktop now" };
                        this.toasts.update(cx, |t, cx| t.push(Tone::Ok, msg, None, cx));
                        this.data.update(cx, |d, cx| d.reload_overview(cx));
                    }
                    Err(e) => {
                        this.desktop = !on;
                        this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't change that", Some(e.into()), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "This PC's desktop": the switch, its plain warning, and where desktop control stands on this PC.
    fn desktop_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let status = familiar_host::desktop_status();
        let usable = !matches!(status, familiar_host::Desktop::Unsupported | familiar_host::Desktop::NoUv);
        let on = self.desktop;
        let this = cx.entity();
        // Turning it on needs Windows and uv; turning it off always works.
        let switch = Switch::new("bs-desktop", on).when(on || usable, |s| {
            s.on_toggle(move |v, _, cx| this.update(cx, |p, cx| p.set_desktop(v, cx)))
        });
        let (status_tone, status_text) = match &status {
            familiar_host::Desktop::Ready(_) if on => (
                theme.muted,
                "Ready. While it uses your desktop the tray icon turns red, and the tray menu has Stop desktop control."
                    .to_owned(),
            ),
            familiar_host::Desktop::Ready(_) => (theme.muted, "Ready on this PC.".to_owned()),
            familiar_host::Desktop::Missing if on => (theme.muted, familiar_host::Desktop::Installing.message()),
            familiar_host::Desktop::Missing => (theme.muted, "Turning it on sets it up once (a minute or two).".to_owned()),
            familiar_host::Desktop::Installing => (theme.muted, status.message()),
            _ => (theme.bad, status.message()),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(label("This PC's desktop"))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.0))
                    .child(div().mt(px(2.0)).when(!on && !usable, |el| el.opacity(0.5)).child(switch))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::MEDIUM).child("Can use this PC's desktop"))
                            .child(
                                div()
                                    .text_size(px(text::SMALL))
                                    .text_color(theme.ink)
                                    .child("It can see your screen and use your mouse and keyboard. Every action asks you first."),
                            )
                            .child(div().text_size(px(text::CAPTION)).text_color(status_tone).child(status_text)),
                    ),
            )
            .into_any_element()
    }

    /// "Folders on this PC": each shared folder with its mode (read only / read & write) and Remove, plus Add folder.
    fn folders_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let caption = |s: &'static str| div().text_size(px(text::CAPTION)).text_color(theme.muted).child(s);
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(label("Folders on this PC"))
            .child(caption(
                "Let it work with files outside its own workspace. Folders are read only unless you say otherwise; \
                 every change it makes still asks you first. Your home folder, app data, system folders and other \
                 teammates' workspaces can't be shared.",
            ));
        if self.codex {
            col = col.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(theme.warn_soft)
                    .child(icons::icon(icons::DANGER_TRIANGLE).size(px(14.0)).mt(px(2.0)).text_color(theme.warn))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.ink).child(
                        "On Codex, Familiar can't limit what it reads: Codex can read files anywhere on this PC, shared \
                         or not. Read only still stops it changing anything in a folder; changes anywhere else are \
                         refused or ask you.",
                    )),
            );
        }
        let list = self.folders.clone().unwrap_or_default();
        for f in list {
            let id = f.id;
            let mode_this = cx.entity();
            let rm_this = cx.entity();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(theme.sunken)
                    .child(icons::icon(icons::FOLDER).size(px(16.0)).text_color(theme.muted))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(text::SMALL)).text_color(theme.ink).truncate().child(crate::folders::short_name(&f)))
                            .child(
                                div()
                                    .font_family(theme.font_mono.clone())
                                    .text_size(px(text::CAPTION))
                                    .text_color(theme.muted)
                                    .truncate()
                                    .child(crate::approval::reveal(&f.path, false).0),
                            ),
                    )
                    .child(
                        Segmented::new(
                            SharedString::from(format!("bs-folder-mode-{id}")),
                            vec![("Read only".into(), None), ("Read & write".into(), None)],
                            f.writable() as usize,
                        )
                        .segment_width(96.0)
                        .on_select(move |i, _, cx| mode_this.update(cx, |p, cx| p.set_folder_mode(id, i == 1, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("bs-folder-rm-{id}")), "Remove")
                            .size(ButtonSize::Small)
                            .ghost()
                            .on_click(move |_, _, cx| rm_this.update(cx, |p, cx| p.remove_folder(id, cx))),
                    ),
            );
        }
        let add_this = cx.entity();
        let full = self.folders.as_ref().is_some_and(|l| l.len() >= 20);
        col.child(
            div().flex().child(
                Button::new("bs-folder-add", "Add folder")
                    .size(ButtonSize::Small)
                    .icon(icons::FOLDER)
                    .disabled(full)
                    .on_click(move |_, _, cx| add_this.update(cx, |p, cx| p.add_folder(cx))),
            ),
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
        let template = self.template.clone();
        let questions = self.questions(window, cx);
        let summary = template.as_ref().map(|t| templates::setup_summary(t, &self.presets, cx));
        let answers: Vec<(String, String)> = self.answers.iter().map(|(k, _, f)| (k.clone(), f.value(cx))).collect();
        let (intro_title, intro_caption) = match template.as_ref().and_then(|t| t.first_task.clone()) {
            Some(task) => (
                "Start on its first task when it's ready".to_owned(),
                format!("Opens its chat and asks: “{}” Uses a little of your plan.", excerpt(&templates::preview(&task, &answers), 160)),
            ),
            None => ("Say hello when it's ready".to_owned(), format!("Opens its chat and asks: “{INTRO}” Uses a little of your plan.")),
        };
        let back_this = cx.entity();
        let intro_this = cx.entity();
        let cancel_this = cx.entity();
        let create_this = cx.entity();
        let intro = self.intro;
        div()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .when(create, |el| {
                let (title, lead) = match &template {
                    Some(t) => (t.name.clone(), t.summary.clone()),
                    None => ("New teammate".to_owned(), "Name it, give it a look and a job. You can change all of this later.".to_owned()),
                };
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .id("bs-back")
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .cursor_pointer()
                                .text_size(px(text::SMALL))
                                .text_color(theme.muted)
                                .hover(|s| s.text_color(theme.ink))
                                .on_click(move |_, _, cx| back_this.update(cx, |_, cx| cx.emit(CreateEvent::Back)))
                                .child(
                                    icons::icon(icons::ALT_ARROW_RIGHT)
                                        .size(px(14.0))
                                        .text_color(theme.muted)
                                        .with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))),
                                )
                                .child("All templates"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(14.0))
                                .when_some(template.as_ref(), |el, t| {
                                    el.child(templates::mascot_tile(format!("bs-tpl-{}", t.id), self.avatar, 56.0, &theme))
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.0))
                                        .min_w_0()
                                        .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child(title))
                                        .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(lead)),
                                ),
                        ),
                )
            })
            .children(questions)
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Name")).child(text_input::field("bs-name", &self.name, 38.0, window, cx)))
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Look")).child(builder))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label(if template.is_some() { "Instructions" } else if create { "What it should do" } else { "Persona" }))
                    .child(text_input::field("bs-persona", &self.persona, 110.0, window, cx))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(if template.is_some() {
                        "Its standing instructions; edit freely. The {{…}} bits are filled in from your answers above."
                    } else if create {
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
            .when_some(self.connectors.clone(), |el, c| el.child(c))
            .when_some(self.triggers.clone(), |el, t| el.child(t))
            .when_some(if create { None } else { self.allowed_view(cx) }, |el, v| el.child(v))
            .when(!create, |el| el.child(self.folders_view(cx)))
            .when(!create && cfg!(windows), |el| el.child(self.desktop_view(cx)))
            .when_some(summary, |el, summary| {
                el.child(div().flex().flex_col().gap(px(6.0)).child(label("What it sets up")).child(summary))
            })
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
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(intro_title))
                                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(intro_caption)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            Button::new("bs-create", match (self.busy, template.is_some()) {
                                (true, _) => "Creating…",
                                (false, true) => "Hire teammate",
                                (false, false) => "Create teammate",
                            })
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
