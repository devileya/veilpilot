//! Other coding agents' CLIs, run one-shot: Codex (`codex exec --json`),
//! Gemini CLI (`gemini -p`), opencode (`opencode run --format json`),
//! GitHub Copilot CLI (`copilot -p`), Aider (`aider --message`), or any
//! command. They use whatever login the CLI already has.
//!
//! None of them takes a system prompt or a VeilPilot conversation id, so the
//! instructions and the recent turns travel in the prompt itself, and
//! screenshots are written to temporary files the CLI is pointed at.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use regex::Regex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use super::{Chunk, ChunkSender, Done, StreamOptions};
use crate::ai::claude_code::is_executable;
use crate::config::CliKind;
use crate::session::{Role, Turn};

#[derive(Clone, Debug, Default)]
pub struct CliOptions {
    pub binary_path: String,
    /// The custom CLI's command line (`{prompt}` is replaced).
    pub custom_command: String,
}

/* ---------- finding the binary ---------- */

/// The login shell's PATH. A GUI app on macOS starts with a minimal PATH, and
/// npm / bun / nvm installs live elsewhere — DevCat asks the shell the same way.
fn shell_path() -> &'static [PathBuf] {
    static PATH: OnceLock<Vec<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        if cfg!(windows) {
            return Vec::new();
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let marker = "__VEILPILOT_PATH__";
        let child = std::process::Command::new(shell)
            .args(["-ilc", &format!("printf '%s%s%s' {marker} \"$PATH\" {marker}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return Vec::new() };
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    return Vec::new();
                }
            }
        }
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            use std::io::Read as _;
            let _ = stdout.read_to_string(&mut out);
        }
        out.split(marker).nth(1).map(|p| std::env::split_paths(p).collect()).unwrap_or_default()
    })
}

fn known_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs = vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join("bin"),
    ];
    if cfg!(windows) {
        if let Ok(app_data) = std::env::var("APPDATA") {
            dirs.push(PathBuf::from(app_data).join("npm"));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs"));
        }
    }
    dirs
}

pub fn resolve_binary(kind: CliKind, configured: &str) -> anyhow::Result<PathBuf> {
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if is_executable(&path) {
            return Ok(path);
        }
        bail!("{} not found at {configured}. Fix the path in settings › AI.", kind.binary().unwrap_or("The CLI"));
    }
    let name = kind.binary().ok_or_else(|| anyhow!("Name the command in settings › AI."))?;
    let exts: &[&str] = if cfg!(windows) { &[".cmd", ".exe", ""] } else { &[""] };
    let process_path: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    process_path
        .iter()
        .chain(known_dirs().iter())
        .chain(shell_path().iter())
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|p| is_executable(p))
        .ok_or_else(|| anyhow!("{name} is not installed. {}.", capitalise(kind.install_hint())))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// The PATH children get: the CLIs are often node scripts that need `node`
/// from the same place they were installed.
fn child_path(binary: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = binary.parent().map(|p| vec![p.to_path_buf()]).unwrap_or_default();
    dirs.extend(shell_path().iter().cloned());
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    dirs.extend(known_dirs());
    std::env::join_paths(dirs).unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detection {
    pub ok: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub async fn detect(kind: CliKind, opts: &CliOptions) -> Detection {
    if kind == CliKind::Custom {
        return if opts.custom_command.trim().is_empty() {
            Detection { error: Some("Name the command in settings › AI.".into()), ..Default::default() }
        } else {
            Detection { ok: true, path: Some(opts.custom_command.clone()), ..Default::default() }
        };
    }
    let binary = match resolve_binary(kind, &opts.binary_path) {
        Ok(b) => b,
        Err(err) => return Detection { error: Some(err.to_string()), ..Default::default() },
    };
    let output = tokio::time::timeout(
        Duration::from_secs(8),
        tokio::process::Command::new(&binary)
            .arg("--version")
            .env("PATH", child_path(&binary))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let path = Some(binary.display().to_string());
    match output {
        Ok(Ok(out)) => {
            let text =
                String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).to_string();
            let version = text.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from);
            Detection { ok: true, path, version, error: None }
        }
        Ok(Err(err)) => Detection { path, error: Some(format!("Could not run it: {err}")), ..Default::default() },
        Err(_) => {
            Detection { path, error: Some("Could not run it: `--version` timed out".into()), ..Default::default() }
        }
    }
}

/* ---------- the prompt ---------- */

/// Recent turns kept in the prompt, since the CLIs do not hold our history.
const HISTORY_TURNS: usize = 6;

/// One prompt carrying the instructions, the conversation so far and the
/// new question.
pub fn compose_prompt(system: &str, messages: &[Turn]) -> String {
    let mut out = String::new();
    if !system.trim().is_empty() {
        out.push_str("<instructions>\n");
        out.push_str(system.trim());
        out.push_str("\n</instructions>\n\n");
    }
    let Some((last, earlier)) = messages.split_last() else { return out };
    let start = earlier.len().saturating_sub(HISTORY_TURNS);
    if start < earlier.len() {
        out.push_str("<conversation_so_far>\n");
        for turn in &earlier[start..] {
            let who = if turn.role == Role::User { "User" } else { "Assistant" };
            out.push_str(&format!("{who}: {}\n\n", turn.text.trim()));
        }
        out.push_str("</conversation_so_far>\n\n");
    }
    out.push_str(last.text.trim());
    out
}

/// Screenshots for the newest turn, written where the CLI can read them.
/// Removed when dropped.
struct Images(Vec<PathBuf>);

impl Images {
    fn write(turn: Option<&Turn>) -> anyhow::Result<Self> {
        let mut paths = Vec::new();
        for (n, img) in turn.map(|t| t.images.as_slice()).unwrap_or_default().iter().enumerate() {
            let bytes = base64::engine::general_purpose::STANDARD.decode(img.base64.as_bytes())?;
            let path = std::env::temp_dir().join(format!("veilpilot-shot-{}-{n}.png", uuid::Uuid::new_v4()));
            std::fs::write(&path, bytes).context("writing the screenshot for the CLI")?;
            paths.push(path);
        }
        Ok(Self(paths))
    }
}

impl Drop for Images {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// `@path` mentions, which Gemini CLI and Copilot CLI read as attachments.
fn mentions(images: &Images) -> String {
    images.0.iter().map(|p| format!(" @{}", p.display())).collect()
}

/// Split a command line the way a shell would, for the custom CLI.
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') if matches!(chars.peek(), Some('"') | Some('\\')) => cur.push(chars.next().unwrap()),
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// How a CLI is invoked: arguments, optional stdin, and how stdout reads.
pub struct Plan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub output: Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Answer text as it is printed.
    Text,
    /// opencode's `--format json` events.
    OpencodeJson,
    /// Codex's `exec --json` events.
    CodexJson,
}

fn plan(
    kind: CliKind,
    opts: &StreamOptions,
    cli: &CliOptions,
    images: &Images,
    last_message: &Path,
) -> anyhow::Result<Plan> {
    let prompt = compose_prompt(&opts.system, &opts.messages);
    let model = (!opts.model.is_empty()).then(|| opts.model.clone());
    let mut args: Vec<String> = Vec::new();
    let mut stdin = None;
    let output;
    let program = if kind == CliKind::Custom {
        let parts = split_command(&cli.custom_command);
        let (program, rest) = parts.split_first().ok_or_else(|| anyhow!("Name the command in settings › AI."))?;
        let mut used = false;
        for part in rest {
            if part.contains("{prompt}") {
                used = true;
                args.push(part.replace("{prompt}", &prompt));
            } else {
                args.push(part.clone());
            }
        }
        if !used {
            stdin = Some(prompt.clone());
        }
        output = Output::Text;
        let p = PathBuf::from(program);
        if p.components().count() > 1 { p } else { resolve_on_path(program).unwrap_or(p) }
    } else {
        let binary = resolve_binary(kind, &cli.binary_path)?;
        match kind {
            CliKind::Codex => {
                // Read-only: a HUD answer never edits files.
                args.extend(["exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only"].map(String::from));
                if let Some(m) = &model {
                    args.extend(["--model".into(), m.clone()]);
                }
                for img in &images.0 {
                    args.extend(["--image".into(), img.display().to_string()]);
                }
                args.extend(["--output-last-message".into(), last_message.display().to_string()]);
                args.push("-".into());
                stdin = Some(prompt);
                output = Output::CodexJson;
            }
            CliKind::Gemini => {
                if let Some(m) = &model {
                    args.extend(["--model".into(), m.clone()]);
                }
                args.extend(["--prompt".into(), format!("{prompt}{}", mentions(images))]);
                output = Output::Text;
            }
            CliKind::Opencode => {
                args.extend(["run", "--format", "json"].map(String::from));
                if let Some(m) = &model {
                    args.extend(["--model".into(), m.clone()]);
                }
                for img in &images.0 {
                    args.extend(["--file".into(), img.display().to_string()]);
                }
                // `--file` takes several values: without the `--` it would
                // swallow the message as one more file.
                args.extend(["--".into(), prompt]);
                output = Output::OpencodeJson;
            }
            CliKind::Copilot => {
                args.extend(["--prompt".into(), format!("{prompt}{}", mentions(images))]);
                if let Some(m) = &model {
                    args.extend(["--model".into(), m.clone()]);
                }
                output = Output::Text;
            }
            CliKind::Aider => {
                args.extend(
                    [
                        "--no-git",
                        "--yes-always",
                        "--no-auto-commits",
                        "--no-pretty",
                        "--no-show-model-warnings",
                        "--no-check-update",
                        "--no-analytics",
                    ]
                    .map(String::from),
                );
                if let Some(m) = &model {
                    args.extend(["--model".into(), m.clone()]);
                }
                args.extend(["--message".into(), prompt]);
                output = Output::Text;
            }
            CliKind::Custom => unreachable!(),
        }
        binary
    };
    Ok(Plan { program, args, stdin, output })
}

fn resolve_on_path(name: &str) -> Option<PathBuf> {
    let process_path: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    process_path
        .iter()
        .chain(known_dirs().iter())
        .chain(shell_path().iter())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/* ---------- output ---------- */

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").unwrap());

/// Aider prints its own banner around the answer.
fn aider_noise(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "Aider v",
        "Main model:",
        "Weak model:",
        "Model:",
        "Git repo:",
        "Repo-map:",
        "Tokens:",
        "Cost:",
        "Use /help",
        "https://aider.chat",
        "Added ",
        "Warning:",
    ];
    let t = line.trim_start();
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Parses one JSON line from opencode; returns text to append.
#[derive(Default)]
struct OpencodeState {
    /// Text already emitted, per part id (a part can be re-sent as it grows).
    parts: std::collections::HashMap<String, usize>,
    cost: Option<f64>,
    reason: Option<String>,
    error: Option<String>,
}

impl OpencodeState {
    fn line(&mut self, line: &str) -> Option<String> {
        let event: Value = serde_json::from_str(line).ok()?;
        match event.get("type").and_then(Value::as_str) {
            Some("text") => {
                let part = event.get("part")?;
                let id = part.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let text = part.get("text").and_then(Value::as_str)?;
                let seen = self.parts.entry(id).or_insert(0);
                let new = text.get(*seen..).unwrap_or("").to_string();
                *seen = text.len();
                (!new.is_empty()).then_some(new)
            }
            Some("step_finish") => {
                let part = event.get("part");
                self.reason = part.and_then(|p| p.get("reason")).and_then(Value::as_str).map(String::from);
                if let Some(cost) = part.and_then(|p| p.get("cost")).and_then(Value::as_f64) {
                    *self.cost.get_or_insert(0.0) += cost;
                }
                None
            }
            Some("error") => {
                let msg = event
                    .pointer("/error/data/message")
                    .or_else(|| event.pointer("/error/message"))
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("opencode reported an error");
                self.error = Some(msg.to_string());
                None
            }
            _ => None,
        }
    }
}

/// Parses one JSON line from `codex exec --json`, current and older event
/// shapes alike; returns text to append.
#[derive(Default)]
struct CodexState {
    streamed: bool,
    error: Option<String>,
}

impl CodexState {
    fn line(&mut self, line: &str) -> Option<String> {
        let event: Value = serde_json::from_str(line).ok()?;
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "item.completed" => {
                let item = event.get("item")?;
                let is_message = matches!(
                    item.get("type").or_else(|| item.get("item_type")).and_then(Value::as_str),
                    Some("agent_message") | Some("assistant_message")
                );
                if !is_message || self.streamed {
                    return None;
                }
                item.get("text").and_then(Value::as_str).map(|t| format!("{t}\n"))
            }
            "error" | "turn.failed" => {
                let msg = event
                    .pointer("/error/message")
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex reported an error");
                self.error = Some(msg.to_string());
                None
            }
            _ => {
                // Older `codex exec --json`: {"msg": {"type": "...", ...}}.
                let msg = event.get("msg")?;
                match msg.get("type").and_then(Value::as_str) {
                    Some("agent_message_delta") => {
                        self.streamed = true;
                        msg.get("delta").and_then(Value::as_str).map(String::from)
                    }
                    Some("agent_message") if !self.streamed => {
                        msg.get("message").and_then(Value::as_str).map(String::from)
                    }
                    Some("error") => {
                        self.error = msg.get("message").and_then(Value::as_str).map(String::from);
                        None
                    }
                    _ => None,
                }
            }
        }
    }
}

pub async fn stream(opts: &StreamOptions, kind: CliKind, tx: &ChunkSender) -> anyhow::Result<()> {
    let cli = opts.cli.clone().unwrap_or_default();
    let images = Images::write(opts.messages.last())?;
    let last_message = std::env::temp_dir().join(format!("veilpilot-codex-{}.txt", uuid::Uuid::new_v4()));
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _remove = Remove(last_message.clone());
    let plan = plan(kind, opts, &cli, &images, &last_message)?;
    let label = kind.binary().unwrap_or("the command");

    let cwd = opts.cwd.clone().filter(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir);
    let mut command = tokio::process::Command::new(&plan.program);
    command
        .args(&plan.args)
        .current_dir(cwd)
        .env("PATH", child_path(&plan.program))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(if plan.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn().map_err(|e| anyhow!("Could not start {label}: {e}"))?;

    if let (Some(text), Some(mut stdin)) = (plan.stdin.clone(), child.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_string(&mut buf).await;
        }
        buf
    });

    let mut stdout = child.stdout.take().context("no stdout")?;
    let mut emitted = String::new();
    let send = |text: String, emitted: &mut String| {
        if !text.is_empty() {
            emitted.push_str(&text);
            let _ = tx.unbounded_send(Chunk::Text(text));
        }
    };

    let mut opencode = OpencodeState::default();
    let mut codex = CodexState::default();
    match plan.output {
        Output::Text => {
            // Read in chunks rather than lines so answers stream; hold back an
            // incomplete UTF-8 tail and an unfinished line for Aider's filter.
            let mut pending: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                pending.extend_from_slice(&buf[..n]);
                let cut = if kind == CliKind::Aider {
                    pending.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0)
                } else {
                    match std::str::from_utf8(&pending) {
                        Ok(_) => pending.len(),
                        Err(e) => e.valid_up_to(),
                    }
                };
                let chunk: Vec<u8> = pending.drain(..cut).collect();
                let text = ANSI.replace_all(&String::from_utf8_lossy(&chunk), "").into_owned();
                let text = if kind == CliKind::Aider {
                    text.lines().filter(|l| !aider_noise(l)).map(|l| format!("{l}\n")).collect()
                } else {
                    text
                };
                send(text, &mut emitted);
            }
            if !pending.is_empty() {
                send(ANSI.replace_all(&String::from_utf8_lossy(&pending), "").into_owned(), &mut emitted);
            }
        }
        Output::OpencodeJson | Output::CodexJson => {
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await? {
                let text = if plan.output == Output::OpencodeJson { opencode.line(&line) } else { codex.line(&line) };
                if let Some(text) = text {
                    send(text, &mut emitted);
                }
            }
        }
    }

    let status = child.wait().await?;
    let stderr = stderr_task.await.unwrap_or_default();
    if let Some(err) = opencode.error.or(codex.error) {
        bail!("{label}: {err}");
    }
    if emitted.trim().is_empty() {
        // `--output-last-message` is Codex's dependable copy of the answer.
        if let Ok(text) = std::fs::read_to_string(&last_message) {
            send(text, &mut emitted);
        }
    }
    if !status.success() || emitted.trim().is_empty() {
        let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
        match failure_reason(&stderr) {
            None => bail!("{label} exited ({code}) without answering."),
            Some(reason) => bail!("{label} exited ({code}) without answering. {reason}"),
        }
    }
    let model = if opts.model.is_empty() { label.to_string() } else { opts.model.clone() };
    let _ = tx.unbounded_send(Chunk::Done(Done {
        stop_reason: opencode.reason.or_else(|| Some("end_turn".into())),
        model: Some(model),
        cost_usd: opencode.cost.filter(|c| *c > 0.0),
    }));
    Ok(())
}

/// The useful part of a failed CLI's stderr: the first line that reads as
/// an error (Node CLIs follow it with a stack trace), else the last lines.
fn failure_reason(stderr: &str) -> Option<String> {
    let clean = ANSI.replace_all(stderr, "");
    let lines: Vec<&str> = clean.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let is_error = |l: &&&str| {
        let lower = l.to_lowercase();
        (lower.starts_with("error") || lower.contains("error:")) && !l.starts_with("at ")
    };
    let text = match lines.iter().find(is_error) {
        Some(line) => line.to_string(),
        None => lines[lines.len().saturating_sub(4)..].join(" "),
    };
    Some(text.chars().take(400).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ImagePart;

    fn turn(role: Role, text: &str) -> Turn {
        Turn { role, text: text.into(), images: vec![] }
    }

    #[test]
    fn prompt_carries_instructions_and_recent_turns() {
        let p =
            compose_prompt("Be brief.", &[turn(Role::User, "q1"), turn(Role::Assistant, "a1"), turn(Role::User, "q2")]);
        assert!(p.starts_with("<instructions>\nBe brief.\n</instructions>"));
        assert!(p.contains("User: q1") && p.contains("Assistant: a1"));
        assert!(p.ends_with("q2"));
        assert_eq!(compose_prompt("", &[turn(Role::User, "only")]), "only");
    }

    #[test]
    fn splits_command_lines_like_a_shell() {
        assert_eq!(split_command(r#"llm -m "gpt 4o" {prompt}"#), vec!["llm", "-m", "gpt 4o", "{prompt}"]);
        assert_eq!(split_command(r"say 'it''s' a\ b ''"), vec!["say", "its", "a b", ""]);
    }

    #[test]
    fn opencode_events() {
        let mut s = OpencodeState::default();
        assert_eq!(s.line(r#"{"type":"step_start","part":{"type":"step-start"}}"#), None);
        assert_eq!(s.line(r#"{"type":"text","part":{"id":"p1","type":"text","text":"PO"}}"#).as_deref(), Some("PO"));
        assert_eq!(s.line(r#"{"type":"text","part":{"id":"p1","type":"text","text":"PONG"}}"#).as_deref(), Some("NG"));
        s.line(r#"{"type":"step_finish","part":{"reason":"stop","cost":0.002}}"#);
        assert_eq!(s.reason.as_deref(), Some("stop"));
        assert_eq!(s.cost, Some(0.002));
        s.line(r#"{"type":"error","error":{"name":"ProviderAuthError","data":{"message":"not logged in"}}}"#);
        assert_eq!(s.error.as_deref(), Some("not logged in"));
    }

    #[test]
    fn codex_events_new_and_old() {
        let mut s = CodexState::default();
        assert_eq!(s.line(r#"{"type":"thread.started","thread_id":"t"}"#), None);
        assert_eq!(s.line(r#"{"type":"item.completed","item":{"type":"reasoning","text":"hm"}}"#), None);
        assert_eq!(
            s.line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"PONG"}}"#).as_deref(),
            Some("PONG\n")
        );
        let mut old = CodexState::default();
        assert_eq!(old.line(r#"{"msg":{"type":"agent_message_delta","delta":"PO"}}"#).as_deref(), Some("PO"));
        assert_eq!(old.line(r#"{"msg":{"type":"agent_message","message":"PONG"}}"#), None, "already streamed");
        let mut bad = CodexState::default();
        bad.line(r#"{"type":"turn.failed","error":{"message":"not logged in"}}"#);
        assert_eq!(bad.error.as_deref(), Some("not logged in"));
    }

    #[test]
    fn failure_reason_prefers_the_error_line() {
        let gemini = "An unexpected critical error occurred:\nError: This account requires setting the GOOGLE_CLOUD_PROJECT env var.\n    at setupUser (file:///x.js:36:15)\n    at async main (file:///y.js:1:1)\n";
        assert_eq!(
            failure_reason(gemini).unwrap(),
            "Error: This account requires setting the GOOGLE_CLOUD_PROJECT env var."
        );
        assert_eq!(failure_reason("one\ntwo\n").unwrap(), "one two");
        assert_eq!(failure_reason("  \n"), None);
    }

    #[test]
    fn plans_each_cli() {
        let opts = StreamOptions {
            kind: crate::config::ProviderKind::AgentCli(CliKind::Custom),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            system: String::new(),
            messages: vec![Turn {
                role: Role::User,
                text: "hi".into(),
                images: vec![ImagePart { mime: "image/png", base64: std::sync::Arc::new("iVBORw0KGgo=".into()) }],
            }],
            max_tokens: 1,
            effort: None,
            show_thinking: false,
            server_side_fallback: false,
            claude_code: None,
            cli: None,
            cwd: None,
        };
        let images = Images::write(opts.messages.last()).unwrap();
        assert_eq!(images.0.len(), 1);
        let custom = CliOptions { custom_command: "/bin/echo -n {prompt}".into(), ..Default::default() };
        let p = plan(CliKind::Custom, &opts, &custom, &images, Path::new("/tmp/x")).unwrap();
        assert_eq!(p.program, PathBuf::from("/bin/echo"));
        assert_eq!(p.args, vec!["-n", "hi"]);
        assert!(p.stdin.is_none());
        let stdin_cli = CliOptions { custom_command: "/bin/cat".into(), ..Default::default() };
        let p = plan(CliKind::Custom, &opts, &stdin_cli, &images, Path::new("/tmp/x")).unwrap();
        assert_eq!(p.stdin.as_deref(), Some("hi"));
        let fake = std::env::temp_dir().join(format!("vp-fake-cli-{}", uuid::Uuid::new_v4()));
        std::fs::write(&fake, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cli = CliOptions { binary_path: fake.display().to_string(), ..Default::default() };
        let p = plan(CliKind::Codex, &opts, &cli, &images, Path::new("/tmp/last")).unwrap();
        assert_eq!(&p.args[..5], &["exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only"]);
        assert!(p.args.windows(2).any(|w| w[0] == "--image"));
        assert_eq!(p.stdin.as_deref(), Some("hi"));
        let p = plan(CliKind::Gemini, &opts, &cli, &images, Path::new("/tmp/last")).unwrap();
        assert!(p.args[1].starts_with("hi @"), "{:?}", p.args);
        let p = plan(CliKind::Opencode, &opts, &cli, &images, Path::new("/tmp/last")).unwrap();
        assert_eq!(&p.args[..3], &["run", "--format", "json"]);
        assert_eq!(&p.args[p.args.len() - 2..], &["--", "hi"], "the message must not read as a --file value");
        let _ = std::fs::remove_file(fake);
    }

    #[test]
    fn custom_cli_runs_end_to_end() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let opts = StreamOptions {
            kind: crate::config::ProviderKind::AgentCli(CliKind::Custom),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            system: "S".into(),
            messages: vec![turn(Role::User, "ping")],
            max_tokens: 1,
            effort: None,
            show_thinking: false,
            server_side_fallback: false,
            claude_code: None,
            cli: Some(CliOptions { custom_command: "/bin/cat".into(), ..Default::default() }),
            cwd: None,
        };
        rt.block_on(stream(&opts, CliKind::Custom, &tx)).unwrap();
        drop(tx);
        let chunks: Vec<Chunk> = futures::executor::block_on(futures::StreamExt::collect(rx));
        let text: String =
            chunks.iter().filter_map(|c| if let Chunk::Text(t) = c { Some(t.as_str()) } else { None }).collect();
        assert!(text.contains("<instructions>\nS\n</instructions>") && text.ends_with("ping"), "{text}");
        assert!(matches!(chunks.last(), Some(Chunk::Done(_))));
    }
}
