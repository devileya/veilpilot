'use strict'

const store = require('./store')
const whisperCpp = require('./whisper-cpp')
const { PROVIDERS } = require('./defaults')

const MAX_BYTES = 25 * 1024 * 1024 // OpenAI's documented ceiling; others are looser

const EXTENSIONS = {
  'audio/webm': 'webm',
  'audio/ogg': 'ogg',
  'audio/mp4': 'mp4',
  'audio/mpeg': 'mp3',
  'audio/wav': 'wav',
  'audio/x-wav': 'wav'
}

function extensionFor (mime) {
  const base = String(mime || '').split(';')[0].trim()
  return EXTENSIONS[base] || 'webm'
}

// Speech-to-text is a separate provider from chat on purpose: Anthropic has no
// transcription endpoint, so "chat on Claude, transcribe on Groq" is a normal
// configuration rather than an error.
function readiness () {
  const cfg = store.readConfig().transcription
  const preset = PROVIDERS[cfg.provider]
  const resolved = store.resolve('transcription')

  // On-device: nothing to authenticate, but the binary and a model have to
  // actually be there.
  if (preset && preset.kind === 'whispercpp') {
    const state = whisperCpp.detectSync(cfg.whisperCpp || {})
    return state.ok
      ? { ok: true, provider: preset.label, model: state.modelName, local: true }
      : { ok: false, reason: state.error, local: true }
  }

  if (preset && preset.transcription === false) {
    return {
      ok: false,
      reason: `${preset.label} has no speech-to-text endpoint. Pick OpenAI, Groq, or a custom OpenAI-compatible server for transcription.`
    }
  }
  if (!resolved.model) {
    return { ok: false, reason: `No transcription model set for ${resolved.label}.` }
  }
  if (resolved.needsKey && !resolved.apiKey) {
    return { ok: false, reason: `No API key for ${resolved.label}.` }
  }
  return { ok: true, provider: resolved.label, model: resolved.model }
}

async function transcribe ({ bytes, mime }) {
  const ready = readiness()
  if (!ready.ok) throw new Error(ready.reason)

  const buffer = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes)
  if (!buffer.length) throw new Error('Recording was empty.')

  const cfg = store.readConfig().transcription
  if (PROVIDERS[cfg.provider]?.kind === 'whispercpp') {
    // No upload, so no size ceiling — the only limit is patience.
    return whisperCpp.transcribe({
      bytes: buffer,
      cfg: cfg.whisperCpp || {},
      language: cfg.language
    })
  }

  if (buffer.length > MAX_BYTES) {
    throw new Error(`Recording is ${(buffer.length / 1048576).toFixed(1)} MB — over the 25 MB upload limit. Turn on chunked transcription in Settings.`)
  }

  const provider = store.resolve('transcription')

  const form = new FormData()
  form.append('file', new Blob([buffer], { type: mime }), `speech.${extensionFor(mime)}`)
  form.append('model', provider.model)
  form.append('response_format', 'json')
  if (cfg.language) form.append('language', cfg.language)

  const headers = {}
  if (provider.apiKey) headers.authorization = `Bearer ${provider.apiKey}`

  const response = await fetch(`${provider.baseUrl}/audio/transcriptions`, {
    method: 'POST',
    headers, // no content-type: fetch must set the multipart boundary itself
    body: form
  })

  const raw = await response.text()
  if (!response.ok) {
    let detail = raw.slice(0, 300)
    try {
      const parsed = JSON.parse(raw)
      detail = parsed?.error?.message || parsed?.message || detail
    } catch { /* keep raw */ }
    throw new Error(`Transcription failed (${response.status}): ${detail}`)
  }

  try {
    const parsed = JSON.parse(raw)
    return String(parsed.text ?? parsed.transcript ?? '').trim()
  } catch {
    return raw.trim() // response_format=text servers
  }
}

module.exports = { transcribe, readiness }
