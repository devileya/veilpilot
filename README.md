# VeilPilot

A heads-up AI copilot for macOS and Windows that **does not appear in your screen share**.

Take a screenshot, transcribe what is being said, ask a model about either — all from
global hotkeys. The answer streams into a translucent panel that the capture pipeline
never sees.

```
┌──────────────────────────────────────────┐
│ ◪ VeilPilot        ● invisible  ○ idle  settings
├──────────────────────────────────────────┤  ← the seam glows while streaming
│ ⌘⇧2  Selesaikan      claude-opus-5 · 11:04│
│ ┌────────┐                                │
│ │ shot   │ 1600×1000 px                   │
│ └────────┘                                │
│ The failure is a null deref in            │
│ `parseConfig` — line 42 reads             │
│ `cfg.db.host` before the await resolves…  │
├──────────────────────────────────────────┤
│ transcript                       12 lines │
│ 11:04:12  so what we need here is…        │
├──────────────────────────────────────────┤
│ › ask anything                          ↵ │
└──────────────────────────────────────────┘
```

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

Requires Node 18+ (built on Node 24).

```bash
cd veilpilot
npm install        # also generates the icons
npm start
```

Package installers:

```bash
npm run dist:mac   # .dmg (arm64 + x64)
npm run dist:win   # NSIS .exe
```

Unsigned builds will warn on first launch. For real distribution, sign and notarize:
set `CSC_LINK` / `CSC_KEY_PASSWORD` and `APPLE_ID` / `APPLE_APP_SPECIFIC_PASSWORD` /
`APPLE_TEAM_ID` before `npm run dist:mac`.

### Permissions

- **macOS, Screen Recording** — required for screenshots. First capture triggers the
  prompt; if you miss it, Settings › Capture has a button to the right pane in System
  Settings. You must quit and relaunch the app after granting it.
- **macOS, Microphone** — required for transcription, requested on first recording.
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
| `⌘⇧\` | Show / hide the panel |
| `⌘⇧↩` | Focus the ask box |
| `⌘⇧.` | Stop generating |
| `⌘⇧K` | New session (clears conversation and transcript) |
| `⌘⇧M` | Click-through — the panel ignores the mouse entirely |
| `⌘⇧P` | Toggle capture protection |
| `⌘⇧=` / `⌘⇧-` | Opacity up / down |
| `⌥⇧` + arrows | Move the panel |

A hotkey another app already owns cannot be registered. VeilPilot tells you which ones
failed on launch instead of leaving a dead key.

Hotkeys that fire a prompt show the panel **without taking focus**, so your meeting app
keeps the keyboard. Only `⌘⇧\` and `⌘⇧↩` focus the panel.

---

## Providers

Two request shapes cover everything, so adding a provider is configuration, not code:

- **OpenAI-compatible** (`POST /chat/completions`) — OpenAI, GLM/Zhipu, Groq, OpenRouter,
  DeepSeek, Ollama, LM Studio, vLLM, or any custom base URL
- **Anthropic** — the official `@anthropic-ai/sdk`, with adaptive thinking, an effort
  setting, and server-side fallback on a declined request

Presets ship with a base URL and a starting model; both are editable, because model IDs
change faster than this README. Add your key in Settings › Keys. Keys are encrypted with
the OS keystore (Keychain / DPAPI) and stored separately from `config.json` —
`safeStorage`, never plaintext in the config.

Set the **effort** on a per-prompt basis: `Selesaikan` and `Review kode` ship at `high`
because they are real problems; the rest run at the global default (`medium`) so a
"what is this" answer comes back fast.

### Transcription is a separate provider

Anthropic has no speech-to-text endpoint. "Chat on Claude, transcribe on Groq" is a normal
setup, not a misconfiguration — Settings › Voice has its own provider, model and key.
Any OpenAI-compatible `POST /audio/transcriptions` works. Groq's
`whisper-large-v3-turbo` is the cheap, fast default worth trying.

**Chunk length** decides the trade: `0` uploads once when you stop recording and is the
most accurate; `15`–`30` gives a rolling transcript during a long call. Each chunk is a
standalone recording, not a slice, because slices of one recording are not independently
decodable and the API rejects them.

### System audio (hearing the other side)

- **Windows** — desktop loopback is mixed in when you tick *System audio*. If it fails,
  the app falls back to the microphone and tells you.
- **macOS** — Chromium gets no system-audio tap. Install a loopback driver
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
src/main/        Electron main process
  index.js         lifecycle, tray, the actions hotkeys fire
  overlay.js       the window: content protection, always-on-top, geometry
  capture.js       desktopCapturer + the region picker
  session.js       conversation, screenshots, transcript
  hotkeys.js       global shortcut registration and failure reporting
  transcribe.js    audio upload to an OpenAI-compatible speech endpoint
  store.js         config.json + OS-keystore secrets
  ipc.js           every renderer-facing handler
  ai/              provider adapters (openai.js, anthropic.js) + dispatch
src/renderer/    the panel: app.js, settings.js, recorder.js, markdown.js
src/region/      the drag-to-select overlay
build/           make-icons.js writes the tray and app icons at install time
```

Config and keys live in Electron's `userData` directory — Settings › Capture prints the
exact path.

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
