//! Settings replaces the panel body rather than floating over it: the window
//! is too small for a modal to leave anything useful behind.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, KeyDownEvent, ScrollHandle, SharedString, Subscription, WeakEntity,
    Window, anchored, deferred, div, prelude::*, px,
};

use super::panel::{Panel, ToastKind};
use super::text_input::{InputEvent, TextInput};
use super::theme;
use super::widgets::{button, caps, checkbox, chrome_button, field, hint, hint_colored, section, swallow_mouse_down};
use crate::ai::claude_code;
use crate::capture;
use crate::config::{Config, HotkeyId, PROVIDERS, PromptPreset, PromptSource, ProviderKind, provider_by_id};
use crate::hotkeys;
use crate::platform::Permission;
use crate::transcribe::{self, Readiness};
use crate::whisper;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Ai,
    Voice,
    Shots,
    Prompts,
    Keys,
}

impl SettingsTab {
    const ALL: [SettingsTab; 5] =
        [SettingsTab::Ai, SettingsTab::Voice, SettingsTab::Shots, SettingsTab::Prompts, SettingsTab::Keys];

    fn label(self) -> &'static str {
        match self {
            SettingsTab::Ai => "AI",
            SettingsTab::Voice => "Voice",
            SettingsTab::Shots => "Capture",
            SettingsTab::Prompts => "Prompts",
            SettingsTab::Keys => "Keys",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "ai" => SettingsTab::Ai,
            "voice" => SettingsTab::Voice,
            "shots" | "capture" => SettingsTab::Shots,
            "prompts" => SettingsTab::Prompts,
            "keys" => SettingsTab::Keys,
            _ => return None,
        })
    }
}

const EFFORTS: [(&str, &str); 5] = [
    ("low", "low — fastest"),
    ("medium", "medium"),
    ("high", "high"),
    ("xhigh", "xhigh"),
    ("max", "max — slowest, most thorough"),
];

/// What the view needs from the panel, copied out so rendering can borrow the
/// view mutably.
struct Snapshot {
    cfg: Config,
    keys: std::collections::BTreeMap<&'static str, bool>,
    readiness: Readiness,
    screen: Permission,
    displays: Vec<(String, String)>,
}

pub struct SettingsView {
    panel: WeakEntity<Panel>,
    tab: SettingsTab,
    inputs: HashMap<String, Entity<TextInput>>,
    input_subs: Vec<Subscription>,
    open_select: Option<String>,
    hotkey_focus: HashMap<String, FocusHandle>,
    hotkey_subs: Vec<Subscription>,
    capturing: Option<String>,
    claude: Option<claude_code::Detection>,
    claude_probe: Option<String>,
    whisper: Option<whisper::Detection>,
    whisper_probe: Option<String>,
    devices: Vec<String>,
    download: Option<(String, String)>,
    scroll: ScrollHandle,
    /// Clicking empty space focuses this, which blurs (and so saves) a field.
    focus_handle: FocusHandle,
}

impl SettingsView {
    pub fn new(panel: WeakEntity<Panel>, tab: SettingsTab, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            panel,
            tab,
            inputs: HashMap::new(),
            input_subs: Vec::new(),
            open_select: None,
            hotkey_focus: HashMap::new(),
            hotkey_subs: Vec::new(),
            capturing: None,
            claude: None,
            claude_probe: None,
            whisper: None,
            whisper_probe: None,
            devices: crate::audio::input_devices(),
            download: None,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        };
        view.schedule_probe(cx);
        view
    }

    pub fn tab(&self) -> SettingsTab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: SettingsTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        self.rebuild(cx);
    }

    /// Drop field state so every value is re-read from the config.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.inputs.clear();
        self.input_subs.clear();
        self.open_select = None;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.schedule_probe(cx);
        cx.notify();
    }

    /// Probing reads the panel, which may be mid-update when settings open
    /// or switch tabs from a panel action — so it runs once that is done.
    fn schedule_probe(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = this.update(cx, |this, cx| {
                this.probe(cx);
                cx.notify();
            });
        });
    }

    /// Commit every field's current text, for when the view is closing.
    pub fn commit_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inputs: Vec<(String, Entity<TextInput>)> =
            self.inputs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (path, input) in inputs {
            let value = input.read(cx).text().to_string();
            self.commit(&path, value, input, window, cx);
        }
        // Closing mid-capture never delivers the hotkey box's blur, which is
        // what brings the global hotkeys back.
        if self.capturing.take().is_some() {
            self.with_panel(window, cx, |panel, window, cx| panel.reapply_hotkeys(window, cx));
        }
    }

    /// Scroll the body to `y` points from the top (dev control channel).
    pub fn scroll_to(&mut self, y: f32, cx: &mut Context<Self>) {
        self.scroll.set_offset(gpui::point(px(0.), px(-y)));
        cx.notify();
    }

    pub fn wants_escape(&self) -> bool {
        self.open_select.is_some()
    }

    pub fn close_popups(&mut self, cx: &mut Context<Self>) {
        self.open_select = None;
        cx.notify();
    }

    fn snapshot(&self, cx: &App) -> Option<Snapshot> {
        let panel = self.panel.upgrade()?;
        let panel = panel.read(cx);
        Some(Snapshot {
            cfg: panel.cfg.clone(),
            keys: panel.secrets.status(),
            readiness: transcribe::readiness(&panel.cfg, &panel.secrets),
            screen: capture::screen_permission(),
            displays: capture::list_displays(),
        })
    }

    /// Finding the CLI and the whisper engine touches the filesystem and runs
    /// processes, so each runs once per path value rather than per render.
    fn probe(&mut self, cx: &mut Context<Self>) {
        let Some(snap) = self.snapshot(cx) else { return };
        let chat_kind = provider_by_id(&snap.cfg.chat.provider).map(|p| p.kind);
        if chat_kind == Some(ProviderKind::ClaudeCode) {
            let key = snap.cfg.chat.claude_code.binary_path.clone();
            if self.claude_probe.as_ref() != Some(&key) {
                self.claude_probe = Some(key.clone());
                self.claude = None;
                cx.spawn(async move |this, cx| {
                    let found = crate::runtime::spawn(async move { claude_code::detect(&key).await }).await.ok();
                    let _ = this.update(cx, |this, cx| {
                        this.claude = found;
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        let voice_kind = provider_by_id(&snap.cfg.transcription.provider).map(|p| p.kind);
        if voice_kind == Some(ProviderKind::WhisperCpp) {
            let w = snap.cfg.transcription.whisper_cpp.clone();
            let key = format!("{}|{}", w.binary_path, w.model_path);
            if self.whisper_probe.as_ref() != Some(&key) || self.whisper.is_none() {
                self.whisper_probe = Some(key);
                self.whisper = Some(whisper::detect(&w));
            }
        }
    }

    fn with_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Panel, &mut Window, &mut Context<Panel>),
    ) {
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| f(panel, window, cx));
        }
    }

    /// Change config, then refresh: structural changes (provider, paths)
    /// change which fields exist, so those re-probe and rebuild.
    fn set(&mut self, window: &mut Window, cx: &mut Context<Self>, structural: bool, change: impl FnOnce(&mut Config)) {
        self.with_panel(window, cx, |panel, window, cx| panel.update_config(window, cx, change));
        if structural {
            self.rebuild(cx);
        } else {
            cx.notify();
        }
    }

    fn toast(&self, kind: ToastKind, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.with_panel(window, cx, |panel, _, cx| panel.toast(kind, text, cx));
    }

    /* ---------- text fields ---------- */

    /// A text field bound to `path`. Saved on blur or Enter, not on every
    /// keystroke.
    fn input(
        &mut self,
        path: &str,
        value: &str,
        placeholder: &str,
        kind: FieldKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        if let Some(existing) = self.inputs.get(path) {
            let existing = existing.clone();
            existing.update(cx, |i, cx| i.set_placeholder(placeholder.to_string(), cx));
            return existing;
        }
        let input = cx.new(|cx| {
            let input = TextInput::new(window, cx).placeholder(placeholder.to_string());
            let mut input = match kind {
                FieldKind::Text => input.mono(),
                FieldKind::Number => input,
                FieldKind::Password => input.masked(),
                FieldKind::Area(rows) => input.mono().multiline(rows),
            };
            input.set_text(value, cx);
            input
        });
        let path_owned = path.to_string();
        self.input_subs.push(cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::Submit) {
                let value = input.read(cx).text().to_string();
                this.commit(&path_owned, value, input.clone(), window, cx);
            }
        }));
        self.inputs.insert(path.to_string(), input.clone());
        input
    }

    fn commit(
        &mut self,
        path: &str,
        value: String,
        input: Entity<TextInput>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let num = |v: &str, min: f64, max: f64, fallback: f64| {
            v.trim().parse::<f64>().map(|n| n.clamp(min, max)).unwrap_or(fallback)
        };

        if let Some(provider) = path.strip_prefix("key.") {
            let key = value.trim().to_string();
            if key.is_empty() {
                return;
            }
            input.update(cx, |i, cx| i.set_text("", cx));
            let provider = provider.to_string();
            self.with_panel(window, cx, |panel, _, cx| panel.set_key(&provider, &key, cx));
            cx.notify();
            return;
        }

        if let Some(rest) = path.strip_prefix("prompt.") {
            let mut parts = rest.splitn(2, '.');
            let (Some(ix), Some(field)) = (parts.next().and_then(|i| i.parse::<usize>().ok()), parts.next()) else {
                return;
            };
            let field = field.to_string();
            self.set(window, cx, false, move |c| {
                if let Some(p) = c.prompts.get_mut(ix) {
                    match field.as_str() {
                        "label" => p.label = value,
                        "text" => p.text = value,
                        "model" => p.model = Some(value.trim().to_string()).filter(|s| !s.is_empty()),
                        _ => {}
                    }
                }
            });
            return;
        }

        let structural = path == "chat.claudeCode.binaryPath" || path.starts_with("transcription.whisperCpp.");
        let path = path.to_string();
        let mut normalized: Option<String> = None;
        self.set(window, cx, false, |c| match path.as_str() {
            "chat.model" => c.chat.model = value.trim().to_string(),
            "chat.baseUrl" => c.chat.base_url = value.trim().to_string(),
            "chat.maxTokens" => {
                c.chat.max_tokens = num(&value, 256.0, 128_000.0, c.chat.max_tokens as f64) as u32;
                normalized = Some(c.chat.max_tokens.to_string());
            }
            "chat.systemPrompt" => c.chat.system_prompt = value,
            "chat.claudeCode.binaryPath" => c.chat.claude_code.binary_path = value.trim().to_string(),
            "chat.claudeCode.cwd" => c.chat.claude_code.cwd = value.trim().to_string(),
            "chat.claudeCode.maxBudgetUsd" => {
                c.chat.claude_code.max_budget_usd = num(&value, 0.0, 100.0, c.chat.claude_code.max_budget_usd);
                normalized = Some(c.chat.claude_code.max_budget_usd.to_string());
            }
            "transcription.model" => c.transcription.model = value.trim().to_string(),
            "transcription.baseUrl" => c.transcription.base_url = value.trim().to_string(),
            "transcription.language" => c.transcription.language = value.trim().to_string(),
            "transcription.pauseSeconds" => {
                let pause = num(&value, 0.0, 10.0, c.transcription.pause_seconds as f64);
                // Tenths are plenty; keeps "1.5" from becoming "1.5000000001".
                c.transcription.pause_seconds = ((pause * 10.0).round() / 10.0) as f32;
                normalized = Some(c.transcription.pause_seconds.to_string());
            }
            "transcription.chunkSeconds" => {
                c.transcription.chunk_seconds = num(&value, 0.0, 120.0, c.transcription.chunk_seconds as f64) as u32;
                normalized = Some(c.transcription.chunk_seconds.to_string());
            }
            "transcription.whisperCpp.modelPath" => c.transcription.whisper_cpp.model_path = value.trim().to_string(),
            "transcription.whisperCpp.binaryPath" => c.transcription.whisper_cpp.binary_path = value.trim().to_string(),
            "transcription.whisperCpp.threads" => {
                c.transcription.whisper_cpp.threads =
                    num(&value, 0.0, 32.0, c.transcription.whisper_cpp.threads as f64) as u32;
                normalized = Some(c.transcription.whisper_cpp.threads.to_string());
            }
            "capture.maxDimension" => {
                c.capture.max_dimension = num(&value, 512.0, 4096.0, c.capture.max_dimension as f64) as u32;
                normalized = Some(c.capture.max_dimension.to_string());
            }
            "capture.keepLast" => {
                c.capture.keep_last = num(&value, 1.0, 20.0, c.capture.keep_last as f64) as usize;
                normalized = Some(c.capture.keep_last.to_string());
            }
            "window.opacity" => {
                c.window.opacity = num(&value, 0.25, 1.0, c.window.opacity as f64) as f32;
                normalized = Some(c.window.opacity.to_string());
            }
            _ => {}
        });
        if let Some(n) = normalized {
            input.update(cx, |i, cx| i.set_text(n, cx));
        }
        if structural {
            self.rebuild(cx);
        }
    }

    /* ---------- selects ---------- */

    fn select(
        &self,
        id: &str,
        current: &str,
        options: Vec<(String, String)>,
        on_pick: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open_select.as_deref() == Some(id);
        let label =
            options.iter().find(|(v, _)| v == current).map(|(_, l)| l.clone()).unwrap_or_else(|| current.to_string());
        let id_owned = id.to_string();
        let on_pick = std::rc::Rc::new(on_pick);

        let menu = open.then(|| {
            deferred(
                anchored().snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id(SharedString::from(format!("{id}-menu")))
                        .occlude()
                        .mt(px(2.))
                        .min_w(px(160.))
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .bg(theme::raise())
                        .border_1()
                        .border_color(theme::lamp())
                        .rounded(px(4.))
                        .py(px(3.))
                        .children(options.iter().enumerate().map(|(n, (value, label))| {
                            let value = value.clone();
                            let pick = on_pick.clone();
                            let selected = value == current;
                            div()
                                .id(SharedString::from(format!("{id}-opt-{n}")))
                                .px(px(9.))
                                .py(px(5.))
                                .text_size(px(12.))
                                .text_color(if selected { theme::lamp() } else { theme::read() })
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::glass()))
                                .child(label.clone())
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_select = None;
                                    pick(this, value.clone(), window, cx);
                                    cx.notify();
                                }))
                        })),
                ),
            )
            .with_priority(1)
        });

        div()
            .relative()
            .w_full()
            .child(swallow_mouse_down(
                div()
                    .id(SharedString::from(format!("{id}-select")))
                    .w_full()
                    .flex()
                    .items_center()
                    .bg(theme::glass())
                    .border_1()
                    .border_color(if open { theme::lamp() } else { theme::edge() })
                    .rounded(px(4.))
                    .px(px(8.))
                    .py(px(7.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .child(div().flex_1().min_w_0().overflow_hidden().child(label))
                    .child(div().text_color(theme::haze()).text_size(px(9.)).child("▾"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_select =
                            if this.open_select.as_deref() == Some(&id_owned) { None } else { Some(id_owned.clone()) };
                        cx.notify();
                    })),
            ))
            .children(menu)
            .into_any_element()
    }

    /* ---------- hotkey fields ---------- */

    fn hotkey_field(&mut self, path: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let handle = match self.hotkey_focus.get(path) {
            Some(h) => h.clone(),
            None => {
                let h = cx.focus_handle();
                let p = path.to_string();
                // While capturing, the global hotkeys must not swallow the
                // combination being typed.
                self.hotkey_subs.push(cx.on_focus(&h, window, move |this, window, cx| {
                    this.capturing = Some(p.clone());
                    this.with_panel(window, cx, |panel, _, _| panel.suspend_hotkeys());
                    cx.notify();
                }));
                let p = path.to_string();
                self.hotkey_subs.push(cx.on_blur(&h, window, move |this, window, cx| {
                    if this.capturing.as_deref() == Some(&p) {
                        this.capturing = None;
                    }
                    this.with_panel(window, cx, |panel, window, cx| panel.reapply_hotkeys(window, cx));
                    cx.notify();
                }));
                self.hotkey_focus.insert(path.to_string(), h.clone());
                h
            }
        };
        let capturing = self.capturing.as_deref() == Some(path);
        let path_owned = path.to_string();
        let shown = if value.is_empty() {
            if capturing { "press keys…".to_string() } else { "click, then press keys".to_string() }
        } else {
            hotkeys::pretty(value)
        };

        swallow_mouse_down(
            div()
                .id(SharedString::from(format!("hk-{path}")))
                .track_focus(&handle)
                .flex_1()
                .min_w(px(80.))
                .bg(theme::glass())
                .border_1()
                .border_color(if capturing { theme::lamp() } else { theme::edge() })
                .rounded(px(4.))
                .px(px(8.))
                .py(px(7.))
                .font_family(theme::mono_family())
                .text_size(px(11.))
                .text_color(if capturing {
                    theme::lamp()
                } else if value.is_empty() {
                    theme::edge()
                } else {
                    theme::read()
                })
                .flex()
                .justify_center()
                .cursor_pointer()
                .child(shown)
                .on_click(move |_, window, _| window.focus(&handle))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    cx.stop_propagation();
                    let ks = &event.keystroke;
                    let plain = !ks.modifiers.modified();
                    if plain && (ks.key == "backspace" || ks.key == "delete") {
                        this.write_hotkey(&path_owned, String::new(), window, cx);
                        return;
                    }
                    if let Some(acc) = hotkeys::from_keystroke(ks) {
                        this.write_hotkey(&path_owned, acc, window, cx);
                    }
                })),
        )
        .into_any_element()
    }

    fn write_hotkey(&mut self, path: &str, accelerator: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.to_string();
        // Saved without re-registering: the field still has focus, and the
        // hotkeys come back when it loses it.
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| {
                if let Some(ix) = path.strip_prefix("prompt.").and_then(|i| i.parse::<usize>().ok()) {
                    if let Some(p) = panel.cfg.prompts.get_mut(ix) {
                        p.accelerator = accelerator;
                    }
                } else if let Some(id) = HotkeyId::ALL.iter().find(|id| hotkey_path(**id) == path) {
                    panel.cfg.hotkeys.set(*id, accelerator);
                }
                panel.save_config();
                cx.notify();
            });
        }
        let _ = window;
        cx.notify();
    }

    /* ---------- views ---------- */

    fn view_claude_code(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cc = snap.cfg.chat.claude_code.clone();
        let status = match &self.claude {
            None => hint("Looking for the CLI…").mb(px(14.)).into_any_element(),
            Some(found) if found.ok => {
                let account = match &found.account {
                    Some(a) if a.logged_in => format!(
                        " Signed in as **{}** — no API key needed.",
                        a.email.clone().or(a.method.clone()).unwrap_or_default()
                    ),
                    _ => " Using the login it already has — no API key needed.".into(),
                };
                hint(format!(
                    "Found **{}** at **{}**.{account}",
                    found.version.clone().unwrap_or_default(),
                    found.path.clone().unwrap_or_default()
                ))
                .mb(px(14.))
                .into_any_element()
            }
            Some(found) => {
                hint_colored(found.error.clone().unwrap_or_default(), theme::live()).mb(px(14.)).into_any_element()
            }
        };
        let found_path = self.claude.as_ref().and_then(|c| c.path.clone()).unwrap_or_else(|| "auto-detect".into());
        let binary =
            self.input("chat.claudeCode.binaryPath", &cc.binary_path, &found_path, FieldKind::Text, window, cx);
        let cwd = self.input("chat.claudeCode.cwd", &cc.cwd, "temp directory", FieldKind::Text, window, cx);
        let budget = self.input(
            "chat.claudeCode.maxBudgetUsd",
            &cc.max_budget_usd.to_string(),
            "0",
            FieldKind::Number,
            window,
            cx,
        );
        vec![
            status,
            field(
                "Binary path",
                input_box(binary),
                Some(hint("Only needed if auto-detection fails. A GUI app does not inherit your shell PATH, so an unusual install location has to be named here.").into_any_element()),
            )
            .into_any_element(),
            field(
                "Working directory",
                input_box(cwd),
                Some(hint("Where Claude Code runs. Leave blank unless you want it to see a specific project.").into_any_element()),
            )
            .into_any_element(),
            field(
                "Fast start",
                checkbox("cc-fast", cc.fast_start, "Skip hooks, MCP servers, plugins and CLAUDE.md", {
                    let this = cx.entity().downgrade();
                    move |on, window, cx| {
                        let _ = this.update(cx, |this, cx| this.set(window, cx, false, |c| c.chat.claude_code.fast_start = on));
                    }
                }),
                Some(hint("Measured here: **~11s** to first token without it, **~1.5s** with it. Leave it on for live use.").into_any_element()),
            )
            .into_any_element(),
            field(
                "Tools",
                checkbox("cc-tools", cc.allow_tools, "Let it read files and run commands", {
                    let this = cx.entity().downgrade();
                    move |on, window, cx| {
                        let _ = this.update(cx, |this, cx| this.set(window, cx, false, |c| c.chat.claude_code.allow_tools = on));
                    }
                }),
                Some(hint("Off means answers only — it never touches your disk. On makes \"fix this\" far stronger but slower, and anything needing permission is denied automatically since nobody is there to approve it.").into_any_element()),
            )
            .into_any_element(),
            field(
                "Spend cap (USD)",
                input_box(budget),
                Some(hint("Per ask. **0** means no cap. Real cost is reported on every answer.").into_any_element()),
            )
            .into_any_element(),
        ]
    }

    fn view_ai(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let chat = snap.cfg.chat.clone();
        let provider = provider_by_id(&chat.provider);
        let kind = provider.map(|p| p.kind);
        let is_claude = kind == Some(ProviderKind::Anthropic);
        let is_cc = kind == Some(ProviderKind::ClaudeCode);
        let me = cx.entity().downgrade();

        let mut out = vec![section("Answers", true).into_any_element()];
        out.push(
            field(
                "Provider",
                self.select(
                    "chat.provider",
                    &chat.provider,
                    PROVIDERS.iter().filter(|p| p.chat).map(|p| (p.id.to_string(), p.label.to_string())).collect(),
                    |this, v, window, cx| this.set(window, cx, true, move |c| c.chat.provider = v),
                    cx,
                ),
                None,
            )
            .into_any_element(),
        );

        if is_cc {
            let model = self.input("chat.model", &chat.model, "whatever Claude Code uses", FieldKind::Text, window, cx);
            out.push(
                field(
                    "Model",
                    input_box(model),
                    Some(
                        hint(
                            "An alias such as **sonnet** or **opus**, or a full model id. Blank keeps the CLI default.",
                        )
                        .into_any_element(),
                    ),
                )
                .into_any_element(),
            );
            out.extend(self.view_claude_code(snap, window, cx));
        } else {
            let placeholder = provider.map(|p| p.model).filter(|m| !m.is_empty()).unwrap_or("model id");
            let model = self.input("chat.model", &chat.model, placeholder, FieldKind::Text, window, cx);
            let max = self.input("chat.maxTokens", &chat.max_tokens.to_string(), "8192", FieldKind::Number, window, cx);
            out.push(row(vec![
                field("Model", input_box(model), None).into_any_element(),
                field("Max tokens", input_box(max), None).into_any_element(),
            ]));
            let base = self.input(
                "chat.baseUrl",
                &chat.base_url,
                provider.map(|p| p.base_url).unwrap_or(""),
                FieldKind::Text,
                window,
                cx,
            );
            out.push(
                field(
                    "Base URL",
                    input_box(base),
                    Some(
                        hint("Leave blank to use the provider default. Any OpenAI-compatible server works here.")
                            .into_any_element(),
                    ),
                )
                .into_any_element(),
            );
        }

        if is_claude || is_cc {
            out.push(
                field(
                    "Effort",
                    self.select(
                        "chat.effort",
                        &chat.effort,
                        EFFORTS.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect(),
                        |this, v, window, cx| this.set(window, cx, false, move |c| c.chat.effort = v),
                        cx,
                    ),
                    Some(
                        hint("How hard Claude thinks before answering. A prompt preset can override this.")
                            .into_any_element(),
                    ),
                )
                .into_any_element(),
            );
        }

        out.push(
            field(
                "Reasoning",
                checkbox("show-thinking", chat.show_thinking, "Show the reasoning summary while it thinks", {
                    let me = me.clone();
                    move |on, window, cx| {
                        let _ = me.update(cx, |this, cx| this.set(window, cx, false, |c| c.chat.show_thinking = on));
                    }
                }),
                None,
            )
            .into_any_element(),
        );
        if is_claude {
            out.push(
                field(
                    "Declines",
                    checkbox(
                        "fallback",
                        chat.server_side_fallback,
                        "Retry on a fallback model if a request is declined",
                        {
                            let me = me.clone();
                            move |on, window, cx| {
                                let _ = me.update(cx, |this, cx| {
                                    this.set(window, cx, false, |c| c.chat.server_side_fallback = on)
                                });
                            }
                        },
                    ),
                    None,
                )
                .into_any_element(),
            );
        }
        let system = self.input("chat.systemPrompt", &chat.system_prompt, "", FieldKind::Area(7), window, cx);
        out.push(
            field(
                "System prompt",
                input_box(system),
                Some(hint("Sets the voice and length of every answer.").into_any_element()),
            )
            .into_any_element(),
        );
        out.push(
            div()
                .flex()
                .child(button("clear-session", "Clear conversation", true, {
                    let me = me.clone();
                    move |_, window, cx| {
                        let _ = me.update(cx, |this, cx| {
                            this.with_panel(window, cx, |panel, _, cx| panel.clear_conversation(cx));
                            this.toast(ToastKind::Info, "Conversation cleared.", window, cx);
                        });
                    }
                }))
                .into_any_element(),
        );
        if provider.is_some_and(|p| !p.vision) {
            out.push(
                hint("**This model has no vision support** — screenshots will not be sent. Pick a vision model to use the screenshot hotkeys.")
                    .mt(px(12.))
                    .into_any_element(),
            );
        }
        out
    }

    fn view_whisper(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let w = snap.cfg.transcription.whisper_cpp.clone();
        let found = self.whisper.clone().unwrap_or_default();
        let install = if cfg!(target_os = "macos") {
            "brew install whisper-cpp"
        } else {
            "see github.com/ggml-org/whisper.cpp for a Windows build"
        };
        let mut out = Vec::new();
        out.push(match &found.binary_path {
            Some(path) => hint(format!("Engine: **{path}**")).mb(px(12.)).into_any_element(),
            None => hint_colored(
                format!("{}\nInstall it with {install}.", found.error.clone().unwrap_or_default()),
                theme::live(),
            )
            .mb(px(12.))
            .into_any_element(),
        });
        if found.model_path.is_none() {
            out.push(
                hint_colored(
                    "No model yet — download one below. Audio stays on this machine either way.",
                    theme::live(),
                )
                .mb(px(12.))
                .into_any_element(),
            );
        }
        out.push(section("Models", false).into_any_element());
        out.push(
            hint(format!(
                "Bigger is more accurate and slower. **small** is the usual choice for languages other than English. Saved under {}",
                whisper::model_dir().display()
            ))
            .mb(px(12.))
            .into_any_element(),
        );
        let downloading = self.download.as_ref().map(|(f, _)| f.clone());
        for m in &found.models {
            let active = found.model_name.as_deref() == Some(m.file);
            let file = m.file.to_string();
            out.push(
                div()
                    .flex()
                    .items_end()
                    .gap(px(9.))
                    .mb(px(11.))
                    .child(
                        div().flex_none().w(px(96.)).text_size(px(11.5)).child(m.label).when(active, |d| {
                            d.child(div().text_size(px(9.5)).text_color(theme::edge()).child("in use"))
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .font_family(theme::mono_family())
                            .text_size(px(10.))
                            .text_color(if m.installed.is_some() { theme::lamp() } else { theme::edge() })
                            .child(m.installed.clone().unwrap_or_else(|| "not downloaded".into())),
                    )
                    .when(m.installed.is_none(), |d| {
                        let busy = downloading.as_deref() == Some(m.file);
                        d.child(button(
                            SharedString::from(format!("get-{}", m.file)),
                            if busy { "Downloading…" } else { "Download" },
                            false,
                            cx.listener(move |this, _, window, cx| this.download_model(file.clone(), window, cx)),
                        ))
                    })
                    .into_any_element(),
            );
        }
        if let Some((_, progress)) = &self.download {
            out.push(hint(progress.clone()).mb(px(14.)).into_any_element());
        }
        let model = self.input(
            "transcription.whisperCpp.modelPath",
            &w.model_path,
            found.model_path.as_deref().unwrap_or("auto"),
            FieldKind::Text,
            window,
            cx,
        );
        out.push(
            field(
                "Model file",
                input_box(model),
                Some(hint("Blank picks the largest model found.").into_any_element()),
            )
            .into_any_element(),
        );
        let engine = self.input(
            "transcription.whisperCpp.binaryPath",
            &w.binary_path,
            found.binary_path.as_deref().unwrap_or("auto"),
            FieldKind::Text,
            window,
            cx,
        );
        out.push(field("Engine path", input_box(engine), None).into_any_element());
        let threads =
            self.input("transcription.whisperCpp.threads", &w.threads.to_string(), "0", FieldKind::Number, window, cx);
        out.push(row(vec![
            field("Threads", input_box(threads), None).into_any_element(),
            field(
                "Output",
                self.select(
                    "transcription.whisperCpp.translate",
                    if w.translate { "yes" } else { "no" },
                    vec![("no".into(), "original language".into()), ("yes".into(), "translate to English".into())],
                    |this, v, window, cx| {
                        this.set(window, cx, false, move |c| c.transcription.whisper_cpp.translate = v == "yes")
                    },
                    cx,
                ),
                None,
            )
            .into_any_element(),
        ]));
        out
    }

    fn download_model(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.download.is_some() {
            return;
        }
        self.download = Some((file.clone(), format!("Downloading {file}…")));
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<whisper::Progress>();
        let name = file.clone();
        let task = crate::runtime::spawn(async move {
            whisper::download_model(&name, move |p| {
                let _ = tx.unbounded_send(p);
            })
            .await
        });
        cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            // Written straight into the view: a 488 MB download would
            // otherwise re-render the whole tab a hundred times.
            while let Some(p) = rx.next().await {
                let mb = |n: u64| n / 1_048_576;
                let text = format!("Downloading {} — {}% ({} / {} MB)", p.file, p.percent, mb(p.received), mb(p.total));
                let _ = this.update(cx, |this, cx| {
                    if let Some(d) = this.download.as_mut() {
                        d.1 = text;
                    }
                    cx.notify();
                });
            }
            let result = task.await.unwrap_or_else(|e| Err(anyhow::anyhow!("download task failed: {e}")));
            let _ = this.update_in(cx, |this, window, cx| {
                this.download = None;
                match result {
                    Ok(_) => this.toast(ToastKind::Info, &format!("{file} is ready."), window, cx),
                    Err(err) => this.toast(ToastKind::Error, &format!("{err:#}"), window, cx),
                }
                this.whisper_probe = None;
                this.rebuild(cx);
            });
        })
        .detach();
    }

    fn view_voice(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let voice = snap.cfg.transcription.clone();
        let provider = provider_by_id(&voice.provider);
        let is_local = provider.is_some_and(|p| p.kind == ProviderKind::WhisperCpp);
        let me = cx.entity().downgrade();
        let mut out = vec![section("Speech to text", true).into_any_element()];
        out.push(match &snap.readiness {
            Readiness::Ready { provider, model, local } => hint(format!(
                "Ready — **{provider}** · {model}{}",
                if *local { " · nothing leaves this machine" } else { "" }
            ))
            .mb(px(14.))
            .into_any_element(),
            Readiness::NotReady { reason, .. } => hint_colored(reason, theme::live()).mb(px(14.)).into_any_element(),
        });
        out.push(
            field(
                "Provider",
                self.select(
                    "transcription.provider",
                    &voice.provider,
                    PROVIDERS
                        .iter()
                        .filter(|p| p.transcription || p.id == voice.provider)
                        .map(|p| {
                            let label = if p.transcription {
                                p.label.to_string()
                            } else {
                                format!("{} (no speech API)", p.label)
                            };
                            (p.id.to_string(), label)
                        })
                        .collect(),
                    |this, v, window, cx| this.set(window, cx, true, move |c| c.transcription.provider = v),
                    cx,
                ),
                None,
            )
            .into_any_element(),
        );
        if is_local {
            let lang = self.input("transcription.language", &voice.language, "auto", FieldKind::Text, window, cx);
            out.push(
                field(
                    "Language",
                    input_box(lang),
                    Some(hint("A language code such as **id** or **en**. Auto-detect is accurate on clear speech; naming it skips the detection pass and is about 40% faster.").into_any_element()),
                )
                .into_any_element(),
            );
            out.extend(self.view_whisper(snap, window, cx));
        } else {
            let placeholder = provider.map(|p| p.transcription_model).filter(|m| !m.is_empty()).unwrap_or("whisper-1");
            let model = self.input("transcription.model", &voice.model, placeholder, FieldKind::Text, window, cx);
            let lang = self.input("transcription.language", &voice.language, "auto", FieldKind::Text, window, cx);
            out.push(row(vec![
                field("Model", input_box(model), None).into_any_element(),
                field("Language", input_box(lang), None).into_any_element(),
            ]));
            let base = self.input(
                "transcription.baseUrl",
                &voice.base_url,
                provider.map(|p| p.base_url).unwrap_or(""),
                FieldKind::Text,
                window,
                cx,
            );
            out.push(field("Base URL", input_box(base), None).into_any_element());
        }

        out.push(section("Recording", false).into_any_element());
        let pause = self.input(
            "transcription.pauseSeconds",
            &voice.pause_seconds.to_string(),
            "0",
            FieldKind::Number,
            window,
            cx,
        );
        let chunk = self.input(
            "transcription.chunkSeconds",
            &voice.chunk_seconds.to_string(),
            "0",
            FieldKind::Number,
            window,
            cx,
        );
        out.push(row(vec![
            field("Pause to transcribe (s)", input_box(pause), None).into_any_element(),
            field("Longest chunk (s)", input_box(chunk), None).into_any_element(),
        ]));
        out.push(
            hint(
                "When the speaker pauses this long, what they said is transcribed straight away while recording goes on — **1–2** suits conversation. A chunk is also cut at the longest length (**0** means one minute when pauses are on). With both at **0**, it transcribes once when you stop. Silence is never sent.",
            )
            .mt(px(-10.))
            .mb(px(15.))
            .into_any_element(),
        );

        out.push(section("Input", false).into_any_element());
        let mut devices = vec![(String::new(), "System default".to_string())];
        devices.extend(self.devices.iter().map(|d| (d.clone(), d.clone())));
        out.push(
            field(
                "Microphone",
                self.select(
                    "transcription.inputDeviceId",
                    &voice.input_device_id,
                    devices,
                    |this, v, window, cx| this.set(window, cx, false, move |c| c.transcription.input_device_id = v),
                    cx,
                ),
                (self.devices.is_empty())
                    .then(|| hint("Device names appear after you grant microphone access once.").into_any_element()),
            )
            .into_any_element(),
        );
        out.push(
            field(
                "System audio",
                checkbox("system-audio", voice.capture_system_audio, "Also record what the other side says", {
                    let me = me.clone();
                    move |on, window, cx| {
                        let _ = me.update(cx, |this, cx| this.set(window, cx, false, |c| c.transcription.capture_system_audio = on));
                    }
                }),
                Some(hint(if cfg!(target_os = "macos") {
                    "macOS gives no system-audio tap to apps. Install a loopback driver such as **BlackHole**, route your meeting output through it, and select it as the microphone above."
                } else {
                    "Windows mixes desktop loopback into the recording. If it fails, the app falls back to the microphone and says so."
                }).into_any_element()),
            )
            .into_any_element(),
        );
        out.push(
            div()
                .flex()
                .child(button("clear-transcript", "Clear transcript", true, {
                    let me = me.clone();
                    move |_, window, cx| {
                        let _ = me.update(cx, |this, cx| {
                            this.with_panel(window, cx, |panel, _, cx| panel.clear_transcript(cx));
                            this.toast(ToastKind::Info, "Transcript cleared.", window, cx);
                        });
                    }
                }))
                .into_any_element(),
        );
        out
    }

    fn view_shots(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let capture = snap.cfg.capture.clone();
        let win = snap.cfg.window.clone();
        let me = cx.entity().downgrade();
        let mut out = vec![section("Screenshots", true).into_any_element()];
        if cfg!(target_os = "macos") && snap.screen != Permission::Granted {
            out.push(
                div()
                    .mb(px(15.))
                    .child(hint_colored(
                        format!("Screen Recording permission is **{}**. Screenshots return an empty frame until it is granted.", snap.screen.as_str()),
                        theme::live(),
                    ))
                    .child(div().mt(px(8.)).flex().child(button("open-screen", "Open System Settings", false, |_, _, _| {
                        crate::platform::open_screen_settings();
                    })))
                    .into_any_element(),
            );
        }
        let mut displays = vec![
            ("cursor".to_string(), "Screen under the pointer".to_string()),
            ("primary".to_string(), "Primary screen".to_string()),
        ];
        displays.extend(snap.displays.clone());
        out.push(
            field(
                "Which screen",
                self.select(
                    "capture.display",
                    &capture.display,
                    displays,
                    |this, v, window, cx| this.set(window, cx, false, move |c| c.capture.display = v),
                    cx,
                ),
                None,
            )
            .into_any_element(),
        );
        let max = self.input(
            "capture.maxDimension",
            &capture.max_dimension.to_string(),
            "1600",
            FieldKind::Number,
            window,
            cx,
        );
        let keep = self.input("capture.keepLast", &capture.keep_last.to_string(), "6", FieldKind::Number, window, cx);
        out.push(row(vec![
            field("Max size (px)", input_box(max), None).into_any_element(),
            field("Keep last", input_box(keep), None).into_any_element(),
        ]));
        let mut prompts = vec![(String::new(), "Nothing — just attach it".to_string())];
        prompts.extend(snap.cfg.prompts.iter().map(|p| (p.id.clone(), p.label.clone())));
        out.push(
            field(
                "Region hotkey asks",
                self.select(
                    "capture.regionPrompt",
                    &capture.region_prompt,
                    prompts,
                    |this, v, window, cx| this.set(window, cx, false, move |c| c.capture.region_prompt = v),
                    cx,
                ),
                Some(hint("Longest side is scaled down before upload, which is what you pay for.").into_any_element()),
            )
            .into_any_element(),
        );

        out.push(section("Panel", false).into_any_element());
        let opacity = self.input("window.opacity", &win.opacity.to_string(), "1", FieldKind::Number, window, cx);
        out.push(field("Opacity", input_box(opacity), None).into_any_element());
        let toggle = |id: &'static str, value: bool, label: &'static str, apply: fn(&mut Config, bool)| {
            let me = me.clone();
            checkbox(id, value, label, move |on, window, cx| {
                let _ = me.update(cx, |this, cx| this.set(window, cx, false, move |c| apply(c, on)));
            })
        };
        out.push(
            field(
                "Capture protection",
                toggle("protection", win.protection, "Hide this panel from screen share and screenshots", |c, on| {
                    c.window.protection = on
                }),
                None,
            )
            .into_any_element(),
        );
        out.push(
            field(
                "Mouse",
                toggle(
                    "click-through",
                    win.click_through,
                    "Click-through — the panel ignores the mouse entirely",
                    |c, on| c.window.click_through = on,
                ),
                None,
            )
            .into_any_element(),
        );
        out.push(
            field(
                "Launch",
                toggle("show-on-start", win.show_on_start, "Show the panel when VeilPilot starts", |c, on| {
                    c.window.show_on_start = on
                }),
                None,
            )
            .into_any_element(),
        );
        out.push(hint(format!("Config: **{}**", crate::config::config_path().display())).into_any_element());
        out
    }

    fn view_prompts(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![
            section("Prompt hotkeys", true).into_any_element(),
            hint("Each one is a hotkey that captures what it needs and asks straight away. Use **{{transcript}}** and **{{input}}** to drop in the live transcript or whatever is typed in the ask box.")
                .mb(px(14.))
                .into_any_element(),
        ];
        let sources = vec![
            ("screenshot".to_string(), "screenshot".to_string()),
            ("transcript".to_string(), "transcript".to_string()),
            ("both".to_string(), "both".to_string()),
        ];
        let mut efforts = vec![(String::new(), "default effort".to_string())];
        efforts.extend(["low", "medium", "high", "xhigh", "max"].iter().map(|e| (e.to_string(), e.to_string())));

        for (ix, p) in snap.cfg.prompts.iter().enumerate() {
            let label = self.input(&format!("prompt.{ix}.label"), &p.label, "Name", FieldKind::Text, window, cx);
            let text = self.input(&format!("prompt.{ix}.text"), &p.text, "", FieldKind::Area(3), window, cx);
            let model = self.input(
                &format!("prompt.{ix}.model"),
                p.model.as_deref().unwrap_or(""),
                "model override",
                FieldKind::Text,
                window,
                cx,
            );
            let hk = self.hotkey_field(&format!("prompt.{ix}"), &p.accelerator, window, cx);
            let source = self.select(
                &format!("prompt.{ix}.source"),
                p.source.as_str(),
                sources.clone(),
                move |this, v, window, cx| {
                    let source = match v.as_str() {
                        "transcript" => PromptSource::Transcript,
                        "both" => PromptSource::Both,
                        _ => PromptSource::Screenshot,
                    };
                    this.set(window, cx, false, move |c| {
                        if let Some(p) = c.prompts.get_mut(ix) {
                            p.source = source;
                        }
                    });
                },
                cx,
            );
            let effort = self.select(
                &format!("prompt.{ix}.effort"),
                p.effort.as_deref().unwrap_or(""),
                efforts.clone(),
                move |this, v, window, cx| {
                    this.set(window, cx, false, move |c| {
                        if let Some(p) = c.prompts.get_mut(ix) {
                            p.effort = Some(v).filter(|s| !s.is_empty());
                        }
                    });
                },
                cx,
            );
            out.push(
                div()
                    .border_1()
                    .border_color(theme::edge())
                    .rounded(px(5.))
                    .p(px(11.))
                    .mb(px(9.))
                    .child(
                        div()
                            .flex()
                            .gap(px(9.))
                            .mb(px(9.))
                            .child(div().flex_grow().flex_basis(px(0.)).min_w_0().child(input_box(label)))
                            .child(div().flex_grow().flex_basis(px(0.)).min_w_0().flex().child(hk))
                            .child(div().flex_grow().flex_basis(px(0.)).min_w_0().child(source)),
                    )
                    .child(input_box(text))
                    .child(
                        div()
                            .flex()
                            .gap(px(9.))
                            .mt(px(9.))
                            .items_center()
                            .child(div().flex_1().min_w_0().child(effort))
                            .child(div().flex_1().min_w_0().child(input_box(model)))
                            .child(button(
                                SharedString::from(format!("drop-prompt-{ix}")),
                                "Remove",
                                true,
                                cx.listener(move |this, _, window, cx| {
                                    this.set(window, cx, true, move |c| {
                                        if ix < c.prompts.len() {
                                            c.prompts.remove(ix);
                                        }
                                    });
                                }),
                            )),
                    )
                    .into_any_element(),
            );
        }
        out.push(
            div()
                .flex()
                .child(button(
                    "add-prompt",
                    "Add a prompt",
                    false,
                    cx.listener(|this, _, window, cx| {
                        let id = format!("custom_{}", chrono::Utc::now().timestamp_millis());
                        this.set(window, cx, true, move |c| {
                            c.prompts.push(PromptPreset {
                                id,
                                label: "New prompt".into(),
                                accelerator: String::new(),
                                source: PromptSource::Screenshot,
                                text: "Jelaskan apa yang ada di layar ini.".into(),
                                effort: None,
                                model: None,
                            })
                        });
                    }),
                ))
                .into_any_element(),
        );
        out
    }

    fn view_keys(&mut self, snap: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![
            section("API keys", true).into_any_element(),
            hint("Stored encrypted by the OS keystore, never in the config file.").mb(px(14.)).into_any_element(),
        ];
        for p in PROVIDERS.iter().filter(|p| p.needs_key) {
            let saved = snap.keys.get(p.id).copied().unwrap_or(false);
            let input = self.input(
                &format!("key.{}", p.id),
                "",
                if saved { "•••••••• saved" } else { "paste an API key" },
                FieldKind::Password,
                window,
                cx,
            );
            out.push(
                div()
                    .flex()
                    .items_end()
                    .gap(px(9.))
                    .mb(px(11.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(96.))
                            .text_size(px(11.5))
                            .child(p.label)
                            .child(div().text_size(px(9.5)).text_color(theme::edge()).child(p.kind.as_str())),
                    )
                    .child(div().flex_1().min_w_0().child(input_box(input)))
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::mono_family())
                            .text_size(px(10.))
                            .text_color(if saved { theme::lamp() } else { theme::edge() })
                            .child(if saved { "saved" } else { "none" }),
                    )
                    .into_any_element(),
            );
            if !p.key_url.is_empty() {
                let url = p.key_url;
                out.push(
                    div()
                        .id(SharedString::from(format!("key-url-{}", p.id)))
                        .ml(px(105.))
                        .mt(px(-6.))
                        .mb(px(12.))
                        .text_size(px(10.5))
                        .text_color(theme::lamp())
                        .cursor_pointer()
                        .child(url)
                        .on_click(move |_, _, _| crate::platform::open_external(url))
                        .into_any_element(),
                );
            }
        }

        out.push(section("Global hotkeys", false).into_any_element());
        out.push(
            hint("Click a box and press the combination. At least one modifier is required. A key another app already owns is reported as unavailable.")
                .mb(px(14.))
                .into_any_element(),
        );
        for id in HotkeyId::ALL {
            let value = snap.cfg.hotkeys.get(id).to_string();
            let hk = self.hotkey_field(&hotkey_path(id), &value, window, cx);
            out.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .mb(px(11.))
                    .child(div().flex_none().w(px(96.)).text_size(px(11.5)).child(id.settings_label()))
                    .child(hk)
                    .into_any_element(),
            );
        }
        out.push(
            div()
                .mt(px(8.))
                .flex()
                .child(button("quit", "Quit VeilPilot", false, |_, _, cx| cx.quit()))
                .into_any_element(),
        );
        out
    }
}

pub fn hotkey_path(id: HotkeyId) -> String {
    format!("hotkeys.{id:?}")
}

#[derive(Clone, Copy)]
enum FieldKind {
    Text,
    Number,
    Password,
    Area(usize),
}

fn input_box(input: Entity<TextInput>) -> AnyElement {
    div()
        .w_full()
        .bg(theme::glass())
        .border_1()
        .border_color(theme::edge())
        .rounded(px(4.))
        .px(px(8.))
        .py(px(7.))
        .text_size(px(11.5))
        .line_height(px(17.))
        .child(input)
        .into_any_element()
}

fn row(children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .gap(px(9.))
        .children(children.into_iter().map(|c| div().flex_grow().flex_basis(px(0.)).min_w_0().child(c)))
        .into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snap) = self.snapshot(cx) else { return div().into_any_element() };
        let body = match self.tab {
            SettingsTab::Ai => self.view_ai(&snap, window, cx),
            SettingsTab::Voice => self.view_voice(&snap, window, cx),
            SettingsTab::Shots => self.view_shots(&snap, window, cx),
            SettingsTab::Prompts => self.view_prompts(&snap, window, cx),
            SettingsTab::Keys => self.view_keys(&snap, window, cx),
        };
        let current = self.tab;
        let panel = self.panel.clone();

        div()
            .id("settings")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::void())
            .occlude()
            .child(
                div()
                    .id("settings-rail")
                    .flex_none()
                    .h(px(theme::RAIL_H))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(theme::PAD))
                    .pr(px(10.))
                    .border_b_1()
                    .border_color(theme::edge())
                    .on_mouse_down(gpui::MouseButton::Left, {
                        let panel = panel.clone();
                        move |_, _, cx| {
                            if let Some(native) = panel.upgrade().and_then(|p| p.read(cx).native) {
                                cx.defer(move |_| native.begin_drag());
                            }
                        }
                    })
                    .child(caps("Settings").text_color(theme::haze()))
                    .child(div().flex_1())
                    .children(SettingsTab::ALL.iter().map(|tab| {
                        let tab = *tab;
                        let on = tab == current;
                        swallow_mouse_down(
                            div()
                                .id(SharedString::from(format!("tab-{}", tab.label())))
                                .px(px(5.))
                                .py(px(4.))
                                .cursor_pointer()
                                .text_size(theme::label_size())
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(if on { theme::lamp() } else { theme::edge() })
                                .hover(move |s| if on { s } else { s.text_color(theme::haze()) })
                                .child(tab.label().to_uppercase())
                                .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx))),
                        )
                    }))
                    .child(chrome_button("settings-done", "done", {
                        let panel = panel.clone();
                        move |_, window, cx| {
                            if let Some(p) = panel.upgrade() {
                                p.update(cx, |p, cx| {
                                    p.close_settings(window, cx);
                                    window.focus(&p.focus_handle(cx));
                                });
                            }
                        }
                    })),
            )
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p(px(theme::PAD))
                    .flex()
                    .flex_col()
                    .text_color(theme::read())
                    .text_size(px(12.))
                    // Fields, selects and buttons stop their own mouse-down, so
                    // this only sees clicks on empty space.
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            window.focus(&this.focus_handle);
                            if this.open_select.take().is_some() {
                                cx.notify();
                            }
                        }),
                    )
                    .children(body),
            )
            .into_any_element()
    }
}

use gpui::Focusable as _;
