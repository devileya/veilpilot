'use strict'

const path = require('node:path')
const {
  BrowserWindow, desktopCapturer, ipcMain, screen, shell, systemPreferences
} = require('electron')
const store = require('./store')
const overlay = require('./overlay')

const MAC_SCREEN_PREFS =
  'x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture'

function screenPermission () {
  if (process.platform !== 'darwin') return 'granted'
  return systemPreferences.getMediaAccessStatus('screen')
}

function openScreenSettings () {
  if (process.platform === 'darwin') shell.openExternal(MAC_SCREEN_PREFS)
}

function pickDisplay (pref) {
  const all = screen.getAllDisplays()
  if (pref && pref !== 'cursor' && pref !== 'primary') {
    const match = all.find((d) => String(d.id) === String(pref))
    if (match) return match
  }
  if (pref === 'primary') return screen.getPrimaryDisplay()
  return screen.getDisplayNearestPoint(screen.getCursorScreenPoint())
}

function listDisplays () {
  const primary = screen.getPrimaryDisplay().id
  return screen.getAllDisplays().map((d, i) => ({
    id: String(d.id),
    label: `Display ${i + 1}${d.id === primary ? ' (primary)' : ''} · ${d.size.width}×${d.size.height}`
  }))
}

const PERMISSION_HELP =
  'macOS is refusing screen capture. Grant Screen Recording to VeilPilot in ' +
  'System Settings › Privacy & Security › Screen Recording, then quit and ' +
  'relaunch the app — the permission only takes effect on a fresh launch.'

async function grabDisplay (display) {
  const scale = display.scaleFactor || 1

  if (process.platform === 'darwin' && screenPermission() !== 'granted') {
    throw new Error(PERMISSION_HELP)
  }

  let sources
  try {
    sources = await desktopCapturer.getSources({
      types: ['screen'],
      fetchWindowIcons: false,
      thumbnailSize: {
        width: Math.round(display.size.width * scale),
        height: Math.round(display.size.height * scale)
      }
    })
  } catch (err) {
    // Electron reports this as a bare "Failed to get sources", which tells
    // nobody what to do about it.
    throw new Error(process.platform === 'darwin'
      ? PERMISSION_HELP
      : `Screen capture failed: ${err.message}`)
  }

  if (!sources.length) throw new Error('No screen sources available')
  const source =
    sources.find((s) => String(s.display_id) === String(display.id)) || sources[0]
  if (source.thumbnail.isEmpty()) {
    throw new Error('Screen capture returned an empty frame — grant Screen Recording permission')
  }
  return source.thumbnail
}

function downscale (image, maxDimension) {
  const size = image.getSize()
  const longest = Math.max(size.width, size.height)
  if (!maxDimension || longest <= maxDimension) return image
  const ratio = maxDimension / longest
  return image.resize({
    width: Math.round(size.width * ratio),
    height: Math.round(size.height * ratio),
    quality: 'good'
  })
}

// Content protection already keeps the panel out of the frame. If the user
// turned it off, hide the panel for the duration of the grab so the
// screenshot isn't a picture of the screenshot tool.
async function withoutSelfCapture (fn) {
  const win = overlay.get()
  const mustHide = win && win.isVisible() && !store.readConfig().window.protection
  if (!mustHide) return fn()
  win.hide()
  try {
    await new Promise((r) => setTimeout(r, 120))
    return await fn()
  } finally {
    win.showInactive()
  }
}

async function captureFull () {
  const cfg = store.readConfig().capture
  const display = pickDisplay(cfg.display)
  const raw = await withoutSelfCapture(() => grabDisplay(display))
  return finish(raw, display, cfg)
}

async function captureRegion () {
  const cfg = store.readConfig().capture
  const display = pickDisplay('cursor')
  const raw = await withoutSelfCapture(() => grabDisplay(display))

  const rect = await selectRegion(display, raw.toDataURL())
  if (!rect) return null

  const scale = display.scaleFactor || 1
  const size = raw.getSize()
  const crop = {
    x: Math.max(0, Math.round(rect.x * scale)),
    y: Math.max(0, Math.round(rect.y * scale)),
    width: Math.round(rect.width * scale),
    height: Math.round(rect.height * scale)
  }
  crop.width = Math.min(crop.width, size.width - crop.x)
  crop.height = Math.min(crop.height, size.height - crop.y)
  if (crop.width < 8 || crop.height < 8) return null

  return finish(raw.crop(crop), display, cfg)
}

function finish (image, display, cfg) {
  const scaled = downscale(image, cfg.maxDimension)
  const size = scaled.getSize()
  return {
    buffer: scaled.toPNG(),
    dataUrl: scaled.toDataURL(),
    mime: 'image/png',
    width: size.width,
    height: size.height,
    displayId: String(display.id),
    at: Date.now()
  }
}

/* ---------- region selector ---------- */

let pending = null

function registerIpc () {
  ipcMain.handle('region:image', () => (pending ? pending.dataUrl : null))
  ipcMain.on('region:done', (_e, rect) => {
    if (pending) pending.settle(rect)
  })
}

function selectRegion (display, dataUrl) {
  if (pending) pending.settle(null)

  return new Promise((resolve) => {
    const picker = new BrowserWindow({
      ...display.bounds,
      frame: false,
      transparent: true,
      backgroundColor: '#00000000',
      hasShadow: false,
      roundedCorners: false,
      resizable: false,
      movable: false,
      minimizable: false,
      maximizable: false,
      fullscreenable: false,
      skipTaskbar: true,
      enableLargerThanScreen: true,
      webPreferences: {
        preload: path.join(__dirname, '..', 'preload.js'),
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: false
      }
    })

    let done = false
    const settle = (rect) => {
      if (done) return
      done = true
      pending = null
      if (!picker.isDestroyed()) picker.destroy()
      resolve(rect || null)
    }

    pending = { dataUrl, settle }

    picker.setContentProtection(true)
    picker.setAlwaysOnTop(true, 'screen-saver', 1)
    picker.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true })
    picker.on('closed', () => settle(null))
    picker.on('blur', () => settle(null))
    picker.loadFile(path.join(__dirname, '..', 'region', 'index.html'))
    picker.once('ready-to-show', () => {
      picker.show()
      picker.focus()
    })
  })
}

module.exports = {
  captureFull,
  captureRegion,
  listDisplays,
  screenPermission,
  openScreenSettings,
  registerIpc
}
