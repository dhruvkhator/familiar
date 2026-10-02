//! `familiar-native --gallery`: every component, both palettes, the mascot in every state and the motion
//! primitives, with sample data — the review surface for the look. Sections switch with the same route crossfade
//! the app uses; `--section <name>` opens one directly (overview, controls, mascot, surfaces, motion, feedback,
//! icons).

use familiar_ui::anim::{self, Crossfade, Expand};
use familiar_ui::appearance::{self, AppearanceMode};
use familiar_ui::components::{
    AvatarRow, Button, ButtonSize, HoverCard, Led, LedStatus, RunStatus, SectionHeader, Segmented, SidebarItem,
    Skeleton, StatusChip, Switch, badge, card, chip, divider, empty, group_label, tooltip_text,
};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Accessory, Avatar, Mascot, MascotState, PALETTE};
use familiar_ui::motion::{self, ReduceMotion};
use familiar_ui::notice::{NoticeChipIcon, notice_chip};
use familiar_ui::theme::{RADIUS_CARD, Theme, Tone, text, to_hex};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::data::{self, Teammate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Overview,
    Controls,
    Mascot,
    Surfaces,
    Motion,
    Feedback,
    Icons,
}

impl Section {
    const ALL: [Self; 7] =
        [Self::Overview, Self::Controls, Self::Mascot, Self::Surfaces, Self::Motion, Self::Feedback, Self::Icons];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Palette & type",
            Self::Controls => "Controls",
            Self::Mascot => "Mascot",
            Self::Surfaces => "Cards & lists",
            Self::Motion => "Motion",
            Self::Feedback => "Feedback",
            Self::Icons => "Icons",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Overview => icons::STAR,
            Self::Controls => icons::WIDGET,
            Self::Mascot => icons::MAGIC_STICK_3,
            Self::Surfaces => icons::LIST,
            Self::Motion => icons::REFRESH,
            Self::Feedback => icons::BELL,
            Self::Icons => icons::EYE,
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Self::Overview => "The web token table in both appearances, and the Geist type scale.",
            Self::Controls => "Buttons, segmented controls, switches, chips, badges and status LEDs.",
            Self::Mascot => "Every state, shape, accessory and size — rasterised from the web artwork.",
            Self::Surfaces => "Cards that lift, sidebar rows with a sliding selection, rows, empty and loading states.",
            Self::Motion => "Appear, stagger, crossfade, expand, springs and the working pulse.",
            Self::Feedback => "Notices, toasts and tooltips.",
            Self::Icons => "The bundled Solar (Linear) set.",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "overview" | "palette" => Self::Overview,
            "controls" => Self::Controls,
            "mascot" => Self::Mascot,
            "surfaces" | "cards" => Self::Surfaces,
            "motion" => Self::Motion,
            "feedback" => Self::Feedback,
            "icons" => Self::Icons,
            _ => return None,
        })
    }
}

pub struct Gallery {
    section: Crossfade<Section>,
    scroll: ScrollHandle,
    toasts: Entity<ToastStack>,
    teammates: Vec<Teammate>,
    replay: u32,
    tab: Crossfade<usize>,
    expands: Vec<Expand>,
    demo_nav: usize,
    demo_seg: usize,
    demo_switch: bool,
}

impl Gallery {
    pub fn new(section: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        familiar_ui::observe_window(window, cx);
        Self {
            section: Crossfade::new(section.and_then(Section::parse).unwrap_or(Section::Overview)),
            scroll: ScrollHandle::new(),
            toasts: cx.new(|_| ToastStack::new()),
            teammates: data::sample_teammates(),
            replay: 0,
            tab: Crossfade::new(0),
            expands: vec![Expand::new(true), Expand::new(false), Expand::new(false)],
            demo_nav: 0,
            demo_seg: 1,
            demo_switch: true,
        }
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let current = *self.section.current();
        let mut nav = div().flex().flex_col().gap(px(2.0)).px(px(8.0));
        for s in Section::ALL {
            let this = this.clone();
            nav = nav.child(
                SidebarItem::new(SharedString::from(format!("gal-{s:?}")), s.label())
                    .icon(s.icon())
                    .selected(current == s)
                    .on_click(move |_, _, cx| {
                        this.update(cx, |g, cx| {
                            if g.section.set(s) {
                                g.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                                cx.notify();
                            }
                        })
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(232.0))
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(16.0))
                    .pt(px(18.0))
                    .pb(px(16.0))
                    .child(Mascot::new("gallery-logo", self.teammates[0].avatar, MascotState::Idle, 28.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).child("Familiar"))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Design gallery")),
                    ),
            )
            .child(nav)
    }

    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let section = *self.section.current();
        let mode = appearance::mode(cx);
        let selected = AppearanceMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
        let reduced = motion::preference(cx) == ReduceMotion::On;
        let this = cx.entity();
        div()
            .flex()
            .items_center()
            .gap(px(16.0))
            .px(px(32.0))
            .h(px(64.0))
            .flex_none()
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.bg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child(section.label()))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).truncate().child(section.blurb())),
            )
            .child(
                Segmented::new(
                    "appearance",
                    AppearanceMode::ALL.iter().map(|m| (SharedString::from(m.label()), Some(m.icon()))).collect(),
                    selected,
                )
                .segment_width(84.0)
                .on_select(|i, _, cx| appearance::set_mode(AppearanceMode::ALL[i], cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .child("Reduce motion")
                    .child(Switch::new("reduce-motion", reduced).on_toggle(move |on, _, cx| {
                        motion::set_preference(if on { ReduceMotion::On } else { ReduceMotion::System }, cx);
                        this.update(cx, |_, cx| cx.notify());
                    })),
            )
    }

    fn page(&mut self, section: &Section, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match section {
            Section::Overview => self.overview(cx).into_any_element(),
            Section::Controls => self.controls(cx).into_any_element(),
            Section::Mascot => self.mascots(cx).into_any_element(),
            Section::Surfaces => self.surfaces(cx).into_any_element(),
            Section::Motion => self.motion(window, cx).into_any_element(),
            Section::Feedback => self.feedback(cx).into_any_element(),
            Section::Icons => self.icon_sheet(cx).into_any_element(),
        }
    }

    // --- Overview ---------------------------------------------------------------------------------------------

    fn overview(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let palette = |t: Theme, title: &'static str| {
            let mut grid = div().flex().flex_wrap().gap(px(12.0));
            for (name, color) in t.tokens() {
                grid = grid.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .w(px(84.0))
                        .child(div().h(px(44.0)).rounded(px(10.0)).bg(color).border_1().border_color(t.line))
                        .child(div().text_size(px(text::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(t.ink).child(name))
                        .child(
                            div()
                                .text_size(px(text::MICRO))
                                .font_family(t.font_mono.clone())
                                .text_color(t.muted)
                                .child(SharedString::from(format!("#{:06x}", to_hex(color)))),
                        ),
                );
            }
            // A miniature of the app in that palette: a card with a teammate row, chips and a primary button.
            let mini = div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .p(px(14.0))
                .rounded(px(RADIUS_CARD))
                .bg(t.surface)
                .border_1()
                .border_color(t.line)
                .shadow(t.card_shadow(0.0))
                .child(Mascot::new(format!("pal-{title}"), self.teammates[1].avatar, MascotState::NeedsYou, 40.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).text_color(t.ink).child("Milo"))
                        .child(div().text_size(px(text::CAPTION)).text_color(t.warn).child("Needs you")),
                )
                .child(
                    div()
                        .px(px(7.0))
                        .h(px(20.0))
                        .flex()
                        .items_center()
                        .rounded(px(6.0))
                        .bg(t.accent_soft)
                        .text_color(t.accent)
                        .text_size(px(text::CAPTION))
                        .font_weight(FontWeight::MEDIUM)
                        .child("running"),
                )
                .child(
                    div()
                        .px(px(12.0))
                        .h(px(30.0))
                        .flex()
                        .items_center()
                        .rounded(px(10.0))
                        .bg(t.accent)
                        .text_color(t.accent_ink)
                        .text_size(px(text::SMALL))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Approve"),
                );
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(340.0))
                .gap(px(16.0))
                .p(px(20.0))
                .rounded(px(RADIUS_CARD))
                .bg(t.bg)
                .border_1()
                .border_color(t.line)
                .text_color(t.ink)
                .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child(title))
                .child(grid)
                .child(mini)
        };
        let ramp = [
            ("Display · 28 semibold", text::DISPLAY, FontWeight::SEMIBOLD, "Good afternoon"),
            ("Headline · 22 semibold", text::HEADLINE, FontWeight::SEMIBOLD, "Meet your first teammate"),
            ("Title · 17 semibold", text::TITLE, FontWeight::SEMIBOLD, "Recently done"),
            ("Lead · 15", text::LEAD, FontWeight::NORMAL, "3 things waiting on you."),
            ("Body · 14", text::BODY, FontWeight::NORMAL, "Summarised yesterday's support inbox and drafted six replies."),
            ("Small · 13 medium", text::SMALL, FontWeight::MEDIUM, "Ada · Working"),
            ("Caption · 12", text::CAPTION, FontWeight::NORMAL, "12 min ago"),
        ];
        let mut type_card = card(cx).p(px(20.0)).flex().flex_col().gap(px(14.0));
        for (i, (label, size, weight, sample)) in ramp.into_iter().enumerate() {
            type_card = type_card.child(anim::stagger(
                ("type", i),
                i,
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(20.0))
                    .child(div().w(px(170.0)).flex_none().text_size(px(text::CAPTION)).text_color(theme.muted).child(label))
                    .child(div().text_size(px(size)).font_weight(weight).text_color(theme.ink).child(sample)),
            ));
        }
        type_card = type_card.child(divider(cx)).child(
            div()
                .flex()
                .items_baseline()
                .gap(px(20.0))
                .child(div().w(px(170.0)).flex_none().text_size(px(text::CAPTION)).text_color(theme.muted).child("Mono · Geist Mono 13"))
                .child(
                    div()
                        .text_size(px(text::SMALL))
                        .font_family(theme.font_mono.clone())
                        .text_color(theme.ink)
                        .child("cargo run -p familiar-native -- --gallery"),
                ),
        );
        div()
            .flex()
            .flex_col()
            .gap(px(28.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(16.0))
                    .child(anim::appear("pal-light", palette(Theme::light(), "Light")))
                    .child(anim::appear("pal-dark", palette(Theme::dark(), "Dark"))),
            )
            .child(div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Type scale")).child(type_card))
    }

    // --- Controls ---------------------------------------------------------------------------------------------

    fn controls(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let this2 = cx.entity();
        let toasts = self.toasts.clone();
        let buttons = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                row()
                    .child(Button::new("b-primary", "Create a teammate").primary().icon(icons::PLUS).on_click(
                        move |_, _, cx| {
                            toasts.update(cx, |t, cx| t.push(Tone::Ok, "Teammate created", Some("Say hello to Pip.".into()), cx))
                        },
                    ))
                    .child(Button::new("b-secondary", "Open chat").icon(icons::CHAT_ROUND_LINE))
                    .child(Button::new("b-ghost", "Skip for now").ghost())
                    .child(Button::new("b-danger", "Delete").danger()),
            )
            .child(
                row()
                    .child(Button::new("b-sm", "Small").size(ButtonSize::Small))
                    .child(Button::new("b-md", "Medium"))
                    .child(Button::new("b-lg", "Large").primary().size(ButtonSize::Large))
                    .child(Button::new("b-disabled", "Disabled").primary().disabled(true))
                    .child(Button::new("b-disabled2", "Disabled").disabled(true)),
            )
            .child(
                row()
                    .child(Button::icon_only("i-search", icons::MAGNIFER).tooltip("Search"))
                    .child(Button::icon_only("i-bell", icons::BELL).tooltip("Notifications"))
                    .child(Button::icon_only("i-settings", icons::SETTINGS).tooltip("Settings"))
                    .child(Button::icon_only("i-moon", icons::MOON).tooltip("Dark appearance"))
                    .child(Button::icon_only("i-plus", icons::PLUS).secondary().tooltip("New teammate")),
            );
        let seg = Segmented::new(
            "demo-seg",
            vec![("Day".into(), None), ("Week".into(), None), ("Month".into(), None)],
            self.demo_seg,
        )
        .on_select(move |i, _, cx| {
            this.update(cx, |g, cx| {
                g.demo_seg = i;
                cx.notify();
            })
        });
        let switch = Switch::new("demo-switch", self.demo_switch).on_toggle(move |on, _, cx| {
            this2.update(cx, |g, cx| {
                g.demo_switch = on;
                cx.notify();
            })
        });
        let tones = [Tone::Muted, Tone::Accent, Tone::Ok, Tone::Warn, Tone::Bad];
        let mut chips = row();
        for (tone, label) in tones.into_iter().zip(["muted", "accent", "ok", "warn", "bad"]) {
            chips = chips.child(chip(tone, label, cx));
        }
        let mut statuses = row();
        for s in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::WaitingApproval,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            statuses = statuses.child(StatusChip::new(s));
        }
        let mut badges = row();
        for n in [1, 3, 12, 128] {
            badges = badges.children(badge(n, cx));
        }
        let mut leds = row().gap(px(18.0));
        for (s, label) in [
            (LedStatus::Idle, "idle"),
            (LedStatus::Running, "running"),
            (LedStatus::Paused, "paused"),
            (LedStatus::Online, "online"),
            (LedStatus::Offline, "offline"),
        ] {
            leds = leds.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::SMALL))
                    .child(Led::new(s))
                    .child(label),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(panel("Buttons", "Hover washes fade over 150 ms; press nudges half a pixel. Icon buttons carry tooltips.", buttons, cx))
            .child(panel(
                "Segmented & switch",
                "The selection pill and the knob ride springs.",
                row().gap(px(20.0)).child(seg).child(switch),
                cx,
            ))
            .child(panel(
                "Chips, status, badges, LEDs",
                "Tones from the web; the running chip and LED pulse on the shared 30 fps clock.",
                div().flex().flex_col().gap(px(14.0)).child(chips).child(statuses).child(badges).child(leds),
                cx,
            ))
    }

    // --- Mascot -----------------------------------------------------------------------------------------------

    fn mascots(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut header = div().flex().gap(px(8.0)).pl(px(96.0));
        for s in MascotState::ALL {
            header = header.child(
                div()
                    .w(px(96.0))
                    .flex()
                    .justify_center()
                    .text_size(px(text::CAPTION))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if s == MascotState::NeedsYou { theme.warn } else { theme.muted })
                    .child(s.label()),
            );
        }
        let mut grid = div().flex().flex_col().gap(px(10.0)).child(header);
        for (r, t) in self.teammates.iter().take(4).enumerate() {
            let mut line = div().flex().items_center().gap(px(8.0)).child(
                div().w(px(96.0)).text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(t.name.clone()),
            );
            for (c, s) in MascotState::ALL.into_iter().enumerate() {
                line = line.child(
                    div()
                        .w(px(96.0))
                        .h(px(84.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(anim::stagger(("m-in", r * 5 + c), r + c, div().child(Mascot::new(format!("grid-{r}-{c}"), t.avatar, s, 64.0)))),
                );
            }
            grid = grid.child(line);
        }
        let mut accessories = row().gap(px(18.0));
        for (i, a) in Accessory::ALL.into_iter().enumerate() {
            let avatar = Avatar { shape: (i % 5) as u8, color: PALETTE[i % 6], eyes: 0, mouth: 0, accessory: a };
            accessories = accessories.child(tile(
                Mascot::new(format!("acc-{i}"), avatar, MascotState::Idle, 54.0),
                format!("{a:?}").to_lowercase(),
                cx,
            ));
        }
        let mut faces = row().gap(px(18.0));
        for i in 0..5u8 {
            let avatar = Avatar { shape: i, color: PALETTE[(i as usize + 2) % 6], eyes: i, mouth: i % 4, accessory: Accessory::None };
            faces = faces.child(tile(Mascot::new(format!("face-{i}"), avatar, MascotState::Idle, 54.0), format!("shape {i} · eyes {i}"), cx));
        }
        let mut sizes = row().gap(px(18.0)).items_end();
        for s in [24.0, 30.0, 40.0, 54.0, 72.0, 96.0] {
            sizes = sizes.child(tile(Mascot::new(format!("size-{s}"), self.teammates[2].avatar, MascotState::Idle, s), format!("{s} px"), cx));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(panel(
                "States",
                "Working bobs and glances; needs-you tilts with the amber ring; done smiles; paused desaturates; idle blinks.",
                grid,
                cx,
            ))
            .child(panel("Accessories", "All seven, on the five body shapes.", accessories, cx))
            .child(panel("Shapes & faces", "Eye and mouth styles from the avatar builder.", faces, cx))
            .child(panel("Sizes", "Rasterised at device pixels per size, so every size stays crisp.", sizes, cx))
    }

    // --- Surfaces ---------------------------------------------------------------------------------------------

    fn surfaces(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let mut cards = row().gap(px(16.0)).items_start();
        cards = cards.child(
            card(cx)
                .w(px(220.0))
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(div().font_weight(FontWeight::MEDIUM).child("Resting card"))
                .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("Surface, hairline, soft shadow.")),
        );
        for (i, t) in self.teammates.iter().take(3).enumerate() {
            cards = cards.child(
                div().w(px(220.0)).child(
                    HoverCard::new(("lift", i)).child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(AvatarRow::new(format!("lift-{i}"), t.avatar, t.name.clone(), t.state).size(36.0))
                            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("Hover me — I lift on a spring.")),
                    ),
                ),
            );
        }
        let navs = [("Today", icons::HOME, 0usize), ("Needs you", icons::BELL, 2), ("Settings", icons::SETTINGS, 0)];
        let mut side = div()
            .w(px(256.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .p(px(8.0))
            .rounded(px(RADIUS_CARD))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.line);
        for (i, (label, glyph, n)) in navs.into_iter().enumerate() {
            let this = this.clone();
            side = side.child(SidebarItem::new(("demo-nav", i), label).icon(glyph).badge(n).selected(self.demo_nav == i).on_click(
                move |_, _, cx| {
                    this.update(cx, |g, cx| {
                        g.demo_nav = i;
                        cx.notify();
                    })
                },
            ));
        }
        side = side.child(div().h(px(10.0))).child(group_label("Teammates", cx)).child(div().h(px(4.0)));
        for (i, t) in self.teammates.iter().take(4).enumerate() {
            let this = this.clone();
            let idx = 10 + i;
            side = side.child(
                SidebarItem::new(("demo-mate", i), t.name.clone())
                    .leading(Mascot::new(format!("demo-mate-{i}"), t.avatar, t.state, 30.0))
                    .sublabel(t.state.label(), (t.state == MascotState::NeedsYou).then_some(theme.warn))
                    .selected(self.demo_nav == idx)
                    .on_click(move |_, _, cx| {
                        this.update(cx, |g, cx| {
                            g.demo_nav = idx;
                            cx.notify();
                        })
                    }),
            );
        }
        let mut rows = card(cx).flex().flex_col().flex_1().min_w(px(300.0));
        for (i, t) in self.teammates.iter().take(4).enumerate() {
            if i > 0 {
                rows = rows.child(divider(cx));
            }
            rows = rows.child(
                div()
                    .id(("row", i))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .hover(|s| s.bg(theme.hover))
                    .child(div().flex_1().child(AvatarRow::new(format!("row-{i}"), t.avatar, t.name.clone(), t.state)))
                    .child(StatusChip::new(match t.state {
                        MascotState::Working => RunStatus::Running,
                        MascotState::NeedsYou => RunStatus::WaitingApproval,
                        MascotState::Done => RunStatus::Succeeded,
                        _ => RunStatus::Queued,
                    })),
            );
        }
        let loading = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(300.0))
            .gap(px(10.0))
            .child(Skeleton::new(28.0).width(200.0))
            .child(Skeleton::new(64.0).radius(RADIUS_CARD))
            .child(Skeleton::new(64.0).radius(RADIUS_CARD))
            .child(Skeleton::new(14.0).width(260.0));
        div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(panel("Cards", "Resting, and hover-lift (2 px rise + deeper shadow on a spring).", cards, cx))
            .child(panel(
                "Sidebar & rows",
                "Selection wash and the accent indicator spring in; hover washes fade.",
                row().gap(px(20.0)).items_start().child(side).child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap(px(12.0))
                        .child(
                            SectionHeader::new("Teammates")
                                .count(self.teammates.len())
                                .action(Button::new("see-all", "See all").ghost().size(ButtonSize::Small)),
                        )
                        .child(rows),
                ),
                cx,
            ))
            .child(panel(
                "Empty & loading",
                "The dashed empty panel, and skeletons with a soft shimmer sweep.",
                row()
                    .gap(px(20.0))
                    .items_start()
                    .child(div().flex_1().min_w(px(300.0)).child(empty(
                        "No teammates yet",
                        Some("Name one, give it a look and a job.".into()),
                        cx,
                    )))
                    .child(loading),
                cx,
            ))
    }

    // --- Motion -----------------------------------------------------------------------------------------------

    fn motion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let replay = self.replay;
        let mut list = div().flex().flex_col().gap(px(6.0));
        for (i, t) in self.teammates.iter().enumerate() {
            list = list.child(anim::stagger(
                SharedString::from(format!("stagger-{replay}-{i}")),
                i,
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(10.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.line)
                    .child(AvatarRow::new(format!("stag-{i}"), t.avatar, t.name.clone(), t.state)),
            ));
        }
        let replay = {
            let this = this.clone();
            Button::new("replay", "Replay").icon(icons::REFRESH).size(ButtonSize::Small).on_click(move |_, _, cx| {
                this.update(cx, |g, cx| {
                    g.replay += 1;
                    cx.notify();
                })
            })
        };

        // Crossfade demo.
        let tab_index = *self.tab.current();
        let tabs = {
            let this = this.clone();
            Segmented::new(
                "xfade-tabs",
                vec![("Today".into(), Some(icons::HOME)), ("Inbox".into(), Some(icons::BELL)), ("Notes".into(), Some(icons::PEN))],
                tab_index,
            )
            .segment_width(92.0)
            .on_select(move |i, _, cx| {
                this.update(cx, |g, cx| {
                    g.tab.set(i);
                    cx.notify();
                })
            })
        };
        let teammates = self.teammates.clone();
        let reduced = motion::reduced_motion(cx);
        let mut tab = std::mem::replace(&mut self.tab, Crossfade::new(0));
        let xfade = tab.render("xfade", reduced, window, cx, |i, _, cx| {
            let theme = Theme::of(cx).clone();
            let (title, body, mate) = match i {
                0 => ("Good afternoon", "3 things waiting on you.", &teammates[0]),
                1 => ("Needs you", "Milo wants to send an email to design@.", &teammates[1]),
                _ => ("Notes", "Juniper finished the onboarding checklist.", &teammates[2]),
            };
            card(cx)
                .h(px(120.0))
                .p(px(18.0))
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(Mascot::new(format!("xf-{i}"), mate.avatar, mate.state, 64.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(theme.muted).child(body)),
                )
                .into_any_element()
        });
        self.tab = tab;

        // Expand demo.
        let items = [
            ("What does Ada do?", "Ada triages new GitHub issues every morning, labels them, and drafts replies for anything that looks like a duplicate."),
            ("When does Milo ask first?", "Before anything leaves your computer — emails, posts, payments — Milo pauses and asks. You can change that in Rules."),
            ("Can teammates share notes?", "Yes. Memory is per teammate, but you can promote a note to the shared space from any chat."),
        ];
        let mut expands = card(cx).flex().flex_col().overflow_hidden();
        for (i, (q, a)) in items.into_iter().enumerate() {
            let openness = self.expands[i].openness();
            let this = this.clone();
            if i > 0 {
                expands = expands.child(divider(cx));
            }
            expands = expands.child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(("exp", i))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(16.0))
                            .h(px(46.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.hover))
                            .on_click(move |_, _, cx| {
                                this.update(cx, |g, cx| {
                                    g.expands[i].toggle();
                                    cx.notify();
                                })
                            })
                            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(q))
                            .child(
                                // gpui divs can't rotate at this rev: swap chevrons with a crossfade instead.
                                div()
                                    .relative()
                                    .size(px(16.0))
                                    .child(icon(icons::ALT_ARROW_RIGHT).absolute().size(px(16.0)).text_color(theme.muted).opacity(1.0 - openness))
                                    .child(icon(icons::ALT_ARROW_DOWN).absolute().size(px(16.0)).text_color(theme.accent).opacity(openness)),
                            ),
                    )
                    .child(self.expands[i].render(
                        ("exp-body", i),
                        window,
                        cx,
                        div().px(px(16.0)).pb(px(14.0)).text_size(px(text::SMALL)).text_color(theme.muted).child(a),
                    )),
            );
        }

        let pulses = row()
            .gap(px(24.0))
            .child(tile(Led::new(LedStatus::Running).size(10.0), "LED", cx))
            .child(tile(StatusChip::new(RunStatus::Running), "chip", cx))
            .child(tile(Mascot::new("pulse-mascot", self.teammates[0].avatar, MascotState::Working, 54.0), "mascot bob", cx))
            .child(tile(div().w(px(120.0)).child(Skeleton::new(14.0)), "shimmer", cx));

        div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                row()
                    .gap(px(20.0))
                    .items_start()
                    .child(div().flex_1().min_w(px(320.0)).child(panel(
                        "Appear & stagger",
                        "Fade + 6 px rise (420 ms expo-out), 45 ms per row.",
                        div().flex().flex_col().gap(px(12.0)).child(replay).child(list),
                        cx,
                    )))
                    .child(div().flex_1().min_w(px(320.0)).child(panel(
                        "Crossfade",
                        "Route contents: the old page leaves fast, the new one fades and rises in.",
                        div().flex().flex_col().gap(px(14.0)).child(tabs).child(xfade),
                        cx,
                    ))),
            )
            .child(panel("Expand", "Measured-height tween (260 ms quint-out), then auto height.", expands, cx))
            .child(panel(
                "Working pulse",
                "One shared 30 fps clock for every pulse; parks itself when nothing is working.",
                pulses,
                cx,
            ))
            .child(
                div()
                    .text_size(px(text::CAPTION))
                    .text_color(theme.muted)
                    .child("Springs live on Controls (segmented, switch) and Cards & lists (hover lift, sidebar selection). Toggle Reduce motion to see everything settle instantly."),
            )
    }

    // --- Feedback ---------------------------------------------------------------------------------------------

    fn feedback(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let fire = |id: &'static str, label: &'static str, tone: Tone, title: &'static str, body: &'static str| {
            let toasts = self.toasts.clone();
            Button::new(id, label).on_click(move |_, _, cx| {
                toasts.update(cx, |t, cx| t.push(tone, title, Some(body.into()), cx));
            })
        };
        let toasts = row()
            .child(fire("t-ok", "Success toast", Tone::Ok, "Saved", "Ada's schedule now runs every weekday at 9:00."))
            .child(fire("t-bad", "Error toast", Tone::Bad, "Couldn't connect", "Familiar's server isn't answering on 47080."))
            .child(fire("t-info", "Info toast", Tone::Accent, "Juniper finished", "The onboarding checklist is up to date."));
        let notices = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(notice_chip(&theme, true, "Scheduled work paused", "Subscription limit reached — chats still run. Resumes at 14:00.", NoticeChipIcon::Plain))
            .child(notice_chip(
                &theme,
                false,
                "Run failed",
                "exit status 1: error: missing environment variable DOCS_TOKEN (needed by scripts/deploy-docs.ps1)",
                NoticeChipIcon::Tile,
            ));
        let tips = row()
            .child(
                div()
                    .id("tip-a")
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_dashed()
                    .border_color(theme.line)
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .tooltip(tooltip_text("Tooltips fade in on the inverted plate"))
                    .child("Hover for a tooltip"),
            )
            .child(Button::icon_only("tip-b", icons::COPY).tooltip("Copy message"));
        div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(panel("Toasts", "Bottom-right; fade + 8 px rise in, quicker fade out; ~4 s each, four at most.", toasts, cx))
            .child(panel("Notices", "Tinted, wrapping failure chips (vendored from zeron) with a copy button.", notices, cx))
            .child(panel("Tooltips", "Themed, with a quick fade.", tips, cx))
    }

    // --- Icons ------------------------------------------------------------------------------------------------

    fn icon_sheet(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut grid = div().flex().flex_wrap().gap(px(10.0));
        for (i, path) in icons::ALL.iter().enumerate() {
            let name = path.trim_start_matches("icons/").trim_end_matches(".svg");
            grid = grid.child(anim::stagger(
                ("icon-in", i),
                i / 6,
                div()
                    .id(("icon", i))
                    .w(px(104.0))
                    .h(px(78.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.line)
                    .hover(|s| s.bg(theme.hover))
                    .child(icon(path).size(px(22.0)).text_color(theme.ink))
                    .child(div().text_size(px(text::MICRO)).text_color(theme.muted).child(name)),
            ));
        }
        div().flex().flex_col().gap(px(12.0)).child(grid).child(
            div()
                .text_size(px(text::CAPTION))
                .text_color(theme.muted)
                .child("Solar Icons (Linear) by 480 Design, CC BY 4.0 — plus a few zeron hand-drawn ports in the same style."),
        )
    }

    fn render_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut section = std::mem::replace(&mut self.section, Crossfade::new(Section::Overview));
        let reduced = motion::reduced_motion(cx);
        let element = section.render("section", reduced, window, cx, |s, window, cx| self.page(s, window, cx));
        self.section = section;
        element
    }
}

fn row() -> Div {
    div().flex().flex_wrap().items_center().gap(px(10.0))
}

/// A labelled gallery panel.
fn panel(title: &'static str, description: &'static str, content: impl IntoElement, cx: &mut Context<Gallery>) -> Div {
    let theme = Theme::of(cx).clone();
    card(cx)
        .p(px(20.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child(title))
                .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(description)),
        )
        .child(content)
}

/// A specimen with a caption underneath.
fn tile(content: impl IntoElement, caption: impl Into<SharedString>, cx: &mut Context<Gallery>) -> Div {
    let theme = Theme::of(cx);
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .child(div().min_h(px(40.0)).flex().items_center().child(content))
        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(caption.into()))
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        anim::frame(window);
        let theme = Theme::of(cx).clone();
        let sidebar = self.sidebar(cx).into_any_element();
        let top = self.top_bar(cx).into_any_element();
        let page = self.render_section(window, cx);
        div()
            .size_full()
            .flex()
            .bg(theme.bg)
            .text_color(theme.ink)
            .text_size(px(text::BODY))
            .font_family(theme.font_sans.clone())
            .child(sidebar)
            .child(
                div().flex_1().min_w_0().h_full().flex().flex_col().child(top).child(
                    div().flex_1().min_h_0().child(
                        edge_faded(
                            24.0,
                            true,
                            true,
                            div()
                                .id("gallery-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .child(div().w_full().px(px(32.0)).pt(px(24.0)).pb(px(48.0)).child(page)),
                        )
                        .fade_overflow_y(&self.scroll),
                    ),
                ),
            )
            .child(self.toasts.clone())
    }
}
