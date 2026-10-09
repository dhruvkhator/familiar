//! The shell's sidebar: Today, Needs you and Schedules, the teammates with their mascots and live status, Settings,
//! and whether the computer is online. Its own view, drawn cached by the shell: working teammates' mascots animate
//! here without redrawing the page beside it, and the page's live text doesn't redraw the sidebar.

use familiar_ui::anim;
use familiar_ui::appearance::{self, AppearanceMode};
use familiar_ui::components::{Button, ButtonSize, Led, LedStatus, SidebarItem, Skeleton, group_label};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons;
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{SIDEBAR_WIDTH, Theme, text};
use gpui::{
    App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, px,
};

use crate::data::{AppData, DataEvent, Part, Status};
use crate::shell::{Route, Shell};

pub struct Sidebar {
    data: Entity<AppData>,
    shell: WeakEntity<Shell>,
    /// The shell's page, for the selected row.
    route: Route,
    scroll: ScrollHandle,
}

impl Sidebar {
    pub fn new(data: Entity<AppData>, shell: WeakEntity<Shell>, cx: &mut Context<Self>) -> Self {
        // The teammates, their status (which waiting approvals feed) and the computer.
        cx.subscribe(&data, |_, _, ev: &DataEvent, cx| {
            if matches!(ev, DataEvent::Updated(Part::Overview | Part::Pending)) {
                cx.notify();
            }
        })
        .detach();
        Self { data, shell, route: Route::Today, scroll: ScrollHandle::new() }
    }

    pub fn set_route(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }

    fn navigate(shell: &WeakEntity<Shell>, route: Route, cx: &mut App) {
        let _ = shell.update(cx, |s, cx| s.navigate(route, cx));
    }
}

/// The data is still on its way (skeleton rows).
pub fn loading(data: &Entity<AppData>, cx: &App) -> bool {
    let d = data.read(cx);
    d.overview.is_none() && d.status == Status::Connecting
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("Sidebar");
        let theme = Theme::of(cx).clone();
        let shell = self.shell.clone();
        let current = self.route.clone();
        let dark = theme.is_dark();
        let pending = self.data.read(cx).pending.len();
        let nav = |id: &'static str, label: &'static str, glyph: &'static str, route: Route, badge: usize| {
            let shell = shell.clone();
            let selected = current == route;
            SidebarItem::new(id, label)
                .icon(glyph)
                .selected(selected)
                .badge(badge)
                .on_click(move |_, _, cx| Self::navigate(&shell, route.clone(), cx))
        };
        let mut teammates = div().flex().flex_col().gap(px(2.0));
        if loading(&self.data, cx) {
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
        } else {
            let list = self.data.read(cx).teammates();
            if list.is_empty() {
                teammates = teammates.child(
                    div().px(px(12.0)).text_size(px(text::SMALL)).text_color(theme.muted).child("No teammates yet."),
                );
            }
            for (i, t) in list.into_iter().enumerate() {
                let shell = shell.clone();
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
                            .on_click(move |_, _, cx| Self::navigate(&shell, route.clone(), cx)),
                    ),
                ));
            }
        }
        let pc_online = self.data.read(cx).pc_online();
        let this_toggle = cx.entity();
        let shell_new = self.shell.clone();
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
                    // Under the app's own title bar the sidebar already starts lower.
                    .pt(px(if crate::titlebar::CUSTOM { 4.0 } else { 16.0 }))
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
                                crate::prefs::update(|p| p.theme = next);
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
                        .track_scroll(&self.scroll)
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
                                .child(nav("nav-needs", "Needs you", icons::BELL, Route::NeedsYou, pending))
                                .child(nav("nav-schedules", "Schedules", icons::CALENDAR, Route::Schedules, 0)),
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
                                                .tooltip("New teammate")
                                                .on_click(move |_, _, cx| Self::navigate(&shell_new, Route::NewTeammate, cx)),
                                        ),
                                )
                                .child(teammates),
                        ),
                )
                .fade_overflow_y(&self.scroll),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p(px(8.0))
                    .border_t_1()
                    .border_color(theme.line)
                    .child(nav("nav-settings", "Settings", icons::SETTINGS, Route::Settings, 0))
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
}
