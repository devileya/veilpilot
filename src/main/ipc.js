'use strict'

const { app, ipcMain, clipboard, shell, systemPreferences } = require('electron')
const store = require('./store')
const session = require('./session')
const overlay = require('./overlay')
const capture = require('./capture')
const hotkeys = require('./hotkeys')
const transcribe = require('./transcribe')
const ai = require('./ai')
const claudeCode = require('./ai/claude-code')
const whisperCpp = require('./whisper-cpp')
const { PROVIDERS } = require('./defaults')

let handlers = {}
let lastHotkeyErrors = []
let recordingState = {}

const toast = (kind, text) => overlay.send('toast', { kind, text })

function providerList () {
  return Object.entries(PROVIDERS).map(([id, p]) => ({
    id,
    label: p.label,
    kind: p.kind,
    baseUrl: p.baseUrl,
    model: p.model,
    transcriptionModel: p.transcriptionModel || '',
    vision: !!p.vision,
    transcription: !!p.transcription,
    chat: p.chat !== false,
    needsKey: p.needsKey !== false,
    keyUrl: p.keyUrl || ''
  }))
}

function bootstrap () {
  return {
    platform: process.platform,
    version: app.getVersion(),
    config: store.readConfig(),
    providers: providerList(),
    keys: store.keyStatus(),
    displays: capture.listDisplays(),
    window: overlay.status(),
    transcriptLines: session.transcriptLines(),
    recording: recordingState,
    transcription: transcribe.readiness(),
    screenPermission: capture.screenPermission(),
    hotkeyErrors: lastHotkeyErrors,
    paths: { config: store.configPath(), secrets: store.secretsPath() }
  }
}

function reapplyHotkeys () {
  lastHotkeyErrors = hotkeys.apply(handlers)
  if (lastHotkeyErrors.length) {
    overlay.send('hotkeys:errors', lastHotkeyErrors)
  }
  return lastHotkeyErrors
}

function register (appHandlers) {
  handlers = appHandlers

  ipcMain.handle('app:bootstrap', () => bootstrap())

  ipcMain.handle('config:set', (_e, patch) => {
    const before = store.readConfig()
    const next = store.writeConfig(patch || {})
    const hotkeysChanged =
      JSON.stringify(before.hotkeys) !== JSON.stringify(next.hotkeys) ||
      JSON.stringify(before.prompts) !== JSON.stringify(next.prompts)
    if (hotkeysChanged) reapplyHotkeys()
    overlay.send('status', bootstrap())
    return next
  })

  ipcMain.handle('secrets:set', (_e, { provider, key }) => {
    store.setKey(provider, String(key || '').trim())
    overlay.send('status', bootstrap())
    return store.keyStatus()
  })

  ipcMain.handle('hotkeys:reapply', () => reapplyHotkeys())

  ipcMain.handle('claudecode:detect', () =>
    claudeCode.detect(store.readConfig().chat.claudeCode.binaryPath))

  ipcMain.handle('whisper:detect', () =>
    whisperCpp.detectSync(store.readConfig().transcription.whisperCpp || {}))

  ipcMain.handle('whisper:download', async (_e, file) => {
    try {
      const result = await whisperCpp.downloadModel(file, (progress) =>
        overlay.send('whisper:progress', progress))
      overlay.send('status', bootstrap())
      toast('info', `${file} is ready.`)
      return result
    } catch (err) {
      toast('error', err.message)
      return { ok: false, error: err.message }
    }
  })

  /* ---------- asking ---------- */

  ipcMain.handle('ai:ask', (_e, payload) => ai.ask(payload || {}))
  ipcMain.handle('ai:cancel', () => ai.cancel())

  ipcMain.handle('session:clear', () => {
    ai.cancel()
    session.clear()
    overlay.send('session:cleared')
    return true
  })

  ipcMain.handle('transcript:clear', () => {
    session.clearTranscript()
    overlay.send('transcript:cleared')
    return true
  })

  /* ---------- capture ---------- */

  ipcMain.handle('capture:take', async (_e, { mode = 'full', promptId = null } = {}) => {
    try {
      const shot = mode === 'region' ? await capture.captureRegion() : await capture.captureFull()
      if (!shot) return { ok: false, cancelled: true }
      const id = session.addShot(shot)
      overlay.send('shot:added', {
        id, dataUrl: shot.dataUrl, width: shot.width, height: shot.height, at: shot.at
      })
      if (promptId) ai.ask({ promptId, shotIds: [id] })
      return { ok: true, id }
    } catch (err) {
      toast('error', err.message)
      return { ok: false, error: err.message }
    }
  })

  /* ---------- transcription ---------- */

  ipcMain.handle('transcribe:audio', async (_e, { bytes, mime }) => {
    try {
      const text = await transcribe.transcribe({ bytes: Buffer.from(bytes), mime })
      if (!text) return { ok: true, text: '' }
      const line = session.appendTranscript(text)
      if (line) overlay.send('transcript:append', line)
      return { ok: true, text }
    } catch (err) {
      toast('error', err.message)
      return { ok: false, error: err.message }
    }
  })

  // The renderer owns the recorder and paints its own indicator; main only
  // needs the flag so other surfaces (tray, future menu) can read it.
  ipcMain.on('rec:state', (_e, next) => { recordingState = next || {} })

  /* ---------- window ---------- */

  ipcMain.handle('win:move', (_e, { dx, dy }) => overlay.move(dx || 0, dy || 0))
  ipcMain.handle('win:resize', (_e, { dw, dh }) => overlay.resizeBy(dw || 0, dh || 0))
  ipcMain.handle('win:opacity', (_e, value) => overlay.setOpacity(value))
  ipcMain.handle('win:clickThrough', (_e, on) => overlay.setClickThrough(on))
  ipcMain.handle('win:protection', (_e, on) => {
    const state = overlay.setProtection(on)
    overlay.send('status', bootstrap())
    return state
  })
  ipcMain.handle('win:hide', () => overlay.hide())
  ipcMain.handle('win:quit', () => handlers.quit())

  /* ---------- system ---------- */

  ipcMain.handle('perms:screen', () => capture.screenPermission())
  ipcMain.handle('perms:openScreenSettings', () => capture.openScreenSettings())
  ipcMain.handle('perms:microphone', async () => {
    if (process.platform !== 'darwin') return 'granted'
    const status = systemPreferences.getMediaAccessStatus('microphone')
    if (status === 'not-determined') {
      const granted = await systemPreferences.askForMediaAccess('microphone')
      return granted ? 'granted' : 'denied'
    }
    return status
  })

  ipcMain.handle('clipboard:write', (_e, text) => clipboard.writeText(String(text || '')))
  ipcMain.handle('shell:open', (_e, url) => {
    if (/^https?:\/\//i.test(String(url || ''))) shell.openExternal(url)
  })

  return { bootstrap, reapplyHotkeys }
}

module.exports = { register, bootstrap, reapplyHotkeys: () => reapplyHotkeys() }
