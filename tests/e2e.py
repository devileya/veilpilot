#!/usr/bin/env python3
"""End-to-end tests: drive the real app through its dev control channel.

    python3 tests/e2e.py [--release] [--claude-code]

Starts VeilPilot in a throwaway data directory with VEILPILOT_DEV=1, points it
at tests/mock_llm.py, and exercises the features a user reaches by hotkey,
click, and keystroke — then checks the app's state and what it sent over the
wire. Nothing touches your real config or keychain entry.

--claude-code also runs three tiny real asks through your `claude` CLI login
(a few cents). Speech tests use whisper.cpp and a ggml model when present.
"""

import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import base64
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASE = "--release" in sys.argv
WITH_CLAUDE_CODE = "--claude-code" in sys.argv
BIN = ROOT / "target" / ("release" if RELEASE else "debug") / "veilpilot"

passed, failed = [], []


def check(name, condition, detail=""):
    (passed if condition else failed).append(name)
    mark = "ok  " if condition else "FAIL"
    print(f"  [{mark}] {name}" + (f" — {detail}" if detail and not condition else ""))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class App:
    def __init__(self, data_dir):
        self.env = dict(os.environ, VEILPILOT_DATA_DIR=str(data_dir))
        self.proc = None

    def start(self):
        env = dict(self.env, VEILPILOT_DEV="1", VEILPILOT_ALLOW_UNPERMITTED_CAPTURE="1")
        self.log = open(Path(self.env["VEILPILOT_DATA_DIR"]) / "app.log", "w")
        self.proc = subprocess.Popen([str(BIN)], env=env, stdout=self.log, stderr=subprocess.STDOUT)
        for _ in range(100):
            time.sleep(0.1)
            try:
                if self.send("state").startswith("{"):
                    return
            except RuntimeError:
                pass
        raise RuntimeError("app did not start")

    def send(self, *words):
        out = subprocess.run([str(BIN), "--send", *words], env=self.env, capture_output=True, text=True, timeout=180)
        if out.returncode == 2:
            raise RuntimeError(out.stderr.strip())
        return out.stdout.strip()

    def state(self):
        return json.loads(self.send("state"))

    def config(self):
        return json.loads(self.send("config"))

    def set_config(self, patch):
        assert self.send("set-config", json.dumps(patch)) == "ok"

    def idle(self):
        self.send("wait-idle")

    def ask(self, text):
        self.send("action", "Send")
        self.send("type", text)
        self.send("key", "enter")
        self.idle()
        return self.state()["cards"][-1]

    def quit(self):
        try:
            self.send("quit")
        except Exception:
            pass
        if self.proc:
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def requests(mock_dir):
    path = mock_dir / "requests.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def png_size(data_url):
    png = base64.b64decode(data_url.split(",", 1)[1])
    return struct.unpack(">II", png[16:24])


def main():
    if not BIN.exists():
        print(f"build first: cargo build{' --release' if RELEASE else ''}")
        return 2

    data = Path(tempfile.mkdtemp(prefix="veilpilot-e2e-"))
    mock_dir = data / "mock"
    mock_dir.mkdir()
    (data / "models").mkdir()
    real_models = Path.home() / "Library/Application Support/VeilPilot/models"
    for model in sorted(real_models.glob("ggml-*.bin")) if real_models.exists() else []:
        (data / "models" / model.name).symlink_to(model)

    port = free_port()
    base = f"http://127.0.0.1:{port}"
    mock = subprocess.Popen([sys.executable, str(ROOT / "tests/mock_llm.py"), str(port)], cwd=mock_dir)
    app = App(data)
    try:
        app.start()
        run(app, base, mock_dir, data)
    finally:
        app.quit()
        mock.kill()
        subprocess.run(["security", "delete-generic-password", "-s", f"VeilPilot ({data})", "-a", "api-keys"],
                       capture_output=True)
        shutil.rmtree(data, ignore_errors=True)

    print(f"\n{len(passed)} passed, {len(failed)} failed")
    for name in failed:
        print(f"  failed: {name}")
    return 1 if failed else 0


def run(app, base, mock_dir, data):
    s = app.state()
    print("launch")
    check("panel is visible and focused", s["visible"] and s["focused"])
    check("every global hotkey registered", s["hotkeyErrors"] == [], s["hotkeyErrors"])
    check("capture protection on by default", s["protection"])

    print("asking (OpenAI-compatible)")
    app.set_config({"chat": {"provider": "custom", "baseUrl": f"{base}/v1", "model": "mock-model"}})
    card = app.ask("why is it failing?")
    check("typed question streams an answer", card["done"] and "**root cause**" in card["text"], card)
    check("reasoning is captured", card["thinking"] > 0)
    check("ask box is cleared after sending", app.state()["input"] == "")
    req = requests(mock_dir)[-1]
    check("system prompt is sent", req["body"]["messages"][0]["role"] == "system")
    app.ask("and the fix?")
    roles = [m["role"] for m in requests(mock_dir)[-1]["body"]["messages"]]
    check("follow-up carries the history", roles == ["system", "user", "assistant", "user"], roles)

    app.send("action", "Send")
    app.send("type", "slow answer")
    app.send("key", "enter")
    time.sleep(1.0)
    check("answer is streaming", app.state()["streaming"])
    app.send("action", "Cancel")
    time.sleep(0.3)
    s = app.state()
    check("cancel hotkey stops the answer", not s["streaming"] and s["cards"][-1]["notes"] == ["Stopped."])

    app.send("type", "slow again")
    app.send("key", "enter")
    time.sleep(0.8)
    app.send("key", "escape")
    time.sleep(0.3)
    s = app.state()
    check("Escape while streaming cancels, panel stays", not s["streaming"] and s["visible"])
    app.send("key", "escape")
    time.sleep(0.3)
    check("Escape when idle hides the panel", not app.state()["visible"])
    app.send("action", "ToggleOverlay")
    time.sleep(0.3)
    s = app.state()
    check("toggle shows it again with the ask box focused", s["visible"] and s["inputFocused"])

    card = app.ask("boom")
    check("server error becomes an error card", "mock server exploded" in (card["error"] or ""), card)
    card = app.ask("cut it")
    check("cut stream is flagged incomplete", "The connection ended before the answer finished." in card["notes"], card)

    print("screenshots")
    app.send("action", "NewSession")
    app.send("prompt", "explain")
    app.idle()
    card = app.state()["cards"][-1]
    check("prompt hotkey captures and asks", card["label"] == "Jelaskan" and card["shots"] == 1 and card["done"], card)
    content = requests(mock_dir)[-1]["body"]["messages"][-1]["content"]
    check("screenshot is sent as a PNG data URL",
          isinstance(content, list) and content[1]["image_url"]["url"].startswith("data:image/png;base64,"))
    check("preset text is sent", content[0]["text"].startswith("Jelaskan apa yang ada di layar"))

    app.send("action", "ShotOnly")
    app.idle()
    s = app.state()
    check("attach-only adds a chip and focuses the ask box", len(s["attachments"]) == 1 and s["inputFocused"], s)
    app.send("type", "what is this?")
    app.send("key", "enter")
    app.idle()
    check("attachment is consumed by the ask", app.state()["attachments"] == [])

    app.send("action", "RegionShot")
    for _ in range(40):
        if app.state()["regionOpen"]:
            break
        time.sleep(0.1)
    check("region picker opens", app.state()["regionOpen"])
    check("region selection completes", app.send("region", "100", "100", "300", "200") == "ok")
    app.idle()
    url = [p for p in requests(mock_dir)[-1]["body"]["messages"][-1]["content"] if p["type"] == "image_url"][0]
    w, h = png_size(url["image_url"]["url"])
    check("region crop has the selected size (Retina = 2x)", (w, h) in [(300, 200), (600, 400)], (w, h))
    app.send("action", "RegionShot")
    time.sleep(1.0)
    app.send("key", "escape")
    time.sleep(0.3)
    check("Escape cancels the picker", not app.state()["regionOpen"])

    print("transcript prompts")
    app.send("action", "NewSession")
    app.send("prompt", "recap")
    time.sleep(0.2)
    check("empty transcript warns instead of asking", "Transcript is empty" in app.state()["toasts"][-1])
    app.send("transcript", "Kita perlu deploy hari Jumat.")
    app.send("prompt", "recap")
    app.idle()
    text = requests(mock_dir)[-1]["body"]["messages"][-1]["content"]
    check("transcript fills {{transcript}}", text.endswith("Kita perlu deploy hari Jumat."), text[-60:])

    print("Anthropic")
    app.send("set-key", "anthropic", "sk-ant-e2e")
    app.set_config({"chat": {"provider": "anthropic", "baseUrl": base, "model": "claude-opus-5", "effort": "medium"}})
    app.send("action", "NewSession")
    card = app.ask("hello")
    req = requests(mock_dir)[-1]
    check("Anthropic headers", req["headers"].get("x-api-key") == "sk-ant-e2e"
          and req["headers"].get("anthropic-version") == "2023-06-01"
          and req["headers"].get("anthropic-beta") == "server-side-fallback-2026-07-01", req["headers"])
    body = req["body"]
    check("Anthropic body", body.get("fallbacks") == "default"
          and body.get("thinking") == {"type": "adaptive", "display": "summarized"}
          and body.get("output_config") == {"effort": "medium"}, body.keys())
    check("Anthropic answer streams", card["done"] and "root cause" in card["text"])
    card = app.ask("please refuse")
    check("refusal is explained", card["notes"] == ["Declined (cyber). Mock decline."], card["notes"])
    card = app.ask("trigger fallback")
    check("fallback routing is noted", card["notes"] == ["Routed to claude-opus-4-8"]
          and card["meta"].startswith("claude-opus-4-8"), card)
    before = len(requests(mock_dir))
    card = app.ask("nofallback please")
    tries = requests(mock_dir)[before:]
    check("rejected fallback retries once without it", len(tries) == 2 and "fallbacks" not in tries[1]["body"]
          and card["done"] and not card["error"])
    app.send("prompt", "solve")
    app.idle()
    body = requests(mock_dir)[-1]["body"]
    check("preset effort overrides the default", body.get("output_config") == {"effort": "high"})
    check("Claude gets the image before the text", body["messages"][-1]["content"][0]["type"] == "image")

    print("guards")
    app.set_config({"chat": {"provider": "deepseek", "baseUrl": f"{base}/v1", "model": ""}})
    app.send("set-key", "deepseek", "dk-e2e")
    app.send("prompt", "explain")
    app.idle()
    card = app.state()["cards"][-1]
    check("non-vision model gets no image, with a note", "no vision support" in " ".join(card["notes"]))
    app.set_config({"chat": {"provider": "openai", "baseUrl": "", "model": ""}})
    card = app.ask("hi")
    check("missing key is an error card", card["error"] == "No API key for OpenAI. Open settings › Keys and paste one.")

    print("window")
    app.send("action", "ToggleProtection")
    check("protection toggles off", not app.state()["protection"])
    app.send("action", "ToggleProtection")
    check("protection toggles on", app.state()["protection"])
    app.send("action", "ClickThrough")
    check("click-through toggles", app.state()["clickThrough"])
    app.send("action", "ClickThrough")
    app.send("action", "OpacityDown")
    check("opacity steps down", abs(app.state()["opacity"] - 0.92) < 0.001)
    for _ in range(3):
        app.send("action", "OpacityUp")
    check("opacity caps at 100%", app.state()["opacity"] == 1.0)
    x0 = app.state()["frame"]["x"]
    app.send("action", "MoveLeft")
    time.sleep(0.8)
    check("move hotkey moves the panel", app.state()["frame"]["x"] == x0 - 48)
    saved = json.loads((data / "config.json").read_text())["window"]["bounds"]
    check("moved position is saved", saved and saved["x"] == x0 - 48, saved)
    app.send("action", "MoveRight")

    print("global hotkeys (Carbon event path)")
    app.send("press", "Command+Shift+\\")
    time.sleep(0.3)
    check("⌘⇧\\ hides", not app.state()["visible"])
    app.send("press", "Command+Shift+\\")
    time.sleep(0.3)
    check("⌘⇧\\ shows", app.state()["visible"])
    app.send("press", "Command+Shift+K")
    time.sleep(0.2)
    check("⌘⇧K starts a new session", app.state()["cards"] == [])

    print("settings")
    app.set_config({"chat": {"provider": "custom", "baseUrl": f"{base}/v1", "model": "mock-model"}})
    app.send("key", "cmd-,")
    time.sleep(0.3)
    check("⌘, opens settings", app.state()["settings"] == "Ai")
    app.send("settings", "prompts")
    time.sleep(0.5)
    app.send("click", "215", "175")  # first prompt's hotkey box
    time.sleep(0.3)
    app.send("key", "cmd-shift-9")
    time.sleep(0.3)
    check("hotkey box captures a new combination", app.config()["prompts"][0]["accelerator"] == "Command+Shift+9")
    app.send("settings", "close")
    time.sleep(0.3)
    app.send("press", "Command+Shift+9")
    time.sleep(0.3)
    app.idle()
    check("the new hotkey fires its prompt", any(c["label"] == "Jelaskan" for c in app.state()["cards"]))

    app.send("settings", "keys")
    time.sleep(0.5)
    app.send("click", "250", "127")  # OpenAI key field
    time.sleep(0.2)
    app.send("type", "sk-e2e-key")
    app.send("key", "enter")
    time.sleep(0.3)
    check("pasted key is saved", app.state()["toasts"][-1] == "Info: API key saved.")
    check("key never lands in config.json", "sk-e2e-key" not in (data / "config.json").read_text())
    app.send("settings", "close")

    print("speech to text")
    wav = data / "speech.wav"
    voice = subprocess.run(["say", "-v", "Damayanti", "Halo semua, kita perlu deploy hari Jumat.", "-o",
                            str(data / "speech.aiff")], capture_output=True)
    if voice.returncode == 0 and subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@44100", "-c", "2",
                                                 str(data / "speech.aiff"), str(wav)]).returncode == 0:
        if list((data / "models").glob("ggml-*.bin")) and shutil.which("whisper-cli"):
            app.set_config({"transcription": {"provider": "local", "language": ""}})
            out = app.send("transcribe", str(wav))
            check("on-device whisper.cpp transcribes speech", "deploy" in out.lower(), out)
        else:
            print("  [skip] whisper.cpp or a model is not installed")
        app.set_config({"transcription": {"provider": "custom", "baseUrl": f"{base}/v1", "model": "whisper-1"}})
        out = app.send("transcribe", str(wav))
        req = requests(mock_dir)[-1]
        check("cloud transcription uploads 16 kHz WAV", out == "ok mock cloud transcript" and req["hasWav"], out)
        app.set_config({"transcription": {"provider": "anthropic"}})
        check("chat-only provider is refused for speech", "no speech-to-text endpoint" in app.send("transcribe", str(wav)))
    else:
        print("  [skip] could not synthesise speech")

    if WITH_CLAUDE_CODE:
        print("Claude Code (real, a few cents)")
        app.set_config({"chat": {"provider": "claudecode", "model": "sonnet", "effort": "low", "baseUrl": ""}})
        app.send("action", "NewSession")
        card = app.ask("Reply with exactly the word: PINEAPPLE")
        check("Claude Code answers", "PINEAPPLE" in card["text"] and "$" in card["meta"], card)
        card = app.ask("Which word did I just ask you to reply with? One word.")
        check("Claude Code resumes the session", "PINEAPPLE" in card["text"].upper(), card)


if __name__ == "__main__":
    sys.exit(main())
