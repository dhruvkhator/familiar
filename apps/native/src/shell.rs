//! The default launch: the spike's teammate list, restyled with the kit — a sidebar of teammates (mascots + live
//! status from the local API) and a "Today" mock in the main area, with route crossfades between pages.

use std::collections::HashMap;

use familiar_ui::anim::{self, Crossfade, Expand};
use familiar_ui::appearance::{self, AppearanceMode};
use familiar_ui::components::{
    Button, ButtonSize, HoverCard, Led, LedStatus, RunStatus, SectionHeader, SidebarItem, Skeleton, StatusChip, card,
    chip, divider, empty, group_label,
};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::notice::{NoticeChipIcon, notice_chip};
use familiar_ui::theme::{RADIUS_CARD, SIDEBAR_WIDTH, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::data::{self, Live, Teammate};

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Today,
    NeedsYou,
    Teammate(SharedString),
}

enum Load {
    Loading,
    Ready(Live),
    /// The API was unreachable; the shell falls back to sample teammates and says so.
    Failed(String),
}

pub struct Shell {
    load: Load,
    route: Crossfade<Route>,
    toasts: Entity<ToastStack>,
    scroll: ScrollHandle,
    side_scroll: ScrollHandle,
    expanded: HashMap<usize, Expand>,
    approved: bool,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        familiar_ui::observe_window(window, cx);
        let task = gpui_tokio::Tokio::spawn(cx, data::fetch_live());
        cx.spawn(async move |this, cx| {
            let load = match task.await {
                Ok(Ok(live)) => Load::Ready(live),
                Ok(Err(e)) => Load::Failed(format!("{e:#}")),
                Err(e) => Load::Failed(format!("{e:#}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.load = load;
                cx.notify();
            });
        })
        .detach();
        Self {
            load: Load::Loading,
            route: Crossfade::new(Route::Today),
            toasts: cx.new(|_| ToastStack::new()),
            scroll: ScrollHandle::new(),
            side_scroll: ScrollHandle::new(),
            expanded: (0..4).map(|i| (i, Expand::new(false))).collect(),
            approved: false,
        }
    }

    fn teammates(&self) -> Vec<Teammate> {
        match &self.load {
            Load::Ready(live) => live.teammates.clone(),
            Load::Failed(_) => data::sample_teammates(),
            Load::Loading => Vec::new(),
        }
    }

    fn pending(&self) -> usize {
        match &self.load {
            Load::Ready(live) => live.pending,
            Load::Failed(_) => usize::from(!self.approved),
            Load::Loading => 0,
        }
    }

    fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route.set(route) {
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let current = self.route.current().clone();
        let dark = theme.is_dark();
        let nav = |id: &'static str, label: &'static str, glyph: &'static str, route: Route, badge: usize| {
            let this = this.clone();
            let selected = current == route;
            SidebarItem::new(id, label).icon(glyph).selected(selected).badge(badge).on_click(move |_, _, cx| {
                this.update(cx, |shell, cx| shell.navigate(route.clone(), cx))
            })
        };
        let mut teammates = div().flex().flex_col().gap(px(2.0));
        match &self.load {
            Load::Loading => {
                for i in 0..4 {
                    teammates = teammates.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .h(px(48.0))
                            .px(px(8.0))
                            .child(Skeleton::new(30.0).width(30.0).radius(15.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.0))
                                    .child(Skeleton::new(10.0).width(90.0 - i as f32 * 8.0))
                                    .child(Skeleton::new(8.0).width(56.0)),
                            ),
                    );
                }
            }
            _ => {
                let list = self.teammates();
                if list.is_empty() {
                    teammates = teammates.child(
                        div().px(px(12.0)).text_size(px(text::SMALL)).text_color(theme.muted).child("No teammates yet."),
                    );
                }
                for (i, t) in list.into_iter().enumerate() {
                    let this = this.clone();
                    let route = Route::Teammate(t.id.clone());
                    let color = (t.state == MascotState::NeedsYou).then_some(theme.warn);
                    teammates = teammates.child(anim::stagger(
                        SharedString::from(format!("side-in-{}", t.id)),
                        i,
                        div().child(
                            SidebarItem::new(SharedString::from(format!("side-{}", t.id)), t.name.clone())
                                .leading(Mascot::new(format!("side-{}", t.id), t.avatar, t.state, 30.0))
                                .sublabel(t.state.label(), color)
                                .selected(current == route)
                                .on_click(move |_, _, cx| this.update(cx, |s, cx| s.navigate(route.clone(), cx))),
                        ),
                    ));
                }
            }
        }
        let pc_online = matches!(&self.load, Load::Ready(live) if live.pc_online);
        let this_toggle = cx.entity();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(16.0))
                    .pt(px(16.0))
                    .pb(px(12.0))
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.ink)
                            .child("Familiar"),
                    )
                    .child(
                        Button::icon_only("theme-toggle", if dark { icons::SUN } else { icons::MOON })
                            .tooltip(if dark { "Light appearance" } else { "Dark appearance" })
                            .on_click(move |_, _, cx| {
                                let next = if dark { AppearanceMode::Light } else { AppearanceMode::Dark };
                                appearance::set_mode(next, cx);
                                this_toggle.update(cx, |_, cx| cx.notify());
                            }),
                    ),
            )
            .child(
                edge_faded(
                    18.0,
                    true,
                    true,
                    div()
                        .id("side-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.side_scroll)
                        .px(px(8.0))
                        .flex()
                        .flex_col()
                        .gap(px(20.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(nav("nav-today", "Today", icons::HOME, Route::Today, 0))
                                .child(nav("nav-needs", "Needs you", icons::BELL, Route::NeedsYou, self.pending())),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .pr(px(4.0))
                                        .child(group_label("Teammates", cx))
                                        .child(
                                            Button::icon_only("new-teammate", icons::PLUS)
                                                .size(ButtonSize::Small)
                                                .tooltip("New teammate"),
                                        ),
                                )
                                .child(teammates),
                        ),
                )
                .fade_overflow_y(&self.side_scroll)
                .into_any_element(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p(px(8.0))
                    .border_t_1()
                    .border_color(theme.line)
                    .child(SidebarItem::new("nav-settings", "Settings").icon(icons::SETTINGS))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(14.0))
                            .h(px(36.0))
                            .text_size(px(text::SMALL))
                            .text_color(theme.ink)
                            .child(Led::new(if pc_online { LedStatus::Online } else { LedStatus::Offline }))
                            .child(if pc_online { "Computer online" } else { "Computer offline" }),
                    ),
            )
    }

    fn page(&mut self, route: &Route, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match route {
            Route::Today => self.today(window, cx).into_any_element(),
            Route::NeedsYou => self.needs_you(cx).into_any_element(),
            Route::Teammate(id) => self.teammate_page(id, cx).into_any_element(),
        }
    }

    fn today(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if matches!(self.load, Load::Loading) {
            return div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .child(Skeleton::new(36.0).width(240.0))
                .child(Skeleton::new(96.0).radius(RADIUS_CARD))
                .child(Skeleton::new(160.0).radius(RADIUS_CARD));
        }
        let teammates = self.teammates();
        let pending = self.pending();
        let working = teammates.iter().filter(|t| t.state == MascotState::Working).count();
        let subtitle = if pending > 0 {
            format!("{pending} thing{} waiting on you.", if pending == 1 { "" } else { "s" })
        } else if working > 0 {
            format!("{working} task{} in progress.", if working == 1 { "" } else { "s" })
        } else {
            "Everything is quiet.".to_owned()
        };
        let this = cx.entity();

        // Teammate strip.
        let mut strip = div().flex().gap(px(12.0)).flex_wrap();
        for (i, t) in teammates.iter().enumerate() {
            let route = Route::Teammate(t.id.clone());
            let this = this.clone();
            let status = if t.state == MascotState::NeedsYou { theme.warn } else { theme.muted };
            strip = strip.child(anim::stagger(
                SharedString::from(format!("strip-in-{}", t.id)),
                i,
                div().child(
                    HoverCard::new(SharedString::from(format!("strip-{}", t.id)))
                        .flat()
                        .padding(10.0)
                        .on_click(move |_, _, cx| this.update(cx, |s, cx| s.navigate(route.clone(), cx)))
                        .child(
                            div()
                                .w(px(84.0))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(6.0))
                                .child(Mascot::new(format!("strip-{}", t.id), t.avatar, t.state, 64.0))
                                .child(
                                    div()
                                        .text_size(px(text::SMALL))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.ink)
                                        .truncate()
                                        .child(t.name.clone()),
                                )
                                .child(div().text_size(px(text::CAPTION)).text_color(status).child(t.state.label())),
                        ),
                ),
            ));
        }
        strip = strip.child(anim::stagger(
            "strip-in-new",
            teammates.len(),
            div().child(
                HoverCard::new("strip-new").flat().padding(10.0).child(
                    div()
                        .w(px(84.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .size(px(64.0))
                                .rounded_full()
                                .border_2()
                                .border_dashed()
                                .border_color(theme.line)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(icons::PLUS).size(px(22.0)).text_color(theme.muted)),
                        )
                        .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("New")),
                ),
            ),
        ));

        let find = |state: MascotState| teammates.iter().find(|t| t.state == state).cloned();
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(32.0))
            .child(anim::appear(
                "today-head",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(px(text::DISPLAY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.ink)
                            .child(greeting()),
                    )
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(subtitle)),
            ))
            .when_some(
                match &self.load {
                    Load::Failed(e) => Some(e.clone()),
                    _ => None,
                },
                |el, e| {
                    el.child(anim::appear(
                        "today-offline",
                        div().child(notice_chip(
                            &theme,
                            true,
                            "Couldn't reach Familiar — showing sample teammates",
                            e,
                            NoticeChipIcon::Tile,
                        )),
                    ))
                },
            )
            .child(strip);

        // Needs you: an approval mock.
        if let Some(t) = find(MascotState::NeedsYou).filter(|_| !self.approved) {
            let approve = this.clone();
            let deny = this.clone();
            let toasts = self.toasts.clone();
            let toasts2 = self.toasts.clone();
            let name = t.name.clone();
            page = page.child(
                div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Needs you")).child(anim::appear(
                    "approval",
                    card(cx).p(px(16.0)).flex().gap(px(14.0)).items_start().children([
                        Mascot::new(format!("approval-{}", t.id), t.avatar, t.state, 40.0).into_any_element(),
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.0))
                                            .child(div().font_weight(FontWeight::MEDIUM).child(t.name.clone()))
                                            .child(chip(Tone::Warn, "wants to send an email", cx)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(text::SMALL))
                                            .text_color(theme.muted)
                                            .child("To design@ — \"Here's the weekly summary of the onboarding study…\""),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.0))
                                    .child(Button::new("approve", "Approve").primary().icon(icons::CHECK).on_click(
                                        move |_, _, cx| {
                                            let name = name.clone();
                                            approve.update(cx, |s, cx| {
                                                s.approved = true;
                                                cx.notify();
                                            });
                                            toasts.update(cx, |t, cx| {
                                                t.push(Tone::Ok, "Approved", Some(format!("{name} will send it now.").into()), cx)
                                            });
                                        },
                                    ))
                                    .child(Button::new("deny", "Not now").on_click(move |_, _, cx| {
                                        deny.update(cx, |s, cx| {
                                            s.approved = true;
                                            cx.notify();
                                        });
                                        toasts2.update(cx, |t, cx| t.push(Tone::Muted, "Declined", None, cx));
                                    })),
                            )
                            .into_any_element(),
                    ]),
                )),
            );
        }

        // Happening now.
        if let Some(t) = find(MascotState::Working) {
            page = page.child(
                div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Happening now")).child(anim::appear(
                    "now",
                    div().child(
                        HoverCard::new("now-card").child(
                            div()
                                .flex()
                                .gap(px(14.0))
                                .items_start()
                                .child(Mascot::new(format!("now-{}", t.id), t.avatar, t.state, 40.0))
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
                                                .child(div().font_weight(FontWeight::MEDIUM).child(t.name.clone()))
                                                .child(StatusChip::new(RunStatus::Running)),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(text::SMALL))
                                                .text_color(theme.muted)
                                                .truncate()
                                                .child("Triage new GitHub issues and draft replies"),
                                        )
                                        .child(
                                            div()
                                                .pt(px(4.0))
                                                .text_size(px(text::SMALL))
                                                .text_color(theme.ink)
                                                .child("Reading #482 \"Sidebar flickers on resize\" — looks like a duplicate of #455…"),
                                        ),
                                ),
                        ),
                    ),
                )),
            );
        }

        // Recently done — expandable rows.
        let done: [(&str, &str, RunStatus, &str); 4] = [
            ("Summarised yesterday's support inbox", "12 min ago", RunStatus::Succeeded, "18 threads read · 3 need a human · drafted 6 replies"),
            ("Updated the onboarding checklist in Notion", "1 h ago", RunStatus::Succeeded, "Added the SSO step and linked the new video"),
            ("Tried to deploy the docs site", "2 h ago", RunStatus::Failed, "Build failed: missing env var DOCS_TOKEN"),
            ("Weekly metrics digest", "Yesterday", RunStatus::Succeeded, "Signups +8% · churn flat · NPS 54"),
        ];
        let mut list = card(cx).flex().flex_col().overflow_hidden();
        for (i, (what, when, status, detail)) in done.into_iter().enumerate() {
            let who = teammates.get(i % teammates.len().max(1)).cloned();
            let exp = self.expanded.entry(i).or_insert_with(|| Expand::new(false));
            let openness = exp.openness();
            let this = this.clone();
            if i > 0 {
                list = list.child(divider(cx));
            }
            list = list.child(anim::stagger(
                ("done-in", i),
                i,
                div().flex().flex_col().child(
                    div()
                        .id(("done", i))
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .px(px(16.0))
                        .py(px(11.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.hover))
                        .on_click(move |_, _, cx| {
                            this.update(cx, |s, cx| {
                                if let Some(e) = s.expanded.get_mut(&i) {
                                    e.toggle();
                                }
                                cx.notify();
                            })
                        })
                        .when_some(who.clone(), |el, t| {
                            let state = if status == RunStatus::Succeeded { MascotState::Done } else { MascotState::Idle };
                            el.child(Mascot::new(format!("done-{i}"), t.avatar, state, 30.0))
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(what))
                                .child(
                                    div().text_size(px(text::CAPTION)).text_color(theme.muted).child(SharedString::from(
                                        format!("{} · {when}", who.map(|t| t.name).unwrap_or_default()),
                                    )),
                                ),
                        )
                        .child(StatusChip::new(status))
                        .child(
                            div()
                                .relative()
                                .top(px(-2.0 * openness))
                                .child(icon(icons::ALT_ARROW_DOWN).size(px(14.0)).text_color(theme.muted).opacity(0.5 + 0.5 * openness)),
                        ),
                ).child(exp.render(("done-detail", i), window, cx, div().pl(px(58.0)).pr(px(16.0)).pb(px(12.0)).text_size(px(text::SMALL)).text_color(theme.muted).child(detail))),
            ));
        }
        page = page.child(div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Recently done")).child(list));

        // Coming up.
        let upcoming = [("Morning briefing", "in 2h", Tone::Accent), ("Tidy the shared drive", "tomorrow", Tone::Muted)];
        let mut coming = card(cx).flex().flex_col();
        for (i, (what, when, tone)) in upcoming.into_iter().enumerate() {
            let who = teammates.get((i + 3) % teammates.len().max(1)).cloned();
            if i > 0 {
                coming = coming.child(divider(cx));
            }
            coming = coming.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(16.0))
                    .py(px(11.0))
                    .when_some(who.clone(), |el, t| el.child(Mascot::new(format!("up-{i}"), t.avatar, MascotState::Idle, 30.0)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(div().child(what))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(who.map(|t| t.name).unwrap_or_default())),
                    )
                    .child(chip(tone, when, cx)),
            );
        }
        page.child(div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Coming up")).child(coming))
    }

    fn needs_you(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .text_size(px(text::HEADLINE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.ink)
                    .child("Needs you"),
            )
            .child(anim::appear(
                "needs-empty",
                empty(
                    if self.pending() == 0 { "You're all caught up" } else { "Approvals live on Today for now" },
                    Some("When a teammate needs a decision, it shows up here.".into()),
                    cx,
                ),
            ))
    }

    fn teammate_page(&mut self, id: &SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Some(t) = self.teammates().into_iter().find(|t| &t.id == id) else {
            return div().child(empty("Teammate not found", None, cx));
        };
        div()
            .flex()
            .flex_col()
            .gap(px(24.0))
            .child(anim::appear(
                SharedString::from(format!("mate-head-{id}")),
                div()
                    .flex()
                    .items_center()
                    .gap(px(18.0))
                    .child(Mascot::new(format!("page-{}", t.id), t.avatar, t.state, 88.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(text::HEADLINE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.ink)
                                    .child(t.name.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.0))
                                    .child(chip(
                                        match t.state {
                                            MascotState::Working => Tone::Accent,
                                            MascotState::NeedsYou => Tone::Warn,
                                            MascotState::Done => Tone::Ok,
                                            _ => Tone::Muted,
                                        },
                                        t.state.label(),
                                        cx,
                                    ))
                                    .when_some(t.model.clone(), |el, m| el.child(chip(Tone::Muted, m, cx))),
                            ),
                    ),
            ))
            .child(anim::appear(
                SharedString::from(format!("mate-body-{id}")),
                empty("Chat comes next", Some("This page is a placeholder while the chat screen is ported.".into()), cx),
            ))
    }
}

/// The web's `greeting()`, by local hour.
fn greeting() -> &'static str {
    let h = local_hour();
    if h < 5 {
        "Still up?"
    } else if h < 12 {
        "Good morning"
    } else if h < 18 {
        "Good afternoon"
    } else {
        "Good evening"
    }
}

#[cfg(windows)]
fn local_hour() -> u16 {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    // SAFETY: GetLocalTime fills the caller-provided SYSTEMTIME and cannot fail.
    unsafe { GetLocalTime(&raw mut t) };
    t.hour
}

#[cfg(not(windows))]
fn local_hour() -> u16 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    ((secs / 3600) % 24) as u16
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        anim::frame(window);
        let theme = Theme::of(cx).clone();
        let sidebar = self.sidebar(cx).into_any_element();
        let page = self.render_route(window, cx);
        div()
            .size_full()
            .flex()
            .bg(theme.bg)
            .text_color(theme.ink)
            .text_size(px(text::BODY))
            .font_family(theme.font_sans.clone())
            .child(sidebar)
            .child(
                div().flex_1().min_w_0().h_full().child(
                    edge_faded(
                        24.0,
                        true,
                        true,
                        div()
                            .id("main-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(div().w_full().flex().justify_center().px(px(40.0)).py(px(36.0)).child(
                                div().w_full().max_w(px(760.0)).child(page),
                            )),
                    )
                    .fade_overflow_y(&self.scroll),
                ),
            )
            .child(self.toasts.clone())
    }
}

impl Shell {
    /// The route crossfade. The crossfade state is moved out while it renders so the page builders can borrow the
    /// shell mutably alongside the view context.
    fn render_route(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut route = std::mem::replace(&mut self.route, Crossfade::new(Route::Today));
        let reduced = familiar_ui::motion::reduced_motion(cx);
        let element = route.render("route", reduced, window, cx, |r, window, cx| self.page(r, window, cx));
        self.route = route;
        element
    }
}
