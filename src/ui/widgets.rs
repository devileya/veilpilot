//! Small building blocks shared by the panel and settings.

use gpui::{
    App, ClickEvent, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::theme;

/// The tracked small-caps label used for all chrome. GPUI has no
/// letter-spacing or text-transform, so the text is uppercased here.
pub fn caps(text: impl AsRef<str>) -> Div {
    div().text_size(theme::label_size()).font_weight(FontWeight::MEDIUM).child(text.as_ref().to_uppercase())
}

/// A printed hotkey: amber mono on a faint amber chip.
pub fn key_badge(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .font_family(theme::mono_family())
        .text_size(px(10.))
        .text_color(theme::lamp())
        .bg(theme::lamp_soft())
        .border_1()
        .border_color(theme::lamp_border())
        .rounded(px(3.))
        .px(px(5.))
        .py(px(1.))
        .child(text.into())
}

/// Stops a click on a control from also starting a window drag or reaching
/// whatever is underneath.
pub fn swallow_mouse_down(el: Stateful<Div>) -> Stateful<Div> {
    el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The bordered uppercase button (`.btn`).
pub fn button(
    id: impl Into<ElementId>,
    label: impl AsRef<str>,
    danger: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let hover = if danger { theme::live() } else { theme::lamp() };
    swallow_mouse_down(
        div()
            .id(id.into())
            .flex_none()
            .bg(theme::glass())
            .border_1()
            .border_color(theme::edge())
            .rounded(px(4.))
            .px(px(10.))
            .py(px(6.))
            .text_size(px(10.))
            .text_color(theme::read())
            .cursor_pointer()
            .hover(move |s| s.border_color(hover).text_color(hover))
            .child(label.as_ref().to_uppercase())
            .on_click(on_click),
    )
}

/// Text-only chrome button (`.chrome`): haze, amber on hover.
pub fn chrome_button(
    id: impl Into<ElementId>,
    label: impl AsRef<str>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    swallow_mouse_down(
        div()
            .id(id.into())
            .flex_none()
            .px(px(2.))
            .py(px(4.))
            .text_size(theme::label_size())
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::haze())
            .cursor_pointer()
            .hover(|s| s.text_color(theme::lamp()))
            .child(label.as_ref().to_uppercase())
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
            .gap(px(8.))
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .mt(px(2.))
                    .size(px(12.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if checked { theme::lamp() } else { theme::edge() })
                    .bg(if checked { theme::lamp() } else { theme::glass() })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when_some(checked.then_some(()), |d, _| {
                        d.child(div().text_size(px(9.)).text_color(theme::void()).child("✓"))
                    }),
            )
            .child(div().text_size(px(11.5)).line_height(px(16.5)).text_color(theme::read()).child(label.into()))
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
        .child(caps(label).text_color(theme::haze()).mb(px(5.)))
        .child(control)
        .when_some(hint, |d, hint| d.child(hint))
}

/// Hint text under a field. `**bold**` segments render brighter, standing in
/// for the `<b>` tags the HTML hints used.
pub fn hint(text: impl AsRef<str>) -> Div {
    hint_colored(text, theme::edge())
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
            color: theme::hsla(if bold && color == theme::edge() { theme::haze() } else { color }),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    runs.retain(|r| r.len > 0);
    div().mt(px(5.)).text_size(px(10.5)).line_height(px(15.75)).child(gpui::StyledText::new(plain).with_runs(runs))
}

pub fn section(label: impl AsRef<str>, first: bool) -> Div {
    caps(label)
        .text_color(theme::lamp())
        .mb(px(11.))
        .when(!first, |d| d.pt(px(6.)).border_t_1().border_color(theme::edge()))
}

use gpui::prelude::FluentBuilder as _;
