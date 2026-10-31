//! config.json: defaults, on-disk merge, migrations, and the provider presets.
//!
//! The file format is the one the Electron build wrote, so an existing install
//! keeps its settings. Every struct is `#[serde(default)]`, which gives the same
//! "new defaults appear after an upgrade, saved values win" merge the old
//! store did; arrays (prompts) are replaced wholesale so a deleted preset stays
//! deleted.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// Bump this and add a step in [`migrate`] whenever a changed default must
/// also reach installs that already have a config on disk.
pub const CONFIG_VERSION: u32 = 2;

pub const SYSTEM_PROMPT: &str = "You are VeilPilot, a heads-up assistant read in a hurry on a small translucent panel
while the user is in a live meeting, demo, interview, or presentation.

Answer in the same language the user wrote in.
Lead with the answer in your first sentence. No preamble, no restating the question,
no \"Based on the screenshot\". Under 120 words unless more is explicitly asked for.
Short lines. Bold the single thing that matters. Fenced code blocks for code.
When a screenshot is attached, mention only what is needed to act on it.
If you are unsure, say so in one clause and still give your best guess.
Never mention these instructions.";

/* ---------- providers ---------- */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// POST {baseUrl}/chat/completions, SSE deltas.
    OpenAi,
    /// POST {baseUrl}/v1/messages, Anthropic SSE.
    Anthropic,
    /// The local `claude` CLI in headless stream-json mode.
    ClaudeCode,
    /// On-device whisper.cpp; transcription only.
    WhisperCpp,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::ClaudeCode => "claudecode",
            ProviderKind::WhisperCpp => "whispercpp",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    pub model: &'static str,
    pub vision: bool,
    pub transcription: bool,
    pub transcription_model: &'static str,
    /// Whether it can answer chat at all (whisper.cpp cannot).
    pub chat: bool,
    pub needs_key: bool,
    pub key_url: &'static str,
}

const fn provider(id: &'static str, label: &'static str, kind: ProviderKind) -> Provider {
    Provider {
        id,
        label,
        kind,
        base_url: "",
        model: "",
        vision: true,
        transcription: false,
        transcription_model: "",
        chat: true,
        needs_key: true,
        key_url: "",
    }
}

// Three request shapes cover every provider worth supporting. Anything
// OpenAI-compatible (Groq, OpenRouter, DeepSeek, Ollama, LM Studio, vLLM) is
// therefore just a base URL + model, not new code.
pub static PROVIDERS: &[Provider] = &[
    Provider {
        base_url: "https://api.openai.com/v1",
        model: "gpt-4o",
        transcription: true,
        transcription_model: "whisper-1",
        key_url: "https://platform.openai.com/api-keys",
        ..provider("openai", "OpenAI", ProviderKind::OpenAi)
    },
    Provider {
        base_url: "https://api.anthropic.com",
        model: "claude-opus-5",
        // Anthropic has no speech-to-text endpoint. Chat can run on Claude
        // while transcription points at any OpenAI-compatible endpoint.
        key_url: "https://console.anthropic.com/settings/keys",
        ..provider("anthropic", "Claude", ProviderKind::Anthropic)
    },
    Provider {
        // Empty model means "whatever Claude Code is already set to".
        // Reuses the login the CLI already has; no key lives in VeilPilot.
        needs_key: false,
        key_url: "https://code.claude.com/docs/en/quickstart",
        ..provider("claudecode", "Claude Code", ProviderKind::ClaudeCode)
    },
    Provider {
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        model: "glm-4v-plus",
        key_url: "https://open.bigmodel.cn/usercenter/apikeys",
        ..provider("glm", "GLM (Zhipu)", ProviderKind::OpenAi)
    },
    Provider {
        base_url: "https://api.groq.com/openai/v1",
        model: "llama-3.2-90b-vision-preview",
        transcription: true,
        transcription_model: "whisper-large-v3-turbo",
        key_url: "https://console.groq.com/keys",
        ..provider("groq", "Groq", ProviderKind::OpenAi)
    },
    Provider {
        base_url: "https://openrouter.ai/api/v1",
        model: "anthropic/claude-sonnet-4.5",
        key_url: "https://openrouter.ai/keys",
        ..provider("openrouter", "OpenRouter", ProviderKind::OpenAi)
    },
    Provider {
        base_url: "https://api.deepseek.com/v1",
        model: "deepseek-chat",
        vision: false,
        key_url: "https://platform.deepseek.com/api_keys",
        ..provider("deepseek", "DeepSeek", ProviderKind::OpenAi)
    },
    Provider {
        base_url: "http://127.0.0.1:11434/v1",
        model: "llama3.2-vision",
        needs_key: false,
        key_url: "https://ollama.com/download",
        ..provider("ollama", "Ollama (local)", ProviderKind::OpenAi)
    },
    Provider {
        vision: false,
        transcription: true,
        chat: false,
        needs_key: false,
        key_url: "https://github.com/ggml-org/whisper.cpp",
        ..provider("local", "Local (whisper.cpp)", ProviderKind::WhisperCpp)
    },
    Provider {
        base_url: "http://127.0.0.1:1234/v1",
        transcription: true,
        transcription_model: "whisper-1",
        needs_key: false,
        ..provider("custom", "Custom (OpenAI-compatible)", ProviderKind::OpenAi)
    },
];

pub fn provider_by_id(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/* ---------- config sections ---------- */

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaudeCodeConfig {
    /// '' -> probe PATH and the usual install locations.
    pub binary_path: String,
    /// '' -> the OS temp dir, so no project is implied.
    pub cwd: String,
    /// --safe-mode --strict-mcp-config: skips hooks, MCP, plugins and
    /// CLAUDE.md. ~11s -> ~1.5s to first token.
    pub fast_start: bool,
    /// --tools "": answers only, never touches the disk.
    pub allow_tools: bool,
    /// 0 -> no cap.
    pub max_budget_usd: f64,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            binary_path: String::new(),
            cwd: String::new(),
            fast_start: true,
            allow_tools: false,
            max_budget_usd: 0.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatConfig {
    pub provider: String,
    /// '' -> use the provider preset's base URL.
    pub base_url: String,
    /// '' -> use the provider preset's model.
    pub model: String,
    /// low | medium | high | xhigh | max (Claude and Claude Code only).
    pub effort: String,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub server_side_fallback: bool,
    pub show_thinking: bool,
    pub claude_code: ClaudeCodeConfig,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            base_url: String::new(),
            model: String::new(),
            effort: "medium".into(),
            max_tokens: 8192,
            system_prompt: SYSTEM_PROMPT.into(),
            server_side_fallback: true,
            show_thinking: true,
            claude_code: ClaudeCodeConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct WhisperCppConfig {
    /// '' -> probe PATH and the usual install locations.
    pub binary_path: String,
    /// '' -> probe the model directory.
    pub model_path: String,
    /// 0 -> let whisper.cpp decide.
    pub threads: u32,
    /// true -> always output English.
    pub translate: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptionConfig {
    /// On-device by default: meeting audio never leaves the machine.
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// '' -> autodetect.
    pub language: String,
    /// Longest segment while recording; >= 5 -> cut at least this often.
    /// 0 -> no fixed length.
    pub chunk_seconds: u32,
    /// Silence after speech that sends what was said straight to
    /// transcription; 0 -> no pause cuts.
    pub pause_seconds: f32,
    /// Input device name; '' -> system default.
    pub input_device_id: String,
    pub capture_system_audio: bool,
    pub whisper_cpp: WhisperCppConfig,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            base_url: String::new(),
            model: String::new(),
            language: String::new(),
            chunk_seconds: 0,
            pause_seconds: 1.5,
            input_device_id: String::new(),
            capture_system_audio: false,
            whisper_cpp: WhisperCppConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct CaptureConfig {
    /// Downscale before upload; caps cost and latency.
    pub max_dimension: u32,
    /// 'cursor' | 'primary' | a display id as a string.
    pub display: String,
    pub keep_last: usize,
    /// Which preset the region-shot hotkey runs; '' just attaches.
    pub region_prompt: String,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { max_dimension: 1600, display: "cursor".into(), keep_last: 6, region_prompt: "explain".into() }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowConfig {
    pub bounds: Option<WindowBounds>,
    pub opacity: f32,
    pub protection: bool,
    pub click_through: bool,
    pub show_on_start: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { bounds: None, opacity: 1.0, protection: true, click_through: false, show_on_start: true }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PromptSource {
    #[default]
    Screenshot,
    Transcript,
    Both,
}

impl PromptSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PromptSource::Screenshot => "screenshot",
            PromptSource::Transcript => "transcript",
            PromptSource::Both => "both",
        }
    }

    pub fn wants_shot(self) -> bool {
        matches!(self, PromptSource::Screenshot | PromptSource::Both)
    }

    pub fn wants_transcript(self) -> bool {
        matches!(self, PromptSource::Transcript | PromptSource::Both)
    }
}

/// A hotkey bound to a prompt. `source` decides what gets attached.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PromptPreset {
    pub id: String,
    pub label: String,
    pub accelerator: String,
    pub source: PromptSource,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HotkeyConfig {
    pub toggle_overlay: String,
    pub shot_only: String,
    pub region_shot: String,
    pub record_toggle: String,
    pub send: String,
    pub cancel: String,
    pub click_through: String,
    pub toggle_protection: String,
    pub new_session: String,
    pub opacity_up: String,
    pub opacity_down: String,
    pub move_up: String,
    pub move_down: String,
    pub move_left: String,
    pub move_right: String,
}

/// `Command` on macOS, `Control` elsewhere — the same split Electron's
/// accelerators made, so saved configs keep working.
pub fn primary_modifier() -> &'static str {
    if cfg!(target_os = "macos") { "Command" } else { "Control" }
}

fn acc(suffix: &str) -> String {
    format!("{}+Shift+{}", primary_modifier(), suffix)
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle_overlay: acc("\\"),
            shot_only: acc("S"),
            region_shot: acc("A"),
            record_toggle: acc("R"),
            send: acc("Return"),
            cancel: acc("."),
            click_through: acc("M"),
            toggle_protection: acc("P"),
            new_session: acc("K"),
            opacity_up: acc("="),
            opacity_down: acc("-"),
            move_up: "Alt+Shift+Up".into(),
            move_down: "Alt+Shift+Down".into(),
            move_left: "Alt+Shift+Left".into(),
            move_right: "Alt+Shift+Right".into(),
        }
    }
}

/// The non-prompt hotkeys, in the order Settings › Keys lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HotkeyId {
    ToggleOverlay,
    ShotOnly,
    RegionShot,
    RecordToggle,
    Send,
    Cancel,
    NewSession,
    ClickThrough,
    ToggleProtection,
    OpacityUp,
    OpacityDown,
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
}

impl HotkeyId {
    pub const ALL: [HotkeyId; 15] = [
        HotkeyId::ToggleOverlay,
        HotkeyId::ShotOnly,
        HotkeyId::RegionShot,
        HotkeyId::RecordToggle,
        HotkeyId::Send,
        HotkeyId::Cancel,
        HotkeyId::NewSession,
        HotkeyId::ClickThrough,
        HotkeyId::ToggleProtection,
        HotkeyId::OpacityUp,
        HotkeyId::OpacityDown,
        HotkeyId::MoveUp,
        HotkeyId::MoveDown,
        HotkeyId::MoveLeft,
        HotkeyId::MoveRight,
    ];

    /// Short label for Settings › Keys.
    pub fn settings_label(self) -> &'static str {
        match self {
            HotkeyId::ToggleOverlay => "Show / hide",
            HotkeyId::ShotOnly => "Attach shot",
            HotkeyId::RegionShot => "Region shot",
            HotkeyId::RecordToggle => "Transcribe",
            HotkeyId::Send => "Focus ask box",
            HotkeyId::Cancel => "Stop",
            HotkeyId::NewSession => "New session",
            HotkeyId::ClickThrough => "Click-through",
            HotkeyId::ToggleProtection => "Protection",
            HotkeyId::OpacityUp => "Opacity +",
            HotkeyId::OpacityDown => "Opacity −",
            HotkeyId::MoveUp => "Move up",
            HotkeyId::MoveDown => "Move down",
            HotkeyId::MoveLeft => "Move left",
            HotkeyId::MoveRight => "Move right",
        }
    }

    /// Longer label used when reporting a registration failure.
    pub fn failure_label(self) -> &'static str {
        match self {
            HotkeyId::ToggleOverlay => "Show / hide",
            HotkeyId::ShotOnly => "Attach screenshot",
            HotkeyId::RegionShot => "Region screenshot",
            HotkeyId::RecordToggle => "Start / stop transcribe",
            HotkeyId::Send => "Focus the ask box",
            HotkeyId::Cancel => "Stop generating",
            HotkeyId::NewSession => "New session",
            HotkeyId::ClickThrough => "Click-through",
            HotkeyId::ToggleProtection => "Capture protection",
            HotkeyId::OpacityUp => "Opacity up",
            HotkeyId::OpacityDown => "Opacity down",
            HotkeyId::MoveUp => "Move up",
            HotkeyId::MoveDown => "Move down",
            HotkeyId::MoveLeft => "Move left",
            HotkeyId::MoveRight => "Move right",
        }
    }
}

impl HotkeyConfig {
    pub fn get(&self, id: HotkeyId) -> &str {
        match id {
            HotkeyId::ToggleOverlay => &self.toggle_overlay,
            HotkeyId::ShotOnly => &self.shot_only,
            HotkeyId::RegionShot => &self.region_shot,
            HotkeyId::RecordToggle => &self.record_toggle,
            HotkeyId::Send => &self.send,
            HotkeyId::Cancel => &self.cancel,
            HotkeyId::NewSession => &self.new_session,
            HotkeyId::ClickThrough => &self.click_through,
            HotkeyId::ToggleProtection => &self.toggle_protection,
            HotkeyId::OpacityUp => &self.opacity_up,
            HotkeyId::OpacityDown => &self.opacity_down,
            HotkeyId::MoveUp => &self.move_up,
            HotkeyId::MoveDown => &self.move_down,
            HotkeyId::MoveLeft => &self.move_left,
            HotkeyId::MoveRight => &self.move_right,
        }
    }

    pub fn set(&mut self, id: HotkeyId, value: String) {
        let slot = match id {
            HotkeyId::ToggleOverlay => &mut self.toggle_overlay,
            HotkeyId::ShotOnly => &mut self.shot_only,
            HotkeyId::RegionShot => &mut self.region_shot,
            HotkeyId::RecordToggle => &mut self.record_toggle,
            HotkeyId::Send => &mut self.send,
            HotkeyId::Cancel => &mut self.cancel,
            HotkeyId::NewSession => &mut self.new_session,
            HotkeyId::ClickThrough => &mut self.click_through,
            HotkeyId::ToggleProtection => &mut self.toggle_protection,
            HotkeyId::OpacityUp => &mut self.opacity_up,
            HotkeyId::OpacityDown => &mut self.opacity_down,
            HotkeyId::MoveUp => &mut self.move_up,
            HotkeyId::MoveDown => &mut self.move_down,
            HotkeyId::MoveLeft => &mut self.move_left,
            HotkeyId::MoveRight => &mut self.move_right,
        };
        *slot = value;
    }
}

pub fn default_prompts() -> Vec<PromptPreset> {
    let p = |id: &str, label: &str, key: &str, source, effort: Option<&str>, text: &str| PromptPreset {
        id: id.into(),
        label: label.into(),
        accelerator: acc(key),
        source,
        text: text.into(),
        effort: effort.map(Into::into),
        model: None,
    };
    vec![
        p(
            "explain",
            "Jelaskan",
            "1",
            PromptSource::Screenshot,
            None,
            "Jelaskan apa yang ada di layar ini dan apa maksudnya.",
        ),
        p(
            "solve",
            "Selesaikan",
            "2",
            PromptSource::Screenshot,
            Some("high"),
            "Ada masalah di layar ini. Sebutkan akar masalahnya, lalu langkah perbaikan yang konkret dan bisa langsung dikerjakan.",
        ),
        p(
            "code",
            "Review kode",
            "3",
            PromptSource::Screenshot,
            Some("high"),
            "Review kode di layar ini: bug, edge case, dan hal yang perlu diperbaiki. Tunjukkan potongan kode versi perbaikannya.",
        ),
        p(
            "answer",
            "Jawab",
            "4",
            PromptSource::Screenshot,
            None,
            "Jawab pertanyaan yang terlihat di layar. Jawabannya dulu, lalu satu baris alasan.",
        ),
        p(
            "translate",
            "Terjemahkan",
            "5",
            PromptSource::Screenshot,
            None,
            "Terjemahkan semua teks di layar ke Bahasa Indonesia yang natural. Pertahankan strukturnya.",
        ),
        p(
            "recap",
            "Ringkas obrolan",
            "6",
            PromptSource::Transcript,
            None,
            "Ringkas transkrip berikut jadi poin-poin, lalu satu pertanyaan bagus untuk ditanyakan berikutnya.\n\n{{transcript}}",
        ),
        p(
            "reply",
            "Bantu jawab",
            "7",
            PromptSource::Transcript,
            None,
            "Berdasarkan transkrip ini, susun jawaban singkat yang bisa saya ucapkan sekarang.\n\n{{transcript}}",
        ),
    ]
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub chat: ChatConfig,
    pub transcription: TranscriptionConfig,
    pub capture: CaptureConfig,
    pub window: WindowConfig,
    pub prompts: Vec<PromptPreset>,
    pub hotkeys: HotkeyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            chat: ChatConfig::default(),
            transcription: TranscriptionConfig::default(),
            capture: CaptureConfig::default(),
            window: WindowConfig::default(),
            prompts: default_prompts(),
            hotkeys: HotkeyConfig::default(),
        }
    }
}

impl Config {
    pub fn prompt(&self, id: &str) -> Option<&PromptPreset> {
        self.prompts.iter().find(|p| p.id == id)
    }
}

/* ---------- paths ---------- */

/// The directory Electron called `userData`, so config and models carry over.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VEILPILOT_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("VeilPilot")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/* ---------- load / save ---------- */

/// A saved config always beats a new default, so changing a default only
/// reaches people who have never run the app. Anything that must also reach
/// existing installs is a migration here, indexed by the version it upgrades
/// FROM.
fn migrate(cfg: &mut Config, from: u32, has_key: &dyn Fn(&str) -> bool) -> bool {
    let mut migrated = false;
    let mut v = from;
    while v > 0 && v < CONFIG_VERSION {
        if v == 1 {
            // 1 -> 2: transcription moved on-device. Only move people who never
            // set a cloud key — for them cloud transcription could not have
            // worked anyway, and anyone who did configure one keeps it.
            if cfg.transcription.provider == "openai" && !has_key("openai") {
                cfg.transcription.provider = "local".into();
            }
            migrated = true;
        }
        v += 1;
    }
    migrated
}

/// Parse a config file's text. Unknown or malformed fields fall back to their
/// defaults rather than discarding the whole file.
pub fn parse(text: &str, has_key: &dyn Fn(&str) -> bool) -> (Config, bool) {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("[config] unreadable, using defaults: {err}");
            return (Config::default(), false);
        }
    };
    let from = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let mut cfg = from_value_lenient(value);

    // No version on disk means a fresh install, which starts on the current
    // defaults and needs no migrating.
    let migrated = migrate(&mut cfg, from, has_key);
    cfg.version = CONFIG_VERSION;
    let needs_write = migrated || (from != 0 && from != CONFIG_VERSION);
    (cfg, needs_write)
}

/// Deserialize section by section so one bad value only resets its own
/// section instead of the whole config.
fn from_value_lenient(value: serde_json::Value) -> Config {
    if let Ok(cfg) = serde_json::from_value::<Config>(value.clone()) {
        return cfg;
    }
    let mut cfg = Config::default();
    let Some(obj) = value.as_object() else { return cfg };
    macro_rules! section {
        ($key:literal, $field:ident) => {
            if let Some(v) = obj.get($key) {
                match serde_json::from_value(v.clone()) {
                    Ok(parsed) => cfg.$field = parsed,
                    Err(err) => eprintln!("[config] {} unreadable, using defaults: {err}", $key),
                }
            }
        };
    }
    section!("chat", chat);
    section!("transcription", transcription);
    section!("capture", capture);
    section!("window", window);
    section!("prompts", prompts);
    section!("hotkeys", hotkeys);
    cfg
}

pub fn load(has_key: &dyn Fn(&str) -> bool) -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, needs_write) = parse(&text, has_key);
            if needs_write {
                if let Err(err) = save(&cfg) {
                    eprintln!("[config] could not persist migrated config: {err:#}");
                }
            }
            cfg
        }
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!("[config] unreadable, using defaults: {err}");
            }
            Config::default()
        }
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("creating the config directory")?;
    }
    let text = serde_json::to_string_pretty(cfg)?;
    // Write-then-rename so a crash mid-write never leaves half a config.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).context("writing config")?;
    std::fs::rename(&tmp, &path).context("replacing config")?;
    Ok(())
}

/* ---------- resolution ---------- */

/// A config section collapsed together with its provider preset, so callers
/// never have to remember which blanks fall back to what.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub label: String,
    pub kind: ProviderKind,
    pub vision: bool,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub needs_key: bool,
}

fn resolve_with(provider_id: &str, base_url: &str, model: &str, chat: bool, key: &dyn Fn(&str) -> String) -> Resolved {
    let preset = provider_by_id(provider_id).unwrap_or(&PROVIDERS[0]);
    let base = if base_url.is_empty() { preset.base_url } else { base_url };
    let model = if !model.is_empty() {
        model.to_string()
    } else if chat {
        preset.model.to_string()
    } else {
        preset.transcription_model.to_string()
    };
    Resolved {
        label: preset.label.to_string(),
        kind: preset.kind,
        vision: preset.vision,
        base_url: base.trim_end_matches('/').to_string(),
        model,
        api_key: key(provider_id),
        needs_key: preset.needs_key,
    }
}

pub fn resolve_chat(cfg: &Config, key: &dyn Fn(&str) -> String) -> Resolved {
    resolve_with(&cfg.chat.provider, &cfg.chat.base_url, &cfg.chat.model, true, key)
}

pub fn resolve_transcription(cfg: &Config, key: &dyn Fn(&str) -> String) -> Resolved {
    let t = &cfg.transcription;
    resolve_with(&t.provider, &t.base_url, &t.model, false, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn electron_config_round_trips() {
        let text = r#"{
          "version": 2,
          "chat": { "provider": "claudecode", "model": "sonnet", "effort": "low",
                    "claudeCode": { "binaryPath": "", "fastStart": true, "maxBudgetUsd": 0 } },
          "window": { "bounds": { "x": 1142, "y": 49, "width": 431, "height": 581 },
                      "opacity": 1, "protection": false },
          "prompts": [{ "id": "explain", "label": "Jelaskan", "accelerator": "Command+Shift+1",
                        "source": "screenshot", "text": "x" }]
        }"#;
        let (cfg, write) = parse(text, &|_| false);
        assert!(!write);
        assert_eq!(cfg.chat.provider, "claudecode");
        assert_eq!(cfg.chat.model, "sonnet");
        assert_eq!(cfg.chat.max_tokens, 8192, "missing fields take defaults");
        assert!(cfg.chat.claude_code.fast_start);
        assert!(!cfg.window.protection);
        assert_eq!(cfg.window.bounds.unwrap().width, 431.0);
        assert_eq!(cfg.prompts.len(), 1, "arrays replace defaults wholesale");
        assert_eq!(cfg.transcription.provider, "local");
        assert_eq!(cfg.transcription.pause_seconds, 1.5, "existing installs get pause cuts");
        // Saving and re-reading keeps the same shape.
        let again = serde_json::to_string(&cfg).unwrap();
        assert!(again.contains("\"claudeCode\""));
        assert!(again.contains("\"systemPrompt\""));
        let (back, _) = parse(&again, &|_| false);
        assert_eq!(back, cfg);
    }

    #[test]
    fn migration_from_v1_moves_keyless_installs_on_device() {
        let text = r#"{ "version": 1, "transcription": { "provider": "openai" } }"#;
        let (cfg, write) = parse(text, &|_| false);
        assert!(write);
        assert_eq!(cfg.transcription.provider, "local");
        assert_eq!(cfg.version, CONFIG_VERSION);

        let (cfg, _) = parse(text, &|id| id == "openai");
        assert_eq!(cfg.transcription.provider, "openai", "anyone with a key keeps cloud");
    }

    #[test]
    fn fresh_install_is_not_migrated() {
        let (cfg, write) = parse(r#"{ "transcription": { "provider": "openai" } }"#, &|_| false);
        assert!(!write);
        assert_eq!(cfg.transcription.provider, "openai");
    }

    #[test]
    fn bad_section_only_resets_itself() {
        let (cfg, _) =
            parse(r#"{ "version": 2, "chat": { "maxTokens": "lots" }, "capture": { "keepLast": 3 } }"#, &|_| false);
        assert_eq!(cfg.chat.max_tokens, 8192);
        assert_eq!(cfg.capture.keep_last, 3);
    }

    #[test]
    fn resolve_falls_back_to_presets() {
        let mut cfg = Config::default();
        cfg.chat.provider = "groq".into();
        let r = resolve_chat(&cfg, &|_| "k".into());
        assert_eq!(r.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(r.model, "llama-3.2-90b-vision-preview");
        let t = resolve_transcription(
            &Config {
                transcription: TranscriptionConfig { provider: "groq".into(), ..Default::default() },
                ..Default::default()
            },
            &|_| String::new(),
        );
        assert_eq!(t.model, "whisper-large-v3-turbo");
        cfg.chat.base_url = "http://x/v1///".into();
        assert_eq!(resolve_chat(&cfg, &|_| String::new()).base_url, "http://x/v1");
    }
}
