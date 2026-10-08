'use strict'

const $ = (id) => document.getElementById(id)

const el = {
  shell: $('shell'),
  veilState: $('veilState'),
  recState: $('recState'),
  gear: $('gear'),
  readout: $('readout'),
  legend: $('legend'),
  legendKeys: $('legendKeys'),
  legendFoot: $('legendFoot'),
  cards: $('cards'),
  drawer: $('drawer'),
  drawerHead: $('drawerHead'),
  drawerCount: $('drawerCount'),
  transcript: $('transcript'),
  shots: $('shots'),
  input: $('input'),
  grip: $('grip'),
  toasts: $('toasts')
}

let state = null
let card = null
let attachments = []
let transcriptCount = 0
let repaintQueued = false

/* ---------- formatting ---------- */

const MAC_GLYPHS = {
  Command: '⌘', CommandOrControl: '⌘', Cmd: '⌘',
  Control: '⌃', Ctrl: '⌃',
  Alt: '⌥', Option: '⌥',
  Shift: '⇧',
  Return: '↩', Enter: '↩', Escape: '⎋', Space: '␣',
  Up: '↑', Down: '↓', Left: '←', Right: '→',
  Backspace: '⌫', Delete: '⌦', Tab: '⇥'
}

const WIN_WORDS = {
  CommandOrControl: 'Ctrl', Command: 'Win', Control: 'Ctrl',
  Return: 'Enter', Escape: 'Esc'
}

function prettyAccel (accelerator) {
  if (!accelerator) return '—'
  const parts = String(accelerator).split('+')
  if (state?.platform === 'darwin') {
    return parts.map((p) => MAC_GLYPHS[p] || p).join('')
  }
  return parts.map((p) => WIN_WORDS[p] || p).join('+')
}

const clock = (ts) =>
  new Date(ts || Date.now()).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

const stamp = (ts) =>
  new Date(ts || Date.now()).toLocaleTimeString([], {
    hour: '2-digit', minute: '2-digit', second: '2-digit'
  })

/* ---------- top rail ---------- */

function paintRail () {
  if (!state) return
  const hidden = state.window.protection !== false
  el.veilState.classList.toggle('is-hidden', hidden)
  el.veilState.classList.toggle('is-exposed', !hidden)
  el.veilState.querySelector('.label').textContent = hidden ? 'invisible' : 'visible'
  el.veilState.title = hidden
    ? `Hidden from screen capture. ${prettyAccel(state.config.hotkeys.toggleProtection)} to turn off.`
    : 'WARNING: this panel will appear in a screen share.'
}

function paintRec ({ recording, uploading }) {
  el.recState.classList.toggle('is-live', !!recording)
  el.recState.classList.toggle('is-working', !recording && !!uploading)
  el.recState.querySelector('.label').textContent =
    recording ? 'recording' : uploading ? 'transcribing' : 'idle'
}

/* ---------- legend ---------- */

function paintLegend () {
  if (!state) return
  const { hotkeys, prompts } = state.config
  const rows = [
    ...prompts.map((p) => [p.accelerator, `${p.label} — ${p.source === 'transcript' ? 'from transcript' : 'screenshot + ask'}`]),
    [hotkeys.regionShot, 'Select a region, then ask'],
    [hotkeys.shotOnly, 'Attach a screenshot, type your own question'],
    [hotkeys.recordToggle, 'Start / stop transcribing'],
    [hotkeys.toggleOverlay, 'Hide or show this panel'],
    [hotkeys.cancel, 'Stop generating']
  ]

  el.legendKeys.innerHTML = rows
    .filter(([accel]) => accel)
    .map(([accel, what]) =>
      `<dt><span class="key">${Markdown.escape(prettyAccel(accel))}</span></dt>` +
      `<dd>${Markdown.escape(what)}</dd>`)
    .join('')

  const chat = state.config.chat
  const provider = state.providers.find((p) => p.id === chat.provider)
  const label = Markdown.escape(provider?.label || chat.provider)
  // Claude Code picks its own model when none is named, so there is nothing
  // honest to print after the provider name.
  const model = chat.model || provider?.model || ''
  const keyed = state.keys[chat.provider] || provider?.needsKey === false
  el.legendFoot.innerHTML = keyed
    ? `Answering with <b>${label}</b>${model ? ` · ${Markdown.escape(model)}` : ''}`
    : `No API key for <b>${label}</b> yet — open <b>settings</b> to add one.`
}

const showLegend = (show) => { el.legend.hidden = !show }

/* ---------- answer cards ---------- */

function newCard (meta) {
  showLegend(false)

  const node = document.createElement('article')
  node.className = 'card'

  const shotsHtml = (meta.shots || []).map((s) =>
    `<div class="attach"><img src="${s.dataUrl}" alt="" />` +
    `<span class="dims">${s.width}×${s.height} px</span></div>`
  ).join('')

  const accel = state?.config.prompts.find((p) => p.label === meta.label)?.accelerator
  node.innerHTML =
    '<div class="eyebrow">' +
      (accel ? `<span class="key">${Markdown.escape(prettyAccel(accel))}</span>` : '') +
      `<span class="what">${Markdown.escape(meta.label || 'Ask')}</span>` +
      `<span class="meta">${Markdown.escape(meta.model || '')} · ${clock()}</span>` +
    '</div>' +
    shotsHtml +
    '<div class="think" hidden></div>' +
    '<div class="notes"></div>' +
    '<div class="body"></div>' +
    '<div class="cardFoot" hidden><button data-act="copy">copy</button></div>'

  el.cards.appendChild(node)

  card = {
    id: meta.id,
    node,
    body: node.querySelector('.body'),
    meta: node.querySelector('.meta'),
    think: node.querySelector('.think'),
    notes: node.querySelector('.notes'),
    foot: node.querySelector('.cardFoot'),
    text: '',
    thinking: ''
  }

  for (const note of meta.notes || []) addNote(note)
  el.shell.classList.add('is-streaming')
  scrollReadout()
}

function addNote (text) {
  if (!card) return
  const node = document.createElement('div')
  node.className = 'note'
  node.textContent = text
  card.notes.appendChild(node)
}

function queueRepaint () {
  if (repaintQueued || !card) return
  repaintQueued = true
  requestAnimationFrame(() => {
    repaintQueued = false
    if (!card) return
    const atBottom = el.readout.scrollHeight - el.readout.scrollTop - el.readout.clientHeight < 60
    card.body.innerHTML = Markdown.render(card.text) + '<span class="cursor"></span>'
    if (card.thinking) {
      card.think.hidden = false
      card.think.textContent = card.thinking.slice(-400)
    }
    if (atBottom) scrollReadout()
  })
}

function scrollReadout () {
  el.readout.scrollTop = el.readout.scrollHeight
}

function finishCard (info) {
  el.shell.classList.remove('is-streaming')
  if (!card) {
    if (info?.error) errorCard(info.error)
    return
  }

  if (info?.error) {
    card.body.innerHTML = `<p>${Markdown.escape(info.error)}</p>`
    card.node.classList.add('is-error')
  } else {
    card.body.innerHTML = Markdown.render(card.text) ||
      '<p class="note">No text came back.</p>'
    card.foot.hidden = !card.text
  }

  // The real model and cost are only known once the turn ends — Claude Code
  // in particular picks its own model unless one is named.
  if (info?.model || info?.costUsd != null) {
    const bits = [info.model, clock()]
    if (info.costUsd != null) bits.push(`$${Number(info.costUsd).toFixed(4)}`)
    card.meta.textContent = bits.filter(Boolean).join(' · ')
  }

  if (info?.cancelled) addNote('Stopped.')
  if (info?.stopReason === 'incomplete') addNote('The connection ended before the answer finished.')
  if (card.thinking) card.think.textContent = card.thinking.slice(-400)

  card = null
  scrollReadout()
}

function errorCard (message) {
  newCard({ id: 'local', label: 'Error' })
  card.node.classList.add('is-error')
  card.body.innerHTML = `<p>${Markdown.escape(message)}</p>`
  el.shell.classList.remove('is-streaming')
  card = null
}

/* ---------- attachments ---------- */

function paintAttachments () {
  el.shots.innerHTML = attachments.map((s) =>
    `<span class="chip"><img src="${s.dataUrl}" alt="" />` +
    `<span>${s.width}×${s.height}</span>` +
    `<button data-drop="${s.id}" title="Remove">×</button></span>`
  ).join('')
}

/* ---------- transcript ---------- */

function paintTranscriptCount () {
  el.drawerCount.textContent = transcriptCount ? `${transcriptCount} lines` : 'empty'
}

function appendTranscript (line) {
  const node = document.createElement('div')
  node.className = 'line'
  node.innerHTML =
    `<time>${Markdown.escape(stamp(line.at))}</time>` +
    `<span class="said">${Markdown.escape(line.text)}</span>`
  el.transcript.appendChild(node)
  transcriptCount += 1
  paintTranscriptCount()
  if (el.drawer.classList.contains('collapsed')) el.drawer.classList.remove('collapsed')
  el.transcript.scrollTop = el.transcript.scrollHeight
}

/* ---------- toasts ---------- */

function toast (kind, text) {
  const node = document.createElement('div')
  node.className = `toast ${kind || 'info'}`
  node.textContent = text
  el.toasts.appendChild(node)
  setTimeout(() => node.remove(), kind === 'error' ? 7000 : 3600)
}

/* ---------- asking ---------- */

async function send () {
  const text = el.input.value.trim()
  if (!text && !attachments.length) return
  const shotIds = attachments.map((s) => s.id)
  el.input.value = ''
  autosize()
  attachments = []
  paintAttachments()
  // Errors arrive on ai:end and are drawn into the card; nothing to do here.
  await window.veil.ask({ input: text, shotIds })
}

function autosize () {
  el.input.style.height = 'auto'
  el.input.style.height = `${Math.min(96, el.input.scrollHeight)}px`
}

/* ---------- wiring ---------- */

function applyState (next) {
  state = next
  paintRail()
  paintLegend()
  Recorder.setConfig({
    ...state.config.transcription,
    // Decides the audio format the recorder hands over: WAV for on-device
    // whisper.cpp, Opus for anything uploaded.
    localEngine: state.transcription?.local === true
  })
  if (window.Settings?.isOpen()) window.Settings.render(state)
}

async function refresh () {
  applyState(await window.veil.bootstrap())
}

el.gear.addEventListener('click', () => window.Settings.open(state))
el.veilState.addEventListener('click', async () => {
  await window.veil.win.protection(state.window.protection === false)
})
el.recState.addEventListener('click', () => Recorder.toggle())

el.drawerHead.addEventListener('click', () => {
  el.drawer.classList.toggle('collapsed')
})

el.shots.addEventListener('click', (event) => {
  const id = event.target.dataset?.drop
  if (!id) return
  attachments = attachments.filter((s) => s.id !== id)
  paintAttachments()
})

el.cards.addEventListener('click', (event) => {
  const action = event.target.dataset?.act
  if (action === 'copy') {
    const body = event.target.closest('.card')?.querySelector('.body')
    if (body) {
      window.veil.copy(body.innerText)
      toast('info', 'Copied.')
    }
    return
  }
  const link = event.target.closest('a[href]')
  if (link) {
    event.preventDefault()
    window.veil.openExternal(link.getAttribute('href'))
  }
})

el.input.addEventListener('input', autosize)

el.input.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault()
    send()
  }
})

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    if (window.Settings.isOpen()) return window.Settings.close()
    if (el.shell.classList.contains('is-streaming')) return window.veil.cancel()
    return window.veil.win.hide()
  }
  if (event.key === ',' && (event.metaKey || event.ctrlKey)) {
    event.preventDefault()
    window.Settings.open(state)
  }
})

// Frameless transparent windows get no native resize edges on Windows, so the
// grip drives setBounds through the main process instead.
el.grip.addEventListener('mousedown', (event) => {
  event.preventDefault()
  const onMove = (move) => window.veil.win.resize(move.movementX, move.movementY)
  const onUp = () => {
    window.removeEventListener('mousemove', onMove)
    window.removeEventListener('mouseup', onUp)
  }
  window.addEventListener('mousemove', onMove)
  window.addEventListener('mouseup', onUp)
})

/* ---------- main-process events ---------- */

window.veil.on.status(applyState)
window.veil.on.toast(({ kind, text }) => toast(kind, text))

window.veil.on.aiStart((meta) => newCard(meta))
window.veil.on.aiDelta(({ id, text }) => {
  if (!card || card.id !== id) return
  card.text += text
  queueRepaint()
})
window.veil.on.aiThinking(({ id, text }) => {
  if (!card || card.id !== id) return
  card.thinking += text
  queueRepaint()
})
window.veil.on.aiNote(({ text }) => addNote(text))
window.veil.on.aiEnd((info) => finishCard(info))

window.veil.on.shotAdded((shot) => {
  attachments.push(shot)
  if (attachments.length > 4) attachments = attachments.slice(-4)
  paintAttachments()
})

window.veil.on.transcriptAppend(appendTranscript)
window.veil.on.transcriptCleared(() => {
  el.transcript.innerHTML = ''
  transcriptCount = 0
  paintTranscriptCount()
})

window.veil.on.sessionCleared(() => {
  el.cards.innerHTML = ''
  el.transcript.innerHTML = ''
  transcriptCount = 0
  attachments = []
  card = null
  el.shell.classList.remove('is-streaming')
  paintAttachments()
  paintTranscriptCount()
  showLegend(true)
})

window.veil.on.recToggle(() => Recorder.toggle())
window.veil.on.focusInput(() => {
  el.input.focus()
  el.input.setSelectionRange(el.input.value.length, el.input.value.length)
})

window.veil.on.hotkeyErrors((failures) => {
  const list = failures.map((f) => `${prettyAccel(f.accelerator)} (${f.label})`).join(', ')
  toast('warn', `Some hotkeys could not be registered — another app owns them: ${list}`)
})

/* ---------- boot ---------- */

Recorder.init({
  onState: paintRec,
  onError: (message) => toast('error', message)
})

;(async () => {
  const boot = await window.veil.bootstrap()
  applyState(boot)
  window.Settings.init({ refresh, toast })

  for (const line of boot.transcriptLines || []) appendTranscript(line)
  paintTranscriptCount()
  paintRec({ recording: false, uploading: false })

  if (boot.platform === 'darwin' && boot.screenPermission !== 'granted') {
    toast('warn', 'Screen Recording permission is not granted yet — screenshots will come back empty. Open settings › Capture to fix it.')
  }
  if (boot.hotkeyErrors?.length) {
    toast('warn', `${boot.hotkeyErrors.length} hotkey(s) are already taken by another app. Change them in settings › Keys.`)
  }
})()
