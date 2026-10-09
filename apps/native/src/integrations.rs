//! Integrations (the web's `pages/Integrations.tsx`): connectors, the MCP servers that give teammates tools. The
//! installed ones (on/off, edit, delete with a confirmation), the catalog of ready-made ones (`/api/connectors/presets`)
//! and your own server: a program on this PC (command, arguments, environment variables) or an online one (an address
//! and headers). Installing or editing happens in a dialog over the page. Telegram has its own card
//! ([`crate::telegram`]).
//!
//! Secrets are write-only: typed into masked fields, read out only to send them (held in [`Zeroizing`] strings, and
//! wiped from the request body once it went), never shown again (the API reports their names only) and never logged.
//! The form, with whatever was typed in it, is dropped when it closes and when the page leaves the screen. A server's
//! command, arguments and address are shown as stored, with hidden characters written out, under a plain warning that
//! the command runs on this PC. Live: a `connectors` notice refreshes the list while the page is on screen.

use std::collections::{BTreeMap, HashSet};

use familiar_client::{Connector, ConnectorPatch, ConnectorPreset, ConnectorSecrets, NewConnector, SecretField};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Skeleton, Switch, card, chip, divider};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, RADIUS_DIALOG, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::approval::{reveal, strip_hidden};
use crate::crm_model::safe_url;
use crate::data::{AppData, DataEvent};
use crate::telegram::TelegramCard;
use crate::text_input;

/// What the connector dialog is for.
#[derive(Clone)]
enum FormKind {
    /// Installing a catalog entry.
    Preset(ConnectorPreset),
    /// Your own MCP server.
    Custom,
    /// Changing an installed one (with its catalog entry, when it came from one).
    Edit(Connector, Option<ConnectorPreset>),
}

/// The connector dialog.
struct Form {
    kind: FormKind,
    name: Entity<InputState>,
    /// An online server (an address and headers) rather than a program on this PC.
    http: bool,
    command: Entity<InputState>,
    args: Entity<InputState>,
    url: Entity<InputState>,
    /// A catalog entry's secret fields, each with its masked box.
    fields: Vec<(SecretField, Entity<InputState>)>,
    /// Your own server's environment variables or headers: a name and a masked value per line.
    rows: Vec<(Entity<InputState>, Entity<InputState>)>,
    error: Option<String>,
    saving: bool,
}

impl Form {
    fn preset(&self) -> Option<&ConnectorPreset> {
        match &self.kind {
            FormKind::Preset(p) => Some(p),
            FormKind::Edit(_, p) => p.as_ref(),
            FormKind::Custom => None,
        }
    }

    fn editing(&self) -> Option<&Connector> {
        match &self.kind {
            FormKind::Edit(c, _) => Some(c),
            _ => None,
        }
    }

    /// The command (and the address) come from the catalog and can't be changed here.
    fn fixed(&self) -> bool {
        self.preset().is_some()
    }
}

pub struct IntegrationsPage {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    presets: Option<Vec<ConnectorPreset>>,
    connectors: Option<Vec<Connector>>,
    error: Option<String>,
    form: Option<Form>,
    /// An install form asked for (`integrations/<preset>`) and not opened yet: it opens once the catalog is in.
    want_install: Option<String>,
    confirm_delete: Option<Uuid>,
    /// Connectors with a request in flight.
    busy: HashSet<Uuid>,
    /// On screen: notices refresh the list (else they mark it stale).
    shown: bool,
    stale: bool,
    scroll: ScrollHandle,
    form_scroll: ScrollHandle,
    telegram: Entity<TelegramCard>,
}

impl IntegrationsPage {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.changed(cx),
            DataEvent::Changed(Some(n)) if n.t == "connectors" => this.changed(cx),
            _ => {}
        })
        .detach();
        let telegram = cx.new(|cx| TelegramCard::new(data.clone(), toasts.clone(), cx));
        let mut this = Self {
            data,
            toasts,
            presets: None,
            connectors: None,
            error: None,
            form: None,
            want_install: None,
            confirm_delete: None,
            busy: HashSet::new(),
            shown: true,
            stale: false,
            scroll: ScrollHandle::new(),
            form_scroll: ScrollHandle::new(),
            telegram,
        };
        let client = this.client(cx);
        crate::data::swr(&mut this, &client, "/api/connectors/presets".into(), cx, |this, p: Vec<ConnectorPreset>, _| {
            this.presets = Some(p)
        });
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

    /// The shell shows or hides the page. Hidden: the open dialog (and every secret typed into it) is dropped, and
    /// notices only mark the list stale.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if !shown {
            self.form = None;
            self.confirm_delete = None;
        } else if std::mem::take(&mut self.stale) {
            self.reload(cx);
        }
        self.telegram.update(cx, |t, cx| t.set_shown(shown, cx));
        cx.notify();
    }

    /// Open a catalog entry's install form (by its id) as soon as the catalog is in.
    pub fn install_when_ready(&mut self, preset: String, cx: &mut Context<Self>) {
        self.want_install = Some(preset);
        cx.notify();
    }

    /// Scroll the page to `y` px from its top (the bench's catalog shot).
    pub fn scroll_to(&mut self, y: f32, cx: &mut Context<Self>) {
        self.scroll.set_offset(gpui::point(px(0.0), px(-y)));
        cx.notify();
    }

    /// Open the "Add your own server" dialog (`edit`: an installed connector's, by name, instead). For the bench.
    pub fn open_dialog(&mut self, edit: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let found = edit.and_then(|n| self.connectors.iter().flatten().find(|c| c.name == n).cloned());
        let kind = match found {
            Some(c) => {
                let preset = c.preset.as_deref().and_then(|id| self.presets.iter().flatten().find(|x| x.id == id)).cloned();
                FormKind::Edit(c, preset)
            }
            None => FormKind::Custom,
        };
        self.open_form(kind, window, cx);
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
        let task = Tokio::spawn(cx, async move { client.connectors().await });
        cx.spawn(async move |this, cx| {
            let Ok(r) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(list) => {
                        p.connectors = Some(list);
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run a call for one connector: busy meanwhile, a toast on failure, the list reloaded after.
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
        if let Some(c) = self.connectors.as_mut().and_then(|l| l.iter_mut().find(|c| c.id == id)) {
            c.enabled = on;
        }
        let client = self.client(cx);
        let patch = ConnectorPatch { enabled: Some(on), ..Default::default() };
        self.act(id, async move { client.update_connector(id, &patch).await }, "Couldn't change it", cx, |_, _, _| {});
    }

    fn delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = None;
        let name = self.connectors.iter().flatten().find(|c| c.id == id).map(|c| strip_hidden(&c.name, false)).unwrap_or_default();
        let client = self.client(cx);
        self.act(id, async move { client.delete_connector(id).await }, "Couldn't delete it", cx, move |p, r, cx| {
            if r.is_ok() {
                p.connectors.iter_mut().for_each(|l| l.retain(|c| c.id != id));
                p.toast(Tone::Ok, format!("Removed {name}"), Some("Teammates lose its tools from their next run.".into()), cx);
            }
        });
    }

    // ---- the dialog -----------------------------------------------------------------------------------------------

    /// An input that redraws as it changes and saves on Enter.
    fn line(&self, placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = text_input::new_line(placeholder.to_owned(), masked, window, cx);
        cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.save(cx),
            InputEvent::Change => {
                if let Some(f) = this.form.as_mut() {
                    f.error = None;
                }
                cx.notify()
            }
            _ => {}
        })
        .detach();
        input
    }

    fn open_form(&mut self, kind: FormKind, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.line("my-server", false, window, cx);
        let command = self.line("npx", false, window, cx);
        let args = self.line("-y @modelcontextprotocol/server-memory", false, window, cx);
        let url = self.line("https://example.com/mcp", false, window, cx);
        let mut http = false;
        let mut fields = Vec::new();
        let mut rows = Vec::new();
        let set = |input: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| {
            input.update(cx, |s, cx| s.set_value(v, window, cx))
        };
        let preset = match &kind {
            FormKind::Preset(p) => Some(p.clone()),
            FormKind::Edit(_, p) => p.clone(),
            FormKind::Custom => None,
        };
        match &kind {
            FormKind::Preset(p) => {
                set(&name, p.id.clone(), window, cx);
                http = p.transport == "http";
                set(&command, p.command.clone().unwrap_or_default(), window, cx);
                set(&args, join_args(p.args.as_deref().unwrap_or_default()), window, cx);
                set(&url, p.url.clone().unwrap_or_default(), window, cx);
            }
            FormKind::Edit(c, _) => {
                set(&name, c.name.clone(), window, cx);
                http = c.transport == "http";
                set(&command, c.command.clone().unwrap_or_default(), window, cx);
                set(&args, join_args(c.args.as_deref().unwrap_or_default()), window, cx);
                set(&url, c.url.clone().unwrap_or_default(), window, cx);
            }
            FormKind::Custom => {}
        }
        match &preset {
            Some(p) => {
                for f in &p.secret_fields {
                    fields.push((f.clone(), self.line("", true, window, cx)));
                }
            }
            None => {
                // Editing your own server: a line per stored secret (its name; the value is never shown), else one blank line.
                let stored = match &kind {
                    FormKind::Edit(c, _) => c.stored_secret_names(),
                    _ => Vec::new(),
                };
                for n in stored {
                    let (k, v) = self.secret_row(http, window, cx);
                    set(&k, n, window, cx);
                    rows.push((k, v));
                }
                if rows.is_empty() {
                    rows.push(self.secret_row(http, window, cx));
                }
            }
        }
        // The first thing to fill in has the cursor.
        let first = fields.first().map(|(_, i)| i.clone()).unwrap_or_else(|| name.clone());
        first.update(cx, |s, cx| s.focus(window, cx));
        self.form = Some(Form { kind, name, http, command, args, url, fields, rows, error: None, saving: false });
        self.form_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    fn secret_row(&self, http: bool, window: &mut Window, cx: &mut Context<Self>) -> (Entity<InputState>, Entity<InputState>) {
        let key = self.line(if http { "Authorization" } else { "API_KEY" }, false, window, cx);
        let value = self.line(if http { "Bearer …" } else { "value" }, true, window, cx);
        (key, value)
    }

    fn close_form(&mut self, cx: &mut Context<Self>) {
        self.form = None;
        cx.notify();
    }

    /// Check the dialog and send it.
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.form.as_mut() else { return };
        if f.saving {
            return;
        }
        let built = build(f, cx);
        let Some(f) = self.form.as_mut() else { return };
        let request = match built {
            Ok(r) => r,
            Err(e) => {
                f.error = Some(e);
                cx.notify();
                return;
            }
        };
        f.saving = true;
        f.error = None;
        cx.notify();
        let client = self.client(cx);
        let creating = matches!(request, Request::Create(_));
        let task = Tokio::spawn(cx, async move {
            match request {
                Request::Create(mut body) => {
                    let r = client.create_connector(&body).await;
                    wipe(&mut body.secrets);
                    r
                }
                Request::Update(id, mut patch) => {
                    let r = client.update_connector(id, &patch).await;
                    wipe(&mut patch.secrets);
                    r
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(c) => {
                        let name = strip_hidden(&c.name, false);
                        p.form = None;
                        if creating {
                            p.toast(Tone::Ok, format!("Installed {name}"), Some("Choose which teammates may use it on each one's Settings tab.".into()), cx);
                        } else {
                            p.toast(Tone::Ok, format!("Saved {name}"), None, cx);
                        }
                        p.reload(cx);
                    }
                    Err(e) => {
                        if let Some(f) = p.form.as_mut() {
                            f.saving = false;
                            f.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- drawing ----------------------------------------------------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child("Integrations"))
            .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(
                "Tools your teammates can use (GitHub, Notion, Slack, your own), and a way to reach them from your phone.",
            ))
            .into_any_element()
    }

    /// A section title with its explanation.
    fn section_head(title: &'static str, hint: &'static str, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(hint))
    }

    fn installed(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        match (&self.connectors, &self.error) {
            (None, Some(e)) => div().text_size(px(text::SMALL)).text_color(theme.bad).child(e.clone()).into_any_element(),
            (None, None) => Skeleton::new(64.0).radius(RADIUS_CARD).into_any_element(),
            (Some(list), _) if list.is_empty() => div()
                .px(px(16.0))
                .py(px(16.0))
                .rounded(px(RADIUS_CARD))
                .border_1()
                .border_dashed()
                .border_color(theme.line)
                .text_size(px(text::SMALL))
                .text_color(theme.muted)
                .child("Nothing installed yet. Pick one from the catalog below, or add your own server.")
                .into_any_element(),
            (Some(list), _) => {
                let mut c = card(cx).flex().flex_col().overflow_hidden();
                for (i, conn) in list.iter().enumerate() {
                    if i > 0 {
                        c = c.child(divider(cx));
                    }
                    c = c.child(anim::stagger(SharedString::from(format!("conn-in-{}", conn.id)), i, div().child(self.connector_row(conn, cx))));
                }
                c.into_any_element()
            }
        }
    }

    fn connector_row(&self, c: &Connector, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = c.id;
        let key = id.as_u128() as u64;
        let busy = self.busy.contains(&id);
        let name = strip_hidden(&c.name, false);
        let http = c.transport == "http";
        let target = if http { c.url.clone().unwrap_or_default() } else { command_line(c) };
        let (target, hidden) = reveal(&target, false);
        let preset_name = c.preset.as_deref().and_then(|p| self.presets.iter().flatten().find(|x| x.id == p)).map(|p| p.name.clone());
        let (t_on, t_edit, t_del) = (cx.entity(), cx.entity(), cx.entity());
        let conn = c.clone();
        let mut row = div().flex().flex_col().child(
            div()
                .flex()
                .items_start()
                .gap(px(14.0))
                .px(px(16.0))
                .py(px(14.0))
                .child(tile(if http { icons::LINK } else { icons::PLUG }, c.enabled, &theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.0))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().font_weight(FontWeight::MEDIUM).text_color(if c.enabled { theme.ink } else { theme.muted }).child(name.clone()))
                                .when_some(preset_name.filter(|p| !p.eq_ignore_ascii_case(&name)), |el, p| {
                                    el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(p))
                                })
                                .child(chip(Tone::Muted, if http { "Online server" } else { "Program on this PC" }, cx))
                                .when(c.has_secrets, |el| el.child(chip(Tone::Ok, "Secrets stored", cx)))
                                .when(!c.enabled, |el| el.child(chip(Tone::Warn, "Off", cx))),
                        )
                        .child(
                            div()
                                .font_family(theme.font_mono.clone())
                                .text_size(px(text::CAPTION))
                                .text_color(if hidden { theme.bad } else { theme.muted })
                                .truncate()
                                .child(target),
                        )
                        .child(
                            div()
                                .text_size(px(text::CAPTION))
                                .text_color(theme.muted)
                                .child(format!("Its tools: mcp__{name}__…")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            Switch::new(("conn-on", key), c.enabled)
                                .on_toggle(move |on, _, cx| t_on.update(cx, |p, cx| p.toggle(id, on, cx))),
                        )
                        .child(
                            Button::icon_only(("conn-edit", key), icons::PEN)
                                .size(ButtonSize::Small)
                                .disabled(busy)
                                .tooltip("Edit")
                                .on_click(move |_, window, cx| {
                                    let conn = conn.clone();
                                    t_edit.update(cx, |p, cx| {
                                        let preset = conn.preset.as_deref().and_then(|id| p.presets.iter().flatten().find(|x| x.id == id)).cloned();
                                        p.open_form(FormKind::Edit(conn, preset), window, cx)
                                    })
                                }),
                        )
                        .child(
                            Button::icon_only(("conn-del", key), icons::TRASH)
                                .size(ButtonSize::Small)
                                .disabled(busy)
                                .tooltip("Remove")
                                .on_click(move |_, _, cx| {
                                    t_del.update(cx, |p, cx| {
                                        p.confirm_delete = Some(id);
                                        cx.notify()
                                    })
                                }),
                        ),
                ),
        );
        if self.confirm_delete == Some(id) {
            let (yes, no) = (cx.entity(), cx.entity());
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pl(px(66.0))
                    .pr(px(16.0))
                    .pb(px(12.0))
                    .child(div().flex_1().text_size(px(text::SMALL)).text_color(theme.bad).child(format!(
                        "Remove {name}? Teammates lose its tools, and its stored secrets are deleted."
                    )))
                    .child(Button::new(("conn-del-no", key), "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        no.update(cx, |p, cx| {
                            p.confirm_delete = None;
                            cx.notify()
                        })
                    }))
                    .child(
                        Button::new(("conn-del-yes", key), "Remove")
                            .danger()
                            .size(ButtonSize::Small)
                            .on_click(move |_, _, cx| yes.update(cx, |p, cx| p.delete(id, cx))),
                    ),
            );
        }
        row.into_any_element()
    }

    fn catalog(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(presets) = self.presets.as_ref() else {
            return div()
                .flex()
                .gap(px(12.0))
                .child(div().flex_1().child(Skeleton::new(120.0).radius(RADIUS_CARD)))
                .child(div().flex_1().child(Skeleton::new(120.0).radius(RADIUS_CARD)))
                .into_any_element();
        };
        let installed: HashSet<&str> = self.connectors.iter().flatten().filter_map(|c| c.preset.as_deref()).collect();
        let mut grid = div().flex().flex_col().gap(px(12.0));
        for (r, pair) in presets.chunks(2).enumerate() {
            let mut line = div().flex().gap(px(12.0));
            for (k, p) in pair.iter().enumerate() {
                line = line.child(div().flex_1().min_w_0().flex().child(anim::stagger(
                    SharedString::from(format!("preset-in-{}", p.id)),
                    r * 2 + k,
                    div().flex_1().flex().child(self.preset_card(p, installed.contains(p.id.as_str()), cx)),
                )));
            }
            if pair.len() == 1 {
                line = line.child(div().flex_1());
            }
            grid = grid.child(line);
        }
        let _ = theme;
        grid.into_any_element()
    }

    fn preset_card(&self, p: &ConnectorPreset, installed: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let http = p.transport == "http";
        let docs = p.docs_url.as_deref().and_then(safe_url);
        let this = cx.entity();
        let preset = p.clone();
        card(cx)
            .flex_1()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(initials_tile(&p.name, &theme))
                    .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child(strip_hidden(&p.name, false)))
                    .child(chip(Tone::Muted, if http { "Online" } else { "On this PC" }, cx)),
            )
            .child(div().flex_1().text_size(px(text::SMALL)).text_color(theme.muted).child(strip_hidden(&p.description, false)))
            .when(p.verify, |el| {
                el.child(div().text_size(px(text::CAPTION)).text_color(theme.warn).child("Its package name wasn't confirmed: check the docs first."))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pt(px(4.0))
                    .child(if installed {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(text::SMALL))
                            .text_color(theme.ok)
                            .child(icon(icons::CHECK).size(px(14.0)).text_color(theme.ok))
                            .child("Installed")
                            .into_any_element()
                    } else {
                        Button::new(SharedString::from(format!("preset-install-{}", p.id)), "Install")
                            .primary()
                            .size(ButtonSize::Small)
                            .on_click(move |_, window, cx| {
                                let preset = preset.clone();
                                this.update(cx, |p, cx| p.open_form(FormKind::Preset(preset), window, cx))
                            })
                            .into_any_element()
                    })
                    .when_some(docs, |el, url| {
                        el.child(
                            Button::new(SharedString::from(format!("preset-docs-{}", p.id)), "Docs")
                                .ghost()
                                .size(ButtonSize::Small)
                                .icon(icons::LINK)
                                .tooltip(url.clone())
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The connector dialog, over the page.
    fn form_view(&self, f: &Form, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let preset = f.preset().cloned();
        let editing = f.editing().cloned();
        let label = |s: &str| div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(s.to_owned());
        let caption = |s: String, bad: bool| div().text_size(px(text::CAPTION)).text_color(if bad { theme.bad } else { theme.muted }).child(s);
        let (title, lead) = match (&f.kind, &preset) {
            (FormKind::Preset(p), _) => (format!("Install {}", strip_hidden(&p.name, false)), strip_hidden(&p.description, false)),
            (FormKind::Edit(c, _), _) => (format!("Edit {}", strip_hidden(&c.name, false)), "Changes apply from each teammate's next run.".to_owned()),
            (FormKind::Custom, _) => ("Add your own server".to_owned(), "Any MCP server: a program this PC runs, or one online.".to_owned()),
        };
        let mut body = div().flex().flex_col().gap(px(16.0));

        // What it is, and what that means for this PC.
        let command = f.command.read(cx).value().trim().to_owned();
        let args = f.args.read(cx).value().trim().to_owned();
        let url = f.url.read(cx).value().trim().to_owned();
        body = body.child(warning(f.http, &command, &args, &url, &theme));

        // Name.
        let name = f.name.read(cx).value().trim().to_owned();
        let name_hint = match (name.is_empty(), name_problem(&name)) {
            (false, Some(why)) => (why.to_owned(), true),
            _ => (format!("Its tools show up as mcp__{}__… in approvals and rules.", if name.is_empty() { "name" } else { &name }), false),
        };
        body = body.child(
            div().flex().flex_col().gap(px(6.0)).child(label("Name")).child(mono(text_input::field("conn-name", &f.name, 38.0, window, cx), &theme)).child(caption(name_hint.0, name_hint.1)),
        );

        // Kind (your own server only).
        if !f.fixed() && editing.is_none() {
            let this = cx.entity();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("Kind"))
                    .child(
                        div().flex().child(
                            Segmented::new("conn-kind", vec![("Program on this PC".into(), Some(icons::MONITOR)), ("Online server".into(), Some(icons::LINK))], f.http as usize)
                                .segment_width(170.0)
                                .on_select(move |i, window, cx| {
                                    this.update(cx, |p, cx| {
                                        let http = i == 1;
                                        let rows = p.form.as_ref().is_some_and(|f| f.http != http);
                                        if rows {
                                            // The secrets change meaning (environment variables / headers): start them afresh.
                                            let fresh = p.secret_row(http, window, cx);
                                            if let Some(f) = p.form.as_mut() {
                                                f.http = http;
                                                f.rows = vec![fresh];
                                                f.error = None;
                                            }
                                        }
                                        cx.notify()
                                    })
                                }),
                        ),
                    ),
            );
        }

        // Where it is.
        if f.http {
            let field = if f.fixed() { fixed_box(&url, &theme) } else { mono(text_input::field("conn-url", &f.url, 38.0, window, cx), &theme).into_any_element() };
            let bad = (!url.is_empty()).then(|| url_problem(&url)).flatten();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("Address"))
                    .child(field)
                    .when_some(bad, |el, why| el.child(caption(why.to_owned(), true))),
            );
        } else {
            let cmd = if f.fixed() { fixed_box(&command, &theme) } else { mono(text_input::field("conn-cmd", &f.command, 38.0, window, cx), &theme).into_any_element() };
            let args_hint = match &preset {
                Some(p) if p.description.to_lowercase().contains("replace the last") => strip_hidden(&p.description, false),
                _ => "Separated by spaces; put quotes around one that has spaces in it.".to_owned(),
            };
            body = body
                .child(div().flex().flex_col().gap(px(6.0)).child(label("Command")).child(cmd))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(label("Arguments"))
                        .child(mono(text_input::field("conn-args", &f.args, 38.0, window, cx), &theme))
                        .child(caption(args_hint, false)),
                );
        }

        // Secrets.
        let stored = editing.as_ref().filter(|c| c.has_secrets).map(|c| c.stored_secret_names());
        if !f.fields.is_empty() {
            let mut col = div().flex().flex_col().gap(px(12.0));
            for (i, (field, input)) in f.fields.iter().enumerate() {
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(label(&strip_hidden(&field.label, false)))
                        .child(mono(text_input::field(("conn-secret", i), input, 38.0, window, cx), &theme))
                        .when_some(field.help.clone().filter(|h| !h.trim().is_empty()), |el, h| el.child(caption(strip_hidden(&h, false), false))),
                );
            }
            body = body.child(col);
        } else if f.preset().is_none() {
            body = body.child(self.rows_view(f, window, cx));
        }
        let secret_note = match (&stored, f.fields.is_empty() && f.preset().is_some()) {
            (_, true) => None,
            (Some(names), _) if !names.is_empty() => Some(format!(
                "Stored: {}. Leave every value empty to keep them; filling in any replaces them all. Values are never shown again.",
                names.iter().map(|n| strip_hidden(n, false)).collect::<Vec<_>>().join(", ")
            )),
            (Some(_), _) => Some("Secrets are stored. Leave every value empty to keep them; filling in any replaces them all.".to_owned()),
            (None, _) => Some("Stored encrypted on this computer and never shown again, not even to you.".to_owned()),
        };
        if let Some(note) = secret_note {
            body = body.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.0))
                    .child(div().pt(px(1.0)).child(icon(icons::LOCK).size(px(14.0)).text_color(theme.muted)))
                    .child(div().flex_1().min_w_0().text_size(px(text::CAPTION)).text_color(theme.muted).child(note)),
            );
        }
        if let Some(e) = f.error.clone() {
            body = body.child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(theme.bad_soft)
                    .text_size(px(text::SMALL))
                    .text_color(theme.bad)
                    .child(e),
            );
        }

        let (go, cancel) = (cx.entity(), cx.entity());
        // Installing from the catalog needs its secrets (editing keeps the stored ones when left empty).
        let secrets_in = editing.is_some() || f.fields.iter().all(|(_, i)| !i.read(cx).value().trim().is_empty());
        let ready = !name.is_empty() && name_problem(&name).is_none() && secrets_in && if f.http { !url.is_empty() } else { !command.is_empty() };
        let action = match (&f.kind, f.saving) {
            (FormKind::Edit(..), true) => "Saving…",
            (FormKind::Edit(..), false) => "Save changes",
            (_, true) => "Installing…",
            (_, false) => "Install",
        };
        let max_h = (f32::from(window.viewport_size().height) - 150.0).max(320.0);
        let close = cx.entity();
        div()
            .id("conn-dialog")
            .absolute()
            .inset_0()
            .occlude()
            .bg(if theme.is_dark() { gpui::black().opacity(0.5) } else { theme.ink.opacity(0.18) })
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(move |ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    close.update(cx, |p, cx| p.close_form(cx));
                    cx.stop_propagation();
                }
            })
            .child(anim::appear(
                "conn-dialog-in",
                div()
                    .w(px(580.0))
                    .flex()
                    .flex_col()
                    .rounded(px(RADIUS_DIALOG))
                    .border_1()
                    .border_color(theme.line)
                    .bg(theme.surface)
                    .shadow(theme.float_shadow())
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .px(px(24.0))
                            .pt(px(22.0))
                            .pb(px(14.0))
                            .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(lead)),
                    )
                    .child(
                        div()
                            .id("conn-dialog-scroll")
                            .max_h(px(max_h - 140.0))
                            .overflow_y_scroll()
                            .track_scroll(&self.form_scroll)
                            .px(px(24.0))
                            .pb(px(8.0))
                            .child(body),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(8.0))
                            .px(px(24.0))
                            .py(px(16.0))
                            .border_t_1()
                            .border_color(theme.line)
                            .child(Button::new("conn-cancel", "Cancel").ghost().on_click(move |_, _, cx| cancel.update(cx, |p, cx| p.close_form(cx))))
                            .child(
                                Button::new("conn-save", action)
                                    .primary()
                                    .disabled(f.saving || !ready)
                                    .on_click(move |_, _, cx| go.update(cx, |p, cx| p.save(cx))),
                            ),
                    ),
            ))
            .into_any_element()
    }

    /// Your own server's environment variables (or headers): name and masked value per line, remove, add another.
    fn rows_view(&self, f: &Form, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let mut col = div().flex().flex_col().gap(px(8.0)).child(
            div()
                .text_size(px(text::SMALL))
                .font_weight(FontWeight::MEDIUM)
                .child(if f.http { "Headers (secret)" } else { "Environment variables (secret)" }),
        );
        for (i, (k, v)) in f.rows.iter().enumerate() {
            let this = cx.entity();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(190.0)).flex_none().child(mono(text_input::field(("conn-row-k", i), k, 38.0, window, cx), &theme)))
                    .child(div().text_color(theme.muted).child(if f.http { ":" } else { "=" }))
                    .child(div().flex_1().min_w_0().child(mono(text_input::field(("conn-row-v", i), v, 38.0, window, cx), &theme)))
                    .child(Button::icon_only(("conn-row-rm", i), icons::CLOSE).size(ButtonSize::Small).ghost().tooltip("Remove this line").on_click(move |_, _, cx| {
                        this.update(cx, |p, cx| {
                            if let Some(f) = p.form.as_mut()
                                && i < f.rows.len()
                            {
                                f.rows.remove(i);
                            }
                            cx.notify()
                        })
                    })),
            );
        }
        let this = cx.entity();
        let http = f.http;
        col.child(
            div().flex().child(
                Button::new("conn-row-add", if f.http { "Add a header" } else { "Add a variable" })
                    .ghost()
                    .size(ButtonSize::Small)
                    .icon(icons::PLUS)
                    .disabled(f.rows.len() >= 20)
                    .on_click(move |_, window, cx| {
                        this.update(cx, |p, cx| {
                            let row = p.secret_row(http, window, cx);
                            row.0.update(cx, |s, cx| s.focus(window, cx));
                            if let Some(f) = p.form.as_mut() {
                                f.rows.push(row);
                            }
                            cx.notify()
                        })
                    }),
            ),
        )
        .into_any_element()
    }
}

impl Render for IntegrationsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("IntegrationsPage");
        let theme = Theme::of(cx).clone();
        // An install form asked for by name opens once the catalog says what it is (after this frame: it focuses a field).
        if let Some(want) = self.want_install.clone()
            && let Some(presets) = self.presets.as_ref()
        {
            self.want_install = None;
            if let Some(p) = presets.iter().find(|p| p.id == want).cloned() {
                cx.defer_in(window, move |this, window, cx| this.open_form(FormKind::Preset(p), window, cx));
            }
        }
        let this = cx.entity();
        let connectors = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(Self::section_head(
                "Connectors",
                "MCP servers that give teammates tools. Install one here, then choose which teammates may use it on each one's Settings tab.",
                &theme,
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(theme.sunken)
                    .child(div().pt(px(1.0)).child(icon(icons::SHIELD).size(px(16.0)).text_color(theme.accent)))
                    .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).text_color(theme.ink).child(
                        "A teammate asks you before it uses a connector's tools, until you allow them: \"Always allow\" on an approval does that for tools that only read.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).text_color(theme.muted).child("Installed"))
                    .child(
                        Button::new("conn-custom", "Add your own server")
                            .size(ButtonSize::Small)
                            .icon(icons::PLUS)
                            .on_click(move |_, window, cx| this.update(cx, |p, cx| p.open_form(FormKind::Custom, window, cx))),
                    ),
            )
            .child(self.installed(cx));
        let phone = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(Self::section_head(
                "On your phone",
                "Chat with your teammates and approve what they ask from Telegram, wherever you are.",
                &theme,
            ))
            .child(self.telegram.clone());
        let catalog = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(Self::section_head(
                "Add a connector",
                "Ready-made servers for common tools. Each asks for its key once; its tools then work for the teammates you pick.",
                &theme,
            ))
            .child(self.catalog(cx));
        let page = div()
            .flex()
            .flex_col()
            .gap(px(36.0))
            .child(anim::appear("integ-head", div().child(self.header(cx))))
            .child(connectors)
            .child(phone)
            .child(catalog);
        let dialog = self.form.as_ref().map(|f| self.form_view(f, window, cx));
        div()
            .relative()
            .size_full()
            .child(
                edge_faded(
                    24.0,
                    true,
                    true,
                    div().id("integ-scroll").size_full().overflow_y_scroll().track_scroll(&self.scroll).child(
                        div().w_full().flex().justify_center().px(px(40.0)).py(px(36.0)).child(div().w_full().max_w(px(760.0)).child(page)),
                    ),
                )
                .fade_overflow_y(&self.scroll),
            )
            .children(dialog)
    }
}

// ---- pieces ---------------------------------------------------------------------------------------------------------

/// A field in the monospace face (commands, names, secrets).
fn mono(field: impl IntoElement, theme: &Theme) -> gpui::Div {
    div().font_family(theme.font_mono.clone()).child(field)
}

/// A catalog entry's fixed command or address, shown as it will run.
fn fixed_box(s: &str, theme: &Theme) -> AnyElement {
    let (shown, hidden) = reveal(s, false);
    div()
        .px(px(12.0))
        .py(px(9.0))
        .rounded(px(RADIUS_CONTROL))
        .bg(theme.sunken)
        .font_family(theme.font_mono.clone())
        .text_size(px(text::SMALL))
        .text_color(if hidden { theme.bad } else { theme.ink })
        .child(shown)
        .into_any_element()
}

/// The square at the start of an installed connector's row.
fn tile(glyph: &'static str, on: bool, theme: &Theme) -> gpui::Div {
    div()
        .size(px(36.0))
        .flex_none()
        .rounded(px(RADIUS_CONTROL))
        .bg(if on { theme.accent_soft } else { theme.sunken })
        .flex()
        .items_center()
        .justify_center()
        .child(icon(glyph).size(px(18.0)).text_color(if on { theme.accent } else { theme.muted }))
}

/// A catalog entry's square: its initials (no brand marks are bundled).
fn initials_tile(name: &str, theme: &Theme) -> gpui::Div {
    let initials: String = strip_hidden(name, false).split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
    div()
        .size(px(32.0))
        .flex_none()
        .rounded(px(9.0))
        .bg(theme.sunken)
        .border_1()
        .border_color(theme.line)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(text::SMALL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.ink)
        .child(initials)
}

/// What installing this means for the PC: the program it starts (and that it runs with your access), or where the
/// teammates' calls go.
fn warning(http: bool, command: &str, args: &str, url: &str, theme: &Theme) -> AnyElement {
    let (title, body) = if http {
        let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned));
        (
            "Teammates' tool calls go to another server".to_owned(),
            match host {
                Some(h) => format!("What a teammate sends this connector goes to {}, with the headers below.", reveal(&h, false).0),
                None => "What a teammate sends this connector goes to the address below, with the headers below.".to_owned(),
            },
        )
    } else {
        let line = [command, args].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
        (
            "This runs a program on this PC".to_owned(),
            if line.is_empty() {
                "Whenever a teammate uses it, Familiar starts the command below, with your access to your files and the network. Only add servers you trust.".to_owned()
            } else {
                format!(
                    "Whenever a teammate uses it, Familiar starts `{}` with your access to your files and the network. Only add servers you trust.",
                    reveal(&line, false).0
                )
            },
        )
    };
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .px(px(14.0))
        .py(px(10.0))
        .rounded(px(RADIUS_CONTROL))
        .border_1()
        .border_color(theme.warn.opacity(0.45))
        .bg(theme.warn_soft)
        .child(div().pt(px(1.0)).child(icon(icons::DANGER_TRIANGLE).size(px(16.0)).text_color(theme.warn)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.ink).child(body)),
        )
        .into_any_element()
}

// ---- the request ----------------------------------------------------------------------------------------------------

enum Request {
    Create(NewConnector),
    Update(Uuid, ConnectorPatch),
}

/// Overwrite the secret values in a request body once it was sent.
fn wipe(secrets: &mut Option<ConnectorSecrets>) {
    if let Some(s) = secrets.as_mut() {
        s.env.values_mut().chain(s.headers.values_mut()).for_each(|v| v.zeroize());
    }
}

/// The dialog as a request, or what to fix first.
fn build(f: &Form, cx: &App) -> Result<Request, String> {
    let name = f.name.read(cx).value().trim().to_owned();
    if let Some(why) = name_problem(&name) {
        return Err(why.to_owned());
    }
    let transport = if f.http { "http" } else { "stdio" };
    let (command, args, url) = if f.http {
        let url = f.url.read(cx).value().trim().to_owned();
        if let Some(why) = url_problem(&url) {
            return Err(why.to_owned());
        }
        (None, None, Some(url))
    } else {
        let command = f.command.read(cx).value().trim().to_owned();
        if command.is_empty() {
            return Err("Say which program to run (for example npx or uvx).".to_owned());
        }
        (Some(command), Some(split_args(&f.args.read(cx).value())), None)
    };
    let editing = f.editing().cloned();
    let secrets = if !f.fields.is_empty() {
        let values: Vec<(String, String, Zeroizing<String>)> =
            f.fields.iter().map(|(field, input)| (field.key.clone(), field.label.clone(), Zeroizing::new(input.read(cx).value().to_string()))).collect();
        preset_secrets(&values, editing.is_some())?
    } else if f.preset().is_some() {
        None
    } else {
        let rows: Vec<(String, Zeroizing<String>)> =
            f.rows.iter().map(|(k, v)| (k.read(cx).value().trim().to_owned(), Zeroizing::new(v.read(cx).value().to_string()))).collect();
        secret_rows(&rows, f.http)?
    };
    let secrets = secrets.map(|map| {
        let mut s = ConnectorSecrets::default();
        if f.http {
            s.headers = map;
        } else {
            s.env = map;
        }
        s
    });
    Ok(match editing {
        Some(c) => Request::Update(
            c.id,
            ConnectorPatch {
                name: Some(name),
                // A catalog entry's command and address stay as they are.
                command: if f.fixed() { None } else { command },
                args,
                url: if f.fixed() { None } else { url },
                secrets,
                ..Default::default()
            },
        ),
        None => Request::Create(NewConnector {
            name: Some(name),
            preset: f.preset().map(|p| p.id.clone()),
            transport: Some(transport.to_owned()),
            command,
            args,
            url,
            secrets,
            enabled: Some(true),
        }),
    })
}

// ---- rules for what's typed (pure) ------------------------------------------------------------------------------

/// Why a connector name won't do (the API's rule: 1–32 of a–z, 0–9, `-`, `_`, starting with a letter or digit, and
/// not one of Familiar's own servers), or `None`.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Give it a name.");
    }
    if name.len() > 32 {
        return Some("At most 32 characters.");
    }
    let b = name.as_bytes();
    if !(b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
        return Some("Start with a lowercase letter or a digit.");
    }
    if !b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_') {
        return Some("Use lowercase letters, digits, - and _ only.");
    }
    if matches!(name, "familiar" | "browser" | "desktop") {
        return Some("That name belongs to one of Familiar's own tools.");
    }
    None
}

/// Why an online server's address won't do, or `None`: `http(s)` only.
pub fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("Paste the server's address.");
    }
    match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty()) => None,
        Ok(_) => Some("Use an https:// address."),
        Err(_) => Some("That isn't a web address. It starts with https://"),
    }
}

/// Arguments as typed: split on spaces, except inside double quotes (`"C:\My files"` stays one).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Arguments back as text [`split_args`] reads the same: one with a space (or nothing) in double quotes.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| if a.is_empty() || a.chars().any(char::is_whitespace) { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How an installed program connector runs: its command and arguments.
pub fn command_line(c: &Connector) -> String {
    let args = join_args(c.args.as_deref().unwrap_or_default());
    [c.command.clone().unwrap_or_default(), args].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A catalog entry's secret fields (key, label, value) → what to send. Installing needs every one; editing keeps the
/// stored ones when all are left empty, and needs every one when any is filled in (saving replaces them all).
pub fn preset_secrets(values: &[(String, String, Zeroizing<String>)], editing: bool) -> Result<Option<BTreeMap<String, String>>, String> {
    let filled = values.iter().filter(|(_, _, v)| !v.trim().is_empty()).count();
    if filled == 0 && (editing || values.is_empty()) {
        return Ok(None);
    }
    if let Some((_, label, _)) = values.iter().find(|(_, _, v)| v.trim().is_empty()) {
        return Err(if editing && filled > 0 {
            format!("Fill in {} too: saving replaces every stored secret.", strip_hidden(label, false))
        } else {
            format!("Fill in {}.", strip_hidden(label, false))
        });
    }
    Ok(Some(values.iter().map(|(k, _, v)| (k.clone(), v.trim().to_owned())).collect()))
}

/// Your own server's secret lines (name, value) → what to send. Blank lines are ignored; all values empty keeps the
/// stored ones (`None`). Otherwise every named line needs a value (saving replaces them all), every value a name, and
/// names are plain ASCII under the API's rules (no `=` / `:`, no spaces; at most 128 characters) without repeats.
pub fn secret_rows(rows: &[(String, Zeroizing<String>)], headers: bool) -> Result<Option<BTreeMap<String, String>>, String> {
    let rows: Vec<&(String, Zeroizing<String>)> = rows.iter().filter(|(k, v)| !k.is_empty() || !v.is_empty()).collect();
    if rows.iter().all(|(_, v)| v.is_empty()) {
        return Ok(None);
    }
    let what = if headers { "header" } else { "variable" };
    let mut out = BTreeMap::new();
    for (k, v) in rows {
        if k.is_empty() {
            return Err(format!("Give every value a {what} name."));
        }
        if v.is_empty() {
            return Err(format!("Fill in the value of {}, or remove that line: saving replaces every stored secret.", strip_hidden(k, false)));
        }
        // Plain ASCII without spaces: nothing hidden in a name the owner later reads back as "Stored: …".
        let bad = k.len() > 128
            || k.chars().any(|c| !c.is_ascii_graphic())
            || if headers { k.contains(':') } else { k.contains('=') };
        if bad {
            return Err(format!("{} isn't a usable {what} name.", reveal(k, false).0));
        }
        if out.insert(k.clone(), v.to_string()).is_some() {
            return Err(format!("{} is there twice.", strip_hidden(k, false)));
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(s: &str) -> Zeroizing<String> {
        Zeroizing::new(s.to_owned())
    }

    #[test]
    fn names() {
        for ok in ["github", "my-server_2", "0x", "a"] {
            assert_eq!(name_problem(ok), None, "{ok}");
        }
        for bad in ["", "GitHub", "-x", "my server", "familiar", "browser", "desktop", &"a".repeat(33), "ünï"] {
            assert!(name_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn addresses() {
        assert_eq!(url_problem("https://mcp.linear.app/mcp"), None);
        assert_eq!(url_problem("http://localhost:3000/mcp"), None);
        assert!(url_problem("file:///c:/x").is_some());
        assert!(url_problem("javascript:alert(1)").is_some());
        assert!(url_problem("mcp.linear.app").is_some());
        assert!(url_problem("").is_some());
    }

    #[test]
    fn arguments_round_trip() {
        assert_eq!(split_args("-y  @scope/server  "), ["-y", "@scope/server"]);
        assert_eq!(split_args(r#"-y server "C:\My files" """#), ["-y", "server", r"C:\My files", ""]);
        assert!(split_args("   ").is_empty());
        let args: Vec<String> = ["-y", "@x/y", r"C:\My files", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(join_args(&args), r#"-y @x/y "C:\My files" """#);
        assert_eq!(split_args(&join_args(&args)), args);
        let c = Connector { command: Some("npx".into()), args: Some(vec!["-y".into(), "a b".into()]), ..Default::default() };
        assert_eq!(command_line(&c), r#"npx -y "a b""#);
    }

    #[test]
    fn catalog_secrets() {
        let one = |v: &str| vec![("TOKEN".to_owned(), "Token".to_owned(), z(v))];
        // Installing needs it; editing keeps the stored one when left empty.
        assert!(preset_secrets(&one(""), false).is_err());
        assert_eq!(preset_secrets(&one(""), true), Ok(None));
        assert_eq!(preset_secrets(&one(" t "), false).unwrap().unwrap()["TOKEN"], "t");
        // Two fields, one filled while editing: both are needed (saving replaces them all).
        let two = vec![("A".to_owned(), "Bot token".to_owned(), z("x")), ("B".to_owned(), "Team ID".to_owned(), z(""))];
        assert!(preset_secrets(&two, true).unwrap_err().contains("Team ID too"));
        // Nothing to fill in: nothing to send.
        assert_eq!(preset_secrets(&[], false), Ok(None));
    }

    #[test]
    fn own_secrets() {
        let rows = |r: &[(&str, &str)]| r.iter().map(|(k, v)| (k.to_string(), z(v))).collect::<Vec<_>>();
        assert_eq!(secret_rows(&rows(&[("", "")]), false), Ok(None));
        // Stored names with no values: keep them.
        assert_eq!(secret_rows(&rows(&[("API_KEY", ""), ("OTHER", "")]), false), Ok(None));
        let got = secret_rows(&rows(&[("API_KEY", "k"), ("", "")]), false).unwrap().unwrap();
        assert_eq!(got.len(), 1);
        assert!(secret_rows(&rows(&[("API_KEY", "k"), ("OTHER", "")]), false).unwrap_err().contains("OTHER"));
        assert!(secret_rows(&rows(&[("", "v")]), false).is_err());
        assert!(secret_rows(&rows(&[("A=B", "v")]), false).is_err());
        assert!(secret_rows(&rows(&[("Authorization:", "v")]), true).is_err());
        assert!(secret_rows(&rows(&[("Authorization", "Bearer x")]), true).is_ok());
        assert!(secret_rows(&rows(&[("A", "1"), ("A", "2")]), false).unwrap_err().contains("twice"));
        assert!(secret_rows(&rows(&[("A\u{202E}", "1")]), false).is_err());
    }
}
