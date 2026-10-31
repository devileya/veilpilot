//! Routes recorded audio to the on-device engine or a cloud endpoint.
//!
//! Speech-to-text is a separate provider from chat on purpose: Anthropic has
//! no transcription endpoint, so "chat on Claude, transcribe on Groq" is a
//! normal configuration rather than an error.

use anyhow::bail;
use serde_json::Value;

use crate::config::{Config, ProviderKind, TranscriptionConfig, provider_by_id, resolve_transcription};
use crate::secrets::Secrets;
use crate::whisper;

/// OpenAI's documented ceiling; others are looser.
const MAX_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Readiness {
    Ready { provider: String, model: String, local: bool },
    NotReady { reason: String, local: bool },
}

impl Readiness {
    pub fn is_local(&self) -> bool {
        matches!(self, Readiness::Ready { local: true, .. } | Readiness::NotReady { local: true, .. })
    }
}

pub fn readiness(cfg: &Config, secrets: &Secrets) -> Readiness {
    let t = &cfg.transcription;
    let preset = provider_by_id(&t.provider);

    // On-device: nothing to authenticate, but the binary and a model have to
    // actually be there.
    if preset.is_some_and(|p| p.kind == ProviderKind::WhisperCpp) {
        let state = whisper::detect(&t.whisper_cpp);
        return if state.ok {
            Readiness::Ready {
                provider: preset.unwrap().label.into(),
                model: state.model_name.unwrap_or_default(),
                local: true,
            }
        } else {
            Readiness::NotReady { reason: state.error.unwrap_or_default(), local: true }
        };
    }

    if let Some(p) = preset.filter(|p| !p.transcription) {
        return Readiness::NotReady {
            reason: format!(
                "{} has no speech-to-text endpoint. Pick OpenAI, Groq, or a custom OpenAI-compatible server for transcription.",
                p.label
            ),
            local: false,
        };
    }
    let resolved = resolve_transcription(cfg, &|id| secrets.get(id));
    if resolved.model.is_empty() {
        return Readiness::NotReady {
            reason: format!("No transcription model set for {}.", resolved.label),
            local: false,
        };
    }
    if resolved.needs_key && resolved.api_key.is_empty() {
        return Readiness::NotReady { reason: format!("No API key for {}.", resolved.label), local: false };
    }
    Readiness::Ready { provider: resolved.label, model: resolved.model, local: false }
}

/// Everything a transcription needs, captured on the main thread so the work
/// itself can run on the I/O runtime.
#[derive(Clone)]
pub struct Job {
    pub local: bool,
    pub transcription: TranscriptionConfig,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

pub fn job(cfg: &Config, secrets: &Secrets) -> anyhow::Result<Job> {
    let ready = readiness(cfg, secrets);
    if let Readiness::NotReady { reason, .. } = &ready {
        bail!("{reason}");
    }
    let resolved = resolve_transcription(cfg, &|id| secrets.get(id));
    Ok(Job {
        local: ready.is_local(),
        transcription: cfg.transcription.clone(),
        base_url: resolved.base_url,
        model: resolved.model,
        api_key: resolved.api_key,
    })
}

/// `wav` is 16 kHz mono 16-bit PCM — what whisper.cpp reads natively, and a
/// format every OpenAI-compatible endpoint accepts.
pub async fn run(job: Job, wav: Vec<u8>) -> anyhow::Result<String> {
    if wav.is_empty() {
        bail!("Recording was empty.");
    }
    if job.local {
        // No upload, so no size ceiling — the only limit is patience.
        return whisper::transcribe(wav, &job.transcription.whisper_cpp, &job.transcription.language).await;
    }
    if wav.len() > MAX_BYTES {
        bail!(
            "Recording is {:.1} MB — over the 25 MB upload limit. Turn on chunked transcription in Settings.",
            wav.len() as f64 / 1_048_576.0
        );
    }

    let part = reqwest::multipart::Part::bytes(wav).file_name("speech.wav").mime_str("audio/wav")?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", job.model.clone())
        .text("response_format", "json");
    if !job.transcription.language.is_empty() {
        form = form.text("language", job.transcription.language.clone());
    }
    let mut request = crate::runtime::http().post(format!("{}/audio/transcriptions", job.base_url)).multipart(form);
    if !job.api_key.is_empty() {
        request = request.bearer_auth(&job.api_key);
    }
    let response = request.send().await?;
    let status = response.status();
    let raw = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let mut detail: String = raw.chars().take(300).collect();
        if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
            if let Some(m) = parsed.pointer("/error/message").or_else(|| parsed.get("message")).and_then(Value::as_str)
            {
                detail = m.to_string();
            }
        }
        bail!("Transcription failed ({}): {detail}", status.as_u16());
    }
    Ok(parse_response(&raw))
}

fn parse_response(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(parsed) => parsed
            .get("text")
            .or_else(|| parsed.get("transcript"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        // response_format=text servers.
        Err(_) => raw.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_only_providers_are_not_ready() {
        let mut cfg = Config::default();
        cfg.transcription.provider = "anthropic".into();
        let secrets = Secrets::empty_for_tests();
        match readiness(&cfg, &secrets) {
            Readiness::NotReady { reason, local } => {
                assert!(reason.contains("no speech-to-text endpoint"));
                assert!(!local);
            }
            other => panic!("{other:?}"),
        }
        cfg.transcription.provider = "openai".into();
        assert!(
            matches!(readiness(&cfg, &secrets), Readiness::NotReady { reason, .. } if reason == "No API key for OpenAI.")
        );
        cfg.transcription.provider = "custom".into();
        assert!(matches!(readiness(&cfg, &secrets), Readiness::Ready { local: false, .. }));
    }

    #[test]
    fn parses_json_and_text_bodies() {
        assert_eq!(parse_response(r#"{"text":" halo "}"#), "halo");
        assert_eq!(parse_response("plain words\n"), "plain words");
    }
}
