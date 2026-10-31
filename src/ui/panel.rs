//! The panel: state, the actions hotkeys / tray / clicks fire, and rendering.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{
    Animation, AnimationExt as _, App, AsyncWindowContext, Context, Entity, FocusHandle, Focusable, FontWeight,
    ImageSource, KeyBinding, MouseButton, MouseMoveEvent, MouseUpEvent, ScrollHandle, SharedString, Subscription, Task,
    WeakEntity, Window, actions, div, img, linear_color_stop, linear_gradient, prelude::*, px, relative,
};
use tokio_util::sync::CancellationToken;

use super::markdown;
use super::region::{RegionPicker, RegionResult};
use super::settings::{SettingsTab, SettingsView};
use super::text_input::{InputEvent, TextInput};
use super::theme;
use super::widgets::{caps, chrome_button, key_badge, swallow_mouse_down};
use crate::ai::{self, Chunk, ClaudeCodeOptions, StreamEvent, StreamOptions};
use crate::audio::{self, Recorder};
use crate::capture::{self, Shot};
use crate::config::{self, Config, HotkeyId, ProviderKind, provider_by_id, resolve_chat};
use crate::hotkeys::{Action, Failure, Hotkeys, pretty};
use crate::platform::{self, NativeWindow, Permission, Rect};
use crate::secrets::Secrets;
use crate::session::{ImagePart, Role, Session, Turn};
use crate::transcribe;
use crate::tray::{Tray, TrayCommand};

actions!(panel, [Escape, OpenSettings]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Escape, Some("Panel")),
        KeyBinding::new("secondary-,", OpenSettings, Some("Panel")),
    ]);
}

pub const DEFAULT_SIZE: (f64, f64) = (430.0, 580.0);
pub const MIN_SIZE: (f64, f64) = (320.0, 240.0);
const MARGIN: f64 = 24.0;

/* ---------- small state types ---------- */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Card {
    pub id: u64,
    pub label: String,
    pub accelerator: Option<String>,
    pub meta: String,
    pub shots: Vec<Arc<Shot>>,
    pub thinking: String,
    pub notes: Vec<String>,
    pub text: String,
    pub error: Option<String>,
    pub done: bool,
}

struct ActiveAsk {
    id: u64,
    cancel: CancellationToken,
}

struct Recording {
    recorder: Recorder,
    rollover: Option<Task<()>>,
}

#[derive(Clone, Copy)]
enum ShotMode {
    Full,
    Region,
}

struct Resize {
    start_frame: Rect,
    start_cursor: (f64, f64),
}

/// Messages from the platform threads (hotkeys and the tray).
pub enum External {
    Hotkey(u32),
    Tray(TrayCommand),
}

/* ---------- the panel ---------- */

pub struct Panel {
    pub cfg: Config,
    pub secrets: Secrets,
    pub session: Session,
    hotkeys: Hotkeys,
    pub hotkey_errors: Vec<Failure>,
    pub tray: Option<Tray>,
    pub native: Option<NativeWindow>,
    focus_handle: FocusHandle,

    active: Option<ActiveAsk>,
    seq: u64,
    pub cards: Vec<Card>,
    pub input: Entity<TextInput>,
    pub attachments: Vec<Arc<Shot>>,

    pub drawer_open: bool,
    transcript_scroll: ScrollHandle,
    readout_scroll: ScrollHandle,

    recording: Option<Recording>,
    pub uploading: usize,

    pub toasts: Vec<Toast>,
    toast_seq: u64,
    pub settings: Option<Entity<SettingsView>>,
    caret_visible: bool,
    blink: Option<Task<()>>,
    resize: Option<Resize>,
    save_bounds: Option<Task<()>>,
    region_open: bool,
    /// Screenshots in flight, so "idle" means nothing is pending at all.
    pub capturing: usize,
    mark: Arc<gpui::RenderImage>,
    input_focused: bool,
    _subscriptions: Vec<Subscription>,
}

impl Panel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let secrets = Secrets::load();
        let cfg = config::load(&|id| secrets.has(id));
        let input = cx.new(|cx| TextInput::new(window, cx).placeholder("ask anything").composer(4));

        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.send(window, cx),
                InputEvent::Focus => {
                    this.input_focused = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.input_focused = false;
                    cx.notify();
                }
                InputEvent::Changed => {}
            }),
            cx.observe_window_bounds(window, |this, _, cx| this.queue_save_bounds(cx)),
        ];

        let mark_px = 22;
        let mark = crate::icons::draw_mark(
            mark_px,
            &crate::icons::MarkStyle {
                inset: 0.5,
                radius: 2.0,
                fill: crate::icons::LAMP,
                stroke: crate::icons::LAMP,
                background: None,
            },
        );
        let mark = capture::render_image(&image::RgbaImage::from_raw(mark_px, mark_px, mark).expect("mark size"));

        let mut panel = Self {
            cfg,
            secrets,
            session: Session::default(),
            hotkeys: Hotkeys::new(),
            hotkey_errors: Vec::new(),
            tray: None,
            native: NativeWindow::from_window(window),
            focus_handle: cx.focus_handle(),
            active: None,
            seq: 0,
            cards: Vec::new(),
            input,
            attachments: Vec::new(),
            drawer_open: false,
            transcript_scroll: ScrollHandle::new(),
            readout_scroll: ScrollHandle::new(),
            recording: None,
            uploading: 0,
            toasts: Vec::new(),
            toast_seq: 0,
            settings: None,
            caret_visible: true,
            blink: None,
            resize: None,
            save_bounds: None,
            region_open: false,
            capturing: 0,
            mark,
            input_focused: false,
            _subscriptions: subscriptions,
        };
        panel.configure_window(cx);
        panel
    }

    /// Everything that has to happen once the window and the app exist.
    pub fn start(
        &mut self,
        external: futures::channel::mpsc::UnboundedSender<External>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tx = external.clone();
        Hotkeys::listen(move |id| {
            let _ = tx.unbounded_send(External::Hotkey(id));
        });
        self.reapply_hotkeys(window, cx);

        let tx = external.clone();
        match Tray::new(&self.cfg, move |cmd| {
            let _ = tx.unbounded_send(External::Tray(cmd));
        }) {
            Ok(tray) => self.tray = Some(tray),
            Err(err) => eprintln!("[tray] unavailable: {err:#}"),
        }

        // The CLI is a large bundle, so the first ask of a session pays ~7s of
        // cold start against ~1.5s warm. `--version` costs nothing and no
        // tokens, but it pulls the bundle into the OS page cache.
        if self.cfg.chat.provider == "claudecode" {
            let path = self.cfg.chat.claude_code.binary_path.clone();
            crate::runtime::spawn(async move {
                let _ = ai::claude_code::detect(&path).await;
            });
        }
        // Same idea for on-device speech: whisper.cpp re-reads the model on
        // every pass.
        if self.cfg.transcription.provider == "local" {
            crate::whisper::warm(&self.cfg.transcription.whisper_cpp);
        }

        if cfg!(target_os = "macos") && capture::screen_permission() != Permission::Granted {
            self.toast(
                ToastKind::Warn,
                "Screen Recording permission is not granted yet — screenshots will come back empty. Open settings › Capture to fix it.",
                cx,
            );
        }
        if !self.hotkey_errors.is_empty() {
            let n = self.hotkey_errors.len();
            self.toast(
                ToastKind::Warn,
                format!("{n} hotkey(s) are already taken by another app. Change them in settings › Keys."),
                cx,
            );
        }

        if self.cfg.window.show_on_start {
            self.show(true, window, cx);
        }
    }

    /* ---------- window ---------- */

    /// Run a native window call once GPUI has finished the current update.
    /// AppKit answers calls like `makeKeyAndOrderFront:` and `setFrame:` with
    /// synchronous callbacks into GPUI, which must not arrive while this
    /// entity is mid-update.
    fn with_native(&self, cx: &mut Context<Self>, f: impl FnOnce(NativeWindow) + 'static) {
        if let Some(native) = self.native {
            cx.defer(move |_| f(native));
        }
    }

    fn configure_window(&mut self, cx: &mut Context<Self>) {
        let Some(native) = self.native else { return };
        // The whole point of the app.
        native.set_content_protection(self.cfg.window.protection);
        native.set_opacity(self.cfg.window.opacity);
        native.set_ignore_mouse(self.cfg.window.click_through);
        let bounds = self.usable_bounds();
        self.with_native(cx, move |native| {
            native.configure_overlay(1, true);
            native.set_frame(bounds);
        });
    }

    fn default_bounds() -> Rect {
        let area = platform::primary_display().map(|d| d.work_area).unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        });
        Rect {
            x: area.x + area.width - DEFAULT_SIZE.0 - MARGIN,
            y: area.y + MARGIN,
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
        }
    }

    /// A saved position is only usable if some display still contains it —
    /// otherwise unplugging a monitor leaves the panel parked offscreen with
    /// no way to click it back.
    fn usable_bounds(&self) -> Rect {
        let Some(saved) = self.cfg.window.bounds else { return Self::default_bounds() };
        let visible = platform::displays().iter().any(|d| {
            let a = d.work_area;
            saved.x + saved.width > a.x + 40.0
                && saved.x < a.x + a.width - 40.0
                && saved.y + 20.0 >= a.y
                && saved.y < a.y + a.height - 40.0
        });
        if visible {
            Rect { x: saved.x, y: saved.y, width: saved.width.max(MIN_SIZE.0), height: saved.height.max(MIN_SIZE.1) }
        } else {
            Self::default_bounds()
        }
    }

    fn queue_save_bounds(&mut self, cx: &mut Context<Self>) {
        self.save_bounds = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let _ = this.update(cx, |this, _| {
                let Some(native) = this.native else { return };
                if !native.is_visible() {
                    return;
                }
                let f = native.frame();
                this.cfg.window.bounds =
                    Some(config::WindowBounds { x: f.x, y: f.y, width: f.width, height: f.height });
                this.save_config();
            });
        }));
    }

    pub fn is_visible(&self) -> bool {
        self.native.map(|n| n.is_visible()).unwrap_or(true)
    }

    pub fn show(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.with_native(cx, move |native| {
            native.show(focus);
            native.configure_overlay(1, true);
        });
        // Keep focus where it was inside the panel (usually the ask box);
        // only claim it when nothing in here has it.
        if focus && !self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.with_native(cx, |native| native.hide());
        cx.notify();
    }

    pub fn toggle_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_visible() {
            self.hide(cx);
        } else {
            self.show(true, window, cx);
        }
    }

    pub fn move_by(&mut self, dx: f64, dy: f64, cx: &mut Context<Self>) {
        self.with_native(cx, move |native| {
            let f = native.frame();
            native.set_frame(Rect { x: f.x + dx, y: f.y + dy, ..f });
        });
        // Programmatic moves do not always reach GPUI's bounds observer.
        self.queue_save_bounds(cx);
    }

    pub fn set_opacity(&mut self, value: f32, cx: &mut Context<Self>) -> f32 {
        let next = if value.is_finite() { value.clamp(0.25, 1.0) } else { 1.0 };
        self.cfg.window.opacity = next;
        if let Some(native) = self.native {
            native.set_opacity(next);
        }
        self.save_config();
        cx.notify();
        next
    }

    pub fn set_click_through(&mut self, on: bool, cx: &mut Context<Self>) {
        self.cfg.window.click_through = on;
        if let Some(native) = self.native {
            native.set_ignore_mouse(on);
        }
        self.save_config();
        self.refresh_tray();
        cx.notify();
    }

    pub fn set_protection(&mut self, on: bool, cx: &mut Context<Self>) {
        self.cfg.window.protection = on;
        if let Some(native) = self.native {
            native.set_content_protection(on);
        }
        self.save_config();
        self.refresh_tray();
        cx.notify();
    }

    /* ---------- config ---------- */

    pub fn save_config(&self) {
        if let Err(err) = config::save(&self.cfg) {
            eprintln!("[config] could not save: {err:#}");
        }
    }

    /// Apply a settings change: save, and re-register hotkeys when they moved.
    pub fn update_config(&mut self, window: &mut Window, cx: &mut Context<Self>, change: impl FnOnce(&mut Config)) {
        let before = self.cfg.clone();
        change(&mut self.cfg);
        if self.cfg == before {
            return;
        }
        self.save_config();
        if before.hotkeys != self.cfg.hotkeys || before.prompts != self.cfg.prompts {
            self.reapply_hotkeys(window, cx);
        }
        if before.window != self.cfg.window {
            if let Some(native) = self.native {
                native.set_content_protection(self.cfg.window.protection);
                native.set_opacity(self.cfg.window.opacity);
                native.set_ignore_mouse(self.cfg.window.click_through);
            }
        }
        self.refresh_tray();
        cx.notify();
    }

    fn refresh_tray(&self) {
        if let Some(tray) = &self.tray {
            tray.refresh(&self.cfg);
        }
    }

    pub fn reapply_hotkeys(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.hotkey_errors = self.hotkeys.apply(&self.cfg);
        if !self.hotkey_errors.is_empty() {
            eprintln!(
                "[hotkeys] unavailable: {}",
                self.hotkey_errors.iter().map(|f| f.accelerator.as_str()).collect::<Vec<_>>().join(", ")
            );
            let list = self
                .hotkey_errors
                .iter()
                .map(|f| format!("{} ({})", pretty(&f.accelerator), f.label))
                .collect::<Vec<_>>()
                .join(", ");
            self.toast(
                ToastKind::Warn,
                format!("Some hotkeys could not be registered — another app owns them: {list}"),
                cx,
            );
        }
    }

    /// While a hotkey field captures keys, the global shortcuts step aside.
    pub fn suspend_hotkeys(&mut self) {
        self.hotkeys.suspend();
    }

    pub fn set_key(&mut self, provider: &str, key: &str, cx: &mut Context<Self>) {
        match self.secrets.set(provider, key) {
            Ok(()) => self.toast(ToastKind::Info, "API key saved.", cx),
            Err(err) => self.toast(ToastKind::Error, format!("Could not save the API key: {err:#}"), cx),
        }
        cx.notify();
    }

    /* ---------- external events ---------- */

    pub fn handle_external(&mut self, event: External, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            External::Tray(cmd) => match cmd {
                TrayCommand::ToggleOverlay => self.toggle_overlay(window, cx),
                TrayCommand::RecordToggle => self.record_toggle(window, cx),
                TrayCommand::NewSession => self.new_session(cx),
                TrayCommand::ToggleProtection => self.toggle_protection(cx),
                TrayCommand::ClickThrough => self.toggle_click_through(cx),
                TrayCommand::Quit => cx.quit(),
            },
            External::Hotkey(id) => {
                if let Some(action) = self.hotkeys.action(id) {
                    self.run_action(action, window, cx);
                }
            }
        }
    }

    pub fn run_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Prompt(id) => self.run_prompt(id, window, cx),
            Action::Builtin(id) => match id {
                HotkeyId::ToggleOverlay => self.toggle_overlay(window, cx),
                HotkeyId::ShotOnly => self.shot_only(window, cx),
                HotkeyId::RegionShot => self.region_shot(window, cx),
                HotkeyId::RecordToggle => self.record_toggle(window, cx),
                HotkeyId::Send => self.focus_input(window, cx),
                HotkeyId::Cancel => {
                    if self.cancel(cx) {
                        self.toast(ToastKind::Info, "Stopped.", cx);
                    }
                }
                HotkeyId::NewSession => self.new_session(cx),
                HotkeyId::ClickThrough => self.toggle_click_through(cx),
                HotkeyId::ToggleProtection => self.toggle_protection(cx),
                HotkeyId::OpacityUp => self.nudge_opacity(0.08, cx),
                HotkeyId::OpacityDown => self.nudge_opacity(-0.08, cx),
                HotkeyId::MoveUp => self.move_by(0.0, -48.0, cx),
                HotkeyId::MoveDown => self.move_by(0.0, 48.0, cx),
                HotkeyId::MoveLeft => self.move_by(-48.0, 0.0, cx),
                HotkeyId::MoveRight => self.move_by(48.0, 0.0, cx),
            },
        }
    }

    /* ---------- actions ---------- */

    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show(true, window, cx);
        self.close_settings(window, cx);
        self.input.update(cx, |input, cx| {
            input.focus(window);
            input.move_to_end(cx);
        });
    }

    pub fn toggle_click_through(&mut self, cx: &mut Context<Self>) {
        let next = !self.cfg.window.click_through;
        self.set_click_through(next, cx);
        self.toast(
            ToastKind::Info,
            if next { "Click-through on — the panel ignores the mouse." } else { "Click-through off." },
            cx,
        );
    }

    pub fn toggle_protection(&mut self, cx: &mut Context<Self>) {
        let next = !self.cfg.window.protection;
        self.set_protection(next, cx);
        if next {
            self.toast(ToastKind::Info, "Capture protection on — hidden from screen share.", cx);
        } else {
            self.toast(ToastKind::Warn, "Capture protection OFF — this panel will appear in a screen share.", cx);
        }
    }

    pub fn nudge_opacity(&mut self, delta: f32, cx: &mut Context<Self>) {
        let value = self.set_opacity(self.cfg.window.opacity + delta, cx);
        self.toast(ToastKind::Info, format!("Opacity {}%", (value * 100.0).round()), cx);
    }

    pub fn new_session(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
        self.session.clear();
        self.cards.clear();
        self.attachments.clear();
        self.toast(ToastKind::Info, "New session.", cx);
        cx.notify();
    }

    pub fn clear_conversation(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
        self.session.clear();
        self.cards.clear();
        self.attachments.clear();
        cx.notify();
    }

    pub fn clear_transcript(&mut self, cx: &mut Context<Self>) {
        self.session.clear_transcript();
        cx.notify();
    }

    pub fn run_prompt(&mut self, prompt_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = self.cfg.prompt(&prompt_id).cloned() else {
            self.toast(ToastKind::Error, format!("Prompt \"{prompt_id}\" no longer exists."), cx);
            return;
        };
        // Shown without focus so the meeting app keeps the keyboard.
        self.show(false, window, cx);

        if preset.source.wants_transcript() && self.session.transcript_text().is_empty() {
            let key = pretty(&self.cfg.hotkeys.record_toggle);
            self.toast(ToastKind::Warn, format!("Transcript is empty — start recording with {key}."), cx);
            return;
        }

        if !preset.source.wants_shot() {
            self.ask(Some(prompt_id), String::new(), Vec::new(), window, cx);
            return;
        }

        let shot = self.take_shot(ShotMode::Full, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let shot = shot.await;
            let _ = this.update_in(cx, |this, window, cx| match shot {
                Ok(Some(shot)) => this.ask(Some(prompt_id), String::new(), vec![shot], window, cx),
                Ok(None) => {}
                Err(err) => this.toast(ToastKind::Error, err.to_string(), cx),
            });
        })
        .detach();
    }

    pub fn shot_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show(true, window, cx);
        let shot = self.take_shot(ShotMode::Full, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let shot = shot.await;
            let _ = this.update_in(cx, |this, window, cx| match shot {
                Ok(Some(_)) => this.focus_input(window, cx),
                Ok(None) => {}
                Err(err) => this.toast(ToastKind::Error, err.to_string(), cx),
            });
        })
        .detach();
    }

    pub fn region_shot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.region_open {
            return;
        }
        let prompt = self.cfg.capture.region_prompt.clone();
        let shot = self.take_shot(ShotMode::Region, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let shot = shot.await;
            let _ = this.update_in(cx, |this, window, cx| match shot {
                Ok(Some(shot)) => {
                    this.show(false, window, cx);
                    if !prompt.is_empty() && this.cfg.prompt(&prompt).is_some() {
                        this.ask(Some(prompt), String::new(), vec![shot], window, cx);
                    } else {
                        this.focus_input(window, cx);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    this.show(false, window, cx);
                    this.toast(ToastKind::Error, err.to_string(), cx);
                }
            });
        })
        .detach();
    }

    /// Grab the screen (or a region of it), store the shot, and attach it.
    fn take_shot(
        &mut self,
        mode: ShotMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Option<Arc<Shot>>>> {
        let pref = match mode {
            ShotMode::Full => self.cfg.capture.display.clone(),
            ShotMode::Region => "cursor".into(),
        };
        let max_dimension = self.cfg.capture.max_dimension;
        let keep_last = self.cfg.capture.keep_last;
        // Content protection already keeps the panel out of the frame. With
        // it off, the panel hides for the grab so the screenshot is not a
        // picture of the screenshot tool.
        let must_hide = self.is_visible() && !self.cfg.window.protection;
        if matches!(mode, ShotMode::Region) {
            self.region_open = true;
        }
        self.capturing += 1;

        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let display = capture::pick_display(&pref).ok_or_else(|| anyhow::anyhow!("No display found"))?;
                if must_hide {
                    this.update(cx, |this, cx| this.hide(cx))?;
                    cx.background_executor().timer(Duration::from_millis(120)).await;
                }
                let grabbed = cx.background_spawn(async move { capture::grab(&display) }).await;
                if must_hide {
                    this.update_in(cx, |this, window, cx| this.show(false, window, cx))?;
                }
                let frame = grabbed?;

                let image = match mode {
                    ShotMode::Full => Some(frame.image),
                    ShotMode::Region => {
                        let frame = Arc::new(frame);
                        let rect = RegionPicker::pick(frame.clone(), cx).await;
                        match rect {
                            RegionResult::Selected(rect) => capture::crop(&frame, rect),
                            RegionResult::Cancelled => None,
                        }
                    }
                };
                let Some(image) = image else { return anyhow::Ok(None) };
                let shot = cx.background_spawn(async move { capture::finish(image, max_dimension) }).await?;
                let shot = this.update(cx, |this, cx| {
                    let shot = this.session.add_shot(shot, keep_last);
                    this.attachments.push(shot.clone());
                    if this.attachments.len() > 4 {
                        let drop = this.attachments.len() - 4;
                        this.attachments.drain(..drop);
                    }
                    cx.notify();
                    shot
                })?;
                anyhow::Ok(Some(shot))
            }
            .await;
            let _ = this.update(cx, |this, _| {
                this.capturing = this.capturing.saturating_sub(1);
                if matches!(mode, ShotMode::Region) {
                    this.region_open = false;
                }
            });
            result
        })
    }

    /* ---------- asking ---------- */

    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        let shots = std::mem::take(&mut self.attachments);
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.ask(None, text, shots, window, cx);
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(active) = self.active.take() else { return false };
        active.cancel.cancel();
        cx.notify();
        true
    }

    fn fail(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.seq += 1;
        self.cards.push(Card {
            id: self.seq,
            label: "Error".into(),
            accelerator: None,
            meta: clock(capture::now_ms()),
            shots: Vec::new(),
            thinking: String::new(),
            notes: Vec::new(),
            text: String::new(),
            error: Some(message.into()),
            done: true,
        });
        self.readout_scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn ask(
        &mut self,
        prompt_id: Option<String>,
        input: String,
        shots: Vec<Arc<Shot>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel(cx);
        self.close_settings(window, cx);
        // Shots sent with this ask are consumed, so a follow-up question
        // does not upload them a second time.
        self.attachments.retain(|a| !shots.iter().any(|s| s.id == a.id));

        let provider = resolve_chat(&self.cfg, &|id| self.secrets.get(id));
        let transcript = self.session.transcript_text();
        let prompt = ai::build_prompt(&self.cfg, prompt_id.as_deref(), &input, &transcript);

        if prompt.text.is_empty() {
            return self.fail("Nothing to ask — type something or attach a screenshot.", cx);
        }
        if provider.kind == ProviderKind::WhisperCpp {
            return self.fail("Local (whisper.cpp) only transcribes — pick a chat provider in Settings.", cx);
        }
        if provider.needs_key && provider.api_key.is_empty() {
            return self.fail(format!("No API key for {}. Open settings › Keys and paste one.", provider.label), cx);
        }
        // Claude Code picks its own model when none is named, so a blank is valid.
        if provider.model.is_empty() && provider.kind != ProviderKind::ClaudeCode {
            return self.fail(format!("No model set for {}. Add one in Settings.", provider.label), cx);
        }

        let mut notes = Vec::new();
        let mut images: Vec<ImagePart> =
            shots.iter().map(|s| ImagePart { mime: s.mime(), base64: s.base64.clone() }).collect();
        if !images.is_empty() && !provider.vision {
            notes.push(format!(
                "{} ({}) has no vision support — the screenshot was not sent.",
                provider.label, provider.model
            ));
            images.clear();
        }

        self.session.push_turn(Turn { role: Role::User, text: prompt.text.clone(), images });

        self.seq += 1;
        let id = self.seq;
        let model = prompt.model.clone().unwrap_or_else(|| provider.model.clone());
        let accelerator = prompt_id
            .as_deref()
            .and_then(|pid| self.cfg.prompt(pid))
            .map(|p| p.accelerator.clone())
            .filter(|a| !a.is_empty());
        let at = capture::now_ms();
        self.cards.push(Card {
            id,
            label: prompt.label.clone(),
            accelerator,
            meta: [model.clone(), clock(at)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
            shots: shots.clone(),
            thinking: String::new(),
            notes,
            text: String::new(),
            error: None,
            done: false,
        });
        self.readout_scroll.scroll_to_bottom();

        let effort = prompt.effort.clone().or_else(|| Some(self.cfg.chat.effort.clone()).filter(|e| !e.is_empty()));
        let claude = (provider.kind == ProviderKind::ClaudeCode).then(|| self.session.claude_session());
        let history = self.session.history();
        let opts = StreamOptions {
            kind: provider.kind,
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            model,
            system: self.cfg.chat.system_prompt.clone(),
            // Claude Code keeps the history itself, so it gets the newest turn only.
            messages: if claude.is_some() { history.last().cloned().into_iter().collect() } else { history },
            max_tokens: if self.cfg.chat.max_tokens == 0 { 8192 } else { self.cfg.chat.max_tokens },
            effort: matches!(provider.kind, ProviderKind::Anthropic | ProviderKind::ClaudeCode)
                .then_some(effort)
                .flatten(),
            show_thinking: self.cfg.chat.show_thinking,
            server_side_fallback: self.cfg.chat.server_side_fallback,
            claude_code: claude.as_ref().map(|handle| {
                let cc = &self.cfg.chat.claude_code;
                ClaudeCodeOptions {
                    binary_path: cc.binary_path.clone(),
                    cwd: cc.cwd.clone(),
                    fast_start: cc.fast_start,
                    allow_tools: cc.allow_tools,
                    max_budget_usd: cc.max_budget_usd,
                    session_id: (!handle.started).then(|| handle.id.clone()),
                    resume_id: handle.started.then(|| handle.id.clone()),
                }
            }),
        };
        let is_claude = claude.is_some();

        let running = ai::start(opts);
        self.active = Some(ActiveAsk { id, cancel: running.cancel.clone() });
        self.start_blink(cx);
        cx.notify();

        let mut events = running.events;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                let keep_going =
                    this.update(cx, |this, cx| this.on_stream_event(id, is_claude, event, cx)).unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }

    /// Returns whether more events are expected.
    fn on_stream_event(&mut self, id: u64, is_claude: bool, event: StreamEvent, cx: &mut Context<Self>) -> bool {
        let is_current = self.active.as_ref().is_some_and(|a| a.id == id);
        let at_bottom = self.readout_at_bottom();
        let Some(card) = self.cards.iter_mut().find(|c| c.id == id) else { return false };

        let terminal = match event {
            StreamEvent::Chunk(Chunk::Text(text)) => {
                if !is_current {
                    return false;
                }
                card.text.push_str(&text);
                false
            }
            StreamEvent::Chunk(Chunk::Thinking(text)) => {
                if !is_current {
                    return false;
                }
                card.thinking.push_str(&text);
                false
            }
            StreamEvent::Chunk(Chunk::Note(text)) => {
                card.notes.push(text);
                false
            }
            StreamEvent::Chunk(Chunk::Done(done)) => {
                let answer = card.text.clone();
                // The real model and cost are only known once the turn ends —
                // Claude Code in particular picks its own model unless named.
                let mut bits = vec![done.model.clone().unwrap_or_default(), clock(capture::now_ms())];
                if let Some(cost) = done.cost_usd {
                    bits.push(format!("${cost:.4}"));
                }
                card.meta = bits.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                card.done = true;
                self.session.push_turn(Turn { role: Role::Assistant, text: answer, images: Vec::new() });
                // Only now is the Claude Code session real and resumable; a
                // failed first attempt must still be able to name it.
                if is_claude {
                    self.session.mark_claude_started();
                }
                true
            }
            StreamEvent::Failed(message) => {
                card.error = Some(message);
                card.done = true;
                true
            }
            StreamEvent::Cancelled => {
                let answer = card.text.clone();
                card.notes.push("Stopped.".into());
                card.done = true;
                if !answer.is_empty() {
                    self.session.push_turn(Turn { role: Role::Assistant, text: answer, images: Vec::new() });
                }
                true
            }
            StreamEvent::Incomplete => {
                let answer = card.text.clone();
                card.notes.push("The connection ended before the answer finished.".into());
                card.done = true;
                self.session.push_turn(Turn { role: Role::Assistant, text: answer, images: Vec::new() });
                true
            }
        };
        if terminal && is_current {
            self.active = None;
        }
        if at_bottom || terminal {
            self.readout_scroll.scroll_to_bottom();
        }
        cx.notify();
        !terminal
    }

    fn readout_at_bottom(&self) -> bool {
        let offset = -self.readout_scroll.offset().y;
        let max = self.readout_scroll.max_offset().height;
        max - offset < px(60.)
    }

    fn start_blink(&mut self, cx: &mut Context<Self>) {
        self.caret_visible = true;
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let streaming = this
                    .update(cx, |this, cx| {
                        this.caret_visible = !this.caret_visible;
                        cx.notify();
                        this.active.is_some()
                    })
                    .unwrap_or(false);
                if !streaming {
                    break;
                }
            }
        }));
    }

    pub fn is_streaming(&self) -> bool {
        self.active.is_some()
    }

    /* ---------- transcription ---------- */

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn record_toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show(false, window, cx);
        if self.recording.is_some() {
            self.stop_recording(cx);
        } else {
            self.start_recording(window, cx);
        }
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let permission = platform::microphone_permission();
        if matches!(permission, Permission::Denied | Permission::Restricted) {
            self.toast(
                ToastKind::Error,
                "Microphone access is blocked. Grant it in System Settings › Privacy & Security › Microphone.",
                cx,
            );
            return;
        }
        if permission == Permission::NotDetermined {
            let answer = platform::request_microphone();
            cx.spawn_in(window, async move |this, cx| {
                let granted = answer.await.unwrap_or(false);
                let _ = this.update_in(cx, |this, window, cx| {
                    if granted {
                        this.start_recording(window, cx);
                    } else {
                        this.toast(
                            ToastKind::Error,
                            "Microphone access is blocked. Grant it in System Settings › Privacy & Security › Microphone.",
                            cx,
                        );
                    }
                });
            })
            .detach();
            return;
        }

        let device = self.cfg.transcription.input_device_id.clone();
        let system_audio = self.cfg.transcription.capture_system_audio;
        let seconds = self.cfg.transcription.chunk_seconds;
        let (recorder, warning) = match Recorder::start(&device, system_audio) {
            Ok(r) => r,
            Err(err) => {
                self.toast(ToastKind::Error, format!("Could not open the microphone: {err:#}"), cx);
                return;
            }
        };
        if let Some(warning) = warning {
            self.toast(ToastKind::Error, warning, cx);
        }

        // Rolling chunks: the buffer is taken every N seconds, so a long call
        // gets a running transcript and the local model stays warm.
        let rollover = (seconds >= 5).then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(seconds as u64)).await;
                    let alive = this
                        .update(cx, |this, cx| {
                            if this.recording.is_some() {
                                this.flush_segment(cx);
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if !alive {
                        break;
                    }
                }
            })
        });
        self.recording = Some(Recording { recorder, rollover });
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.flush_segment(cx);
        if let Some(mut recording) = self.recording.take() {
            recording.rollover.take();
        }
        cx.notify();
    }

    /// Send whatever has been recorded so far to transcription.
    fn flush_segment(&mut self, cx: &mut Context<Self>) {
        let Some(recording) = &self.recording else { return };
        let error = recording.recorder.error();
        let parts = recording.recorder.take();
        if let Some(error) = error {
            self.toast(ToastKind::Error, format!("Recorder error: {error}"), cx);
        }
        let job = match transcribe::job(&self.cfg, &self.secrets) {
            Ok(job) => job,
            Err(err) => {
                self.toast(ToastKind::Error, err.to_string(), cx);
                return;
            }
        };
        self.uploading += 1;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let wav = cx.background_spawn(async move { audio::encode_segment(parts) }).await;
            let result = match wav {
                // Too short to be speech; a mis-tap on the hotkey.
                None => Ok(String::new()),
                Some(wav) => crate::runtime::spawn(transcribe::run(job, wav))
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("transcription task failed: {e}"))),
            };
            let _ = this.update(cx, |this, cx| {
                this.uploading = this.uploading.saturating_sub(1);
                match result {
                    Ok(text) => this.append_transcript(&text, cx),
                    Err(err) => this.toast(ToastKind::Error, format!("{err:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn append_transcript(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.session.append_transcript(text, capture::now_ms()).is_some() {
            self.drawer_open = true;
            self.transcript_scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    /* ---------- toasts ---------- */

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        let text = text.into();
        eprintln!("[toast:{kind:?}] {text}");
        self.toasts.push(Toast { id, kind, text });
        let ttl = if kind == ToastKind::Error { 7000 } else { 3600 };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(ttl)).await;
            let _ = this.update(cx, |this, cx| {
                this.toasts.retain(|t| t.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /* ---------- settings ---------- */

    pub fn open_settings(&mut self, tab: Option<SettingsTab>, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.entity().downgrade();
        match &self.settings {
            Some(view) => {
                if let Some(tab) = tab {
                    view.update(cx, |v, cx| v.set_tab(tab, window, cx));
                }
            }
            None => {
                let view = cx.new(|cx| SettingsView::new(panel, tab.unwrap_or(SettingsTab::Ai), window, cx));
                self.settings = Some(view);
            }
        }
        cx.notify();
    }

    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.settings.take() {
            // Save whatever is still being typed: a field only commits on
            // blur, and the view is gone before that blur would arrive. Run
            // after this update, since committing writes back to the panel.
            window.defer(cx, move |window, cx| view.update(cx, |view, cx| view.commit_all(window, cx)));
            cx.notify();
        }
    }

    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(settings) = &self.settings {
            if settings.read(cx).wants_escape() {
                settings.update(cx, |s, cx| s.close_popups(cx));
                return;
            }
            self.close_settings(window, cx);
            window.focus(&self.focus_handle);
            return;
        }
        if self.is_streaming() {
            self.cancel(cx);
            return;
        }
        self.hide(cx);
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(None, window, cx);
    }

    /* ---------- resize grip ---------- */

    fn grip_down(&mut self, _: &gpui::MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if let Some(native) = self.native {
            self.resize = Some(Resize { start_frame: native.frame(), start_cursor: platform::cursor_position() });
        }
    }

    fn grip_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(resize) = &self.resize else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.resize = None;
            return;
        }
        let (x, y) = platform::cursor_position();
        let f = resize.start_frame;
        let next = Rect {
            width: (f.width + x - resize.start_cursor.0).max(MIN_SIZE.0),
            height: (f.height + y - resize.start_cursor.1).max(MIN_SIZE.1),
            ..f
        };
        self.with_native(cx, move |native| native.set_frame(next));
    }

    fn grip_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.resize.take().is_some() {
            self.queue_save_bounds(cx);
        }
    }

    /* ---------- rendering ---------- */

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hidden = self.cfg.window.protection;
        let veil_color = if hidden { theme::lamp() } else { theme::live() };
        let (rec_label, rec_color) = if self.is_recording() {
            ("recording", theme::live())
        } else if self.uploading > 0 {
            ("transcribing", theme::lamp())
        } else {
            ("idle", theme::haze())
        };
        let streaming = self.is_streaming();

        let dot = |color: gpui::Rgba| div().flex_none().size(px(5.)).rounded_full().bg(color);
        let rec_dot = if self.is_recording() {
            dot(rec_color)
                .with_animation("rec-pulse", Animation::new(Duration::from_millis(1100)).repeat(), |d, t| {
                    d.opacity(0.25 + 0.75 * ((t * std::f32::consts::TAU).cos() * 0.5 + 0.5))
                })
                .into_any_element()
        } else {
            dot(rec_color).into_any_element()
        };

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

        div()
            .id("rail")
            .relative()
            .flex_none()
            .h(px(theme::RAIL_H))
            .flex()
            .items_center()
            .gap(px(9.))
            .pl(px(theme::PAD))
            .pr(px(10.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.with_native(cx, |native| native.begin_drag());
                    // The drag runs its own loop; the save fires once it ends.
                    this.queue_save_bounds(cx);
                }),
            )
            .child(img(ImageSource::Render(self.mark.clone())).flex_none().size(px(11.)))
            .child(div().flex_1().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).child("VeilPilot"))
            .child(swallow_mouse_down(
                div()
                    .id("veil-state")
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .text_color(veil_color)
                    .child(dot(veil_color).when(hidden, |d| {
                        d.shadow(vec![gpui::BoxShadow {
                            color: theme::hsla(theme::lamp()).opacity(0.7),
                            offset: gpui::point(px(0.), px(0.)),
                            blur_radius: px(6.),
                            spread_radius: px(0.),
                        }])
                    }))
                    .child(caps(if hidden { "invisible" } else { "visible" }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let next = !this.cfg.window.protection;
                        this.set_protection(next, cx);
                    })),
            ))
            .child(swallow_mouse_down(
                div()
                    .id("rec-state")
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .text_color(rec_color)
                    .hover(|s| s.text_color(theme::read()))
                    .child(rec_dot)
                    .child(caps(rec_label))
                    .on_click(cx.listener(|this, _, window, cx| this.record_toggle(window, cx))),
            ))
            .child(chrome_button(
                "gear",
                "settings",
                cx.listener(|this, _, window, cx| this.open_settings(None, window, cx)),
            ))
            .child(seam)
    }

    fn render_legend(&self) -> impl IntoElement {
        let h = &self.cfg.hotkeys;
        let mut rows: Vec<(String, String)> = self
            .cfg
            .prompts
            .iter()
            .map(|p| {
                let what =
                    if p.source == config::PromptSource::Transcript { "from transcript" } else { "screenshot + ask" };
                (p.accelerator.clone(), format!("{} — {what}", p.label))
            })
            .collect();
        rows.extend([
            (h.region_shot.clone(), "Select a region, then ask".into()),
            (h.shot_only.clone(), "Attach a screenshot, type your own question".into()),
            (h.record_toggle.clone(), "Start / stop transcribing".into()),
            (h.toggle_overlay.clone(), "Hide or show this panel".into()),
            (h.cancel.clone(), "Stop generating".into()),
        ]);

        let chat = &self.cfg.chat;
        let preset = provider_by_id(&chat.provider);
        let label = preset.map(|p| p.label).unwrap_or(chat.provider.as_str()).to_string();
        let model =
            if chat.model.is_empty() { preset.map(|p| p.model).unwrap_or("").to_string() } else { chat.model.clone() };
        let keyed = self.secrets.has(&chat.provider) || preset.is_some_and(|p| !p.needs_key);

        let foot = if keyed {
            div()
                .flex()
                .flex_wrap()
                .child("Answering with ")
                .child(div().text_color(theme::lamp()).font_weight(FontWeight::MEDIUM).child(label))
                .when(!model.is_empty(), |d| d.child(format!(" · {model}")))
        } else {
            div()
                .flex()
                .flex_wrap()
                .child("No API key for ")
                .child(div().text_color(theme::lamp()).font_weight(FontWeight::MEDIUM).child(label))
                .child(" yet — open ")
                .child(div().text_color(theme::lamp()).font_weight(FontWeight::MEDIUM).child("settings"))
                .child(" to add one.")
        };

        div()
            .text_color(theme::haze())
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::read())
                    .mb(px(14.))
                    .child("Nothing asked yet. Press a key."),
            )
            .child(div().flex().flex_col().gap(px(7.)).children(rows.into_iter().filter(|(a, _)| !a.is_empty()).map(
                |(accel, what)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(11.))
                        .child(div().w(px(46.)).flex_none().flex().child(key_badge(pretty(&accel))))
                        .child(div().flex_1().min_w_0().text_size(px(11.5)).child(what))
                },
            )))
            .child(
                div()
                    .mt(px(16.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(theme::edge())
                    .text_size(px(11.))
                    .line_height(px(16.5))
                    .child(foot),
            )
    }

    fn render_card(&self, card: &Card, first: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let streaming = !card.done && self.active.as_ref().is_some_and(|a| a.id == card.id);
        let body: Vec<gpui::AnyElement> = if let Some(error) = &card.error {
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

        div()
            .pb(px(20.))
            .when(!first, |d| d.border_t_1().border_color(theme::edge()).pt(px(16.)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .mb(px(10.))
                    .text_color(theme::haze())
                    .when_some(card.accelerator.clone(), |d, a| d.child(key_badge(pretty(&a))))
                    .child(caps(&card.label).text_color(theme::read()))
                    .child(
                        div().ml_auto().font_family(theme::mono_family()).text_size(px(9.5)).child(card.meta.clone()),
                    ),
            )
            .children(card.shots.iter().map(|s| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .mb(px(11.))
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
                        .mb(px(11.))
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
                    .map(|n| div().mb(px(10.)).text_size(px(11.)).text_color(theme::lamp()).child(n.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(9.5))
                    .max_w(px(62. * 7.2))
                    .text_size(px(13.5))
                    .line_height(px(21.9))
                    .text_color(theme::read())
                    .children(body),
            )
            .when(card.done && card.error.is_none() && !card.text.is_empty(), |d| {
                d.child(div().mt(px(11.)).flex().child(chrome_button(
                    ("copy", id as usize),
                    "copy",
                    cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                        this.toast(ToastKind::Info, "Copied.", cx);
                    }),
                )))
            })
    }

    fn render_drawer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.session.transcript_lines();
        let count = if lines.is_empty() { "empty".to_string() } else { format!("{} lines", lines.len()) };
        let last = lines.len().saturating_sub(1);
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme::edge())
            .when(self.drawer_open, |d| d.max_h(relative(0.38)))
            .child(
                div()
                    .id("drawer-head")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(theme::PAD))
                    .py(px(9.))
                    .cursor_pointer()
                    .text_color(theme::haze())
                    .hover(|s| s.text_color(theme::read()))
                    .child(caps("transcript"))
                    .child(div().ml_auto().font_family(theme::mono_family()).text_size(px(9.5)).child(count))
                    .child(div().text_size(px(8.)).child(if self.drawer_open { "▲" } else { "▼" }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.drawer_open = !this.drawer_open;
                        cx.notify();
                    })),
            )
            .when(self.drawer_open, |d| {
                d.child(
                    div()
                        .id("transcript")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.transcript_scroll)
                        .px(px(theme::PAD))
                        .pb(px(10.))
                        .font_family(theme::mono_family())
                        .text_size(px(11.))
                        .line_height(px(17.6))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .children(lines.iter().enumerate().map(|(i, line)| {
                            div()
                                .flex()
                                .gap(px(9.))
                                .child(div().flex_none().text_color(theme::edge()).child(stamp(line.at)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_color(if i == last { theme::read() } else { theme::haze() })
                                        .child(line.text.clone()),
                                )
                        })),
                )
            })
    }

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
                    .pt(px(10.))
                    .pb(px(11.))
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
            .bottom(px(56.))
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
        let cards: Vec<gpui::AnyElement> = {
            let cards = self.cards.clone();
            cards.iter().enumerate().map(|(i, c)| self.render_card(c, i == 0, cx).into_any_element()).collect()
        };
        let readout = div()
            .id("readout")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.readout_scroll)
            .p(px(theme::PAD))
            .when(cards.is_empty(), |d| d.child(self.render_legend()))
            .children(cards);

        div()
            .id("shell")
            .key_context("Panel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_open_settings))
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
            .child(self.render_rail(cx))
            .child(readout)
            .child(self.render_drawer(cx))
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
            .child(self.render_toasts())
            .when_some(self.settings.clone(), |d, settings| {
                d.child(div().absolute().top_0().left_0().size_full().child(settings))
            })
    }
}

/* ---------- formatting ---------- */

pub fn clock(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

pub fn stamp(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_default()
}

/// Drive the panel from platform-thread events.
pub fn pump_external(
    panel: WeakEntity<Panel>,
    mut rx: futures::channel::mpsc::UnboundedReceiver<External>,
    cx: &mut AsyncWindowContext,
) -> impl std::future::Future<Output = ()> + '_ {
    async move {
        while let Some(event) = rx.next().await {
            if panel.update_in(cx, |panel, window, cx| panel.handle_external(event, window, cx)).is_err() {
                break;
            }
        }
    }
}
