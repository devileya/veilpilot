pub mod markdown;
pub mod panel;
pub mod region;
pub mod settings;
pub mod text_input;
pub mod theme;
pub mod widgets;

pub fn bind_keys(cx: &mut gpui::App) {
    text_input::bind_keys(cx);
    panel::bind_keys(cx);
    region::bind_keys(cx);
}
