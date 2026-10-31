//! Anthropic Messages API over raw HTTP + SSE (there is no official Rust SDK).

use serde_json::{Value, json};

use super::{Chunk, ChunkSender, Done, StreamOptions, sse};
use crate::session::Turn;

const API_VERSION: &str = "2023-06-01";
/// `fallbacks: "default"` lets the API re-run a policy-declined request on a
/// suitable model inside the same call, routed by why it was declined. The
/// scalar form pairs with exactly this beta header.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

fn to_message(turn: &Turn) -> Value {
    if turn.images.is_empty() {
        return json!({ "role": turn.role.as_str(), "content": turn.text });
    }
    // Images before text: Claude reads the prompt against an image it has
    // already seen.
    let mut content: Vec<Value> = turn
        .images
        .iter()
        .map(|img| {
            json!({
                "type": "image",
                "source": { "type": "base64", "media_type": img.mime, "data": img.base64.as_str() }
            })
        })
        .collect();
    content.push(json!({ "type": "text", "text": turn.text }));
    json!({ "role": turn.role.as_str(), "content": content })
}

pub fn request_body(opts: &StreamOptions, with_fallback: bool) -> Value {
    let mut body = json!({
        "model": opts.model,
        "max_tokens": opts.max_tokens,
        "stream": true,
        "messages": opts.messages.iter().map(to_message).collect::<Vec<_>>(),
        // Adaptive is the thinking mode on current models; "summarized"
        // matters for a HUD because the default shows a long silent pause.
        "thinking": {
            "type": "adaptive",
            "display": if opts.show_thinking { "summarized" } else { "omitted" }
        },
    });
    if !opts.system.is_empty() {
        body["system"] = json!(opts.system);
    }
    if let Some(effort) = &opts.effort {
        body["output_config"] = json!({ "effort": effort });
    }
    if with_fallback {
        body["fallbacks"] = json!("default");
    }
    body
}

fn messages_url(base_url: &str) -> String {
    let base = if base_url.is_empty() { "https://api.anthropic.com" } else { base_url };
    if base.ends_with("/v1") { format!("{base}/messages") } else { format!("{base}/v1/messages") }
}

/// A 400/404 on the beta path means this key or model does not take the
/// fallback parameter. Retried once on the plain endpoint instead.
fn fallback_rejected(status: u16, message: &str) -> bool {
    (status == 400 || status == 404)
        && ["fallback", "beta", "unsupported", "unexpected"].iter().any(|w| message.to_lowercase().contains(w))
}

#[derive(Default)]
struct Outcome {
    model: Option<String>,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
}

/// Map one SSE event; returns a chunk to forward, if any.
fn map_event(event: &Value, outcome: &mut Outcome) -> anyhow::Result<Option<Chunk>> {
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            outcome.model = event.pointer("/message/model").and_then(Value::as_str).map(String::from);
            Ok(None)
        }
        Some("content_block_start") => {
            let block = event.get("content_block");
            if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("fallback") {
                let to =
                    block.and_then(|b| b.pointer("/to/model")).and_then(Value::as_str).unwrap_or("a fallback model");
                return Ok(Some(Chunk::Note(format!("Routed to {to}"))));
            }
            Ok(None)
        }
        Some("content_block_delta") => {
            let delta = event.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                Some("text_delta") => {
                    Ok(delta.and_then(|d| d.get("text")).and_then(Value::as_str).map(|t| Chunk::Text(t.to_string())))
                }
                Some("thinking_delta") => Ok(delta
                    .and_then(|d| d.get("thinking"))
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(|t| Chunk::Thinking(t.to_string()))),
                _ => Ok(None),
            }
        }
        Some("message_delta") => {
            if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                outcome.stop_reason = Some(reason.to_string());
            }
            if let Some(details) = event.pointer("/delta/stop_details").filter(|v| !v.is_null()) {
                outcome.stop_details = Some(details.clone());
            }
            Ok(None)
        }
        Some("error") => {
            let message = event
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Claude reported an error mid-stream.");
            anyhow::bail!("{message}")
        }
        _ => Ok(None),
    }
}

pub async fn stream(opts: &StreamOptions, tx: &ChunkSender) -> anyhow::Result<()> {
    let mut attempt = 0;
    loop {
        let use_fallback = opts.server_side_fallback && attempt == 0;
        attempt += 1;

        let mut request = crate::runtime::http()
            .post(messages_url(&opts.base_url))
            .header("x-api-key", &opts.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&request_body(opts, use_fallback));
        if use_fallback {
            request = request.header("anthropic-beta", FALLBACK_BETA);
        }

        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = sse::describe_error(status, &body);
            if use_fallback && fallback_rejected(status.as_u16(), &body) {
                eprintln!("[anthropic] server-side fallback not accepted, retrying without it");
                continue;
            }
            anyhow::bail!(message);
        }

        let mut outcome = Outcome::default();
        sse::for_each(response, |event| {
            if let Some(chunk) = map_event(&event, &mut outcome)? {
                let _ = tx.unbounded_send(chunk);
            }
            Ok(true)
        })
        .await?;

        // No stop reason means the stream was cut before message_delta.
        if outcome.stop_reason.is_none() {
            return Ok(());
        }

        // Check the stop reason before trusting content: a refusal carries
        // the reason in stop_details, not in the text.
        if outcome.stop_reason.as_deref() == Some("refusal") {
            let details = outcome.stop_details.unwrap_or(Value::Null);
            let category = details.get("category").and_then(Value::as_str).unwrap_or("policy");
            let explanation = details.get("explanation").and_then(Value::as_str).unwrap_or("");
            let _ = tx.unbounded_send(Chunk::Note(format!("Declined ({category}). {explanation}").trim().to_string()));
        }
        let _ = tx.unbounded_send(Chunk::Done(Done {
            stop_reason: outcome.stop_reason,
            model: outcome.model.or_else(|| Some(opts.model.clone())),
            cost_usd: None,
        }));
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_stream_events() {
        let mut o = Outcome::default();
        let ev = |v: Value, o: &mut Outcome| map_event(&v, o).unwrap();
        assert_eq!(ev(json!({"type":"message_start","message":{"model":"claude-opus-5"}}), &mut o), None);
        assert_eq!(o.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(
            ev(json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}), &mut o),
            Some(Chunk::Text("Hi".into()))
        );
        assert_eq!(
            ev(json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}), &mut o),
            Some(Chunk::Thinking("hmm".into()))
        );
        assert_eq!(
            ev(
                json!({"type":"content_block_start","content_block":{"type":"fallback","from":{"model":"a"},"to":{"model":"claude-opus-4-8"}}}),
                &mut o
            ),
            Some(Chunk::Note("Routed to claude-opus-4-8".into()))
        );
        ev(
            json!({"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"category":"cyber"}}}),
            &mut o,
        );
        assert_eq!(o.stop_reason.as_deref(), Some("refusal"));
        assert!(map_event(&json!({"type":"error","error":{"message":"overloaded"}}), &mut o).is_err());
    }

    #[test]
    fn body_shape() {
        let opts = StreamOptions {
            kind: crate::config::ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            api_key: "k".into(),
            model: "claude-opus-5".into(),
            system: "sys".into(),
            messages: vec![Turn {
                role: crate::session::Role::User,
                text: "q".into(),
                images: vec![crate::session::ImagePart {
                    mime: "image/png",
                    base64: std::sync::Arc::new("AA==".into()),
                }],
            }],
            max_tokens: 8192,
            effort: Some("high".into()),
            show_thinking: true,
            server_side_fallback: true,
            claude_code: None,
        };
        let b = request_body(&opts, true);
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["thinking"]["display"], "summarized");
        assert_eq!(b["messages"][0]["content"][0]["type"], "image");
        assert_eq!(b["messages"][0]["content"][1]["text"], "q");
        assert!(request_body(&opts, false).get("fallbacks").is_none());
        assert_eq!(messages_url("https://api.anthropic.com"), "https://api.anthropic.com/v1/messages");
        assert_eq!(messages_url("http://proxy/v1"), "http://proxy/v1/messages");
    }

    #[test]
    fn fallback_rejection_detection() {
        assert!(fallback_rejected(400, "Unexpected value for fallbacks"));
        assert!(!fallback_rejected(401, "fallback"));
        assert!(!fallback_rejected(400, "max_tokens too large"));
    }
}
