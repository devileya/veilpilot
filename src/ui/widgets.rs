//! Small building blocks shared by the panel, settings and the screenshot
//! editor.

use gpui::{
    AnyView, App, AppContext as _, ClickEvent, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};

use super::assets::icon;
use super::theme;

/// A label for chrome: small, medium weight, sentence case. (The name is
/// from when these were tracked capitals.)
pub fn caps(text: impl AsRef<str>) -> Div {
    div().text_size(theme::label_size()).font_weight(FontWeight::MEDIUM).child(text.as_ref().to_string())
}

/// A printed hotkey: amber mono on a faint amber chip.
pub fn key_badge(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .font_family(theme::mono_family())
        .text_size(px(10.))
        .text_color(theme::lamp())
        .bg(theme::lamp_soft())
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .child(text.into())
}

/// Stops a click on a control from also starting a window drag or reaching
/// whatever is underneath.
pub fn swallow_mouse_down(el: Stateful<Div>) -> Stateful<Div> {
    el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/* ---------- tooltips ---------- */

struct Tooltip {
    text: SharedString,
    key: Option<SharedString>,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        // A tooltip is laid out apart from the view it belongs to, so it sets
        // its own type.
        div().pt(px(6.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .bg(theme::raise())
                .border_1()
                .border_color(theme::edge())
                .rounded(px(6.))
                .shadow_md()
                .px(px(8.))
                .py(px(4.))
                .font_family(theme::ui_family())
                .text_size(px(11.5))
                .text_color(theme::read())
                .child(self.text.clone())
                .when_some(self.key.clone(), |d, key| d.child(key_badge(key))),
        )
    }
}

/// What `.tooltip(...)` takes: a line of text and, optionally, the key that
/// does the same thing.
pub fn tooltip(
    text: impl Into<SharedString>,
    key: Option<impl Into<SharedString>>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    let key = key.map(Into::into).filter(|k: &SharedString| !k.is_empty() && k.as_ref() != "—");
    move |_, cx| cx.new(|_| Tooltip { text: text.clone(), key: key.clone() }).into()
}

/* ---------- buttons ---------- */

/// A standard button: a soft fill that lights up under the pointer.
pub fn button(
    id: impl Into<ElementId>,
    label: impl AsRef<str>,
    danger: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let accent = if danger { theme::live() } else { theme::read() };
    swallow_mouse_down(
        div()
            .id(id.into())
            .flex_none()
            .bg(theme::control())
            .border_1()
            .border_color(theme::hairline())
            .rounded(px(theme::RADIUS))
            .px(px(11.))
            .py(px(6.))
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if danger { theme::live() } else { theme::read() })
            .cursor_pointer()
            .hover(move |s| s.bg(theme::control_hover()).border_color(theme::alpha(accent, 0.45)))
            .active(|s| s.opacity(0.8))
            .child(label.as_ref().to_string())
            .on_click(on_click),
    )
}

/// The one button on a surface that is the thing to do: filled with the
/// accent.
pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl AsRef<str>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    swallow_mouse_down(
        div()
            .id(id.into())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .bg(theme::lamp())
            .rounded(px(theme::RADIUS))
            .px(px(12.))
            .py(px(6.))
            .text_size(px(11.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::void())
            .cursor_pointer()
            .hover(|s| s.opacity(0.9))
            .active(|s| s.opacity(0.75))
            .child(label.as_ref().to_string())
            .on_click(on_click),
    )
}

/// A quiet text button: secondary text that brightens under the pointer.
pub fn chrome_button(
    id: impl Into<ElementId>,
    label: impl AsRef<str>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    swallow_mouse_down(
        div()
            .id(id.into())
            .flex_none()
            .px(px(7.))
            .py(px(4.))
            .rounded(px(6.))
            .text_size(theme::label_size())
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::haze())
            .cursor_pointer()
            .hover(|s| s.text_color(theme::read()).bg(theme::control()))
            .child(label.as_ref().to_string())
            .on_click(on_click),
    )
}

/// A square button that is only an icon; give it a tooltip.
pub fn icon_button(
    id: impl Into<ElementId>,
    name: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    // An SVG takes its colour from its own style, not its parent's, so the
    // hover colour reaches it through a group.
    swallow_mouse_down(
        div()
            .id(id.into())
            .group("icon-button")
            .flex_none()
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(theme::RADIUS))
            .cursor_pointer()
            .when(active, |d| d.bg(theme::lamp_soft()))
            .hover(move |s| if active { s } else { s.bg(theme::control_hover()) })
            .active(|s| s.opacity(0.7))
            .child(
                icon(name)
                    .text_color(if active { theme::lamp() } else { theme::haze() })
                    .when(!active, |s| s.group_hover("icon-button", |s| s.text_color(theme::read()))),
            )
            .on_click(on_click),
    )
}

pub fn checkbox(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    swallow_mouse_down(
        div()
            .id(id.into())
            .flex()
            .items_start()
            .gap(px(9.))
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .mt(px(1.))
                    .size(px(15.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if checked { theme::lamp() } else { theme::edge() })
                    .bg(if checked { theme::lamp() } else { theme::glass() })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(checked, |d| d.child(icon("check").size(px(11.)).text_color(theme::void()))),
            )
            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(theme::read()).child(label.into()))
            .on_click(move |_, window, cx| on_toggle(!checked, window, cx)),
    )
}

/// A settings field: label, control, optional hint.
pub fn field(label: impl AsRef<str>, control: impl IntoElement, hint: Option<gpui::AnyElement>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .mb(px(15.))
        .child(caps(label).text_color(theme::haze()).mb(px(6.)))
        .child(control)
        .when_some(hint, |d, hint| d.child(hint))
}

/// Hint text under a field. `**bold**` segments render brighter, standing in
/// for the `<b>` tags the HTML hints used.
pub fn hint(text: impl AsRef<str>) -> Div {
    hint_colored(text, theme::faint())
}

pub fn hint_colored(text: impl AsRef<str>, color: gpui::Rgba) -> Div {
    let text = text.as_ref();
    let mut runs = Vec::new();
    let mut plain = String::new();
    for (i, part) in text.split("**").enumerate() {
        let start = plain.len();
        plain.push_str(part);
        let bold = i % 2 == 1;
        runs.push(gpui::TextRun {
            len: plain.len() - start,
            font: if bold { theme::with_weight(theme::ui_font(), FontWeight::MEDIUM) } else { theme::ui_font() },
            color: theme::hsla(if bold && color == theme::faint() { theme::haze() } else { color }),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    runs.retain(|r| r.len > 0);
    div().mt(px(6.)).text_size(px(11.)).line_height(px(16.)).child(gpui::StyledText::new(plain).with_runs(runs))
}

/// A section heading in settings.
pub fn section(label: impl AsRef<str>, first: bool) -> Div {
    div()
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::read())
        .mb(px(12.))
        .when(!first, |d| d.mt(px(8.)).pt(px(16.)).border_t_1().border_color(theme::hairline()))
        .child(label.as_ref().to_string())
}

use gpui::prelude::FluentBuilder as _;
