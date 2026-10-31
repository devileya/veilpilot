//! Provider adapters and the pieces of an ask that do not touch the UI:
//! prompt building, request options, and running a stream with cancellation.

pub mod anthropic;
pub mod claude_code;
pub mod cli;
pub mod openai;
pub mod sse;

use std::sync::LazyLock;

use futures::channel::mpsc;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, PromptSource, ProviderKind};
use crate::session::Turn;

/// What an adapter yields while streaming.
#[derive(Clone, Debug, PartialEq)]
pub enum Chunk {
    Text(String),
    Thinking(String),
    Note(String),
    Done(Done),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Done {
    pub stop_reason: Option<String>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
}

/// What the UI receives for one ask.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Chunk(Chunk),
    Failed(String),
    Cancelled,
    /// The stream ended without a terminal frame (a proxy cut the connection).
    Incomplete,
}

#[derive(Clone, Debug, Default)]
pub struct ClaudeCodeOptions {
    pub binary_path: String,
    pub cwd: String,
    pub fast_start: bool,
    pub allow_tools: bool,
    pub max_budget_usd: f64,
    pub session_id: Option<String>,
    pub resume_id: Option<String>,
    /// One-off asks (live answers, summaries) leave nothing in Claude
    /// Code's session history.
    pub ephemeral: bool,
}

#[derive(Clone, Debug)]
pub struct StreamOptions {
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub system: String,
    pub messages: Vec<Turn>,
    pub max_tokens: u32,
    pub effort: Option<String>,
    pub show_thinking: bool,
    pub server_side_fallback: bool,
    pub claude_code: Option<ClaudeCodeOptions>,
    pub cli: Option<cli::CliOptions>,
    /// Where agents run: the picked project's folder, or a temp dir.
    pub cwd: Option<std::path::PathBuf>,
}

pub type ChunkSender = mpsc::UnboundedSender<Chunk>;

/* ---------- prompts ---------- */

#[derive(Clone, Debug, PartialEq)]
pub struct BuiltPrompt {
    pub text: String,
    pub label: String,
    pub source: Option<PromptSource>,
    pub effort: Option<String>,
    pub model: Option<String>,
}

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{\s*(\w+)\s*\}\}").unwrap());
static INPUT_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{\s*input\s*\}\}").unwrap());

pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    PLACEHOLDER
        .replace_all(template, |caps: &regex::Captures| {
            let key = &caps[1];
            vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string()).unwrap_or_default()
        })
        .into_owned()
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn build_prompt(cfg: &Config, prompt_id: Option<&str>, input: &str, transcript: &str) -> BuiltPrompt {
    let input = input.trim();
    let Some(preset) = prompt_id.and_then(|id| cfg.prompt(id)) else {
        return BuiltPrompt { text: input.to_string(), label: "Ask".into(), source: None, effort: None, model: None };
    };

    let mut text = fill(&preset.text, &[("transcript", transcript), ("input", input)]);
    // A preset that never references {{input}} still shouldn't throw away
    // text the user typed before firing the hotkey.
    if !input.is_empty() && !INPUT_PLACEHOLDER.is_match(&preset.text) {
        text.push_str("\n\nCatatan dari saya: ");
        text.push_str(input);
    }

    BuiltPrompt {
        text: text.trim().to_string(),
        label: if preset.label.is_empty() { preset.id.clone() } else { preset.label.clone() },
        source: Some(preset.source),
        effort: non_empty(&preset.effort),
        model: non_empty(&preset.model),
    }
}

/* ---------- running ---------- */

pub struct Running {
    pub events: mpsc::UnboundedReceiver<StreamEvent>,
    pub cancel: CancellationToken,
}

/// Start streaming on the I/O runtime. The returned receiver always ends with
/// exactly one terminal event: `Done`, `Failed`, `Cancelled`, or `Incomplete`.
pub fn start(opts: StreamOptions) -> Running {
    let (events_tx, events) = mpsc::unbounded();
    let cancel = CancellationToken::new();
    let token = cancel.clone();

    crate::runtime::spawn(async move {
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded::<Chunk>();
        let forward = {
            let events_tx = events_tx.clone();
            async move {
                use futures::StreamExt as _;
                let mut finished = false;
                while let Some(chunk) = chunk_rx.next().await {
                    finished |= matches!(chunk, Chunk::Done(_));
                    let _ = events_tx.unbounded_send(StreamEvent::Chunk(chunk));
                }
                finished
            }
        };
        let adapter = async move {
            match opts.kind {
                ProviderKind::OpenAi => openai::stream(&opts, &chunk_tx).await,
                ProviderKind::Anthropic => anthropic::stream(&opts, &chunk_tx).await,
                ProviderKind::ClaudeCode => claude_code::stream(&opts, &chunk_tx).await,
                ProviderKind::AgentCli(kind) => cli::stream(&opts, kind, &chunk_tx).await,
                ProviderKind::WhisperCpp => {
                    Err(anyhow::anyhow!("Local (whisper.cpp) only transcribes — pick a chat provider in Settings."))
                }
            }
            // chunk_tx drops here, which ends `forward`.
        };

        let both = async {
            let (result, finished) = futures::join!(adapter, forward);
            (result, finished)
        };

        // Dropping the adapter future closes the HTTP connection or kills the
        // CLI child, which is the whole of cancelling.
        let terminal = tokio::select! {
            _ = token.cancelled() => StreamEvent::Cancelled,
            (result, finished) = both => match result {
                Err(err) => StreamEvent::Failed(format!("{err:#}")),
                Ok(()) if finished => return,
                Ok(()) => StreamEvent::Incomplete,
            },
        };
        let _ = events_tx.unbounded_send(terminal);
    });

    Running { events, cancel }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_known_and_blanks_unknown() {
        assert_eq!(fill("a {{ transcript }} b {{nope}} c", &[("transcript", "T")]), "a T b  c");
    }

    #[test]
    fn free_text_ask_has_no_preset() {
        let p = build_prompt(&Config::default(), None, "  what is this? ", "");
        assert_eq!(p.text, "what is this?");
        assert_eq!(p.label, "Ask");
        assert!(p.source.is_none());
    }

    #[test]
    fn preset_appends_typed_input_when_template_ignores_it() {
        let cfg = Config::default();
        let p = build_prompt(&cfg, Some("explain"), "fokus ke error", "");
        assert!(p.text.starts_with("Jelaskan apa yang ada di layar"));
        assert!(p.text.ends_with("Catatan dari saya: fokus ke error"));
        let p = build_prompt(&cfg, Some("recap"), "", "line one\nline two");
        assert!(p.text.ends_with("line one\nline two"));
        assert_eq!(p.source, Some(PromptSource::Transcript));
        let p = build_prompt(&cfg, Some("solve"), "", "");
        assert_eq!(p.effort.as_deref(), Some("high"));
    }

    #[test]
    fn preset_with_input_placeholder_uses_it_inline() {
        let mut cfg = Config::default();
        cfg.prompts[0].text = "Q: {{input}}".into();
        cfg.prompts[0].effort = Some(String::new());
        let p = build_prompt(&cfg, Some("explain"), "why", "");
        assert_eq!(p.text, "Q: why");
        assert!(p.effort.is_none(), "an empty effort means the default");
    }
}
