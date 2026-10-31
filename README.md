# VeilPilot

A heads-up AI copilot for macOS and Windows that **does not appear in your screen share**.

Take a screenshot, transcribe what is being said, ask a model about either — all from
global hotkeys. The answer streams into a translucent panel that the capture pipeline
never sees.

A native Rust app built on [GPUI](https://gpui.rs), the GPU-accelerated UI framework
behind the Zed editor. No Electron, no web view: the macOS app is a single 16 MB binary.

```
┌──────────────────────────────────────────┐
│ ◪ VeilPilot      ◆ Shop ▾  ✦ Claude Code ▾ ⚙│  ← context (DevCat project) and agent
│ ● invisible  ○ rec  ⚡ auto  @ Arif  ◒ notch│  ← modes, one click each
├──────────────────────────────────────────┤
│ CHAT   LIVE 2   TRANSCRIPT +3             │
├──────────────────────────────────────────┤  ← the seam glows while streaming
│ ⌘⇧2  SELESAIKAN      claude-opus-5 · 11:04│
│ ┌────────┐                                │
│ │ shot   │ 1600×1000 px                   │
│ └────────┘                                │
│ The failure is a null deref in            │
│ `parseConfig` — line 42 reads             │
│ `cfg.db.host` before the await resolves…  │
├──────────────────────────────────────────┤
│ ⌘⇧1 Jelaskan  ⌘⇧2 Selesaikan  ⌘⇧3 Review  ?│  ← the hotkeys stay on screen
│ ⌘⇧R Rec  ⌘⇧A Region  ⌘⇧Y Summary  ⌘⇧\ Hide │
├──────────────────────────────────────────┤
│ › ask anything                          ↵ │
└──────────────────────────────────────────┘
```

Three tabs: **Chat** (your asks), **Live** (answers VeilPilot gave by itself while
listening), and **Transcript** (with Summarize and Save to memory). The strip of hotkeys
under them never goes away, and `?` opens every key at once. In **notch mode** the panel
gives way to a black island around the MacBook notch that drops answers down and folds
back up.

---

## How the invisibility works

One call does it, and it maps to a different OS feature on each platform:

| Platform | Underlying API | Result in a screen share |
|---|---|---|
| macOS | `NSWindow.sharingType = .none` | **Gone.** Viewers see whatever is behind the panel |
| Windows 10 2004+ (build 19041) | `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` | **Gone**, same as macOS |
| Windows older than 2004 | falls back to `WDA_MONITOR` | A **black rectangle** where the panel is |

This is a documented OS capability, not a trick — teleprompters, password managers and
presenter-notes tools use the same flag. It is applied by the window compositor, so it
covers every capture path meeting apps use: ScreenCaptureKit and `CGWindowList` on macOS,
Windows Graphics Capture and DXGI Desktop Duplication on Windows. Zoom, Google Meet,
Teams, Discord, OBS and Loom all go through those.

The app also hides itself from the places a screen share would otherwise expose it: no
Dock icon, no taskbar entry, no macOS menu bar (it runs as an accessory app), and it is
excluded from Mission Control.

### Where it still leaks — read this part

Capture protection stops at the compositor. These are outside it:

- **Capture cards, HDMI splitters, KVMs** — they read the physical framebuffer
- **Display mirroring, AirPlay, Sidecar** — framebuffer again
- **A camera pointed at your screen**
- **A second monitor someone else can see** — the panel is genuinely on your screen
- **Window and process enumeration** — the window still exists in the OS window list with
  its title and process name. `EnumWindows` / `CGWindowListCopyWindowInfo`, and anything
  that merely lists running processes, sees VeilPilot. **Not captured is not the same as
  not detected.** Proctoring and monitoring software works at this level, so this app is
  not an answer to it.

Press the protection hotkey (`⌘⇧P` / `Ctrl+Shift+P`) any time to flip protection off and
verify with a test recording that it is actually doing something. The rail turns red and
reads `visible` when it is off.

---

## Install and run

Requires a Rust toolchain ([rustup](https://rustup.rs), Rust 1.89+). On macOS, Xcode (or
its command-line tools) for the Metal shader compiler; on Windows, Visual Studio's C++
build tools.

```bash
cd veilpilot
cargo run --release
```

Package it:

```bash
scripts/bundle-macos.sh          # target/release/bundle/VeilPilot.app
scripts/bundle-macos.sh --dmg    # ...plus a .dmg
cargo build --release            # Windows: target\release\veilpilot.exe
```

The bundle is signed ad-hoc unless `CODESIGN_IDENTITY` names a Developer ID certificate.
An ad-hoc signature changes with every build, so macOS asks for Screen Recording and
Microphone again after a rebuild; a real identity keeps the grants. Notarize with
`xcrun notarytool` for distribution.

### Permissions

- **macOS, Screen Recording** — required for screenshots. First capture triggers the
  prompt; if you miss it, Settings › Capture has a button to the right pane in System
  Settings. You must quit and relaunch the app after granting it.
- **macOS, Microphone** — required for transcription, requested on first recording.
- **whisper.cpp** — `brew install whisper-cpp` for on-device speech to text (the default).
- **Windows** — no prompts; screenshots and the microphone work directly.

---

## Hotkeys

Defaults. `⌘` is `Ctrl` on Windows. Every one of these is editable in Settings › Keys,
and prompt hotkeys are editable in Settings › Prompts.

| Key | What it does |
|---|---|
| `⌘⇧1` | Screenshot → **Jelaskan** (explain what is on screen) |
| `⌘⇧2` | Screenshot → **Selesaikan** (find the root cause, give concrete steps) |
| `⌘⇧3` | Screenshot → **Review kode** |
| `⌘⇧4` | Screenshot → **Jawab** (answer the question on screen) |
| `⌘⇧5` | Screenshot → **Terjemahkan** |
| `⌘⇧6` | Transcript → **Ringkas obrolan** |
| `⌘⇧7` | Transcript → **Bantu jawab** (draft a reply you can say now) |
| `⌘⇧A` | Drag-select a region, then ask |
| `⌘⇧S` | Attach a screenshot without asking — type your own question |
| `⌘⇧R` | Start / stop transcribing |
| `⌘⇧Y` | Summarize the transcript |
| `⌘⇧U` | Save the summary and transcript to the picked project's memory |
| `⌘⇧L` | Auto-answer on / off (answer questions heard in the transcript) |
| `⌘⇧N` | Notch mode on / off |
| `⌘⇧\` | Show / hide the panel (the island, in notch mode) |
| `⌘⇧↩` | Focus the ask box |
| `⌘⇧.` | Stop generating |
| `⌘⇧K` | New session (clears conversation and transcript) |
| `⌘⇧M` | Click-through — the panel ignores the mouse entirely |
| `⌘⇧P` | Toggle capture protection |
| `⌘⇧=` / `⌘⇧-` | Opacity up / down |
| `⌥⇧` + arrows | Move the panel |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab (panel focused) |

A hotkey another app already owns cannot be registered. VeilPilot tells you which ones
failed on launch instead of leaving a dead key.

Hotkeys that fire a prompt show the panel **without taking focus**, so your meeting app
keeps the keyboard. Only `⌘⇧\` and `⌘⇧↩` focus the panel.

---

## Providers

Three request shapes cover everything, so adding a provider is configuration, not code:

- **OpenAI-compatible** (`POST /chat/completions`) — OpenAI, GLM/Zhipu, Groq, OpenRouter,
  DeepSeek, Ollama, LM Studio, vLLM, or any custom base URL
- **Anthropic** — the Messages API over HTTPS + SSE (there is no official Rust SDK), with
  adaptive thinking, an effort setting, and server-side fallback on a declined request
- **Claude Code** — the `claude` CLI you already have, in headless mode. See below.
- **Other agents on this Mac** — Codex, Gemini CLI, opencode, GitHub Copilot CLI, Aider,
  or any command you name. See [Other agents](#other-agents).
- **DevCat Agent connections** — the model connections you set up in DevCat, with the key
  DevCat keeps.

Pick one from the **✦ agent menu** at the top of the panel; anything that cannot answer
yet says why (no key, not installed). Each provider keeps its own model and base URL, so
switching back and forth loses nothing.

Presets ship with a base URL and a starting model; both are editable, because model IDs
change faster than this README. Add your key in Settings › Keys. Keys live in the OS
keystore (macOS Keychain / Windows Credential Manager), never in `config.json`. Only if no
keystore is available are they written to `secrets.bin` beside the config, and the app
says so in its log.

Set the **effort** on a per-prompt basis: `Selesaikan` and `Review kode` ship at `high`
because they are real problems; the rest run at the global default (`medium`) so a
"what is this" answer comes back fast.

### Claude Code as a provider

Pick **Claude Code** in Settings › AI and there is no key to paste — it runs the `claude`
CLI you already have and uses the login that CLI already holds. VeilPilot stores no
credential of its own for it.

How it runs, and why:

```
claude -p
  --input-format stream-json --output-format stream-json --verbose
  --include-partial-messages          # token-by-token streaming
  --tools ""                          # answers only; never touches your disk
  --permission-prompts none           # nobody is there to approve, so never hang
  --safe-mode --strict-mcp-config     # skip hooks, MCP, plugins, CLAUDE.md
  --system-prompt <your HUD prompt>   # replaces Claude Code's coding prompt
  --session-id <uuid>  /  --resume <uuid>
```

- **Screenshots go straight in.** `--input-format stream-json` takes base64 image blocks,
  so a screenshot needs no temp file and no `Read` tool. Confirmed working.
- **Follow-ups keep context.** The first ask names a session, every later one resumes it,
  so only the newest turn is sent and Claude Code holds the history. A new session
  (`⌘⇧K`) starts a fresh one.
- **Fast start matters.** Measured on this machine: **~11s** to first token with hooks,
  MCP servers and plugins loading, **~1.5s** with `--safe-mode --strict-mcp-config`. It is
  on by default. Turn it off only if you need your MCP servers.
- **First ask of a session is slower** (~7s) because the CLI is a large bundle and the
  process is cold; subsequent asks in the same session run ~1.5s. VeilPilot runs
  `claude --version` at startup to warm the page cache, which costs nothing.
- **Real cost is reported.** Every answer's eyebrow shows the actual dollar figure the CLI
  returns. Settings has a per-ask spend cap.
- **Tools are off by default.** Turning them on in Settings makes "fix this" far stronger
  but slower, and anything that would need permission is denied automatically.

Settings › AI reports what it found: the version, the path, and the account you are
signed in as (via `claude auth status`). An installed-but-logged-out CLI is caught there
with a fix, rather than failing confusingly on your first ask mid-meeting.

Two things to know. Resuming requires session persistence, so your asks are written to
Claude Code's own session store under `~/.claude` like any other session — including
whatever was on screen. And a GUI app does not inherit your shell `PATH`, so VeilPilot
probes the usual install locations (`~/.local/bin`, `~/.claude/bin`, `~/.claude/local`,
Homebrew, npm global); if yours lives somewhere else, name it in Settings › AI.

### Other agents

Every agent runs headless, one question per run, with the login it already has — VeilPilot
stores no credential for any of them. With a project picked, the agent runs in the
project's code folder; with no context, in an empty temp folder.

| Agent | How it is run |
|---|---|
| Codex | `codex exec --json --sandbox read-only --skip-git-repo-check [--image shot.png] -` (prompt on stdin) |
| Gemini CLI | `gemini --prompt "… @shot.png"` |
| opencode | `opencode run --format json [--file shot.png] -- "…"` (reports cost) |
| Copilot CLI | `copilot --prompt "…"` |
| Aider | `aider --message "…" --no-git --yes-always --no-auto-commits …` |
| Custom | your command; `{prompt}` is replaced by the prompt, else it is sent on stdin |

Screenshots are handed over as temporary PNG files (deleted after the run) to the agents
that can see images. As with Claude Code, a GUI app does not inherit your shell `PATH`:
VeilPilot asks your login shell and the usual install folders, and Settings › AI takes an
explicit path when that is not enough. A failed run shows the agent's own error line.

## Project context and memory (DevCat)

VeilPilot reads DevCat's projects and its shared agent memory. The
**◇ context menu** at the top of the panel picks one:

- **No context** (the default) — the agent knows only this chat, and nothing is saved.
- **A DevCat project** — its memories (newest first, within a budget) are added to the
  system prompt, agents run in the project's folder, and **Save to memory** is allowed.
  Settings › AI can keep the project but leave its memory out.

**Summarize** (`⌘⇧Y`, or the button on the Transcript tab) asks the agent for a summary of
the whole transcript with the prompt in Settings › Voice. **Save to memory** (`⌘⇧U`)
stores that summary plus the transcript as one memory entry in the picked project —
summarizing first if the transcript changed since. It goes through `devcat mcp`, DevCat's
own MCP server, so secrets are scrubbed and the change shows up in DevCat's MCP activity
log; if DevCat is not installed, the entry is written in the same format DevCat writes.
Without a project nothing is saved.

## Live answers

While transcribing, VeilPilot can answer by itself — for a question you might be asked
right now, before you have to ask for help:

- **⚡ Auto-answer** (`⌘⇧L`) — when a transcript line reads as a question (Indonesian or
  English: a question mark, a question word, or "menurut kamu…" style asks).
- **@ Your name** — when someone says your name. Set it in Settings › Voice (comma
  separated, nicknames welcome); speech to text misspells names, so one letter changed or
  added still counts ("Arief" for "Arif"), but a shorter word does not ("Ari").

The agent sees the last lines of the transcript (20 by default) and either writes what you
can say, in at most three sentences, or replies `SKIP` when the line needs nothing from you
— those are dropped silently. Answers appear on the **Live** tab, apart from your own asks;
the panel switches to it unless you are halfway through typing.

## Notch mode

`⌘⇧N`, the ◒ chip, or Settings › Capture › Display. The panel is replaced by a black island
that hugs the MacBook notch (on a display without one, and on Windows, a pill at the top
of the screen). Its wings show recording, ⚡ / @ modes, unseen answers and protection. A new
answer — live or asked — drops down from it and folds back 12 seconds after it finishes;
hovering keeps it open, a click opens the full chat. `⌘⇧\` hides and shows the island.
It is capture-protected like the panel.

### Speech to text runs on your machine by default

The default transcription engine is **whisper.cpp, on-device**. Meeting audio is never
uploaded anywhere, which is the same promise the invisible panel makes about your screen.
There is no API key and no per-minute cost.

```bash
brew install whisper-cpp     # macOS; Windows builds at github.com/ggml-org/whisper.cpp
```

Then open Settings › Voice and download a model — the panel lists tiny/base/small/medium
with sizes and a download button, and stores them beside your config. **small** (488 MB)
is the right default for anything other than English; `base` is faster but noticeably
weaker on Indonesian.

Measured here on an M-series Mac with `small`, on a 5-second clip:

| | time |
|---|---|
| First pass after launch (model cold on disk) | ~10 s |
| Warm, language auto-detected | ~1.6 s |
| Warm, language set explicitly (`id`) | ~0.8 s |

Auto-detect is accurate on clear speech — it picked Indonesian at 0.98 confidence in
testing — so setting the language in Settings › Voice is about speed, not correctness: it
skips the detection pass. VeilPilot reads the model file through once at startup to take
the cold first pass out of your first chunk.

whisper.cpp only accepts 16 kHz mono PCM, so VeilPilot records raw samples
(CoreAudio / WASAPI) and resamples them itself with a windowed-sinc filter before handing
them over. That keeps ffmpeg off the dependency list — one less thing to install,
especially on Windows.

### Cloud transcription, if you want it

Settings › Voice can also point at any OpenAI-compatible `POST /audio/transcriptions` —
OpenAI, Groq (`whisper-large-v3-turbo` is cheap and fast), or your own server. Note that
Anthropic has no speech-to-text endpoint, so "chat on Claude, transcribe locally" (or on
Groq) is a normal setup, not a misconfiguration.

**Pause to transcribe** (default 1.5 s): when the speaker goes quiet that long, what they
said is transcribed right away while recording carries on, so the transcript fills in
sentence by sentence during the call. A short breath mid-sentence does not cut it. Silence
is never sent to the transcriber; Whisper tends to invent words for it.

**Longest chunk** caps a segment for someone who never pauses (0 means one minute when
pause cuts are on). With both at `0`, it transcribes once when you stop, which gives
Whisper the most context but shows nothing until then and, for cloud transcription, is
limited to the 25 MB upload (about 13 minutes). Segments are cut from one continuous
buffer and transcribed in order, so no audio is lost at the seams.

### System audio (hearing the other side)

- **Windows** — desktop loopback (WASAPI) is mixed in when you tick *System audio*. If it
  fails, the app falls back to the microphone and tells you.
- **macOS** — apps get no system-audio tap. Install a loopback driver
  ([BlackHole](https://existential.audio/blackhole/) is free), route your meeting output
  through it, and pick it as the microphone in Settings › Voice.

---

## Custom prompts

Settings › Prompts. Each preset is a hotkey plus a prompt, and carries:

- **source** — `screenshot`, `transcript`, or `both`; decides what is captured and attached
- **text** — supports `{{transcript}}` and `{{input}}`; anything typed in the ask box is
  appended if the template does not reference `{{input}}`
- **effort** and **model** overrides, per prompt

Add as many as you have free key combinations.

---

## Project layout

```
src/
  main.rs            startup: single instance, the panel window, hotkeys/tray/control wiring
  config.rs          config.json: defaults, provider presets, merge, migrations
  secrets.rs         API keys in the OS keystore
  session.rs         conversation, screenshots, transcript
  hotkeys.rs         Electron-style accelerators -> global shortcuts, failure reporting
  tray.rs            menu-bar / notification-area icon and menu
  capture.rs         screenshots: pick a display, grab, crop, downscale, encode
  audio.rs           microphone (+ WASAPI loopback) capture, resampling, WAV
  transcribe.rs      routes audio to the local engine or a cloud endpoint
  whisper.rs         on-device speech to text: detect, model download, run
  live.rs            live answers: question and name detection, the live prompt
  devcat.rs          DevCat projects, memory (read, brief, save via `devcat mcp`), connections
  ai/                provider adapters + streaming with cancellation
    openai.rs          OpenAI-compatible /chat/completions
    anthropic.rs       Anthropic Messages API (SSE)
    claude_code.rs     the local `claude` CLI, headless stream-json
    cli.rs             Codex, Gemini CLI, opencode, Copilot, Aider, custom commands
  platform/          native window and OS calls GPUI does not expose
    macos.rs           NSWindow sharing type, level, click-through, permissions
    windows.rs         SetWindowDisplayAffinity, tool window, DPI-aware geometry
  ui/                GPUI views
    panel/             the panel and every action
      mod.rs             state, asks, hotkey actions, transcript, summary
      view.rs            header, menus, tabs, hotkey strip, cheatsheet, ask box
      agents.rs          the agent and context menus, system prompt, agent options
      live.rs            live answers, summarize, save to memory
    island.rs          notch mode: the island window
    settings.rs        Settings › AI / Voice / Capture / Prompts / Keys
    region.rs          the drag-to-select overlay
    text_input.rs      text field (GPUI ships none): selection, IME, clipboard, wrapping
    markdown.rs        the answer renderer
  dev.rs             the dev control channel the end-to-end tests drive
scripts/bundle-macos.sh   builds VeilPilot.app / .dmg
tests/e2e.py              end-to-end tests against tests/mock_llm.py
```

Config, models and logs live in `~/Library/Application Support/VeilPilot` on macOS and
`%APPDATA%\VeilPilot` on Windows — the same place the Electron build used, so an existing
install keeps its settings and downloaded models. Settings › Capture prints the exact path.

Answers are drawn by GPUI rather than a browser, so they are not text-selectable; every
answer has a **copy** button that puts its plain text on the clipboard.

---

## Testing

```bash
cargo test                       # unit tests: config, SSE, adapters, markdown, audio, ...
cargo build && python3 tests/e2e.py              # the real app, end to end
python3 tests/e2e.py --release --claude-code     # also a few tiny real Claude Code asks
```

`tests/e2e.py` launches the app in a throwaway data directory with `VEILPILOT_DEV=1`,
which opens a local control channel. The script drives the app the way a user would —
hotkey actions (delivered through the real Carbon hotkey handler), keystrokes, clicks —
against `tests/mock_llm.py`, and checks both the app's state and what it sent over the
wire: streaming, cancel, history, screenshots and region crops, transcript prompts,
Anthropic fallback and refusal handling, window toggles, settings edits, hotkey capture,
API-key storage, DevCat context and memory saves (through the real `devcat mcp` when
installed, against a throwaway DevCat folder), agent CLIs (a custom command and a stand-in
Codex), live answers and SKIP, summary, notch mode, and speech to text through
whisper.cpp. Without `VEILPILOT_DEV=1` the
channel only accepts `show`, which is how a second launch brings the running panel back.

---

## A note on use

The legitimate cases are the reason the OS exposes this flag: presenter notes and
teleprompting, a private AI copilot during a sales call or a demo, keeping a password
manager out of a recording, accessibility aids. Proctored exams and monitored interviews
are the case it does not serve — as the leak list above explains, those systems enumerate
processes rather than inspect pixels, so they see this app regardless of what the screen
share shows.

## License

MIT
