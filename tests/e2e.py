#!/usr/bin/env python3
"""End-to-end tests: drive the real app through its dev control channel.

    python3 tests/e2e.py [--release] [--claude-code]

Starts VeilPilot in a throwaway data directory with VEILPILOT_DEV=1, points it
at tests/mock_llm.py, and exercises the features a user reaches by hotkey,
click, and keystroke — then checks the app's state and what it sent over the
wire. Nothing touches your real config or keychain entry.

--claude-code also runs a few tiny real asks through your `claude` CLI login
(a few cents). Speech tests use whisper.cpp and a ggml model when present.
DevCat projects and memory live in a throwaway DevCat folder; saving memory
goes through the real `devcat mcp` when DevCat is installed.
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
    def __init__(self, data_dir, devcat_dir, extra_env=None):
        # DEVCAT_TEST_MEMORY_SECRETS keeps `devcat mcp` off the keychain.
        self.env = dict(os.environ, VEILPILOT_DATA_DIR=str(data_dir), VEILPILOT_DEVCAT_DIR=str(devcat_dir),
                        DEVCAT_TEST_MEMORY_SECRETS="1", **(extra_env or {}))
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
    devcat = data / "devcat"
    make_devcat(devcat, data / "repo")
    make_claude_memory(data / "claude", data / "work" / "Misc")
    # The update check asks the mock instead of GitHub; no check at launch.
    (data / "config.json").write_text(json.dumps({"updates": {"check": False}}))
    app = App(data, devcat, {"VEILPILOT_CLAUDE_DIR": str(data / "claude"),
                             "VEILPILOT_UPDATE_URL": f"{base}/repos/x/releases/latest"})
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


def make_devcat(devcat, repo):
    """A DevCat data folder with two projects, one of them with a memory."""
    repo.mkdir()
    (devcat / "memory/projects/shop").mkdir(parents=True)
    (devcat / "projects.json").write_text(json.dumps([
        {"id": "shop", "name": "Shop", "order": 0, "folders": [str(repo)]},
        {"id": "misc", "name": "Misc", "order": 1},
    ]))
    (devcat / "memory/projects/shop/release-day.md").write_text(
        "---\nname: release-day\ndescription: Shop releases ship on Fridays after QA signs off\n"
        "metadata:\n  type: project\n  origin: claude-code\n  updated: 2026-10-30T12:00:00Z\n---\n\n"
        "Releases go out Friday afternoon once QA signs off in #qa.\n")


def make_claude_memory(claude, repo):
    """Claude Code's own memory for a repository named like the "Misc" DevCat
    project, which links no folder — as interopera does."""
    repo.mkdir(parents=True)
    folder = claude / "projects" / "".join(c if c.isascii() and c.isalnum() else "-" for c in str(repo))
    (folder / "memory").mkdir(parents=True)
    (folder / "memory" / "MEMORY.md").write_text("# Memory Index\n- [Staging](staging.md)\n")
    (folder / "memory" / "staging.md").write_text(
        "---\nname: staging-branch\ndescription: Misc work branches off origin/staging\nmetadata:\n"
        "  type: feedback\n---\n\nAlways branch Misc work off origin/staging, never main.\n")
    (folder / "session.jsonl").write_text(json.dumps({"cwd": str(repo)}) + "\n")


def script(path, body):
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)
    return path


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
    for _ in range(100):
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
    check("the panel is hidden from screen capture", app.send("capture-as-is", "panel").startswith("error")
          or "coverage=0.000" in app.send("capture-as-is", "panel"))
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

    print("compact and minimized")
    app.send("compact", "on")
    check("compact layout is saved", app.config()["window"]["compact"])
    app.send("compact", "off")
    height = app.state()["frame"]["height"]
    app.send("minimize")
    time.sleep(0.6)
    s = app.state()
    check("minimize folds the panel to its title bar", s["minimized"] and s["frame"]["height"] == 40, s["frame"])
    time.sleep(0.8)
    check("the real size is what gets saved", json.loads((data / "config.json").read_text())["window"]["bounds"]["height"]
          == height)
    app.send("action", "Send")
    time.sleep(0.6)
    s = app.state()
    check("asking opens it back up", not s["minimized"] and s["frame"]["height"] == height, s["frame"])

    print("updates")
    app.send("update-check")
    for _ in range(50):
        if app.state()["update"]["available"]:
            break
        time.sleep(0.1)
    update = app.state()["update"]
    check("a newer release is found", update["available"] and update["available"]["version"] == "9.9.9"
          and update["available"]["asset"].startswith("VeilPilot-9.9.9-"), update)

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

    agents_and_memory(app, base, mock_dir, data)
    live_answers(app, base, mock_dir, data)
    notch_mode(app)

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
        if list((data / "models").glob("ggml-*.bin")) and shutil.which("whisper-cli"):
            pause_test(app, data)
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
        app.set_config({"live": {"autoAnswer": True}})
        app.send("transcript", "Quick check before we continue: what is two plus two?")
        app.idle()
        live = app.state()["live"]
        answer = live[-1]["answer"].lower() if live else ""
        check("Claude Code gives a live answer", bool(live) and live[-1]["done"] and ("4" in answer or "four" in answer),
              live)
        app.set_config({"live": {"autoAnswer": False}})


def agents_and_memory(app, base, mock_dir, data):
    print("context and agents")
    devcat = data / "devcat"
    app.set_config({"chat": {"provider": "custom", "baseUrl": f"{base}/v1", "model": "mock-model"}})
    app.send("action", "NewSession")
    check("no context by default", app.state()["context"] == "")
    app.ask("when do we release?")
    system = requests(mock_dir)[-1]["body"]["messages"][0]["content"]
    check("no context sends no project memory", "Project context" not in system)

    app.send("menu", "context")
    check("context menu opens", app.state()["openMenu"] == "Context")
    app.send("choose", "shop")
    s = app.state()
    check("picking a project sets the context", s["context"] == "shop" and s["openMenu"] is None)
    check("the toast counts its memories", "Shop — 1 memory available" in s["toasts"][-1], s["toasts"])
    app.ask("when do we release?")
    system = requests(mock_dir)[-1]["body"]["messages"][0]["content"]
    check("the project's memory reaches the model", 'Project context: "Shop"' in system
          and "Releases go out Friday afternoon" in system, system[-300:])
    app.set_config({"context": {"useMemory": False}})
    app.ask("and again?")
    system = requests(mock_dir)[-1]["body"]["messages"][0]["content"]
    check("memory can be switched off", "Releases go out" not in system)
    app.set_config({"context": {"useMemory": True}})

    agents = {a["id"]: a for a in json.loads(app.send("agents"))}
    groups = [a["group"] for a in agents.values()]
    check("agent menu groups stay together", groups == sorted(groups, key=["Model APIs", "Agents on this Mac",
                                                                           "DevCat Agent"].index), groups)
    check("custom CLI asks for its command first", agents["customcli"]["unavailable"] == "set the command in Settings")

    fake = script(data / "fake-cli.sh", "printf 'CLI heard: '\ncat | tail -c 60\n")
    app.set_config({"chat": {"agents": {"customCommand": str(fake)}}})
    app.send("menu", "agent")
    app.send("choose", "customcli")
    check("switching agent is announced", app.state()["toasts"][-1] == "Info: Answering with Custom CLI.")
    card = app.ask("ping from e2e")
    check("a custom CLI answers through stdin", card["done"] and "CLI heard:" in card["text"]
          and "ping from e2e" in card["text"], card)

    args_file = data / "codex-args.txt"
    codex = script(data / "fake-codex", f"""printf '%s\\n' "$@" > '{args_file}'
pwd -P >> '{args_file}'
cat > /dev/null
echo '{{"type":"thread.started","thread_id":"t1"}}'
echo '{{"type":"item.completed","item":{{"id":"i0","type":"reasoning","text":"thinking"}}}}'
echo '{{"type":"item.completed","item":{{"id":"i1","type":"agent_message","text":"Codex says **hi**"}}}}'
""")
    app.set_config({"chat": {"agents": {"paths": {"codex": str(codex)}}}})
    app.send("menu", "agent")
    app.send("choose", "codex")
    card = app.ask("hello codex")
    args = args_file.read_text().splitlines()
    check("Codex runs read-only, headless", args[:5] == ["exec", "--json", "--skip-git-repo-check", "--sandbox",
                                                         "read-only"] and args[-2] == "-", args)
    check("Codex runs in the project's folder", args[-1] == str((data / "repo").resolve()), args[-1])
    check("Codex's JSON answer is shown", card["done"] and card["text"].strip() == "Codex says **hi**", card)

    app.send("menu", "agent")
    app.send("choose", "custom")
    check("an API provider keeps its model and URL", app.config()["chat"]["baseUrl"] == f"{base}/v1"
          and app.config()["chat"]["model"] == "mock-model", app.config()["chat"])

    app.send("menu", "context")
    app.send("choose", "misc")
    check("Claude Code memory counts for a project named like its folder",
          "Misc — 1 memory available" in app.state()["toasts"][-1], app.state()["toasts"][-1])
    app.ask("which branch for the deploy work?")
    system = requests(mock_dir)[-1]["body"]["messages"][0]["content"]
    check("Claude Code's memory reaches the model", "Always branch Misc work off origin/staging" in system
          and "Memory Index" not in system, system[-300:])
    app.send("menu", "context")
    app.send("choose", "shop")

    print("summary and memory")
    app.send("action", "NewSession")
    app.send("action", "Summarize")
    check("summary of nothing warns", "Nothing to summarize" in app.state()["toasts"][-1], app.state()["toasts"])
    app.send("transcript", "Kita sepakat rilis hari Jumat setelah QA.")
    app.send("action", "Summarize")
    app.idle()
    s = app.state()
    check("summarize makes a summary card", s["cards"][-1]["label"] == "Summary" and s["cards"][-1]["done"]
          and s["summary"], s["cards"][-1])
    sent = requests(mock_dir)[-1]["body"]["messages"][-1]["content"]
    check("the summary prompt carries the transcript", sent.startswith("Ringkas transkrip")
          and "Kita sepakat rilis hari Jumat" in sent, sent[-120:])

    app.send("menu", "context")
    app.send("choose", "")
    before = sorted(p.name for p in (devcat / "memory/projects/shop").iterdir())
    app.send("action", "SaveMemory")
    check("nothing is saved without a project", "Pick a project first" in app.state()["toasts"][-1]
          and sorted(p.name for p in (devcat / "memory/projects/shop").iterdir()) == before)
    app.send("menu", "context")
    app.send("choose", "shop")
    app.send("action", "SaveMemory")
    saved = None
    for _ in range(100):
        toast = app.state()["toasts"][-1]
        if "Saved to" in toast or "Error" in toast:
            break
        time.sleep(0.2)
    check("save to memory reports success", app.state()["toasts"][-1] == "Info: Saved to Shop's memory.",
          app.state()["toasts"])
    files = [p for p in (devcat / "memory/projects/shop").glob("meeting-*.md")]
    saved = files[0].read_text() if files else ""
    check("the meeting lands in the project's memory", saved.startswith("---\nname: meeting ")
          and "origin: veilpilot" in saved and "## Summary" in saved
          and "Kita sepakat rilis hari Jumat" in saved, saved[:300])
    audit = devcat / "mcp-audit.jsonl"
    if audit.exists():
        entry = json.loads(audit.read_text().splitlines()[-1])
        check("saved through DevCat's MCP server", entry["client"] == "veilpilot" and entry["tool"] == "memory_save"
              and entry["ok"], entry)
    else:
        print("  [skip] DevCat is not installed: the file was written directly")
    app.send("menu", "context")
    app.send("choose", "")


def live_answers(app, base, mock_dir, data):
    print("live answers")
    app.set_config({"live": {"names": "Arif, Pak Siregar", "autoAnswer": False, "answerOnName": False}})
    app.send("action", "NewSession")
    app.send("transcript", "Kapan rilisnya?")
    time.sleep(0.5)
    check("with both modes off, nothing answers by itself", app.state()["live"] == [])
    app.send("action", "ToggleAutoAnswer")
    check("the hotkey turns auto-answer on", app.config()["live"]["autoAnswer"])
    sent = len(requests(mock_dir))
    app.send("transcript", "Oke, kita lanjut ke agenda berikutnya.")
    time.sleep(0.5)
    check("a statement is not answered", len(requests(mock_dir)) == sent and app.state()["live"] == [])
    app.send("transcript", "Kapan versi dua dirilis?")
    app.idle()
    s = app.state()
    check("a question is answered by itself", len(s["live"]) == 1 and s["live"][0]["trigger"] == "question"
          and s["live"][0]["done"] and "root cause" in s["live"][0]["answer"], s["live"])
    check("the answer opens the Live tab", s["tab"] == "Live")
    prompt = requests(mock_dir)[-1]["body"]["messages"][-1]["content"]
    check("the live prompt carries recent lines", "- Oke, kita lanjut" in prompt and "- Kapan versi dua dirilis?" in prompt
          and "SKIP" in prompt, prompt[:200])
    app.send("transcript", "Pertanyaan rhetorical, siapa yang tahu?")
    app.idle()
    check("an answer the model skips is dropped", len(app.state()["live"]) == 1, app.state()["live"])

    app.send("action", "ToggleAutoAnswer")
    app.set_config({"live": {"answerOnName": True}})
    app.send("transcript", "Arief tadi bilang deploy pakai skrip baru.")
    app.idle()
    s = app.state()
    check("someone saying your name (misspelt) gets an answer", len(s["live"]) == 2
          and s["live"][-1]["trigger"] == "your name", s["live"])
    app.send("transcript", "Ari sudah datang belum?")
    time.sleep(0.5)
    check("a different name does not", len(app.state()["live"]) == 2)
    app.send("key", "ctrl-tab")
    check("ctrl-tab moves to the next tab", app.state()["tab"] == "Transcript", app.state()["tab"])
    app.send("key", "ctrl-tab")
    check("and wraps around", app.state()["tab"] == "Chat", app.state()["tab"])
    app.set_config({"live": {"answerOnName": False}})


def notch_mode(app):
    print("notch mode")
    app.send("action", "ToggleNotch")
    time.sleep(1.0)
    s = app.state()
    check("notch mode swaps the panel for the island", s["notch"] and s["islandVisible"] and not s["visible"], s)
    collapsed = s["islandFrame"]
    # Capturing our own window needs no permission, and macOS blanks a
    # protected one even then — so this is what a screen share sees.
    hidden = lambda out: out.startswith("error") or "coverage=0.000" in out
    check("the island is hidden from screen capture", hidden(app.send("capture-as-is", "island")))
    app.send("action", "ToggleProtection")
    time.sleep(0.4)
    check("turning invisible off shows the island to capture", not hidden(app.send("capture-as-is", "island")))
    app.send("action", "ToggleProtection")
    time.sleep(0.4)
    check("and turning it back on hides it again", hidden(app.send("capture-as-is", "island")))
    app.set_config({"live": {"autoAnswer": True}})
    app.send("transcript", "Bagaimana kalau kita tunda ke minggu depan?")
    app.idle()
    time.sleep(0.5)
    grown = app.state()["islandFrame"]
    check("a live answer drops down from the island", grown["height"] > collapsed["height"] + 100, (collapsed, grown))
    app.send("action", "ToggleOverlay")
    time.sleep(0.4)
    check("the hide hotkey hides the island", not app.state()["islandVisible"])
    app.send("action", "ToggleOverlay")
    time.sleep(0.4)
    check("and shows it again", app.state()["islandVisible"])
    app.send("action", "ToggleNotch")
    time.sleep(1.0)
    s = app.state()
    check("panel mode brings the panel back", not s["notch"] and not s["islandVisible"] and s["visible"], s)
    check("the live answer is in the Live tab", len(s["live"]) >= 1)
    app.set_config({"live": {"autoAnswer": False}})

    app.send("key", "escape")
    time.sleep(0.3)
    app.send("spurious-key")
    time.sleep(0.5)
    try:
        alive = app.state() is not None
    except Exception:
        alive = False
    check("a spurious key-window change does not hang the app", alive)
    app.send("action", "ToggleOverlay")
    time.sleep(0.3)


def pause_test(app, data):
    """Three sentences with 2.5 s pauses (and a 0.7 s breath inside the second)
    fed in as a live microphone: each should be transcribed while recording
    goes on, and the breath should not split a sentence."""
    import wave
    sentences = ["Selamat pagi semua, mari kita mulai rapatnya.", "Rilis versi dua dijadwalkan hari Jumat,",
                 "setelah pengujian selesai.", "Terakhir, anggaran pemasaran perlu disetujui minggu ini."]
    frames = []
    for n, text in enumerate(sentences):
        aiff, wav = data / f"p{n}.aiff", data / f"p{n}.wav"
        subprocess.run(["say", "-v", "Damayanti", text, "-o", str(aiff)], check=True)
        subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(wav)], check=True)
        with wave.open(str(wav)) as w:
            frames.append(w.readframes(w.getnframes()))
    gap = lambda s: b"\0\0" * int(16000 * s)
    audio = gap(0.5) + frames[0] + gap(2.5) + frames[1] + gap(0.7) + frames[2] + gap(2.5) + frames[3] + gap(1.0)
    path = data / "meeting.wav"
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(audio)

    app.set_config({"transcription": {"provider": "local", "language": "id", "chunkSeconds": 0, "pauseSeconds": 1.5}})
    app.send("action", "NewSession")
    app.send("feed", str(path), "2")
    live = 0
    deadline = time.time() + 30
    while time.time() < deadline:
        s = app.state()
        live = max(live, len(s["transcript"]) if s["recording"] else 0)
        if len(s["transcript"]) >= 3:
            break
        time.sleep(0.2)
    app.send("action", "RecordToggle")
    app.idle()
    lines = app.state()["transcript"]
    check("pauses transcribe while recording continues", live >= 2, (live, lines))
    check("a short breath does not split a sentence", len(lines) == 3, lines)
    check("lines arrive in spoken order", len(lines) == 3 and lines[0].startswith("Selamat")
          and "jumat" in lines[1].lower() and "pengujian" in lines[1].lower() and lines[2].startswith("Terakhir"), lines)


if __name__ == "__main__":
    sys.exit(main())
