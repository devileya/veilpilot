//! Minimal Server-Sent Events reader for the OpenAI and Anthropic wire formats.

use futures::StreamExt as _;

#[derive(Debug, PartialEq)]
pub enum SseItem {
    Json(serde_json::Value),
    /// `data: [DONE]` — the OpenAI end-of-stream sentinel.
    Done,
}

/// Splits a byte stream into `data:` payloads. Bytes are buffered until a full
/// line arrives, so a UTF-8 character split across network chunks is never
/// decoded in halves.
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseItem> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            let Some(payload) = line.strip_prefix("data:") else { continue };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            if payload == "[DONE]" {
                out.push(SseItem::Done);
                continue;
            }
            // Providers occasionally emit keep-alive or comment frames that
            // are not JSON. Skipping is correct; failing would kill the answer.
            if let Ok(value) = serde_json::from_str(payload) {
                out.push(SseItem::Json(value));
            }
        }
        out
    }
}

/// Feed every SSE item of a response to `on_item`; stops early when it
/// returns `false` or at `[DONE]`. Returns whether `[DONE]` arrived.
pub async fn for_each(
    response: reqwest::Response,
    mut on_item: impl FnMut(serde_json::Value) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let mut decoder = SseDecoder::default();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        for item in decoder.push(&chunk) {
            match item {
                SseItem::Done => return Ok(true),
                SseItem::Json(value) => {
                    if !on_item(value)? {
                        return Ok(false);
                    }
                }
            }
        }
    }
    Ok(false)
}

/// Error bodies are JSON on every provider seen so far, but not reliably
/// shaped, so dig for the usual fields before falling back to raw text.
pub fn describe_error(status: reqwest::StatusCode, body: &str) -> String {
    let mut detail: String = body.chars().take(400).collect();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) {
        let found = parsed
            .pointer("/error/message")
            .or_else(|| parsed.get("message"))
            .or_else(|| parsed.get("error"))
            .and_then(|v| v.as_str());
        if let Some(found) = found {
            detail = found.to_string();
        }
    }
    let hint = match status.as_u16() {
        401 => " — check the API key in Settings",
        404 => " — check the base URL and model name in Settings",
        _ => "",
    };
    format!("{} {}: {}{}", status.as_u16(), status.canonical_reason().unwrap_or(""), detail, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_lines_and_skips_noise() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"data: {\"a\":").is_empty());
        let items = d.push(b"1}\r\n: keep-alive\n\nevent: x\ndata: not json\ndata: [DONE]\n");
        assert_eq!(items, vec![SseItem::Json(serde_json::json!({"a":1})), SseItem::Done]);
    }

    #[test]
    fn keeps_multibyte_characters_whole() {
        let mut d = SseDecoder::default();
        let text = "data: {\"t\":\"héllo\"}\n".as_bytes();
        let split = text.iter().position(|&b| b == 0xc3).unwrap() + 1;
        assert!(d.push(&text[..split]).is_empty());
        let items = d.push(&text[split..]);
        assert_eq!(items, vec![SseItem::Json(serde_json::json!({"t":"héllo"}))]);
    }

    #[test]
    fn describes_errors_with_hints() {
        let msg = describe_error(reqwest::StatusCode::UNAUTHORIZED, r#"{"error":{"message":"bad key"}}"#);
        assert_eq!(msg, "401 Unauthorized: bad key — check the API key in Settings");
        let msg = describe_error(reqwest::StatusCode::BAD_GATEWAY, "upstream down");
        assert_eq!(msg, "502 Bad Gateway: upstream down");
    }
}
