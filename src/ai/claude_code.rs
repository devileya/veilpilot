//! Runs the local `claude` CLI in headless mode and maps its NDJSON stream onto
//! the same chunks the other adapters yield. The point is auth: this uses the
//! login Claude Code already has, so no API key is stored in VeilPilot.
//!
//! Verified wire behaviour (CLI 2.1.280):
//! - `--input-format stream-json` accepts base64 image blocks directly, so a
//!   screenshot needs no Read tool and no temp file.
//! - It requires `--output-format stream-json`.
//! - `{"type":"stream_event","event":...}` wraps raw Anthropic SSE events.
//! - `{"type":"result",...}` is terminal and carries usage and real cost.
//! - `--safe-mode --strict-mcp-config` skips hooks, MCP, plugins and CLAUDE.md;
//!   measured ~11s -> ~1.5s to first token.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use super::{Chunk, ChunkSender, ClaudeCodeOptions, Done, StreamOptions};
use crate::session::Turn;

static CACHED: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(windows) {
        let app_data = std::env::var("APPDATA").unwrap_or_default();
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            home.join("claude").join("bin").join("claude.exe"),
            home.join(".claude").join("bin").join("claude.exe"),
            PathBuf::from(app_data).join("npm").join("claude.cmd"),
            PathBuf::from(local).join("Programs").join("claude").join("claude.exe"),
            home.join(".local").join("bin").join("claude.exe"),
            home.join(".local").join("bin").join("claude.cmd"),
        ]
    } else {
        vec![
            home.join(".local/bin/claude"),
            home.join(".claude/bin/claude"),
            home.join(".claude/local/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
            PathBuf::from("/usr/bin/claude"),
        ]
    }
}

/// A GUI app on macOS inherits a minimal PATH that does not include
/// ~/.local/bin, which is where the installer puts the binary — so PATH alone
/// is not enough to find it.
pub fn resolve_binary(configured: &str) -> anyhow::Result<PathBuf> {
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if is_executable(&path) {
            return Ok(path);
        }
        bail!("Claude Code not found at {configured}. Fix the path in settings › AI.");
    }
    if let Some(path) = CACHED.lock().unwrap().clone().filter(|p| is_executable(p)) {
        return Ok(path);
    }
    let exts: &[&str] = if cfg!(windows) { &[".cmd", ".exe", ""] } else { &[""] };
    let from_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("claude{ext}"))));
    let found = from_path.chain(candidate_paths()).find(|p| is_executable(p));
    match found {
        Some(path) => {
            *CACHED.lock().unwrap() = Some(path.clone());
            Ok(path)
        }
        None => bail!("Claude Code CLI not found. Install it, or set the binary path in settings › AI."),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub logged_in: bool,
    pub method: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detection {
    pub ok: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub account: Option<Account>,
    pub error: Option<String>,
}

async fn run_quick(binary: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(binary).args(args).stdin(Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| anyhow!("timed out"))??;
    if !output.status.success() && output.stdout.is_empty() {
        bail!("exited with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Finding the binary is not enough: an installed-but-logged-out CLI fails at
/// ask time with a confusing error, so the login is checked up front.
pub async fn detect(configured: &str) -> Detection {
    let binary = match resolve_binary(configured) {
        Ok(b) => b,
        Err(err) => return Detection { error: Some(err.to_string()), ..Default::default() },
    };
    let path = binary.display().to_string();
    let (version, auth) = tokio::join!(run_quick(&binary, &["--version"]), run_quick(&binary, &["auth", "status"]));

    let version = match version {
        Ok(v) => v,
        Err(err) => {
            return Detection {
                path: Some(path),
                error: Some(format!("Could not run it: {err}")),
                ..Default::default()
            };
        }
    };

    // An older CLI without `auth status` gets the benefit of the doubt; the
    // ask will surface any problem.
    let account = auth.ok().and_then(|out| serde_json::from_str::<Value>(&out).ok()).map(|v| Account {
        logged_in: v.get("loggedIn").and_then(Value::as_bool).unwrap_or(false),
        method: v.get("authMethod").and_then(Value::as_str).map(String::from),
        email: v.get("email").and_then(Value::as_str).map(String::from),
    });

    if account.as_ref().is_some_and(|a| !a.logged_in) {
        return Detection {
            ok: false,
            path: Some(path),
            version: Some(version),
            account,
            error: Some("Claude Code is installed but not signed in. Run `claude auth login` in a terminal, then reopen this panel.".into()),
        };
    }
    Detection { ok: true, path: Some(path), version: Some(version), account, error: None }
}

pub fn build_args(opts: &StreamOptions, cc: &ClaudeCodeOptions) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Nobody is around to answer a permission prompt, so anything that
        // would ask is denied instead of hanging the panel forever.
        "--permission-prompts",
        "none",
        "--tools",
        if cc.allow_tools { "default" } else { "" },
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if cc.fast_start {
        args.extend(["--safe-mode".into(), "--strict-mcp-config".into()]);
    }
    if !opts.system.is_empty() {
        args.extend(["--system-prompt".into(), opts.system.clone()]);
    }
    if !opts.model.is_empty() {
        args.extend(["--model".into(), opts.model.clone()]);
    }
    if let Some(effort) = &opts.effort {
        args.extend(["--effort".into(), effort.clone()]);
    }
    if cc.max_budget_usd > 0.0 {
        args.extend(["--max-budget-usd".into(), cc.max_budget_usd.to_string()]);
    }
    // Claude Code owns the conversation: the first ask names a session, every
    // later ask resumes it, so only the newest turn is ever sent.
    if let Some(id) = &cc.resume_id {
        args.extend(["--resume".into(), id.clone()]);
    } else if let Some(id) = &cc.session_id {
        args.extend(["--session-id".into(), id.clone()]);
    }
    args
}

pub fn user_message(turn: &Turn) -> String {
    let mut content: Vec<Value> = turn
        .images
        .iter()
        .map(|img| json!({ "type": "image", "source": { "type": "base64", "media_type": img.mime, "data": img.base64.as_str() } }))
        .collect();
    content.push(json!({ "type": "text", "text": turn.text }));
    json!({ "type": "user", "message": { "role": "user", "content": content } }).to_string()
}

/// Map one NDJSON line. `Ok(Some(true))` means the result event arrived.
fn handle_line(line: &str, opts: &StreamOptions, streamed: &mut usize, tx: &ChunkSender) -> anyhow::Result<bool> {
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return Ok(false); // non-JSON noise on stdout is not fatal
    };
    match event.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let inner = event.get("event");
            if inner.and_then(|e| e.get("type")).and_then(Value::as_str) != Some("content_block_delta") {
                return Ok(false);
            }
            let delta = inner.and_then(|e| e.get("delta"));
            match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.and_then(|d| d.get("text")).and_then(Value::as_str) {
                        *streamed += 1;
                        let _ = tx.unbounded_send(Chunk::Text(text.to_string()));
                    }
                }
                Some("thinking_delta") if opts.show_thinking => {
                    if let Some(text) =
                        delta.and_then(|d| d.get("thinking")).and_then(Value::as_str).filter(|t| !t.is_empty())
                    {
                        let _ = tx.unbounded_send(Chunk::Thinking(text.to_string()));
                    }
                }
                _ => {}
            }
            Ok(false)
        }
        Some("result") => {
            if event.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                let message = event
                    .get("result")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .or_else(|| event.get("api_error_status").map(|v| v.to_string()))
                    .unwrap_or_else(|| "Claude Code reported an error.".into());
                bail!(message);
            }
            // Belt and braces: if partial messages were suppressed for any
            // reason, the terminal event still carries the whole answer.
            if *streamed == 0 {
                if let Some(result) = event.get("result").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    let _ = tx.unbounded_send(Chunk::Text(result.to_string()));
                }
            }
            let model = event
                .get("modelUsage")
                .and_then(Value::as_object)
                .and_then(|m| m.keys().next().cloned())
                .or_else(|| (!opts.model.is_empty()).then(|| opts.model.clone()))
                .unwrap_or_else(|| "claude code".into());
            let _ = tx.unbounded_send(Chunk::Done(Done {
                stop_reason: event.get("stop_reason").and_then(Value::as_str).map(String::from),
                model: Some(model),
                cost_usd: event.get("total_cost_usd").and_then(Value::as_f64),
            }));
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub async fn stream(opts: &StreamOptions, tx: &ChunkSender) -> anyhow::Result<()> {
    let cc = opts.claude_code.clone().unwrap_or_default();
    let binary = resolve_binary(&cc.binary_path)?;
    let turn = opts.messages.last().context("Nothing to send.")?;

    let cwd =
        if !cc.cwd.is_empty() && Path::new(&cc.cwd).is_dir() { PathBuf::from(&cc.cwd) } else { std::env::temp_dir() };

    let mut command = tokio::process::Command::new(&binary);
    command
        .args(build_args(opts, &cc))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Cancelling drops this future, and with it the child.
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = command.spawn().map_err(|e| anyhow!("Could not start Claude Code: {e}"))?;

    // EPIPE if the CLI exits before reading stdin; the exit code tells the story.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{}\n", user_message(turn)).as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut buf).await;
        }
        buf
    });

    let stdout = child.stdout.take().context("no stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut streamed = 0usize;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if handle_line(&line, opts, &mut streamed, tx)? {
            return Ok(());
        }
    }

    // Fell off the end without a result event.
    let status = child.wait().await?;
    let stderr = stderr_task.await.unwrap_or_default();
    let tail: Vec<&str> = stderr.trim().lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
    let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
    if tail.is_empty() {
        bail!("Claude Code exited ({code}) without answering.")
    }
    bail!("Claude Code exited ({code}) without answering. {}", tail.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;

    fn opts() -> StreamOptions {
        StreamOptions {
            kind: crate::config::ProviderKind::ClaudeCode,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            system: "sys".into(),
            messages: vec![],
            max_tokens: 1,
            effort: Some("low".into()),
            show_thinking: true,
            server_side_fallback: false,
            claude_code: None,
        }
    }

    #[test]
    fn args_name_then_resume_the_session() {
        let o = opts();
        let first = build_args(
            &o,
            &ClaudeCodeOptions { fast_start: true, session_id: Some("abc".into()), ..Default::default() },
        );
        assert!(first.windows(2).any(|w| w == ["--session-id", "abc"]));
        assert!(first.contains(&"--safe-mode".to_string()));
        assert!(first.windows(2).any(|w| w == ["--tools", ""]));
        assert!(first.windows(2).any(|w| w == ["--effort", "low"]));
        let later = build_args(
            &o,
            &ClaudeCodeOptions {
                resume_id: Some("abc".into()),
                allow_tools: true,
                max_budget_usd: 0.5,
                ..Default::default()
            },
        );
        assert!(later.windows(2).any(|w| w == ["--resume", "abc"]));
        assert!(!later.contains(&"--session-id".to_string()));
        assert!(later.windows(2).any(|w| w == ["--tools", "default"]));
        assert!(later.windows(2).any(|w| w == ["--max-budget-usd", "0.5"]));
    }

    #[test]
    fn maps_ndjson_events() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let o = opts();
        let mut streamed = 0;
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            "garbage",
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"t"}}}"#,
        ];
        for l in lines {
            assert!(!handle_line(l, &o, &mut streamed, &tx).unwrap());
        }
        let done = r#"{"type":"result","is_error":false,"result":"Hello","stop_reason":"end_turn","total_cost_usd":0.0123,"modelUsage":{"claude-sonnet-5":{}}}"#;
        assert!(handle_line(done, &o, &mut streamed, &tx).unwrap());
        drop(tx);
        let got: Vec<Chunk> = futures::executor::block_on(rx.by_ref().collect());
        assert_eq!(got[0], Chunk::Text("Hel".into()));
        assert_eq!(got[1], Chunk::Thinking("t".into()));
        assert_eq!(
            got[2],
            Chunk::Done(Done {
                stop_reason: Some("end_turn".into()),
                model: Some("claude-sonnet-5".into()),
                cost_usd: Some(0.0123)
            })
        );
        let err = r#"{"type":"result","is_error":true,"result":"Credit balance is too low"}"#;
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        assert_eq!(handle_line(err, &o, &mut 0, &tx).unwrap_err().to_string(), "Credit balance is too low");
    }

    #[test]
    fn result_without_partials_still_yields_text() {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let mut streamed = 0;
        handle_line(r#"{"type":"result","result":"whole answer"}"#, &opts(), &mut streamed, &tx).unwrap();
        drop(tx);
        let got: Vec<Chunk> = futures::executor::block_on(rx.collect());
        assert_eq!(got[0], Chunk::Text("whole answer".into()));
    }

    #[test]
    fn user_message_puts_images_first() {
        let turn = Turn {
            role: crate::session::Role::User,
            text: "q".into(),
            images: vec![crate::session::ImagePart { mime: "image/png", base64: std::sync::Arc::new("AA==".into()) }],
        };
        let v: Value = serde_json::from_str(&user_message(&turn)).unwrap();
        assert_eq!(v["message"]["content"][0]["type"], "image");
        assert_eq!(v["message"]["content"][1]["text"], "q");
    }
}
