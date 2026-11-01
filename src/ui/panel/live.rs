//! Answers that arrive by themselves (a question heard in the transcript, or
//! your name), the transcript summary, and saving it to project memory.

use futures::StreamExt as _;
use gpui::{Context, Window};
use tokio_util::sync::CancellationToken;

use super::{CardKind, Latest, Panel, Tab, ToastKind};
use crate::ai::{self, BuiltPrompt, Chunk, StreamEvent};
use crate::capture;
use crate::devcat;
use crate::live::{self, Trigger};
use crate::session::{Role, Turn};

#[derive(Clone, Debug)]
pub struct LiveItem {
    pub id: u64,
    pub trigger: Trigger,
    /// The transcript line that triggered it.
    pub line: String,
    pub answer: String,
    pub done: bool,
    pub error: Option<String>,
    /// The model said the line needs no answer.
    pub skipped: bool,
    pub at: i64,
    pub meta: String,
}

impl LiveItem {
    /// Hidden while it might still turn out to be `SKIP`.
    pub fn visible(&self) -> bool {
        if self.skipped {
            return false;
        }
        if self.error.is_some() || self.done {
            return true;
        }
        let t = self.answer.trim().trim_start_matches(['*', '`', '"']);
        !(t.is_empty() || "skip".starts_with(&t.to_lowercase()))
    }
}

#[derive(Default)]
pub struct LiveState {
    pub items: Vec<LiveItem>,
    /// The answer being written, and how to stop it.
    active: Option<(u64, CancellationToken)>,
    /// A trigger heard while busy; only the newest is kept, since an older
    /// question has usually moved on.
    pending: Option<(String, Trigger)>,
    pub unseen: usize,
    seq: u64,
}

impl LiveState {
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
}

impl Panel {
    /* ---------- live answers ---------- */

    /// A new transcript line: answer it if it is a question (auto-answer on)
    /// or says your name (name mode on).
    pub(super) fn consider_live(&mut self, line: &str, cx: &mut Context<Self>) {
        let l = &self.cfg.live;
        let Some(trigger) = live::trigger(line, l.auto_answer, l.answer_on_name, &l.name_list()) else { return };
        if self.live.busy() {
            self.live.pending = Some((line.to_string(), trigger));
            return;
        }
        self.start_live(line.to_string(), trigger, cx);
    }

    fn start_live(&mut self, line: String, trigger: Trigger, cx: &mut Context<Self>) {
        let provider = match self.resolve_answerer().and_then(|p| Self::usable(&p).map(|_| p)) {
            Ok(p) => p,
            Err(message) => {
                self.toast(ToastKind::Error, format!("Live answer: {message}"), cx);
                return;
            }
        };
        let names = self.cfg.live.name_list();
        let lines = self.session.transcript_lines();
        let keep = (self.cfg.live.context_lines.max(1) as usize).min(lines.len());
        let recent: Vec<String> = lines[lines.len() - keep..].iter().map(|l| l.text.clone()).collect();
        let text = live::prompt(&recent, trigger, &names);

        self.live.seq += 1;
        let id = self.live.seq;
        let model = provider.model.clone();
        self.live.items.push(LiveItem {
            id,
            trigger,
            line,
            answer: String::new(),
            done: false,
            error: None,
            skipped: false,
            at: capture::now_ms(),
            meta: if model.is_empty() { provider.label.clone() } else { model.clone() },
        });

        let messages = vec![Turn { role: Role::User, text, images: Vec::new() }];
        let effort = Some("low".to_string());
        let mut opts = self.stream_options(&provider, model, messages, effort);
        if provider.kind == crate::config::ProviderKind::ClaudeCode {
            // A one-off: live answers stay out of the chat's session.
            opts.claude_code = Some(self.claude_options(None));
        }
        let running = ai::start(opts);
        self.live.active = Some((id, running.cancel.clone()));
        cx.notify();

        let mut events = running.events;
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let more = this.update(cx, |this, cx| this.on_live_event(id, event, cx)).unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_live_event(&mut self, id: u64, event: StreamEvent, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.live.items.iter_mut().find(|i| i.id == id) else { return false };
        let was_visible = item.visible();
        let terminal = match event {
            StreamEvent::Chunk(Chunk::Text(t)) => {
                item.answer.push_str(&t);
                false
            }
            StreamEvent::Chunk(Chunk::Thinking(_)) | StreamEvent::Chunk(Chunk::Note(_)) => false,
            StreamEvent::Chunk(Chunk::Done(done)) => {
                item.done = true;
                item.skipped = live::is_skip(&item.answer);
                let mut bits = vec![done.model.unwrap_or_default()];
                if let Some(cost) = done.cost_usd {
                    bits.push(format!("${cost:.4}"));
                }
                item.meta = bits.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                true
            }
            StreamEvent::Failed(message) => {
                item.done = true;
                item.error = Some(message);
                true
            }
            StreamEvent::Cancelled | StreamEvent::Incomplete => {
                item.done = true;
                item.skipped = item.answer.trim().is_empty() || live::is_skip(&item.answer);
                true
            }
        };
        let now_visible = item.visible();
        if now_visible && !was_visible {
            // A real answer: put it where it is seen — the Live tab, or the
            // island in notch mode.
            self.latest = Some(Latest::Live(id));
            // The ask box is focused nearly always; only a half-typed
            // question keeps you where you are.
            let typing = !self.input.read(cx).text().is_empty();
            if self.minimized.is_some() {
                // The title bar counts it; the Live tab is open on restore.
                self.tab = Tab::Live;
                self.live.unseen += 1;
            } else if self.tab != Tab::Live {
                if !typing && self.settings.is_none() && !self.is_streaming() {
                    self.tab = Tab::Live;
                } else {
                    self.live.unseen += 1;
                }
            }
            if self.cfg.window.notch() {
                self.show_island(true, cx);
            }
        }
        if terminal {
            if self.live.active.as_ref().is_some_and(|(a, _)| *a == id) {
                self.live.active = None;
            }
            self.live.items.retain(|i| !i.skipped);
            if let Some((line, trigger)) = self.live.pending.take() {
                self.start_live(line, trigger, cx);
            }
        }
        cx.notify();
        !terminal
    }

    pub(super) fn cancel_live(&mut self, _cx: &mut Context<Self>) {
        self.live.pending = None;
        if let Some((_, token)) = self.live.active.take() {
            token.cancel();
        }
    }

    pub fn toggle_auto_answer(&mut self, cx: &mut Context<Self>) {
        self.cfg.live.auto_answer = !self.cfg.live.auto_answer;
        self.save_config();
        let on = self.cfg.live.auto_answer;
        self.toast(
            ToastKind::Info,
            if on { "Auto-answer on — questions heard in the transcript get an answer." } else { "Auto-answer off." },
            cx,
        );
        cx.notify();
    }

    pub fn toggle_answer_on_name(&mut self, cx: &mut Context<Self>) {
        if self.cfg.live.name_list().is_empty() && !self.cfg.live.answer_on_name {
            self.toast(ToastKind::Warn, "Set your name first: settings › Voice › Your name.", cx);
            return;
        }
        self.cfg.live.answer_on_name = !self.cfg.live.answer_on_name;
        self.save_config();
        let msg = if self.cfg.live.answer_on_name {
            format!("Answering when someone says {}.", self.cfg.live.name_list().join(" / "))
        } else {
            "Name mode off.".into()
        };
        self.toast(ToastKind::Info, msg, cx);
        cx.notify();
    }

    /* ---------- summary and memory ---------- */

    pub fn summarize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.transcript_lines().is_empty() {
            self.toast(ToastKind::Warn, "Nothing to summarize — the transcript is empty.", cx);
            self.save_after_summary = false;
            return;
        }
        let transcript = self.session.transcript_with_times();
        let text = ai::fill(&self.cfg.live.summary_prompt, &[("transcript", &transcript), ("input", "")]);
        let text = if self.cfg.live.summary_prompt.contains("{{transcript}}") {
            text
        } else {
            format!("{text}\n\n{transcript}")
        };
        let prompt = BuiltPrompt { text, label: "Summary".into(), source: None, effort: None, model: None };
        let key = Some(self.cfg.hotkeys.summarize.clone()).filter(|k| !k.is_empty());
        self.surface(window, cx);
        self.start_ask(prompt, key, Vec::new(), CardKind::Summary, window, cx);
    }

    /// Save the summary (made first if missing or out of date) to the
    /// picked project's DevCat memory.
    pub fn save_memory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.context_project() else {
            self.toast(
                ToastKind::Warn,
                "Pick a project first (the context menu at the top) — nothing is saved to memory without one.",
                cx,
            );
            return;
        };
        if !devcat::present() {
            self.toast(ToastKind::Error, "DevCat's data was not found, so there is no memory to save to.", cx);
            return;
        }
        let stale = self.last_summary.is_none() || self.summary_lines != self.session.transcript_lines().len();
        if stale {
            if self.session.transcript_lines().is_empty() {
                self.toast(ToastKind::Warn, "Nothing to save — record or summarize something first.", cx);
                return;
            }
            self.toast(ToastKind::Info, format!("Summarizing, then saving to {}…", project.name), cx);
            self.save_after_summary = true;
            self.summarize(window, cx);
            return;
        }
        self.save_memory_now(cx);
    }

    pub(super) fn save_memory_now(&mut self, cx: &mut Context<Self>) {
        let (Some(project), Some(summary)) = (self.context_project(), self.last_summary.clone()) else { return };
        let now = chrono::Local::now();
        let name = format!("meeting {}", now.format("%Y-%m-%d %H%M"));
        let description = summary
            .lines()
            .map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']).replace("**", ""))
            .find(|l| !l.trim().is_empty())
            .unwrap_or_else(|| "Meeting notes".into());
        let header = format!(
            "Meeting on {} (recorded with VeilPilot).\n\n## Summary\n\n{}\n",
            now.format("%Y-%m-%d %H:%M"),
            summary.trim()
        );
        let transcript = self.session.transcript_with_times();
        // The transcript gets whatever room DevCat's 16 KB cap leaves.
        let room = devcat::MAX_BODY.saturating_sub(header.len() + 64);
        let mut body = header;
        if room > 200 && !transcript.is_empty() {
            let mut t = transcript;
            if t.len() > room {
                let mut cut = t.len() - room;
                while !t.is_char_boundary(cut) {
                    cut += 1;
                }
                t = format!("…{}", &t[cut..]);
            }
            body.push_str("\n## Transcript\n\n");
            body.push_str(&t);
        }
        let memory = devcat::NewMemory { name, description, body };
        let project_id = project.id.clone();
        let project_name = project.name.clone();
        self.toast(ToastKind::Info, format!("Saving to {project_name}'s memory…"), cx);
        cx.spawn(async move |this, cx| {
            let result = crate::runtime::spawn(async move { devcat::save_memory(&project_id, memory).await })
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.toast(ToastKind::Info, format!("Saved to {project_name}'s memory."), cx),
                Err(err) => this.toast(ToastKind::Error, format!("Could not save to memory: {err:#}"), cx),
            });
        })
        .detach();
    }
}
