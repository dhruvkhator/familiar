//! Themed text fields over gpui-base's unstyled inputs: multi-line (`Textarea`) and single-line (`Input`).

use familiar_ui::theme::{RADIUS_CONTROL, Theme, text};
use gpui::{
    App, AppContext as _, ElementId, Focusable as _, Entity, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Styled as _, Window, div, px,
};
use gpui_base::input::{InputBase, InputBaseState, InputEditorStyle, InputModeKind, InputState, TextareaState};
/// A new field. `submit_on_enter`: Enter emits `PressEnter` instead of a newline (Shift+Enter still breaks).
pub fn new_field(
    placeholder: impl Into<gpui::SharedString>,
    submit_on_enter: bool,
    max_rows: usize,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TextareaState> {
    let placeholder = placeholder.into();
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .submit_on_enter(submit_on_enter)
            .auto_grow(1, max_rows)
    })
}

/// A single-line field (`masked`: a password).
pub fn new_line(placeholder: impl Into<gpui::SharedString>, masked: bool, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let placeholder = placeholder.into();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked))
}

/// A compact one-line field with a leading icon, as tall as the toolbar's other controls (32 px): search boxes.
pub fn compact_field(
    id: impl Into<ElementId>,
    state: &Entity<InputState>,
    glyph: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let theme = Theme::of(cx).clone();
    state.update(cx, |s, _| {
        s.set_editor_style(InputEditorStyle {
            foreground: theme.ink,
            muted_foreground: theme.muted,
            selection: theme.accent_soft,
            caret: theme.accent,
            ..InputEditorStyle::default()
        })
    });
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    let focus = state.clone();
    InputBase::new(id)
        .focused(focused)
        .w_full()
        .h(px(32.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .rounded(px(RADIUS_CONTROL))
        .border_1()
        .border_color(if focused { theme.focus } else { theme.line })
        .bg(theme.surface)
        .text_size(px(text::SMALL))
        .text_color(theme.ink)
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.update(cx, |s, cx| s.focus(window, cx)))
        .child(familiar_ui::icons::icon(glyph).size(px(15.0)).text_color(theme.muted))
        .child(div().flex_1().min_w_0().child(state.clone()))
}

/// Draw `state` as a field (`min_h`: the resting height).
pub fn field<M: InputModeKind>(
    id: impl Into<ElementId>,
    state: &Entity<InputBaseState<M>>,
    min_h: f32,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let theme = Theme::of(cx).clone();
    state.update(cx, |s, _| {
        s.set_editor_style(InputEditorStyle {
            foreground: theme.ink,
            muted_foreground: theme.muted,
            selection: theme.accent_soft,
            caret: theme.accent,
            ..InputEditorStyle::default()
        })
    });
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    let focus = state.clone();
    InputBase::new(id)
        .focused(focused)
        .w_full()
        .min_h(px(min_h))
        .px(px(12.0))
        .py(px(9.0))
        .rounded(px(RADIUS_CONTROL))
        .border_1()
        .border_color(if focused { theme.focus } else { theme.line })
        .bg(theme.surface)
        .text_size(px(text::BODY))
        .text_color(theme.ink)
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.update(cx, |s, cx| s.focus(window, cx)))
        .child(div().w_full().child(state.clone()))
}
