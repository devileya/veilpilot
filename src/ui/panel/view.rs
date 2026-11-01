//! The panel's layout: a header with the context and agent pickers and the
//! mode chips, Chat / Live / Transcript tabs, the hotkey strip (always
//! visible), and the ask box.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, FocusHandle, Focusable, FontWeight, ImageSource,
    MouseButton, SharedString, Window, anchored, deferred, div, img, linear_color_stop, linear_gradient, prelude::*,
    px, relative,
};

use super::{Card, CardKind, Menu, Panel, Tab, ToastKind, stamp};
use crate::config::{HotkeyId, PromptSource, provider_by_id};
use crate::devcat;
use crate::hotkeys::pretty;
use crate::ui::markdown;
use crate::ui::settings::SettingsTab;
use crate::ui::theme;
use crate::ui::widgets::{caps, chrome_button, key_badge, swallow_mouse_down};

/// A pill button in the header (the context and agent pickers).
fn picker(id: &'static str, icon: &str, label: String, open: bool) -> gpui::Stateful<gpui::Div> {
    swallow_mouse_down(
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(5.))
            .min_w_0()
            .max_w(px(150.))
            .px(px(7.))
            .py(px(3.))
            .rounded(px(5.))
            .border_1()
            .border_color(if open { theme::lamp() } else { theme::edge() })
            .bg(theme::glass())
            .text_size(px(11.))
            .cursor_pointer()
            .hover(|s| s.border_color(theme::lamp()))
            .child(div().flex_none().text_color(theme::lamp()).child(icon.to_string()))
            .child(div().min_w_0().truncate().child(label))
            .child(div().flex_none().text_size(px(8.)).text_color(theme::haze()).child("▾")),
    )
}

/// "Custom (OpenAI-compatible)" → "Custom": the header has little room.
fn short_label(label: String) -> String {
    match label.split_once(" (") {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => label,
    }
}

/// A status / mode chip in the header's second row.
fn chip(id: &'static str, label: &str, color: gpui::Rgba, on: bool) -> gpui::Stateful<gpui::Div> {
    swallow_mouse_down(
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(5.))
            .px(px(6.))
            .py(px(2.))
            .rounded(px(10.))
            .border_1()
            .border_color(if on { gpui::Rgba { a: 0.45, ..color } } else { theme::edge() })
            .bg(if on { gpui::Rgba { a: 0.10, ..color } } else { gpui::Rgba { a: 0.0, ..color } })
            .text_color(if on { color } else { theme::haze() })
            .cursor_pointer()
            .hover(|s| s.border_color(theme::read()))
            .child(caps(label)),
    )
}

/// A one-glyph mode toggle for the compact header (the chips' short form).
fn glyph(id: &'static str, text: &'static str, color: gpui::Rgba, on: bool) -> gpui::Stateful<gpui::Div> {
    swallow_mouse_down(
        div()
            .id(id)
            .flex_none()
            .px(px(2.))
            .text_size(px(11.))
            .text_color(if on { color } else { gpui::Rgba { a: 0.35, ..theme::haze() } })
            .cursor_pointer()
            .hover(|s| s.text_color(theme::read()))
            .child(text),
    )
}

/// One row of a dropdown menu.
fn menu_row(
    id: SharedString,
    label: String,
    detail: Option<String>,
    selected: bool,
    disabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .py(px(5.))
        .text_size(px(12.))
        .text_color(if selected {
            theme::lamp()
        } else if disabled {
            theme::edge()
        } else {
            theme::read()
        })
        .when(!disabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::glass())))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .when_some(detail, |d, detail| {
            d.child(div().flex_none().text_size(px(10.)).text_color(theme::haze()).child(detail))
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

fn menu_heading(text: &str) -> AnyElement {
    caps(text).px(px(10.)).pt(px(8.)).pb(px(3.)).text_color(theme::haze()).into_any_element()
}

impl Panel {
    /* ---------- header ---------- */

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = self.cfg.window.compact;
        let streaming = self.is_streaming() || self.live.busy();
        let project = self.context_project();
        let context_label = project.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "No context".into());
        let context_icon = if project.is_some() { "◆" } else { "◇" };

        let seam = div().absolute().left_0().right_0().bottom_0().h(px(1.)).bg(theme::edge()).when(streaming, |d| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .h(px(1.))
                    .w(relative(0.36))
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(theme::hsla(theme::edge()), 0.),
                        linear_color_stop(theme::hsla(theme::lamp()), 1.),
                    ))
                    .with_animation("seam", Animation::new(Duration::from_millis(1700)).repeat(), |d, t| {
                        d.left(relative(-0.36 + 1.36 * t))
                    }),
            )
        });

        let menu_open = self.open_menu;
        let row1 = div()
            .id("rail")
            .flex_none()
            .h(px(theme::RAIL_H))
            .flex()
            .items_center()
            .gap(px(7.))
            .pl(px(theme::PAD))
            .pr(px(10.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.open_menu.take().is_some() {
                        cx.notify();
                    }
                    this.with_native(cx, |native| native.begin_drag());
                    // The drag runs its own loop; the save fires once it ends.
                    this.queue_save_bounds(cx);
                }),
            )
            .child(img(ImageSource::Render(self.mark.clone())).flex_none().size(px(11.)))
            .when(!compact, |d| {
                d.child(div().flex_none().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).child("VeilPilot"))
            })
            .child(div().flex_1())
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .child(
                        picker("context-picker", context_icon, context_label, menu_open == Some(Menu::Context))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.open_menu == Some(Menu::Context) {
                                    this.open_menu = None;
                                } else {
                                    this.open_context_menu();
                                }
                                cx.notify();
                            })),
                    )
                    .when(menu_open == Some(Menu::Context), |d| d.child(self.render_context_menu(cx))),
            )
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .child(
                        picker("agent-picker", "✦", short_label(self.agent_label()), menu_open == Some(Menu::Agent))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_menu =
                                    if this.open_menu == Some(Menu::Agent) { None } else { Some(Menu::Agent) };
                                cx.notify();
                            })),
                    )
                    .when(menu_open == Some(Menu::Agent), |d| d.child(self.render_agent_menu(cx))),
            )
            .when(compact, |d| {
                // The second row's chips, as glyphs.
                let live = &self.cfg.live;
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .child(
                            glyph(
                                "veil-glyph",
                                "●",
                                if self.cfg.window.protection { theme::lamp() } else { theme::live() },
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_protection(cx))),
                        )
                        .child(
                            glyph(
                                "rec-glyph",
                                if self.is_recording() { "◉" } else { "○" },
                                theme::live(),
                                self.is_recording(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.record_toggle(window, cx))),
                        )
                        .child(
                            glyph("auto-glyph", "⚡", theme::lamp(), live.auto_answer)
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_auto_answer(cx))),
                        )
                        .child(
                            glyph("name-glyph", "@", theme::lamp(), live.answer_on_name)
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_answer_on_name(cx))),
                        )
                        .when(self.update.available.is_some(), |d| {
                            d.child(glyph("update-glyph", "↑", theme::lamp(), true).on_click(cx.listener(
                                |this, _, window, cx| this.open_settings(Some(SettingsTab::About), window, cx),
                            )))
                        }),
                )
            })
            .child(chrome_button(
                "layout",
                if compact { "▤" } else { "▭" },
                cx.listener(|this, _, _, cx| this.toggle_compact(cx)),
            ))
            .child(chrome_button("minimize", "—", cx.listener(|this, _, _, cx| this.set_minimized(true, cx))))
            .child(chrome_button("gear", "⚙", cx.listener(|this, _, window, cx| this.open_settings(None, window, cx))));

        let protection = self.cfg.window.protection;
        let (rec_label, rec_color, rec_on) = if self.is_recording() {
            ("● rec", theme::live(), true)
        } else if self.uploading > 0 {
            ("◌ transcribing", theme::lamp(), true)
        } else {
            ("○ rec", theme::haze(), false)
        };
        let rec = chip("rec-chip", rec_label, rec_color, rec_on);
        let rec = if self.is_recording() {
            rec.with_animation("rec-pulse", Animation::new(Duration::from_millis(1100)).repeat(), |d, t| {
                d.opacity(0.55 + 0.45 * ((t * std::f32::consts::TAU).cos() * 0.5 + 0.5))
            })
            .into_any_element()
        } else {
            rec.into_any_element()
        };
        let live = &self.cfg.live;
        let row2 = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(5.))
            .px(px(theme::PAD))
            .pb(px(7.))
            .child(
                chip(
                    "veil-chip",
                    if protection { "● invisible" } else { "● visible" },
                    if protection { theme::lamp() } else { theme::live() },
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_protection(cx))),
            )
            .child(
                div()
                    .id("rec-wrap")
                    .child(rec)
                    .on_click(cx.listener(|this, _, window, cx| this.record_toggle(window, cx))),
            )
            .child(
                chip("auto-chip", "⚡ auto", theme::lamp(), live.auto_answer)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_auto_answer(cx))),
            )
            .child(
                chip(
                    "name-chip",
                    &live.name_list().first().map(|n| format!("@ {n}")).unwrap_or_else(|| "@ name".into()),
                    theme::lamp(),
                    live.answer_on_name,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_answer_on_name(cx))),
            )
            .child(
                chip("notch-chip", "◒ notch", theme::lamp(), self.cfg.window.notch())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_notch(window, cx))),
            )
            .when_some(self.update.available.as_ref(), |d, release| {
                d.child(chip("update-chip", &format!("↑ update {}", release.version), theme::lamp(), true).on_click(
                    cx.listener(|this, _, window, cx| this.open_settings(Some(SettingsTab::About), window, cx)),
                ))
            });

        div().relative().flex_none().flex().flex_col().child(row1).when(!compact, |d| d.child(row2)).child(seam)
    }

    /// Minimized: the title bar alone, with what is going on and a way back.
    fn render_mini(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let unseen = self.live.unseen;
        let status = if self.is_recording() {
            Some(("● rec", theme::live()))
        } else if self.is_streaming() || self.live.busy() {
            Some(("answering…", theme::lamp()))
        } else if self.uploading > 0 {
            Some(("transcribing…", theme::lamp()))
        } else {
            None
        };
        div()
            .id("mini")
            // A fixed height at the top: GPUI may still lay the window out at
            // its old size for a frame.
            .w_full()
            .h(px(super::MINI_HEIGHT as f32))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(7.))
            .pl(px(theme::PAD))
            .pr(px(10.))
            .bg(theme::shell_bg())
            .border_1()
            .border_color(theme::hairline())
            .rounded(px(13.))
            .overflow_hidden()
            .font_family(theme::ui_family())
            .text_color(theme::read())
            .text_size(px(11.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.with_native(cx, |native| native.begin_drag());
                    this.queue_save_bounds(cx);
                }),
            )
            .child(img(ImageSource::Render(self.mark.clone())).flex_none().size(px(11.)))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child("VeilPilot"))
            .when_some(status, |d, (text, color)| d.child(div().flex_none().text_color(color).child(text)))
            .when(unseen > 0, |d| {
                d.child(
                    swallow_mouse_down(
                        div()
                            .id("mini-unseen")
                            .px(px(6.))
                            .rounded(px(8.))
                            .bg(theme::lamp())
                            .text_color(theme::void())
                            .text_size(px(9.5))
                            .font_weight(FontWeight::BOLD)
                            .cursor_pointer()
                            .child(format!("{unseen} new")),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_minimized(false, cx);
                        this.set_tab(Tab::Live, cx);
                    })),
                )
            })
            .child(div().flex_1())
            .child(div().flex_none().size(px(6.)).rounded_full().bg(if self.cfg.window.protection {
                theme::lamp()
            } else {
                theme::live()
            }))
            .child(chrome_button("restore", "▢", cx.listener(|this, _, _, cx| this.set_minimized(false, cx))))
    }

    fn menu_box(id: &'static str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .occlude()
            .mt(px(4.))
            .w(px(250.))
            .max_h(px(340.))
            .overflow_y_scroll()
            .bg(theme::raise())
            .border_1()
            .border_color(theme::lamp())
            .rounded(px(6.))
            .py(px(4.))
            .shadow_lg()
    }

    fn render_context_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.cfg.context.project.clone();
        let projects = devcat::projects();
        let mut rows: Vec<AnyElement> = vec![
            menu_row("ctx-none".into(), "No context".into(), Some("no memory".into()), current.is_empty(), false)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_menu = None;
                    this.set_context("", window, cx);
                }))
                .into_any_element(),
        ];
        if projects.is_empty() {
            rows.push(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_size(px(10.5))
                    .line_height(px(15.))
                    .text_color(theme::edge())
                    .child(if devcat::present() {
                        "No projects in DevCat yet."
                    } else {
                        "Projects and memory come from DevCat — install it and create a project there."
                    })
                    .into_any_element(),
            );
        } else {
            rows.push(menu_heading("DevCat projects"));
            for p in projects {
                let id = p.id.clone();
                let count = self.memory_counts.get(&p.id).copied().unwrap_or(0);
                rows.push(
                    menu_row(
                        SharedString::from(format!("ctx-{}", p.id)),
                        p.name.clone(),
                        Some(if count == 1 { "1 memory".into() } else { format!("{count} memories") }),
                        p.id == current,
                        false,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_menu = None;
                        this.set_context(&id, window, cx);
                    }))
                    .into_any_element(),
                );
            }
        }
        deferred(anchored().snap_to_window_with_margin(px(8.)).child(Self::menu_box("context-menu").children(rows)))
            .with_priority(1)
            .into_any_element()
    }

    fn render_agent_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.cfg.chat.provider.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut group = "";
        for choice in self.agent_choices() {
            if choice.group != group {
                group = choice.group;
                rows.push(menu_heading(group));
            }
            let id = choice.id.clone();
            let disabled = choice.unavailable.is_some();
            rows.push(
                menu_row(
                    SharedString::from(format!("agent-{}", choice.id)),
                    choice.label.clone(),
                    choice.unavailable.clone(),
                    choice.id == current,
                    false,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_menu = None;
                    if disabled {
                        // Still selectable: Settings explains how to set it up.
                        this.set_agent(&id, window, cx);
                        this.open_settings(Some(crate::ui::settings::SettingsTab::Ai), window, cx);
                    } else {
                        this.set_agent(&id, window, cx);
                    }
                }))
                .into_any_element(),
            );
        }
        deferred(anchored().snap_to_window_with_margin(px(8.)).child(Self::menu_box("agent-menu").children(rows)))
            .with_priority(1)
            .into_any_element()
    }

    /* ---------- tabs ---------- */

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = self.cfg.window.compact;
        let lines = self.session.transcript_lines().len();
        div()
            .flex_none()
            .flex()
            .items_end()
            .gap(px(2.))
            .px(px(theme::PAD - 6.))
            .border_b_1()
            .border_color(theme::edge())
            .children(Tab::ALL.iter().map(|tab| {
                let tab = *tab;
                let on = self.tab == tab;
                let badge = match tab {
                    Tab::Chat => None,
                    Tab::Live => (self.live.unseen > 0).then(|| self.live.unseen.to_string()),
                    Tab::Transcript => {
                        if self.transcript_unseen > 0 {
                            Some(format!("+{}", self.transcript_unseen))
                        } else {
                            (lines > 0).then(|| lines.to_string())
                        }
                    }
                };
                let hot = matches!(tab, Tab::Live) && self.live.unseen > 0
                    || matches!(tab, Tab::Transcript) && self.transcript_unseen > 0;
                swallow_mouse_down(
                    div()
                        .id(SharedString::from(format!("tab-{}", tab.name())))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .px(px(if compact { 6. } else { 8. }))
                        .pt(px(if compact { 3. } else { 6. }))
                        .pb(px(if compact { 2. } else { 5. }))
                        .border_b_2()
                        .border_color(if on { theme::lamp() } else { gpui::Rgba { a: 0.0, ..theme::lamp() } })
                        .text_color(if on { theme::read() } else { theme::haze() })
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::read()))
                        .child(caps(tab.name()))
                        .when_some(badge, |d, b| {
                            d.child(
                                div()
                                    .px(px(5.))
                                    .rounded(px(7.))
                                    .text_size(px(9.))
                                    .font_family(theme::mono_family())
                                    .bg(if hot { theme::lamp() } else { theme::glass() })
                                    .text_color(if hot { theme::void() } else { theme::haze() })
                                    .child(b),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx))),
                )
            }))
    }

    /* ---------- chat ---------- */

    fn render_welcome(&self) -> impl IntoElement {
        let project = self.context_project();
        let agent = self.agent_label();
        let preset = provider_by_id(&self.cfg.chat.provider);
        let keyed = self.cfg.chat.provider.starts_with("devcat:")
            || self.secrets.has(&self.cfg.chat.provider)
            || preset.is_some_and(|p| !p.needs_key);
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .text_color(theme::haze())
            .child(div().text_size(px(13.)).text_color(theme::read()).child("Ask anything, or press a key below."))
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(17.))
                    .child(if keyed {
                        format!("Answering with {agent}.")
                    } else {
                        format!("{agent} has no API key yet — open settings › Keys, or pick another agent at the top.")
                    }),
            )
            .child(div().text_size(px(11.5)).line_height(px(17.)).child(match project {
                Some(p) => format!(
                    "Context: {} — the agent gets its memory (DevCat's, and Claude Code's for its folders), and Save to memory files the meeting there.",
                    p.name
                ),
                None => "No context: the agent knows only this chat. Pick a DevCat project at the top to give it project memory.".into(),
            }))
    }

    pub(super) fn render_card(&self, card: &Card, first: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let streaming = !card.done && self.active.as_ref().is_some_and(|a| a.id == card.id);
        let body: Vec<AnyElement> = if let Some(error) = &card.error {
            vec![div().text_color(theme::live()).child(error.clone()).into_any_element()]
        } else if card.done && card.text.is_empty() {
            vec![div().text_size(px(11.)).text_color(theme::lamp()).child("No text came back.").into_any_element()]
        } else {
            markdown::render(
                &card.text,
                markdown::RenderOptions {
                    id: format!("card-{}", card.id).into(),
                    caret: streaming,
                    caret_visible: self.caret_visible,
                },
            )
        };
        let thinking: String = {
            let chars: Vec<char> = card.thinking.chars().collect();
            chars[chars.len().saturating_sub(400)..].iter().collect()
        };
        let copy_text = markdown::plain_text(&card.text);
        let id = card.id;
        let is_summary = card.kind == CardKind::Summary;
        let can_save = is_summary && card.done && card.error.is_none() && self.context_project().is_some();

        div()
            .pb(px(18.))
            .when(!first, |d| d.border_t_1().border_color(theme::edge()).pt(px(14.)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .mb(px(9.))
                    .text_color(theme::haze())
                    .when_some(card.accelerator.clone(), |d, a| d.child(key_badge(pretty(&a))))
                    .child(caps(&card.label).text_color(if is_summary { theme::lamp() } else { theme::read() }))
                    // A spacer, not ml_auto: an auto margin here cancels the gap.
                    .child(div().flex_1())
                    .child(div().font_family(theme::mono_family()).text_size(px(9.5)).child(card.meta.clone())),
            )
            .when(!card.prompt.is_empty(), |d| {
                d.child(
                    div()
                        .mb(px(8.))
                        .text_size(px(11.5))
                        .text_color(theme::haze())
                        .line_clamp(3)
                        .child(card.prompt.clone()),
                )
            })
            .children(card.shots.iter().map(|s| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .mb(px(10.))
                    .child(
                        div()
                            .w(px(76.))
                            .h(px(46.))
                            .overflow_hidden()
                            .border_1()
                            .border_color(theme::edge())
                            .rounded(px(3.))
                            .child(
                                img(ImageSource::Image(s.image.clone())).w(px(76.)).object_fit(gpui::ObjectFit::Cover),
                            ),
                    )
                    .child(
                        div()
                            .font_family(theme::mono_family())
                            .text_size(px(9.5))
                            .text_color(theme::haze())
                            .child(format!("{}×{} px", s.width, s.height)),
                    )
            }))
            .when(!thinking.is_empty(), |d| {
                d.child(
                    div()
                        .mb(px(10.))
                        .pl(px(10.))
                        .border_l_1()
                        .border_color(theme::lamp_dim())
                        .text_size(px(11.))
                        .line_height(px(17.))
                        .text_color(theme::haze())
                        .max_h(px(72.))
                        .overflow_hidden()
                        .font(theme::italic(theme::ui_font()))
                        .child(thinking),
                )
            })
            .children(
                card.notes
                    .iter()
                    .map(|n| div().mb(px(8.)).text_size(px(11.)).text_color(theme::lamp()).child(n.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(9.5))
                    .max_w(px(62. * 7.2))
                    .text_size(px(if self.cfg.window.compact { 12.5 } else { 13.5 }))
                    .line_height(px(if self.cfg.window.compact { 19.5 } else { 21.9 }))
                    .text_color(theme::read())
                    .children(body),
            )
            .when(card.done && card.error.is_none() && !card.text.is_empty(), |d| {
                d.child(
                    div()
                        .mt(px(10.))
                        .flex()
                        .gap(px(14.))
                        .child(chrome_button(
                            ("copy", id as usize),
                            "copy",
                            cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                                this.toast(ToastKind::Info, "Copied.", cx);
                            }),
                        ))
                        .when(can_save, |d| {
                            d.child(chrome_button(
                                ("save-memory", id as usize),
                                "save to memory",
                                cx.listener(|this, _, window, cx| this.save_memory(window, cx)),
                            ))
                        }),
                )
            })
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let cards: Vec<AnyElement> = {
            let cards = self.cards.clone();
            cards.iter().enumerate().map(|(i, c)| self.render_card(c, i == 0, cx).into_any_element()).collect()
        };
        div()
            .id("readout")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.readout_scroll)
            .p(px(theme::PAD))
            .when(cards.is_empty(), |d| d.child(self.render_welcome()))
            .children(cards)
            .into_any_element()
    }

    /* ---------- live ---------- */

    fn render_live(&self, cx: &mut Context<Self>) -> AnyElement {
        let live = &self.cfg.live;
        let items: Vec<_> = self.live.items.iter().filter(|i| i.visible()).cloned().collect();
        let names = live.name_list();
        let status = match (live.auto_answer, live.answer_on_name) {
            (false, false) => "Off. Turn on ⚡ auto to answer questions heard in the transcript, or @ to answer when someone says your name.".to_string(),
            (true, false) => "Answering questions heard in the transcript.".into(),
            (false, true) => format!("Answering when someone says {}.", names.join(" / ")),
            (true, true) => format!("Answering questions, and whenever someone says {}.", names.join(" / ")),
        };
        let recording_hint = (!self.is_recording() && items.is_empty() && (live.auto_answer || live.answer_on_name))
            .then(|| {
                format!(
                    "Start transcribing with {} — answers appear here as people speak.",
                    pretty(&self.cfg.hotkeys.record_toggle)
                )
            });

        div()
            .id("live")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(theme::PAD))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().text_size(px(11.)).line_height(px(16.)).text_color(theme::haze()).child(status))
            .when_some(recording_hint, |d, h| d.child(div().text_size(px(11.)).text_color(theme::lamp()).child(h)))
            .children(items.into_iter().rev().map(|item| {
                let body: Vec<AnyElement> = match &item.error {
                    Some(err) => vec![div().text_color(theme::live()).child(err.clone()).into_any_element()],
                    None => markdown::render(
                        &item.answer,
                        markdown::RenderOptions {
                            id: format!("live-{}", item.id).into(),
                            caret: !item.done,
                            caret_visible: self.caret_visible,
                        },
                    ),
                };
                let copy = markdown::plain_text(&item.answer);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .p(px(10.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(theme::edge())
                    .bg(theme::glass())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .child(caps(item.trigger.label()).text_color(theme::lamp()))
                            .child(
                                div()
                                    .ml_auto()
                                    .font_family(theme::mono_family())
                                    .text_size(px(9.5))
                                    .text_color(theme::haze())
                                    .child(format!("{} · {}", item.meta, stamp(item.at))),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(theme::haze())
                            .font(theme::italic(theme::ui_font()))
                            .child(format!("“{}”", item.line)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .text_size(px(13.5))
                            .line_height(px(21.))
                            .text_color(theme::read())
                            .children(body),
                    )
                    .when(item.done && item.error.is_none(), |d| {
                        d.child(div().flex().child(chrome_button(
                            ("live-copy", item.id as usize),
                            "copy",
                            cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                                this.toast(ToastKind::Info, "Copied.", cx);
                            }),
                        )))
                    })
            }))
            .into_any_element()
    }

    /* ---------- transcript ---------- */

    fn render_transcript(&self, cx: &mut Context<Self>) -> AnyElement {
        let lines = self.session.transcript_lines();
        let last = lines.len().saturating_sub(1);
        let has_project = self.context_project().is_some();
        let h = &self.cfg.hotkeys;
        let action = |id: &'static str, label: &str, key: &str| {
            swallow_mouse_down(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(theme::edge())
                    .bg(theme::glass())
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::lamp()).text_color(theme::lamp()))
                    .child(label.to_string())
                    .when(!key.is_empty(), |d| d.child(key_badge(pretty(key)))),
            )
        };
        let status = if self.is_recording() {
            "Transcribing as people speak…".to_string()
        } else if lines.is_empty() {
            format!("Nothing yet. Press {} to start transcribing.", pretty(&h.record_toggle))
        } else {
            format!("{} lines", lines.len())
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.transcript_scroll)
                    .px(px(theme::PAD))
                    .py(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(div().text_size(px(11.)).text_color(theme::haze()).mb(px(4.)).child(status))
                    .children(lines.iter().enumerate().map(|(i, line)| {
                        div()
                            .flex()
                            .gap(px(9.))
                            .font_family(theme::mono_family())
                            .text_size(px(11.))
                            .line_height(px(17.6))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(gpui::Rgba { a: 0.6, ..theme::haze() })
                                    .child(stamp(line.at)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(if i == last { theme::read() } else { theme::haze() })
                                    .child(line.text.clone()),
                            )
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .px(px(theme::PAD))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(theme::edge())
                    .child(
                        action("summarize", "Summarize", &h.summarize)
                            .on_click(cx.listener(|this, _, window, cx| this.summarize(window, cx))),
                    )
                    .child(
                        action(
                            "save-memory",
                            if has_project { "Save to memory" } else { "Save to memory (pick a project)" },
                            &h.save_memory,
                        )
                        .when(!has_project, |d| d.text_color(theme::edge()))
                        .on_click(cx.listener(|this, _, window, cx| this.save_memory(window, cx))),
                    )
                    .child(
                        action("clear-transcript", "Clear", "")
                            .on_click(cx.listener(|this, _, _, cx| this.clear_transcript(cx))),
                    ),
            )
            .into_any_element()
    }

    /* ---------- hotkeys ---------- */

    /// The keys that matter during a call, always on screen.
    fn strip_keys(&self) -> Vec<(String, String)> {
        let h = &self.cfg.hotkeys;
        let mut keys: Vec<(String, String)> =
            self.cfg.prompts.iter().map(|p| (p.accelerator.clone(), p.label.clone())).collect();
        keys.extend([
            (h.record_toggle.clone(), "Rec".to_string()),
            (h.region_shot.clone(), "Region".into()),
            (h.shot_only.clone(), "Shot".into()),
            (h.summarize.clone(), "Summary".into()),
            (h.toggle_overlay.clone(), "Hide".into()),
        ]);
        keys.retain(|(k, _)| !k.is_empty());
        keys
    }

    fn render_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .items_start()
            .gap(px(6.))
            .px(px(theme::PAD))
            .py(px(6.))
            .border_t_1()
            .border_color(theme::edge())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .max_h(px(if self.cfg.window.compact { 21. } else { 66. }))
                    .overflow_hidden()
                    .children(self.strip_keys().into_iter().map(|(key, label)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .child(key_badge(pretty(&key)))
                            .child(div().text_size(px(10.)).text_color(theme::haze()).pr(px(3.)).child(label))
                    })),
            )
            .child(swallow_mouse_down(
                div()
                    .id("cheatsheet-toggle")
                    .flex_none()
                    .size(px(18.))
                    .rounded_full()
                    .border_1()
                    .border_color(if self.cheatsheet_open { theme::lamp() } else { theme::edge() })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .text_color(if self.cheatsheet_open { theme::lamp() } else { theme::haze() })
                    .cursor_pointer()
                    .child("?")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cheatsheet_open = !this.cheatsheet_open;
                        cx.notify();
                    })),
            ))
    }

    /// Every hotkey, over the tab body.
    fn render_cheatsheet(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let h = &self.cfg.hotkeys;
        let mut rows: Vec<(String, String)> = self
            .cfg
            .prompts
            .iter()
            .map(|p| {
                let what = match p.source {
                    PromptSource::Transcript => "from transcript",
                    PromptSource::Both => "screenshot + transcript",
                    PromptSource::Screenshot => "screenshot + ask",
                };
                (p.accelerator.clone(), format!("{} — {what}", p.label))
            })
            .collect();
        for id in [
            HotkeyId::RegionShot,
            HotkeyId::ShotOnly,
            HotkeyId::RecordToggle,
            HotkeyId::Summarize,
            HotkeyId::SaveMemory,
            HotkeyId::ToggleAutoAnswer,
            HotkeyId::ToggleNotch,
            HotkeyId::ToggleOverlay,
            HotkeyId::Send,
            HotkeyId::Cancel,
            HotkeyId::NewSession,
            HotkeyId::ToggleProtection,
            HotkeyId::ClickThrough,
        ] {
            rows.push((h.get(id).to_string(), id.failure_label().to_string()));
        }
        rows.retain(|(k, _)| !k.is_empty());
        div()
            .id("cheatsheet")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme::void())
            .overflow_y_scroll()
            .p(px(theme::PAD))
            .occlude()
            .child(
                div()
                    .flex()
                    .items_center()
                    .mb(px(10.))
                    .child(caps("Hotkeys").text_color(theme::lamp()))
                    .child(div().flex_1())
                    .child(chrome_button("cheatsheet-close", "close", cx.listener(|this, _, _, cx| {
                        this.cheatsheet_open = false;
                        cx.notify();
                    }))),
            )
            .child(div().flex().flex_col().gap(px(6.)).children(rows.into_iter().map(|(key, what)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().w(px(56.)).flex_none().flex().child(key_badge(pretty(&key))))
                    .child(div().flex_1().min_w_0().text_size(px(11.5)).text_color(theme::haze()).child(what))
            })))
            .child(
                div()
                    .mt(px(12.))
                    .text_size(px(10.5))
                    .line_height(px(15.))
                    .text_color(theme::edge())
                    .child("In the panel: Esc stops or hides · ⌘, settings · Ctrl+Tab next tab. Change any key in settings › Keys."),
            )
    }

    /* ---------- composer and toasts ---------- */

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .border_t_1()
            .border_color(theme::edge())
            .when(!self.attachments.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap(px(6.)).px(px(theme::PAD)).pt(px(8.)).children(
                    self.attachments.iter().map(|s| {
                        let id = s.id.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pl(px(3.))
                            .pr(px(4.))
                            .py(px(3.))
                            .bg(theme::glass())
                            .border_1()
                            .border_color(theme::edge())
                            .rounded(px(3.))
                            .child(div().w(px(30.)).h(px(19.)).overflow_hidden().rounded(px(2.)).child(
                                img(ImageSource::Image(s.image.clone())).w(px(30.)).object_fit(gpui::ObjectFit::Cover),
                            ))
                            .child(
                                div()
                                    .font_family(theme::mono_family())
                                    .text_size(px(9.5))
                                    .text_color(theme::haze())
                                    .child(format!("{}×{}", s.width, s.height)),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("drop-{id}")))
                                    .px(px(3.))
                                    .text_size(px(12.))
                                    .text_color(theme::haze())
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(theme::live()))
                                    .child("×")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.attachments.retain(|a| a.id != id);
                                        cx.notify();
                                    })),
                            )
                    }),
                ))
            })
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .px(px(theme::PAD))
                    .pt(px(9.))
                    .pb(px(10.))
                    .child(div().text_color(theme::lamp()).text_size(px(14.)).line_height(px(20.)).child("›"))
                    .child(div().flex_1().min_w_0().text_size(px(13.)).line_height(px(20.)).child(self.input.clone()))
                    .child(div().mt(px(2.)).opacity(if self.input_focused { 1. } else { 0.45 }).child(key_badge("↩"))),
            )
    }

    fn render_toasts(&self) -> impl IntoElement {
        div()
            .absolute()
            .left(px(theme::PAD))
            .right(px(theme::PAD))
            // Over the top of the tab body: new content arrives at the
            // bottom, and the hotkey strip stays readable.
            .top(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(self.toasts.iter().map(|t| {
                let accent = match t.kind {
                    ToastKind::Error => theme::live(),
                    ToastKind::Warn | ToastKind::Info => theme::lamp(),
                };
                div()
                    .flex()
                    .bg(theme::raise())
                    .border_1()
                    .border_color(theme::edge())
                    .rounded(px(4.))
                    .overflow_hidden()
                    .child(div().flex_none().w(px(2.)).bg(accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(11.))
                            .py(px(8.))
                            .text_size(px(11.5))
                            .line_height(px(16.7))
                            .child(t.text.clone()),
                    )
                    .with_animation(("toast", t.id as usize), Animation::new(Duration::from_millis(160)), |d, t| {
                        d.opacity(t).mt(px(4. * (1. - t)))
                    })
            }))
    }
}

impl Focusable for Panel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Panel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.minimized.is_some() {
            return self.render_mini(cx).into_any_element();
        }
        let body = match self.tab {
            Tab::Chat => self.render_chat(cx),
            Tab::Live => self.render_live(cx),
            Tab::Transcript => self.render_transcript(cx),
        };

        div()
            .id("shell")
            .key_context("Panel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::shell_bg())
            .border_1()
            .border_color(theme::hairline())
            .rounded(px(13.))
            .overflow_hidden()
            .font_family(theme::ui_family())
            .text_color(theme::read())
            .text_size(px(13.))
            // macOS fades the whole window natively; elsewhere the view fades.
            .when(!cfg!(target_os = "macos"), |d| d.opacity(self.cfg.window.opacity))
            .child(self.render_header(cx))
            .child(self.render_tabs(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(body)
                    .when(self.cheatsheet_open, |d| d.child(self.render_cheatsheet(cx)))
                    .child(self.render_toasts()),
            )
            .child(self.render_strip(cx))
            .child(self.render_composer(cx))
            .child(
                div()
                    .id("grip")
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .size(px(15.))
                    .cursor(gpui::CursorStyle::ResizeUpLeftDownRight)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::grip_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::grip_up))
                    .child(
                        div()
                            .absolute()
                            .right(px(3.))
                            .bottom(px(3.))
                            .size(px(7.))
                            .border_r_1()
                            .border_b_1()
                            .border_color(theme::edge()),
                    ),
            )
            .on_mouse_move(cx.listener(Self::grip_move))
            .when_some(self.settings.clone(), |d, settings| {
                d.child(div().absolute().top_0().left_0().size_full().child(settings))
            })
            .into_any_element()
    }
}
