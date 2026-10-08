'use strict'

const fs = require('node:fs')
const path = require('node:path')
const {
  app, Menu, Tray, nativeImage, session: electronSession
} = require('electron')

const store = require('./store')
const sessionState = require('./session')
const overlay = require('./overlay')
const capture = require('./capture')
const hotkeys = require('./hotkeys')
const ipc = require('./ipc')
const ai = require('./ai')

const DEV = process.argv.includes('--dev')
let tray = null

// Last line of defence. Hotkeys fire async work that nobody awaits, so a
// rejection that slips through would otherwise only ever appear in a terminal
// the user does not have open.
process.on('unhandledRejection', (reason) => {
  console.error('[unhandled rejection]', reason)
  overlay.send('toast', { kind: 'error', text: String(reason?.message || reason) })
})

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  app.on('second-instance', () => overlay.show())
}

app.setName('VeilPilot')
// Accessory app: no Dock icon, no menu bar, no App Switcher entry — all of
// which would be visible in a screen share even though the panel is not.
if (process.platform === 'darwin' && app.dock) app.dock.hide()

/* ---------- actions wired to hotkeys, tray, and IPC ---------- */

const toast = (kind, text) => overlay.send('toast', { kind, text })

function announceShot (id, shot) {
  overlay.send('shot:added', {
    id, dataUrl: shot.dataUrl, width: shot.width, height: shot.height, at: shot.at
  })
}

async function takeShot (mode) {
  const shot = mode === 'region' ? await capture.captureRegion() : await capture.captureFull()
  if (!shot) return null
  const id = sessionState.addShot(shot)
  announceShot(id, shot)
  return id
}

async function runPrompt (promptId) {
  const cfg = store.readConfig()
  const preset = (cfg.prompts || []).find((p) => p.id === promptId)
  if (!preset) return toast('error', `Prompt "${promptId}" no longer exists.`)

  // Show without focus so the meeting app keeps the keyboard.
  overlay.show({ focus: false })

  const source = preset.source || 'screenshot'
  const wantsShot = source === 'screenshot' || source === 'both'
  const wantsTranscript = source === 'transcript' || source === 'both'

  if (wantsTranscript && !sessionState.transcriptText()) {
    return toast('warn', `Transcript is empty — start recording with ${cfg.hotkeys.recordToggle}.`)
  }

  let shotIds = []
  if (wantsShot) {
    try {
      const id = await takeShot('full')
      if (!id) return
      shotIds = [id]
    } catch (err) {
      return toast('error', err.message)
    }
  }

  ai.ask({ promptId, shotIds })
}

const handlers = {
  toggleOverlay: () => overlay.toggle(),

  shotOnly: async () => {
    overlay.show({ focus: true })
    try {
      await takeShot('full')
      overlay.send('focus:input')
    } catch (err) {
      toast('error', err.message)
    }
  },

  regionShot: async () => {
    const promptId = store.readConfig().capture.regionPrompt
    try {
      const id = await takeShot('region')
      if (!id) return
      overlay.show({ focus: false })
      if (promptId) ai.ask({ promptId, shotIds: [id] })
      else overlay.send('focus:input')
    } catch (err) {
      toast('error', err.message)
    }
  },

  recordToggle: () => {
    overlay.show({ focus: false })
    overlay.send('rec:toggle')
  },

  focusInput: () => {
    overlay.show({ focus: true })
    overlay.send('focus:input')
  },

  cancel: () => {
    if (ai.cancel()) toast('info', 'Stopped.')
  },

  clickThrough: () => {
    const next = !store.readConfig().window.clickThrough
    overlay.setClickThrough(next)
    overlay.send('status', ipc.bootstrap())
    toast('info', next ? 'Click-through on — the panel ignores the mouse.' : 'Click-through off.')
  },

  toggleProtection: () => {
    const next = !store.readConfig().window.protection
    overlay.setProtection(next)
    overlay.send('status', ipc.bootstrap())
    refreshTray()
    toast(next ? 'info' : 'warn', next
      ? 'Capture protection on — hidden from screen share.'
      : 'Capture protection OFF — this panel will appear in a screen share.')
  },

  newSession: () => {
    ai.cancel()
    sessionState.clear()
    overlay.send('session:cleared')
    toast('info', 'New session.')
  },

  opacity: (delta) => {
    const value = overlay.nudgeOpacity(delta)
    overlay.send('status', ipc.bootstrap())
    toast('info', `Opacity ${Math.round(value * 100)}%`)
  },

  move: (dx, dy) => overlay.move(dx, dy),

  runPrompt,

  quit: () => {
    global.__veilQuitting = true
    app.quit()
  }
}

/* ---------- tray ---------- */

function trayImage () {
  const base = path.join(__dirname, '..', '..', 'build')
  const file = path.join(base, 'trayTemplate.png')
  if (fs.existsSync(file)) {
    const image = nativeImage.createFromPath(file)
    const retina = path.join(base, 'trayTemplate@2x.png')
    if (fs.existsSync(retina)) {
      image.addRepresentation({ scaleFactor: 2, buffer: fs.readFileSync(retina) })
    }
    if (process.platform === 'darwin') image.setTemplateImage(true)
    return image
  }
  return nativeImage.createEmpty()
}

function refreshTray () {
  if (!tray) return
  const cfg = store.readConfig()
  tray.setToolTip(cfg.window.protection
    ? 'VeilPilot — hidden from screen share'
    : 'VeilPilot — VISIBLE in screen share')
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: `Show / hide  (${cfg.hotkeys.toggleOverlay})`, click: handlers.toggleOverlay },
    { label: `Start / stop transcribe  (${cfg.hotkeys.recordToggle})`, click: handlers.recordToggle },
    { label: `New session  (${cfg.hotkeys.newSession})`, click: handlers.newSession },
    { type: 'separator' },
    {
      label: 'Hidden from screen share',
      type: 'checkbox',
      checked: cfg.window.protection !== false,
      click: handlers.toggleProtection
    },
    {
      label: 'Click-through',
      type: 'checkbox',
      checked: !!cfg.window.clickThrough,
      click: handlers.clickThrough
    },
    { type: 'separator' },
    { label: 'Quit VeilPilot', click: handlers.quit }
  ]))
}

/* ---------- lifecycle ---------- */

app.whenReady().then(() => {
  // getUserMedia and desktopCapturer both ask the renderer for permission.
  // The renderer is our own file:// page, so grant media and nothing else.
  const allow = (permission) => permission === 'media'
  electronSession.defaultSession.setPermissionRequestHandler((_wc, permission, done) => done(allow(permission)))
  electronSession.defaultSession.setPermissionCheckHandler((_wc, permission) => allow(permission))

  capture.registerIpc()
  ipc.register(handlers)

  const win = overlay.create()
  win.once('ready-to-show', () => {
    if (store.readConfig().window.showOnStart !== false) overlay.show()
    if (DEV) win.webContents.openDevTools({ mode: 'detach' })
  })

  tray = new Tray(trayImage())
  refreshTray()
  tray.on('click', handlers.toggleOverlay)

  // The CLI is a large bundle, so the first ask of a session pays ~7s of cold
  // start against ~1.5s warm. `--version` costs nothing and no tokens, but it
  // pulls the bundle into the OS page cache.
  if (store.readConfig().chat.provider === 'claudecode') {
    require('./ai/claude-code').detect(store.readConfig().chat.claudeCode.binaryPath)
      .catch(() => {})
  }

  // Same idea for on-device speech: whisper.cpp re-reads the model file on
  // every pass, so a cold page cache costs ~10s on the first chunk against
  // ~1s warm. Reading it through once is free.
  if (store.readConfig().transcription.provider === 'local') {
    require('./whisper-cpp').warm(store.readConfig().transcription.whisperCpp || {})
      .catch(() => {})
  }

  const failures = ipc.reapplyHotkeys()
  if (failures.length) {
    console.warn('[hotkeys] unavailable:', failures.map((f) => f.accelerator).join(', '))
  }
})

// Tray app: closing the panel is hiding it, not quitting.
app.on('window-all-closed', () => {})

app.on('activate', () => overlay.show())

app.on('before-quit', () => {
  global.__veilQuitting = true
  hotkeys.release()
})
