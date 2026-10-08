'use strict'

const fs = require('node:fs')
const fsp = require('node:fs/promises')
const os = require('node:os')
const path = require('node:path')
const { Readable } = require('node:stream')
const { pipeline } = require('node:stream/promises')
const { randomUUID } = require('node:crypto')
const { execFile } = require('node:child_process')
const { app } = require('electron')

/* On-device speech to text. The renderer hands over 16 kHz mono WAV bytes,
   this writes them to a temp file and runs the whisper.cpp CLI over it, so
   meeting audio never leaves the machine. */

const MODELS = {
  'ggml-tiny.bin': { label: 'tiny · 78 MB', bytes: 77_691_713 },
  'ggml-base.bin': { label: 'base · 148 MB', bytes: 147_951_465 },
  'ggml-small.bin': { label: 'small · 488 MB', bytes: 487_601_967 },
  'ggml-medium.bin': { label: 'medium · 1.5 GB', bytes: 1_533_763_059 }
}

const MODEL_BASE_URL = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/'

const BINARY_NAMES = process.platform === 'win32'
  ? ['whisper-cli.exe', 'whisper-cpp.exe', 'main.exe']
  : ['whisper-cli', 'whisper-cpp', 'whisper']

const modelDir = () => path.join(app.getPath('userData'), 'models')

function executable (file) {
  try {
    fs.accessSync(file, fs.constants.X_OK)
    return fs.statSync(file).isFile()
  } catch {
    return false
  }
}

function extraBinaryDirs () {
  if (process.platform === 'win32') {
    return [
      path.join(process.env.LOCALAPPDATA || '', 'Programs', 'whisper.cpp'),
      'C:\\Program Files\\whisper.cpp'
    ]
  }
  return ['/opt/homebrew/bin', '/usr/local/bin', '/usr/bin']
}

// Same PATH problem as the Claude Code CLI: a GUI app on macOS does not see
// Homebrew's bin directory, so the usual locations are probed too.
function resolveBinary (configured) {
  if (configured) {
    if (executable(configured)) return configured
    throw new Error(`whisper.cpp not found at ${configured}. Fix the path in settings › Voice.`)
  }
  const dirs = [...(process.env.PATH || '').split(path.delimiter), ...extraBinaryDirs()]
  for (const dir of dirs) {
    if (!dir) continue
    for (const name of BINARY_NAMES) {
      const file = path.join(dir, name)
      if (executable(file)) return file
    }
  }
  throw new Error('whisper.cpp is not installed. On macOS: brew install whisper-cpp. Or set the binary path in settings › Voice.')
}

function modelCandidates () {
  const dirs = [modelDir(), path.join(os.homedir(), '.cache', 'whisper.cpp')]
  if (process.platform !== 'win32') dirs.push('/opt/homebrew/share/whisper-cpp')
  const found = []
  for (const dir of dirs) {
    let entries = []
    try {
      entries = fs.readdirSync(dir)
    } catch {
      continue
    }
    for (const entry of entries) {
      if (entry.startsWith('ggml-') && entry.endsWith('.bin')) found.push(path.join(dir, entry))
    }
  }
  return found
}

function resolveModel (configured) {
  if (configured) {
    if (fs.existsSync(configured)) return configured
    throw new Error(`Model file not found at ${configured}.`)
  }
  const found = modelCandidates()
  if (!found.length) {
    throw new Error('No Whisper model found. Download one in settings › Voice.')
  }
  // Bigger file means a bigger model, which means better transcription.
  return found.sort((a, b) => fs.statSync(b).size - fs.statSync(a).size)[0]
}

// Every probe here is a filesystem stat, so this stays synchronous and can be
// called straight from the bootstrap payload.
function detectSync (cfg = {}) {
  let binary
  try {
    binary = resolveBinary(cfg.binaryPath)
  } catch (err) {
    return { ok: false, error: err.message, models: listModels() }
  }

  let model = null
  let modelError = null
  try {
    model = resolveModel(cfg.modelPath)
  } catch (err) {
    modelError = err.message
  }

  return {
    ok: !!model,
    binaryPath: binary,
    modelPath: model,
    modelName: model ? path.basename(model) : null,
    error: modelError,
    models: listModels()
  }
}

function listModels () {
  const available = modelCandidates()
  return Object.entries(MODELS).map(([file, meta]) => {
    const installed = available.find((p) => path.basename(p) === file)
    return { file, label: meta.label, installed: installed || null }
  })
}

/* ---------- transcription ---------- */

const run = (binary, args) => new Promise((resolve) => {
  execFile(binary, args, { timeout: 600_000, maxBuffer: 16 * 1024 * 1024 },
    (err, stdout, stderr) => resolve({ err, stdout: String(stdout || ''), stderr: String(stderr || '') }))
})

// whisper.cpp prints the transcript to stdout; -np drops the banner and
// -nt drops the [00:00.000 --> ...] prefixes, leaving just the words.
function buildArgs ({ model, wav, language, translate, threads }) {
  const args = ['-m', model, '-f', wav, '--no-timestamps', '--no-prints']
  if (language) args.push('-l', language)
  else args.push('-l', 'auto')
  if (translate) args.push('--translate')
  if (threads > 0) args.push('-t', String(threads))
  return args
}

// Diagnostics normally go to stderr, but the exact split has moved between
// whisper.cpp releases, so known noise is dropped from stdout as well.
const NOISE = /^(read_audio_data|whisper_\w+|ggml_\w+|main|system_info|error|warning|usage):/

function clean (stdout) {
  return stdout
    .split('\n')
    .map((line) => line.trim())
    // Bracketed and parenthesised markers are Whisper's non-speech labels
    // ([BLANK_AUDIO], (music)) — not something anyone said.
    .filter((line) => line && !NOISE.test(line) &&
      !/^\[[^\]]*\]$/.test(line) && !/^\([^)]*\)$/.test(line))
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim()
}

// The model is re-read on every invocation, so a cold page cache costs ~10s
// against ~1s warm. Reading the file through once at startup removes that
// from the first chunk, and costs no CPU.
function warm (cfg = {}) {
  return new Promise((resolve) => {
    let model
    try {
      model = resolveModel(cfg.modelPath)
    } catch {
      return resolve(false)
    }
    const reader = fs.createReadStream(model, { highWaterMark: 4 * 1024 * 1024 })
    reader.on('data', () => {})
    reader.on('error', () => resolve(false))
    reader.on('close', () => resolve(true))
  })
}

async function transcribe ({ bytes, cfg = {}, language = '' }) {
  const binary = resolveBinary(cfg.binaryPath)
  const model = resolveModel(cfg.modelPath)

  const wav = path.join(os.tmpdir(), `veilpilot-${randomUUID()}.wav`)
  await fsp.writeFile(wav, bytes)

  try {
    const { err, stdout, stderr } = await run(binary, buildArgs({
      model,
      wav,
      language,
      translate: !!cfg.translate,
      threads: Number(cfg.threads) || 0
    }))

    if (err) {
      const detail = (stderr || err.message).trim().split('\n').slice(-3).join(' ')
      throw new Error(`whisper.cpp failed: ${detail}`)
    }

    return clean(stdout)
  } finally {
    fsp.unlink(wav).catch(() => {})
  }
}

/* ---------- model download ---------- */

let downloading = null

async function downloadModel (file, onProgress) {
  if (!MODELS[file]) throw new Error(`Unknown model ${file}`)
  if (downloading) throw new Error('A model download is already running.')

  const target = path.join(modelDir(), file)
  const partial = `${target}.part`
  downloading = file

  try {
    await fsp.mkdir(modelDir(), { recursive: true })
    const response = await fetch(MODEL_BASE_URL + file, { redirect: 'follow' })
    if (!response.ok || !response.body) {
      throw new Error(`Download failed: ${response.status} ${response.statusText}`)
    }

    const total = Number(response.headers.get('content-length')) || MODELS[file].bytes
    let received = 0
    let lastReport = 0

    const counter = new TransformStream({
      transform (chunk, controller) {
        received += chunk.byteLength
        const percent = Math.floor((received / total) * 100)
        if (percent > lastReport) {
          lastReport = percent
          onProgress?.({ file, percent, received, total })
        }
        controller.enqueue(chunk)
      }
    })

    // Written to .part first so an interrupted download is never mistaken
    // for an installed model.
    await pipeline(
      Readable.fromWeb(response.body.pipeThrough(counter)),
      fs.createWriteStream(partial)
    )
    await fsp.rename(partial, target)
    return { ok: true, path: target }
  } catch (err) {
    await fsp.unlink(partial).catch(() => {})
    throw err
  } finally {
    downloading = null
  }
}

module.exports = {
  detect: detectSync,
  detectSync,
  transcribe,
  warm,
  downloadModel,
  listModels,
  modelDir,
  resolveBinary,
  resolveModel,
  MODELS
}
