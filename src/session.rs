//! Conversation, screenshots, and transcript for the current session.

use std::sync::Arc;

use crate::capture::Shot;

const MAX_TURNS: usize = 16;
/// Images are re-uploaded with every follow-up turn, so old ones are dropped
/// from history: the cost is per request, not per conversation.
const TURNS_KEEPING_IMAGES: usize = 2;
const MAX_TRANSCRIPT_LINES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// An image as the provider adapters send it.
#[derive(Clone, Debug)]
pub struct ImagePart {
    pub mime: &'static str,
    pub base64: Arc<String>,
}

/// Neutral history the provider adapters translate from.
#[derive(Clone, Debug)]
pub struct Turn {
    pub role: Role,
    pub text: String,
    pub images: Vec<ImagePart>,
}

#[derive(Clone, Debug)]
pub struct TranscriptLine {
    /// Milliseconds since the epoch.
    pub at: i64,
    pub text: String,
}

/// Claude Code keeps its own conversation; we only hold the handle to it.
#[derive(Clone, Debug, Default)]
pub struct ClaudeHandle {
    pub id: String,
    pub started: bool,
}

#[derive(Default)]
pub struct Session {
    turns: Vec<Turn>,
    shots: Vec<Arc<Shot>>,
    transcript: Vec<TranscriptLine>,
    shot_seq: u64,
    claude: Option<ClaudeHandle>,
}

impl Session {
    pub fn add_shot(&mut self, mut shot: Shot, keep_last: usize) -> Arc<Shot> {
        self.shot_seq += 1;
        shot.id = format!("shot_{}", self.shot_seq);
        let shot = Arc::new(shot);
        self.shots.push(shot.clone());
        let keep = keep_last.max(1);
        if self.shots.len() > keep {
            let drop = self.shots.len() - keep;
            self.shots.drain(..drop);
        }
        shot
    }

    pub fn append_transcript(&mut self, text: &str, at: i64) -> Option<TranscriptLine> {
        let clean = text.trim();
        if clean.is_empty() {
            return None;
        }
        let line = TranscriptLine { at, text: clean.to_string() };
        self.transcript.push(line.clone());
        if self.transcript.len() > MAX_TRANSCRIPT_LINES {
            let drop = self.transcript.len() - MAX_TRANSCRIPT_LINES;
            self.transcript.drain(..drop);
        }
        Some(line)
    }

    pub fn transcript_lines(&self) -> &[TranscriptLine] {
        &self.transcript
    }

    pub fn transcript_text(&self) -> String {
        let start = self.transcript.len().saturating_sub(120);
        self.transcript[start..].iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// The whole transcript with times, for summaries and memory.
    pub fn transcript_with_times(&self) -> String {
        self.transcript
            .iter()
            .map(|l| {
                use chrono::TimeZone as _;
                let t = chrono::Local.timestamp_millis_opt(l.at).single().map(|t| t.format("%H:%M:%S").to_string());
                format!("[{}] {}", t.unwrap_or_default(), l.text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Forget the Claude Code session, so the next ask starts a new one
    /// (after switching agent or project).
    pub fn reset_claude(&mut self) {
        self.claude = None;
    }

    pub fn clear_transcript(&mut self) {
        self.transcript.clear();
    }

    pub fn push_turn(&mut self, turn: Turn) {
        self.turns.push(turn);
        if self.turns.len() > MAX_TURNS {
            let drop = self.turns.len() - MAX_TURNS;
            self.turns.drain(..drop);
        }
    }

    /// History with images kept only on the newest user turns.
    pub fn history(&self) -> Vec<Turn> {
        let user_indexes: Vec<usize> =
            self.turns.iter().enumerate().filter(|(_, t)| t.role == Role::User).map(|(i, _)| i).collect();
        let keep_from = user_indexes.len().saturating_sub(TURNS_KEEPING_IMAGES);
        let allowed = &user_indexes[keep_from..];
        self.turns
            .iter()
            .enumerate()
            .map(|(i, t)| Turn {
                role: t.role,
                text: t.text.clone(),
                images: if allowed.contains(&i) { t.images.clone() } else { Vec::new() },
            })
            .collect()
    }

    /// The first ask names a session with --session-id; every later ask
    /// resumes it, so only the newest turn is sent and Claude Code holds the
    /// history.
    pub fn claude_session(&mut self) -> ClaudeHandle {
        self.claude.get_or_insert_with(|| ClaudeHandle { id: uuid::Uuid::new_v4().to_string(), started: false }).clone()
    }

    pub fn mark_claude_started(&mut self) {
        if let Some(handle) = self.claude.as_mut() {
            handle.started = true;
        }
    }

    pub fn clear(&mut self) {
        *self = Session::default();
    }

    #[cfg(test)]
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: Role, text: &str, images: usize) -> Turn {
        Turn {
            role,
            text: text.into(),
            images: (0..images).map(|_| ImagePart { mime: "image/png", base64: Arc::new("AA==".into()) }).collect(),
        }
    }

    #[test]
    fn images_survive_only_on_newest_user_turns() {
        let mut s = Session::default();
        for i in 0..3 {
            s.push_turn(turn(Role::User, &format!("q{i}"), 1));
            s.push_turn(turn(Role::Assistant, "a", 0));
        }
        let h = s.history();
        assert_eq!(h[0].images.len(), 0);
        assert_eq!(h[2].images.len(), 1);
        assert_eq!(h[4].images.len(), 1);
    }

    #[test]
    fn turns_are_capped() {
        let mut s = Session::default();
        for _ in 0..40 {
            s.push_turn(turn(Role::User, "q", 0));
        }
        assert_eq!(s.turn_count(), MAX_TURNS);
    }

    #[test]
    fn transcript_trims_and_skips_blank() {
        let mut s = Session::default();
        assert!(s.append_transcript("   ", 0).is_none());
        s.append_transcript(" hello ", 1);
        s.append_transcript("world", 2);
        assert_eq!(s.transcript_text(), "hello\nworld");
    }

    #[test]
    fn claude_handle_is_stable_until_cleared() {
        let mut s = Session::default();
        let a = s.claude_session();
        assert!(!a.started);
        s.mark_claude_started();
        let b = s.claude_session();
        assert_eq!(a.id, b.id);
        assert!(b.started);
        s.clear();
        assert_ne!(s.claude_session().id, a.id);
    }
}
