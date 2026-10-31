//! Who answers, and what they know: the agent picked in the header, and the
//! DevCat project (with its memory) or no context at all.

use std::path::PathBuf;

use gpui::Context;

use super::Panel;
use crate::ai::cli::CliOptions;
use crate::ai::{ClaudeCodeOptions, StreamOptions};
use crate::config::{CliKind, PROVIDERS, ProviderKind, RememberedChoice, Resolved, provider_by_id, resolve_chat};
use crate::devcat;
use crate::session::Turn;

/// Memory given to the agent, in bytes. Enough for a few dozen entries
/// without crowding out the question.
const MEMORY_BUDGET: usize = 8 * 1024;

/// One entry in the header's agent menu.
#[derive(Clone, Debug)]
pub struct AgentChoice {
    pub id: String,
    pub label: String,
    pub group: &'static str,
    /// Why it cannot be used right now, if it cannot.
    pub unavailable: Option<String>,
}

impl Panel {
    /// The chat provider, resolved: presets, agent CLIs, and DevCat
    /// connections (whose key is read from DevCat's keychain item once).
    pub(super) fn resolve_answerer(&mut self) -> Result<Resolved, String> {
        let id = self.cfg.chat.provider.clone();
        if !id.starts_with("devcat:") {
            return Ok(resolve_chat(&self.cfg, &|p| self.secrets.get(p)));
        }
        let conn = devcat::connection(&id)
            .ok_or_else(|| "That DevCat connection no longer exists — pick another agent in the header.".to_string())?;
        let key = match self.devcat_keys.get(&conn.id) {
            Some(k) => k.clone(),
            None => {
                let k = devcat::connection_key(&conn.id).map_err(|e| e.to_string())?;
                self.devcat_keys.insert(conn.id.clone(), k.clone());
                k
            }
        };
        let kind = if conn.protocol == "anthropic" { ProviderKind::Anthropic } else { ProviderKind::OpenAi };
        let model = if self.cfg.chat.model.is_empty() { conn.model.clone() } else { self.cfg.chat.model.clone() };
        Ok(Resolved {
            label: format!("{} (DevCat)", conn.name),
            kind,
            vision: true,
            base_url: conn.base_url.trim_end_matches('/').to_string(),
            model,
            api_key: key,
            needs_key: true,
        })
    }

    /// Checks shared by every kind of ask; `Err` is what to tell the user.
    pub(super) fn usable(provider: &Resolved) -> Result<(), String> {
        if provider.kind == ProviderKind::WhisperCpp {
            return Err("Local (whisper.cpp) only transcribes — pick an agent in the header.".into());
        }
        if provider.needs_key && provider.api_key.is_empty() {
            return Err(format!("No API key for {}. Open settings › Keys and paste one.", provider.label));
        }
        // Agent CLIs pick their own model when none is named.
        if provider.model.is_empty() && !provider.kind.is_agent() {
            return Err(format!("No model set for {}. Add one in Settings.", provider.label));
        }
        Ok(())
    }

    /// The DevCat project picked in the header, if it still exists.
    pub fn context_project(&self) -> Option<devcat::Project> {
        devcat::project(&self.cfg.context.project)
    }

    /// The system prompt, plus the picked project's memory.
    pub(super) fn system_prompt(&self) -> String {
        let base = self.cfg.chat.system_prompt.clone();
        let Some(project) = self.context_project() else { return base };
        let memories = if self.cfg.context.use_memory { devcat::memories(&project.id) } else { Vec::new() };
        format!("{base}\n\n{}", devcat::brief(&project, &memories, MEMORY_BUDGET))
    }

    /// Request options for `provider`, before session handling.
    pub(super) fn stream_options(
        &self,
        provider: &Resolved,
        model: String,
        messages: Vec<Turn>,
        effort: Option<String>,
    ) -> StreamOptions {
        let cwd: Option<PathBuf> = self.context_project().and_then(|p| p.folder());
        let cli = match provider.kind {
            ProviderKind::AgentCli(kind) => Some(CliOptions {
                binary_path: self.cfg.chat.agents.paths.get(kind.id()).cloned().unwrap_or_default(),
                custom_command: self.cfg.chat.agents.custom_command.clone(),
            }),
            _ => None,
        };
        StreamOptions {
            kind: provider.kind,
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            model,
            system: self.system_prompt(),
            messages,
            max_tokens: if self.cfg.chat.max_tokens == 0 { 8192 } else { self.cfg.chat.max_tokens },
            effort: matches!(provider.kind, ProviderKind::Anthropic | ProviderKind::ClaudeCode)
                .then_some(effort)
                .flatten(),
            show_thinking: self.cfg.chat.show_thinking,
            server_side_fallback: self.cfg.chat.server_side_fallback,
            claude_code: None,
            cli,
            cwd,
        }
    }

    /// Claude Code options; `session` names or resumes the chat's session,
    /// `None` runs a one-off that leaves nothing in Claude Code's history.
    pub(super) fn claude_options(&self, session: Option<&crate::session::ClaudeHandle>) -> ClaudeCodeOptions {
        let cc = &self.cfg.chat.claude_code;
        ClaudeCodeOptions {
            binary_path: cc.binary_path.clone(),
            cwd: cc.cwd.clone(),
            fast_start: cc.fast_start,
            allow_tools: cc.allow_tools,
            max_budget_usd: cc.max_budget_usd,
            session_id: session.filter(|h| !h.started).map(|h| h.id.clone()),
            resume_id: session.filter(|h| h.started).map(|h| h.id.clone()),
            ephemeral: session.is_none(),
        }
    }

    /// Everything the header's agent menu offers.
    pub fn agent_choices(&self) -> Vec<AgentChoice> {
        let mut out = Vec::new();
        for p in PROVIDERS.iter().filter(|p| p.chat) {
            let group = if p.kind.is_agent() { "Agents on this Mac" } else { "Model APIs" };
            let unavailable = match p.kind {
                ProviderKind::ClaudeCode => {
                    crate::ai::claude_code::resolve_binary(&self.cfg.chat.claude_code.binary_path)
                        .err()
                        .map(|_| "not installed".to_string())
                }
                ProviderKind::AgentCli(CliKind::Custom) => {
                    self.cfg.chat.agents.custom_command.trim().is_empty().then(|| "set the command in Settings".into())
                }
                ProviderKind::AgentCli(kind) => {
                    let configured = self.cfg.chat.agents.paths.get(kind.id()).cloned().unwrap_or_default();
                    crate::ai::cli::resolve_binary(kind, &configured).err().map(|_| "not installed".to_string())
                }
                _ => (p.needs_key && !self.secrets.has(p.id)).then(|| "no API key".into()),
            };
            out.push(AgentChoice { id: p.id.into(), label: p.label.into(), group, unavailable });
        }
        for c in devcat::connections() {
            out.push(AgentChoice {
                id: c.provider_id(),
                label: format!("{} · {}", c.name, c.model),
                group: "DevCat Agent",
                unavailable: None,
            });
        }
        // PROVIDERS interleaves APIs and agents; keep each group together.
        let rank = |g: &str| ["Model APIs", "Agents on this Mac", "DevCat Agent"].iter().position(|x| *x == g);
        out.sort_by_key(|c| rank(c.group));
        out
    }

    pub fn agent_label(&self) -> String {
        let id = &self.cfg.chat.provider;
        if let Some(c) = devcat::connection(id) {
            return c.name;
        }
        provider_by_id(id).map(|p| p.label.to_string()).unwrap_or_else(|| id.clone())
    }

    /// Switch the answering agent, keeping each provider's model and base URL.
    pub fn set_agent(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if id == self.cfg.chat.provider {
            return;
        }
        let id = id.to_string();
        self.update_config(window, cx, |c| {
            let old = c.chat.provider.clone();
            c.chat
                .remembered
                .insert(old, RememberedChoice { model: c.chat.model.clone(), base_url: c.chat.base_url.clone() });
            let next = c.chat.remembered.get(&id).cloned().unwrap_or_default();
            c.chat.provider = id.clone();
            c.chat.model = next.model;
            c.chat.base_url = next.base_url;
        });
        // A different agent cannot resume another's conversation.
        self.session.reset_claude();
        let label = self.agent_label();
        self.toast(super::ToastKind::Info, format!("Answering with {label}."), cx);
    }

    /// Pick a DevCat project, or none (`""`).
    pub fn set_context(&mut self, project_id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let id = project_id.to_string();
        self.update_config(window, cx, |c| c.context.project = id.clone());
        // Claude Code's session runs in the project folder; start a new one.
        self.session.reset_claude();
        match self.context_project() {
            Some(p) => {
                let n = devcat::memories(&p.id).len();
                let count = if n == 1 { "1 memory".to_string() } else { format!("{n} memories") };
                self.toast(super::ToastKind::Info, format!("Context: {} — {count} available.", p.name), cx);
            }
            None => self.toast(super::ToastKind::Info, "No context — the agent knows nothing beyond this chat.", cx),
        }
    }
}
