//! OpenAI-compatible `POST /chat/completions`. Covers OpenAI, GLM (Zhipu),
//! Groq, OpenRouter, DeepSeek, Ollama, LM Studio, vLLM — anything exposing it.

use serde_json::{Value, json};

use super::{Chunk, ChunkSender, Done, StreamOptions, sse};
use crate::session::Turn;

fn to_message(turn: &Turn) -> Value {
    if turn.images.is_empty() {
        return json!({ "role": turn.role.as_str(), "content": turn.text });
    }
    let mut content = vec![json!({ "type": "text", "text": turn.text })];
    content.extend(turn.images.iter().map(|img| {
        json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", img.mime, img.base64) }
        })
    }));
    json!({ "role": turn.role.as_str(), "content": content })
}

pub fn request_body(opts: &StreamOptions) -> Value {
    let mut messages = Vec::new();
    if !opts.system.is_empty() {
        messages.push(json!({ "role": "system", "content": opts.system }));
    }
    messages.extend(opts.messages.iter().map(to_message));
    json!({
        "model": opts.model,
        "stream": true,
        "max_tokens": opts.max_tokens,
        "messages": messages,
    })
}

pub async fn stream(opts: &StreamOptions, tx: &ChunkSender) -> anyhow::Result<()> {
    let mut request =
        crate::runtime::http().post(format!("{}/chat/completions", opts.base_url)).json(&request_body(opts));
    if !opts.api_key.is_empty() {
        request = request.bearer_auth(&opts.api_key);
    }
    // OpenRouter attributes traffic by these; harmless elsewhere.
    if opts.base_url.contains("openrouter.ai") {
        request = request.header("http-referer", "https://github.com/veilpilot").header("x-title", "VeilPilot");
    }

    let response = request.send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(sse::describe_error(status, &body));
    }

    let mut finish_reason: Option<String> = None;
    let saw_done = sse::for_each(response, |chunk| {
        let Some(choice) = chunk.pointer("/choices/0") else { return Ok(true) };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(reason.to_string());
        }
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        // Reasoning models on OpenAI-compatible gateways surface their chain
        // of thought under a non-standard key; show it where Claude's
        // thinking goes.
        let reasoning = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(reasoning) = reasoning {
            let _ = tx.unbounded_send(Chunk::Thinking(reasoning.to_string()));
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let _ = tx.unbounded_send(Chunk::Text(text.to_string()));
        }
        Ok(true)
    })
    .await?;

    // Neither `[DONE]` nor a finish reason: the connection was cut (a proxy
    // timing out, say). Ending without `Done` reports the answer incomplete.
    if !saw_done && finish_reason.is_none() {
        return Ok(());
    }
    let _ = tx.unbounded_send(Chunk::Done(Done {
        stop_reason: finish_reason,
        model: Some(opts.model.clone()),
        cost_usd: None,
    }));
    Ok(())
}
