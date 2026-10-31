//! DevCat integration: its projects, its shared agent memory, and its
//! DevCat Agent connections.
//!
//! DevCat keeps all of these as plain files in its data folder, so VeilPilot
//! reads them directly. Writing memory goes through `devcat mcp` (DevCat's own
//! MCP server, the supported way in: it scrubs secrets, stamps the origin and
//! logs the change); when DevCat's binary is missing, the entry is written in
//! the same format DevCat writes.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

const APP_ID: &str = "dev.devcat.app";
/// DevCat caps a memory body at 16 KB and a description at 400 characters.
pub const MAX_BODY: usize = 16 * 1024;
const MAX_DESCRIPTION: usize = 400;

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VEILPILOT_DEVCAT_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join(APP_ID)
}

/// DevCat has been run on this machine.
pub fn present() -> bool {
    data_dir().join("projects.json").is_file()
}

/* ---------- projects ---------- */

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
    pub order: i64,
    /// Code folders; the first one is where agents run.
    pub folders: Vec<String>,
}

impl Project {
    pub fn folder(&self) -> Option<PathBuf> {
        self.folders.iter().map(PathBuf::from).find(|p| p.is_dir())
    }
}

pub fn projects() -> Vec<Project> {
    let Ok(text) = std::fs::read_to_string(data_dir().join("projects.json")) else { return Vec::new() };
    let mut list: Vec<Project> = serde_json::from_str(&text).unwrap_or_default();
    list.retain(|p| !p.id.is_empty());
    list.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    list
}

pub fn project(id: &str) -> Option<Project> {
    if id.is_empty() {
        return None;
    }
    projects().into_iter().find(|p| p.id == id)
}

/* ---------- memory ---------- */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Memory {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub body: String,
    pub origin: String,
    pub updated: String,
    pub scope: Scope,
}

#[derive(Serialize, Deserialize)]
struct Front {
    name: String,
    description: String,
    metadata: Meta,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    #[serde(rename = "type")]
    kind: String,
    origin: String,
    updated: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

fn split_front(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else { return (None, text) };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            let body = after.strip_prefix('\n').or_else(|| after.strip_prefix("\r\n")).unwrap_or(after);
            (Some(&rest[..end]), body)
        }
        None => (None, text),
    }
}

fn parse(text: &str, stem: &str, scope: Scope) -> Memory {
    let (front, body) = split_front(text);
    let yaml: serde_yaml::Value = front.and_then(|f| serde_yaml::from_str(f).ok()).unwrap_or(serde_yaml::Value::Null);
    let get = |key: &str| -> Option<String> {
        yaml.get(key)
            .or_else(|| yaml.get("metadata").and_then(|m| m.get(key)))
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    Memory {
        name: get("name").unwrap_or_else(|| stem.to_string()),
        description: get("description").unwrap_or_default(),
        kind: get("type").unwrap_or_else(|| "project".into()),
        body: body.trim().to_string(),
        origin: get("origin").unwrap_or_default(),
        updated: get("updated").unwrap_or_default(),
        scope,
    }
}

fn read_place(dir: &Path, scope: Scope) -> Vec<Memory> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().to_string();
            std::fs::read_to_string(&p).ok().map(|t| parse(&t, &stem, scope))
        })
        .collect()
}

fn project_dir(project_id: &str) -> PathBuf {
    data_dir().join("memory").join("projects").join(project_id)
}

/// The project's memories and the global ones, newest first.
pub fn memories(project_id: &str) -> Vec<Memory> {
    let mut list = read_place(&project_dir(project_id), Scope::Project);
    list.extend(read_place(&data_dir().join("memory").join("global"), Scope::Global));
    list.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.name.cmp(&b.name)));
    list
}

/// What the agent is told about the project: its name and as many memories
/// as fit in `budget` bytes, newest first; the rest are listed by name.
pub fn brief(project: &Project, memories: &[Memory], budget: usize) -> String {
    let mut out = format!("Project context: \"{}\" (from DevCat).", project.name);
    if let Some(d) = project.description.as_deref().filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!(" {}", d.trim()));
    }
    if memories.is_empty() {
        return out;
    }
    out.push_str("\nWhat is known about it (shared agent memory; trust it unless the conversation contradicts it):");
    let mut skipped = Vec::new();
    for m in memories {
        // The type ("project", "feedback", ...) is the useful part; a
        // global memory also says so.
        let tag = if m.scope == Scope::Global { format!("{}, global", m.kind) } else { m.kind.clone() };
        let entry = format!("\n\n## {} ({tag})\n{}\n{}", m.name, m.description, m.body);
        if out.len() + entry.len() <= budget {
            out.push_str(&entry);
        } else {
            skipped.push(format!("{} — {}", m.name, m.description));
        }
    }
    if !skipped.is_empty() {
        out.push_str("\n\nAlso in memory (titles only): ");
        out.push_str(&skipped.join("; "));
    }
    out
}

pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(60).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "memory".into() } else { s }
}

static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        r"\bsk-[A-Za-z0-9_\-]{16,}",
        r"\bgh[pousr]_[A-Za-z0-9]{20,}",
        r"\bxox[abposr]-[A-Za-z0-9\-]{10,}",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"\bAIza[0-9A-Za-z_\-]{30,}",
        r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
        r"(?i)(password|passwd|secret|token)\s*[=:]\s*\S+",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

/// Remove things that look like credentials before they reach memory —
/// a transcript can contain anything that was read out loud.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for re in SECRETS.iter() {
        out = re.replace_all(&out, "[redacted]").into_owned();
    }
    out
}

fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[derive(Clone, Debug)]
pub struct NewMemory {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// DevCat's CLI, which serves its MCP server.
pub fn binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("VEILPILOT_DEVCAT_BIN") {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    let exe = if cfg!(windows) { "devcat.exe" } else { "devcat" };
    let mut candidates = vec![PathBuf::from("/Applications/DevCat.app/Contents/MacOS/devcat")];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Applications/DevCat.app/Contents/MacOS/devcat"));
        candidates.push(home.join(".local/bin").join(exe));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(&local).join("DevCat").join(exe));
        candidates.push(PathBuf::from(&local).join("Programs").join("DevCat").join(exe));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(exe)));
    }
    candidates.into_iter().find(|p| crate::ai::claude_code::is_executable(p))
}

/// Save a memory to a project. Returns DevCat's confirmation.
pub async fn save_memory(project_id: &str, memory: NewMemory) -> anyhow::Result<String> {
    if project_id.is_empty() {
        bail!("Pick a project first — memory is only saved to a project you chose.");
    }
    let memory = NewMemory {
        name: memory.name.trim().to_string(),
        description: cap(&scrub(memory.description.lines().next().unwrap_or("").trim()), MAX_DESCRIPTION),
        body: cap(&scrub(memory.body.trim()), MAX_BODY),
    };
    match binary() {
        Some(bin) => save_via_mcp(&bin, project_id, &memory).await,
        None => {
            let path = write_direct(project_id, &memory)?;
            Ok(format!("Saved \"{}\" to {}", memory.name, path.display()))
        }
    }
}

async fn save_via_mcp(bin: &Path, project_id: &str, memory: &NewMemory) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new(bin);
    command.args(["mcp", "--project", project_id]);
    if std::env::var_os("VEILPILOT_DEVCAT_DIR").is_some() {
        command.arg("--data-dir").arg(data_dir());
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();

    let exchange = async {
        let send = |v: Value| format!("{v}\n");
        stdin
            .write_all(
                send(json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-06-18",
                        "clientInfo": { "name": "veilpilot", "version": env!("CARGO_PKG_VERSION") },
                        "capabilities": {}
                    }
                }))
                .as_bytes(),
            )
            .await?;
        wait_for(&mut lines, 1).await?;
        stdin.write_all(send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes()).await?;
        stdin
            .write_all(
                send(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {
                        "name": "memory_save",
                        "arguments": {
                            "name": memory.name,
                            "description": memory.description,
                            "type": "project",
                            "body": memory.body,
                            "scope": "project"
                        }
                    }
                }))
                .as_bytes(),
            )
            .await?;
        let reply = wait_for(&mut lines, 2).await?;
        let text = reply.pointer("/result/content/0/text").and_then(Value::as_str).unwrap_or("Saved.").to_string();
        if reply.pointer("/result/isError").and_then(Value::as_bool).unwrap_or(false) {
            bail!("DevCat refused the memory: {text}");
        }
        if let Some(err) = reply.get("error") {
            bail!("DevCat refused the memory: {}", err.get("message").and_then(Value::as_str).unwrap_or("error"));
        }
        anyhow::Ok(text)
    };
    let result = tokio::time::timeout(Duration::from_secs(20), exchange)
        .await
        .map_err(|_| anyhow!("DevCat's memory server did not answer within 20 s"))?;
    drop(stdin);
    let _ = child.kill().await;
    result
}

async fn wait_for(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
) -> anyhow::Result<Value> {
    while let Some(line) = lines.next_line().await? {
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(value);
        }
    }
    bail!("DevCat's memory server closed before answering")
}

/// The same file DevCat's `memory::save` writes.
fn write_direct(project_id: &str, memory: &NewMemory) -> anyhow::Result<PathBuf> {
    let dir = project_dir(project_id);
    std::fs::create_dir_all(&dir)?;
    let place = dir.join("place.json");
    if !place.exists() {
        std::fs::write(&place, json!({"kind": "project", "id": project_id}).to_string())?;
    }
    let front = Front {
        name: memory.name.clone(),
        description: memory.description.clone(),
        metadata: Meta {
            kind: "project".into(),
            origin: "veilpilot".into(),
            updated: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            source: None,
        },
    };
    let text = format!("---\n{}---\n\n{}\n", serde_yaml::to_string(&front)?, memory.body.trim_end());
    let path = dir.join(format!("{}.md", slug(&memory.name)));
    let tmp = dir.join(format!(".{}.{}.tmp", slug(&memory.name), uuid::Uuid::new_v4()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/* ---------- DevCat Agent connections ---------- */

#[derive(Clone, Debug, PartialEq)]
pub struct Connection {
    pub id: String,
    pub name: String,
    /// "anthropic", "openai-chat" or "openai-responses".
    pub protocol: String,
    pub base_url: String,
    pub model: String,
}

impl Connection {
    pub fn provider_id(&self) -> String {
        format!("devcat:{}", self.id)
    }
}

/// DevCat Agent connections (the model API profiles in DevCat › AI).
pub fn connections() -> Vec<Connection> {
    let Ok(text) = std::fs::read_to_string(data_dir().join("ai_providers.json")) else { return Vec::new() };
    let list: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
    list.iter()
        .filter_map(|p| {
            let agent = p.get("agent").filter(|a| a.is_object())?;
            let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
            Some(Connection {
                id: s(p.get("id")),
                name: s(p.get("name")),
                protocol: s(agent.get("protocol")),
                base_url: s(agent.get("baseUrl")),
                model: s(agent.get("model")),
            })
        })
        .filter(|c| !c.id.is_empty())
        .collect()
}

pub fn connection(provider_id: &str) -> Option<Connection> {
    let id = provider_id.strip_prefix("devcat:")?;
    connections().into_iter().find(|c| c.id == id)
}

/// The API key DevCat keeps for a connection. DevCat stores every secret in
/// one keychain item (service `dev.devcat.app`, account `vault`), so the
/// first read asks for permission to use it — "Always Allow" makes that a
/// one-time prompt.
pub fn connection_key(connection_id: &str) -> anyhow::Result<String> {
    let vault = keyring::Entry::new(APP_ID, "vault")
        .and_then(|e| e.get_password())
        .map_err(|e| anyhow!("Could not read DevCat's keychain item: {e}"))?;
    let map: Value = serde_json::from_str(&vault).context("DevCat's keychain item is not readable")?;
    map.get(format!("ai:{connection_id}:apiKey"))
        .and_then(Value::as_str)
        .map(String::from)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| anyhow!("DevCat has no API key saved for this connection."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devcat_memory_files() {
        let text = "---\nname: admin-api-email\ndescription: 'Emails: DB write before SMTP'\nmetadata:\n  type: project\n  origin: claude-code\n  updated: 2026-10-30T12:44:15Z\n---\n\nThe body.\n";
        let m = parse(text, "x", Scope::Project);
        assert_eq!(m.name, "admin-api-email");
        assert_eq!(m.description, "Emails: DB write before SMTP");
        assert_eq!(m.kind, "project");
        assert_eq!(m.origin, "claude-code");
        assert_eq!(m.updated, "2026-10-30T12:44:15Z");
        assert_eq!(m.body, "The body.");
        let bare = parse("no frontmatter", "stem", Scope::Global);
        assert_eq!(bare.name, "stem");
        assert_eq!(bare.body, "no frontmatter");
    }

    #[test]
    fn slugs_match_devcat() {
        assert_eq!(slug("Deploy: the Steps!"), "deploy-the-steps");
        assert_eq!(slug("  "), "memory");
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("Café ünïcode"), "café-ünïcode");
    }

    #[test]
    fn scrubs_credentials() {
        let s = scrub("key sk-abcdefghijklmnop1234 and password=hunter2 and ghp_abcdefghijklmnopqrstuvwx");
        assert!(!s.contains("sk-abc") && !s.contains("hunter2") && !s.contains("ghp_"), "{s}");
    }

    #[test]
    fn brief_fits_the_budget() {
        let p = Project { id: "p".into(), name: "Shop".into(), ..Default::default() };
        let mems: Vec<Memory> = (0..5)
            .map(|i| Memory {
                name: format!("m{i}"),
                description: format!("desc {i}"),
                kind: "project".into(),
                body: "x".repeat(300),
                origin: String::new(),
                updated: String::new(),
                scope: Scope::Project,
            })
            .collect();
        let b = brief(&p, &mems, 1000);
        assert!(b.starts_with("Project context: \"Shop\""));
        assert!(b.len() <= 1200, "{}", b.len());
        assert!(b.contains("Also in memory (titles only)"));
    }

    #[test]
    fn direct_write_round_trips_through_the_reader() {
        let dir = std::env::temp_dir().join(format!("vp-devcat-{}", uuid::Uuid::new_v4()));
        // SAFETY: tests in this module that touch the env var run here only.
        unsafe { std::env::set_var("VEILPILOT_DEVCAT_DIR", &dir) };
        let path = write_direct(
            "proj-1",
            &NewMemory {
                name: "Meeting 2026-11-01".into(),
                description: "Standup: release Friday".into(),
                body: "Body".into(),
            },
        )
        .unwrap();
        assert!(path.ends_with("meeting-2026-11-01.md"));
        let got = memories("proj-1");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].description, "Standup: release Friday");
        assert_eq!(got[0].origin, "veilpilot");
        assert!(dir.join("memory/projects/proj-1/place.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
