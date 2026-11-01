//! The panel: state, and the actions hotkeys, the tray and clicks fire.
//! Rendering is in `view`, live answers / summaries / memory in `live`, and
//! agent + project resolution in `agents`.

mod agents;
mod live;
mod updates;
mod view;

pub use live::LiveState;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{
    App, AsyncWindowContext, Context, Entity, FocusHandle, KeyBinding, MouseButton, MouseMoveEvent, MouseUpEvent,
    ScrollHandle, Subscription, Task, WeakEntity, Window, actions, prelude::*, px,
};
use tokio_util::sync::CancellationToken;

use super::region::{RegionPicker, RegionResult};
use super::settings::{SettingsTab, SettingsView};
use super::text_input::{InputEvent, TextInput};
use crate::ai::{self, BuiltPrompt, Chunk, StreamEvent};
use crate::audio::{self, Recorder};
use crate::capture::{self, Shot};
use crate::config::{self, Config, HotkeyId, ProviderKind};
use crate::hotkeys::{Action, Failure, Hotkeys, pretty};
use crate::platform::{self, NativeWindow, Permission, Rect};
use crate::secrets::Secrets;
use crate::session::{ImagePart, Role, Session, Turn};
use crate::transcribe;
use crate::tray::{Tray, TrayCommand};

actions!(panel, [Escape, OpenSettings, NextTab, PrevTab]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Escape, Some("Panel")),
        KeyBinding::new("secondary-,", OpenSettings, Some("Panel")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Panel")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Panel")),
    ]);
}

pub const DEFAULT_SIZE: (f64, f64) = (430.0, 580.0);
pub const MIN_SIZE: (f64, f64) = (320.0, 240.0);
/// Height of the minimized panel: the title bar alone.
pub const MINI_HEIGHT: f64 = 40.0;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    Ask,
    /// A transcript summary; its text is what "Save to memory" saves.
    Summary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Live,
    Transcript,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Chat, Tab::Live, Tab::Transcript];

    pub fn name(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::Live => "Live",
            Tab::Transcript => "Transcript",
        }
    }
}

/// The header's open dropdown, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Context,
    Agent,
}

/// The newest answer, wherever it lives — what the notch island shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latest {
    Card(u64),
    Live(u64),
}

#[derive(Clone, Debug)]
pub struct Card {
    pub id: u64,
    pub kind: CardKind,
    /// What was typed, for typed asks (presets show their label instead).
    pub prompt: String,
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
    /// Watches the recorder for pauses and length limits.
    _poller: Task<()>,
    /// Segments in the order they were spoken; one worker transcribes them
    /// one at a time, so lines land in the transcript in order.
    queue: futures::channel::mpsc::UnboundedSender<Vec<(Vec<f32>, u32)>>,
    error_reported: bool,
}

/// How often the recorder is checked for a pause.
const SEGMENT_POLL: Duration = Duration::from_millis(150);
/// With pause cuts on and no chunk length, a monologue still gets cut here.
const DEFAULT_MAX_SEGMENT_SECONDS: f64 = 60.0;

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

    pub tab: Tab,
    pub live: LiveState,
    pub last_summary: Option<String>,
    /// Transcript length when the summary was made: a longer transcript
    /// means the summary is out of date.
    summary_lines: usize,
    save_after_summary: bool,
    /// The summary that "Save to memory" waited for has arrived.
    pending_save: bool,
    pub open_menu: Option<Menu>,
    /// Memories per DevCat project, counted when the context menu opens.
    pub memory_counts: std::collections::HashMap<String, usize>,
    /// Minimized to the title bar; holds the height to restore.
    pub minimized: Option<f64>,
    pub update: updates::UpdateState,
    pub cheatsheet_open: bool,
    pub transcript_unseen: usize,
    pub latest: Option<Latest>,
    devcat_keys: std::collections::HashMap<String, String>,
    pub island: Option<gpui::WindowHandle<super::island::Island>>,
    pub island_native_cache: Option<NativeWindow>,
    /// The island window is being opened (it opens after the current update).
    pub island_opening: bool,
    pub window_handle: gpui::AnyWindowHandle,
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
            tab: Tab::Chat,
            live: LiveState::default(),
            last_summary: None,
            summary_lines: 0,
            save_after_summary: false,
            pending_save: false,
            open_menu: None,
            cheatsheet_open: false,
            transcript_unseen: 0,
            latest: None,
            devcat_keys: Default::default(),
            island: None,
            island_native_cache: None,
            update: Default::default(),
            minimized: None,
            memory_counts: Default::default(),
            island_opening: false,
            window_handle: window.window_handle(),
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

        self.schedule_update_check(cx);

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

        if self.cfg.window.notch() {
            // Notch mode starts as the island; the full panel opens on demand.
            self.show_island(true, cx);
        } else if self.cfg.window.show_on_start {
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
                // Minimized, the window is only its title bar: keep the real height.
                let height = this.minimized.unwrap_or(f.height);
                this.cfg.window.bounds = Some(config::WindowBounds { x: f.x, y: f.y, width: f.width, height });
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

    /// Bring answers into view without taking focus: the panel, or in notch
    /// mode the island (the full panel opens only when asked for).
    pub fn surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cfg.window.notch() {
            self.show_island(true, cx);
        } else {
            self.show(false, window, cx);
        }
    }

    pub fn toggle_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cfg.window.notch() {
            // In notch mode the island is the thing that hides and shows; the
            // full panel goes away with it.
            let showing = self.island_visible();
            self.hide(cx);
            self.show_island(!showing, cx);
            return;
        }
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

    /// Fold the panel up to its title bar, or open it back to its size.
    pub fn set_minimized(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.minimized.is_some() {
            return;
        }
        let Some(native) = self.native else { return };
        let f = native.frame();
        if on {
            self.minimized = Some(f.height);
            self.open_menu = None;
            self.cheatsheet_open = false;
            self.with_native(cx, move |n| {
                n.set_min_size(MIN_SIZE.0, MINI_HEIGHT);
                n.set_frame(Rect { height: MINI_HEIGHT, ..f });
            });
        } else {
            let height = self.minimized.take().unwrap_or(MIN_SIZE.1).max(MIN_SIZE.1);
            if self.tab == Tab::Live {
                self.live.unseen = 0;
            }
            self.with_native(cx, move |n| {
                let f = n.frame();
                n.set_frame(Rect { height, ..f });
                n.set_min_size(MIN_SIZE.0, MIN_SIZE.1);
            });
        }
        cx.notify();
    }

    pub fn toggle_compact(&mut self, cx: &mut Context<Self>) {
        self.cfg.window.compact = !self.cfg.window.compact;
        self.save_config();
        cx.notify();
    }

    pub fn set_protection(&mut self, on: bool, cx: &mut Context<Self>) {
        self.cfg.window.protection = on;
        // Every window of ours: the panel and, in notch mode, the island.
        for native in [self.native, self.island_native()].into_iter().flatten() {
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
            if let Some(island) = self.island_native() {
                island.set_content_protection(self.cfg.window.protection);
            }
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
                HotkeyId::Summarize => self.summarize(window, cx),
                HotkeyId::SaveMemory => self.save_memory(window, cx),
                HotkeyId::ToggleNotch => self.toggle_notch(window, cx),
                HotkeyId::ToggleAutoAnswer => self.toggle_auto_answer(cx),
            },
        }
    }

    /* ---------- actions ---------- */

    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_minimized(false, cx);
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
        self.cancel_live(cx);
        self.session.clear();
        self.cards.clear();
        self.attachments.clear();
        self.live = LiveState::default();
        self.last_summary = None;
        self.latest = None;
        self.transcript_unseen = 0;
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
        self.last_summary = None;
        self.transcript_unseen = 0;
        cx.notify();
    }

    pub fn run_prompt(&mut self, prompt_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = self.cfg.prompt(&prompt_id).cloned() else {
            self.toast(ToastKind::Error, format!("Prompt \"{prompt_id}\" no longer exists."), cx);
            return;
        };
        // Shown without focus so the meeting app keeps the keyboard.
        self.surface(window, cx);

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
                    this.surface(window, cx);
                    if !prompt.is_empty() && this.cfg.prompt(&prompt).is_some() {
                        this.ask(Some(prompt), String::new(), vec![shot], window, cx);
                    } else {
                        this.focus_input(window, cx);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    this.surface(window, cx);
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
        self.tab = Tab::Chat;
        self.latest = Some(Latest::Card(self.seq));
        self.cards.push(Card {
            id: self.seq,
            kind: CardKind::Ask,
            prompt: String::new(),
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
        let transcript = self.session.transcript_text();
        let prompt = ai::build_prompt(&self.cfg, prompt_id.as_deref(), &input, &transcript);
        let accelerator = prompt_id
            .as_deref()
            .and_then(|pid| self.cfg.prompt(pid))
            .map(|p| p.accelerator.clone())
            .filter(|a| !a.is_empty());
        self.start_ask(prompt, accelerator, shots, CardKind::Ask, window, cx);
    }

    /// Ask the picked agent and stream the answer into a new chat card.
    pub(super) fn start_ask(
        &mut self,
        prompt: BuiltPrompt,
        accelerator: Option<String>,
        shots: Vec<Arc<Shot>>,
        kind: CardKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel(cx);
        self.close_settings(window, cx);
        self.open_menu = None;
        self.tab = Tab::Chat;
        // You asked: the answer needs room.
        self.set_minimized(false, cx);
        // Shots sent with this ask are consumed, so a follow-up question
        // does not upload them a second time.
        self.attachments.retain(|a| !shots.iter().any(|s| s.id == a.id));

        if prompt.text.is_empty() {
            return self.fail("Nothing to ask — type something or attach a screenshot.", cx);
        }
        let provider = match self.resolve_answerer().and_then(|p| Self::usable(&p).map(|_| p)) {
            Ok(p) => p,
            Err(message) => return self.fail(message, cx),
        };

        let mut notes = Vec::new();
        let mut images: Vec<ImagePart> =
            shots.iter().map(|s| ImagePart { mime: s.mime(), base64: s.base64.clone() }).collect();
        if !images.is_empty() && !provider.vision {
            notes.push(format!(
                "{} ({}) has no vision support — the screenshot was not sent.",
                provider.label,
                if provider.model.is_empty() { "default model" } else { provider.model.as_str() }
            ));
            images.clear();
        }
        if let Some(project) = self.context_project() {
            notes.push(format!("Context: {}", project.name));
        }

        self.session.push_turn(Turn { role: Role::User, text: prompt.text.clone(), images });

        self.seq += 1;
        let id = self.seq;
        let model = prompt.model.clone().unwrap_or_else(|| provider.model.clone());
        let at = capture::now_ms();
        let shown_model = if model.is_empty() { provider.label.clone() } else { model.clone() };
        let typed = if prompt.label == "Ask" { prompt.text.chars().take(300).collect() } else { String::new() };
        self.cards.push(Card {
            id,
            kind,
            prompt: typed,
            label: prompt.label.clone(),
            accelerator,
            meta: [shown_model, clock(at)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
            shots: shots.clone(),
            thinking: String::new(),
            notes,
            text: String::new(),
            error: None,
            done: false,
        });
        self.readout_scroll.scroll_to_bottom();
        self.latest = Some(Latest::Card(id));

        let effort = prompt.effort.clone().or_else(|| Some(self.cfg.chat.effort.clone()).filter(|e| !e.is_empty()));
        let claude = (provider.kind == ProviderKind::ClaudeCode).then(|| self.session.claude_session());
        let history = self.session.history();
        // Claude Code keeps the history itself, so it gets the newest turn only.
        let messages = if claude.is_some() { history.last().cloned().into_iter().collect() } else { history };
        let mut opts = self.stream_options(&provider, model, messages, effort);
        opts.claude_code = claude.as_ref().map(|h| self.claude_options(Some(h)));
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
                if card.kind == CardKind::Summary {
                    self.last_summary = Some(answer.clone());
                    self.summary_lines = self.session.transcript_lines().len();
                    if std::mem::take(&mut self.save_after_summary) {
                        self.pending_save = true;
                    }
                }
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
        if terminal && self.save_after_summary && self.cards.iter().any(|c| c.id == id && c.kind == CardKind::Summary) {
            // The summary failed or was stopped: there is nothing to save.
            self.save_after_summary = false;
        }
        if std::mem::take(&mut self.pending_save) {
            self.save_memory_now(cx);
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
        self.surface(window, cx);
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
        self.begin_recording(recorder, cx);
    }

    /// When a segment is cut while recording: at a pause, at the chunk
    /// length, or (with neither) only when recording stops.
    fn segment_policy(&self) -> audio::SegmentPolicy {
        let t = &self.cfg.transcription;
        let pause_seconds = t.pause_seconds.max(0.0) as f64;
        let max_seconds = if t.chunk_seconds >= 5 {
            t.chunk_seconds as f64
        } else if pause_seconds > 0.0 {
            DEFAULT_MAX_SEGMENT_SECONDS
        } else {
            0.0
        };
        audio::SegmentPolicy { pause_seconds, max_seconds }
    }

    /// Start recording from `recorder` (a microphone, or the dev channel's
    /// simulated one).
    pub fn begin_recording(&mut self, recorder: Recorder, cx: &mut Context<Self>) {
        let (queue, mut segments) = futures::channel::mpsc::unbounded::<Vec<(Vec<f32>, u32)>>();

        // The worker outlives the recording so the last segments still finish
        // after stop; it ends once the queue is closed and drained.
        cx.spawn(async move |this, cx| {
            while let Some(parts) = segments.next().await {
                let job = this.update(cx, |this, _| transcribe::job(&this.cfg, &this.secrets)).ok();
                let result = match job {
                    None => break,
                    Some(Err(err)) => Err(err),
                    Some(Ok(job)) => match cx.background_spawn(async move { audio::encode_segment(parts) }).await {
                        // Too short to be speech; a mis-tap on the hotkey.
                        None => Ok(String::new()),
                        Some(wav) => crate::runtime::spawn(transcribe::run(job, wav))
                            .await
                            .unwrap_or_else(|e| Err(anyhow::anyhow!("transcription task failed: {e}"))),
                    },
                };
                let _ = this.update(cx, |this, cx| {
                    this.uploading = this.uploading.saturating_sub(1);
                    match result {
                        Ok(text) => this.append_transcript(&text, cx),
                        Err(err) => this.toast(ToastKind::Error, format!("{err:#}"), cx),
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        let poller = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SEGMENT_POLL).await;
                let alive = this.update(cx, |this, cx| this.poll_recording(cx)).unwrap_or(false);
                if !alive {
                    break;
                }
            }
        });
        self.recording = Some(Recording { recorder, _poller: poller, queue, error_reported: false });
        cx.notify();
    }

    /// Returns whether a recording is still running.
    fn poll_recording(&mut self, cx: &mut Context<Self>) -> bool {
        let policy = self.segment_policy();
        let Some(recording) = self.recording.as_mut() else { return false };
        let error = if recording.error_reported { None } else { recording.recorder.error() };
        if error.is_some() {
            recording.error_reported = true;
        }
        let segment = recording.recorder.poll(policy);
        if let Some(error) = error {
            self.toast(ToastKind::Error, format!("Recorder error: {error}"), cx);
        }
        if let Some(segment) = segment {
            self.enqueue_segment(segment, cx);
        }
        true
    }

    fn enqueue_segment(&mut self, segment: audio::Segment, cx: &mut Context<Self>) {
        // Silence is not sent: Whisper invents words for it.
        if !segment.speech {
            return;
        }
        let Some(recording) = &self.recording else { return };
        if recording.queue.unbounded_send(segment.parts).is_ok() {
            self.uploading += 1;
            cx.notify();
        }
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let rest = self.recording.as_ref().map(|r| r.recorder.finish());
        if let Some(rest) = rest {
            self.enqueue_segment(rest, cx);
        }
        // Dropping the recording stops the device and closes the queue.
        self.recording = None;
        cx.notify();
    }

    pub fn append_transcript(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(line) = self.session.append_transcript(text, capture::now_ms()) {
            if self.tab != Tab::Transcript {
                self.transcript_unseen += 1;
            }
            self.transcript_scroll.scroll_to_bottom();
            self.consider_live(&line.text, cx);
            cx.notify();
        }
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        match tab {
            Tab::Live => self.live.unseen = 0,
            Tab::Transcript => {
                self.transcript_unseen = 0;
                self.transcript_scroll.scroll_to_bottom();
            }
            Tab::Chat => self.readout_scroll.scroll_to_bottom(),
        }
        cx.notify();
    }

    fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(Tab::ALL[(i + 1) % Tab::ALL.len()], cx);
    }

    fn on_prev_tab(&mut self, _: &PrevTab, _: &mut Window, cx: &mut Context<Self>) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()], cx);
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
