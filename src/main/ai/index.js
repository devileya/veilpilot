'use strict'

const store = require('../store')
const session = require('../session')
const overlay = require('../overlay')
const openai = require('./openai')
const anthropic = require('./anthropic')

const ADAPTERS = { openai, anthropic }

let active = null
let seq = 0

function fill (template, vars) {
  return String(template || '').replace(/\{\{\s*(\w+)\s*\}\}/g, (_m, key) =>
    key in vars ? vars[key] : ''
  )
}

function buildPrompt ({ promptId, input }) {
  const cfg = store.readConfig()
  const preset = promptId ? cfg.prompts.find((p) => p.id === promptId) : null

  if (!preset) {
    return { text: String(input || '').trim(), label: 'Ask', source: 'none', effort: null }
  }

  const vars = { transcript: session.transcriptText(), input: String(input || '').trim() }
  let text = fill(preset.text, vars)

  // A preset that never references {{input}} still shouldn't throw away text
  // the user typed before firing the hotkey.
  if (vars.input && !/\{\{\s*input\s*\}\}/.test(preset.text)) {
    text += `\n\nCatatan dari saya: ${vars.input}`
  }

  return {
    text: text.trim(),
    label: preset.label || preset.id,
    source: preset.source || 'screenshot',
    effort: preset.effort || null,
    model: preset.model || null
  }
}

function fail (id, message, meta = {}) {
  overlay.send('ai:end', { id, error: message, ...meta })
  return { ok: false, error: message }
}

async function ask ({ promptId, input, shotIds = [] } = {}) {
  cancel()

  const cfg = store.readConfig()
  const provider = store.resolve('chat')
  const prompt = buildPrompt({ promptId, input })
  const id = `req_${++seq}`

  if (!prompt.text) return fail(id, 'Nothing to ask — type something or attach a screenshot.')
  if (provider.needsKey && !provider.apiKey) {
    return fail(id, `No API key for ${provider.label}. Open settings › Keys and paste one.`)
  }
  if (!provider.model) return fail(id, `No model set for ${provider.label}. Add one in Settings.`)

  const shots = shotIds.map((sid) => session.getShot(sid)).filter(Boolean)
  const notes = []
  let images = shots.map((s) => ({
    mime: s.mime,
    base64: s.buffer.toString('base64')
  }))

  if (images.length && !provider.vision) {
    notes.push(`${provider.label} (${provider.model}) has no vision support — the screenshot was not sent.`)
    images = []
  }

  session.pushTurn({ role: 'user', text: prompt.text, images })

  const adapter = ADAPTERS[provider.kind]
  const controller = new AbortController()
  active = { id, controller }

  overlay.send('ai:start', {
    id,
    label: prompt.label,
    provider: provider.label,
    model: prompt.model || provider.model,
    prompt: prompt.text,
    notes,
    shots: shots.map((s) => ({
      id: s.id, dataUrl: s.dataUrl, width: s.width, height: s.height, at: s.at
    }))
  })

  const options = {
    baseUrl: provider.baseUrl,
    apiKey: provider.apiKey,
    model: prompt.model || provider.model,
    system: cfg.chat.systemPrompt,
    messages: session.history(),
    maxTokens: cfg.chat.maxTokens || 8192,
    signal: controller.signal,
    ...(provider.kind === 'anthropic'
      ? {
          effort: prompt.effort || cfg.chat.effort || 'medium',
          showThinking: cfg.chat.showThinking !== false,
          serverSideFallback: cfg.chat.serverSideFallback !== false
        }
      : {})
  }

  let answer = ''
  try {
    for await (const chunk of adapter.stream(options)) {
      if (active?.id !== id) return { ok: false, error: 'superseded' }
      if (chunk.type === 'text') {
        answer += chunk.text
        overlay.send('ai:delta', { id, text: chunk.text })
      } else if (chunk.type === 'thinking') {
        overlay.send('ai:thinking', { id, text: chunk.text })
      } else if (chunk.type === 'note') {
        overlay.send('ai:note', { id, text: chunk.text })
      } else if (chunk.type === 'done') {
        session.pushTurn({ role: 'assistant', text: answer, images: [] })
        active = null
        overlay.send('ai:end', {
          id,
          stopReason: chunk.stopReason,
          usage: chunk.usage,
          model: chunk.model
        })
        return { ok: true, id }
      }
    }
    // Stream ended without a terminal frame (proxy cut the connection).
    session.pushTurn({ role: 'assistant', text: answer, images: [] })
    active = null
    overlay.send('ai:end', { id, stopReason: 'incomplete' })
    return { ok: true, id }
  } catch (err) {
    active = null
    if (err?.name === 'AbortError') {
      if (answer) session.pushTurn({ role: 'assistant', text: answer, images: [] })
      overlay.send('ai:end', { id, cancelled: true })
      return { ok: false, error: 'cancelled' }
    }
    console.error('[ai]', err)
    return fail(id, err?.message || String(err))
  }
}

function cancel () {
  if (!active) return false
  active.controller.abort()
  active = null
  return true
}

const isBusy = () => !!active

module.exports = { ask, cancel, isBusy }
