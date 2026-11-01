#!/usr/bin/env python3
"""A stand-in for the OpenAI-compatible, Anthropic, and transcription APIs.

Every request is appended to requests.jsonl in the working directory so the
end-to-end script can check exactly what the app sent. Behaviour is steered by
words in the last user message:

  slow      stream one word every 300 ms (for cancel tests)
  refuse    Anthropic: stop_reason "refusal" with stop_details
  fallback  Anthropic: a fallback content block before the answer
  nofallback  Anthropic: reject the fallbacks parameter with a 400
  boom      respond 500 with a JSON error body
  cut       end the stream without a terminal frame
  rhetorical  OpenAI: a live-answer prompt whose last transcript line says it
            gets exactly SKIP (the model declining to answer)
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWER = (
    "The **root cause** is `cfg.db` read before the await.\n\n"
    "1. Await the loader\n2. Then read `host`\n\n"
    "```js\nconst cfg = await load()\n```\n"
    "See [docs](https://example.com/docs)."
)


def live_line(text):
    """The newest transcript line of a live-answer prompt, or None."""
    head, sep, _ = text.partition("\n\nThe last line")
    if not sep:
        return None
    return head.splitlines()[-1]


def last_user_text(body):
    for message in reversed(body.get("messages", [])):
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, str):
            return content
        return " ".join(part.get("text", "") for part in content if part.get("type") == "text")
    return ""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def handle(self):
        # A cancelled answer hangs up mid-stream; that is the point, not an error.
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def record(self, body, extra=None):
        entry = {"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": body}
        if extra:
            entry.update(extra)
        with open("requests.jsonl", "a") as f:
            f.write(json.dumps(entry) + "\n")

    def sse_start(self):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()

    def send_event(self, payload, event=None):
        text = ""
        if event:
            text += f"event: {event}\n"
        text += f"data: {payload}\n\n"
        self.wfile.write(text.encode())
        self.wfile.flush()

    def error_json(self, status, message):
        data = json.dumps({"error": {"type": "invalid_request_error", "message": message}}).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        # GitHub's "latest release", for the update check.
        if self.path.endswith("/releases/latest"):
            data = json.dumps({
                "tag_name": "v9.9.9", "html_url": "https://example.com/releases/v9.9.9",
                "draft": False, "prerelease": False,
                "assets": [{"name": f"VeilPilot-9.9.9-{suffix}", "browser_download_url": f"https://example.com/{suffix}",
                            "size": 1} for suffix in ["macos-universal.dmg", "windows-x64.zip", "linux-x64.tar.gz"]],
            }).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.error_json(404, f"no route {self.path}")

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(length)

        if self.path.endswith("/audio/transcriptions"):
            ctype = self.headers.get("content-type", "")
            self.record(None, {"bytes": len(raw), "contentType": ctype,
                               "hasWav": b"RIFF" in raw[:4096], "hasModel": b'name="model"' in raw})
            data = json.dumps({"text": "mock cloud transcript"}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return

        body = json.loads(raw or b"{}")
        self.record(body)
        text = last_user_text(body)

        if "boom" in text:
            return self.error_json(500, "mock server exploded")

        if self.path.endswith("/chat/completions"):
            return self.openai(body, text)
        if self.path.endswith("/messages"):
            return self.anthropic(body, text)
        self.error_json(404, f"no route {self.path}")

    def openai(self, body, text):
        self.sse_start()
        self.send_event(json.dumps({"choices": [{"delta": {"reasoning_content": "Thinking about it. "}}]}))
        line = live_line(text)
        words = ["SKIP"] if line and "rhetorical" in line else ANSWER.split(" ")
        for i, word in enumerate(words):
            piece = word if i == 0 else " " + word
            self.send_event(json.dumps({"choices": [{"delta": {"content": piece}}]}))
            if "slow" in text:
                time.sleep(0.3)
        if "cut" in text:
            return
        self.send_event(json.dumps({"choices": [{"delta": {}, "finish_reason": "stop"}]}))
        self.send_event("[DONE]")

    def anthropic(self, body, text):
        if "fallbacks" in body and "nofallback" in text:
            return self.error_json(400, "Unexpected value for fallbacks")
        self.sse_start()
        model = body.get("model")
        if "fallback" in text and "nofallback" not in text:
            model = "claude-opus-4-8"
        self.send_event(json.dumps({"type": "message_start", "message": {"model": model}}), "message_start")
        index = 0
        if "fallback" in text and "nofallback" not in text:
            self.send_event(json.dumps({"type": "content_block_start", "index": 0, "content_block": {
                "type": "fallback", "from": {"model": body.get("model")}, "to": {"model": "claude-opus-4-8"}}}),
                "content_block_start")
            index = 1
        self.send_event(json.dumps({"type": "content_block_delta", "index": index,
                                    "delta": {"type": "thinking_delta", "thinking": "Considering. "}}), "content_block_delta")
        stop = "end_turn"
        details = None
        if "refuse" in text:
            stop = "refusal"
            details = {"type": "refusal", "category": "cyber", "explanation": "Mock decline."}
        else:
            for word in ANSWER.split(" "):
                self.send_event(json.dumps({"type": "content_block_delta", "index": index + 1,
                                            "delta": {"type": "text_delta", "text": word + " "}}), "content_block_delta")
                if "slow" in text:
                    time.sleep(0.3)
        if "cut" in text:
            return
        delta = {"stop_reason": stop}
        if details:
            delta["stop_details"] = details
        self.send_event(json.dumps({"type": "message_delta", "delta": delta, "usage": {"output_tokens": 42}}), "message_delta")
        self.send_event(json.dumps({"type": "message_stop"}), "message_stop")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
