'use strict'

const path = require('node:path')
const { BrowserWindow, screen } = require('electron')
const store = require('./store')

const DEFAULT_SIZE = { width: 430, height: 580 }
const MARGIN = 24

let win = null
let saveTimer = null

function defaultBounds () {
  const area = screen.getPrimaryDisplay().workArea
  return {
    x: area.x + area.width - DEFAULT_SIZE.width - MARGIN,
    y: area.y + MARGIN,
    ...DEFAULT_SIZE
  }
}

// A saved position is only usable if some display still contains it —
// otherwise unplugging a monitor leaves the overlay parked offscreen with no
// way to click it back.
function usableBounds (saved) {
  if (!saved || typeof saved.x !== 'number') return defaultBounds()
  const visible = screen.getAllDisplays().some((d) => {
    const a = d.workArea
    return saved.x + saved.width > a.x + 40 &&
      saved.x < a.x + a.width - 40 &&
      saved.y + 20 >= a.y &&
      saved.y < a.y + a.height - 40
  })
  return visible ? saved : defaultBounds()
}

function create () {
  const cfg = store.readConfig()
  const bounds = usableBounds(cfg.window.bounds)

  win = new BrowserWindow({
    ...bounds,
    minWidth: 320,
    minHeight: 240,
    show: false,
    frame: false,
    transparent: true,
    backgroundColor: '#00000000',
    hasShadow: false,
    roundedCorners: false,
    resizable: true,
    maximizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    acceptFirstMouse: true,
    title: 'VeilPilot',
    webPreferences: {
      preload: path.join(__dirname, '..', 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
      backgroundThrottling: false
    }
  })

  // The whole point of the app. On macOS this maps to NSWindowSharingNone; on
  // Windows 10 2004+ to WDA_EXCLUDEFROMCAPTURE (older builds degrade to a
  // black rectangle — see README).
  win.setContentProtection(cfg.window.protection !== false)

  // 'screen-saver' keeps the panel above a fullscreen Zoom/Meet window.
  win.setAlwaysOnTop(true, 'screen-saver', 1)
  win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true })
  if (typeof win.setHiddenInMissionControl === 'function') win.setHiddenInMissionControl(true)
  if (cfg.window.opacity < 1) win.setOpacity(cfg.window.opacity)
  if (cfg.window.clickThrough) win.setIgnoreMouseEvents(true, { forward: true })

  win.loadFile(path.join(__dirname, '..', 'renderer', 'index.html'))

  win.on('resize', queueSave)
  win.on('move', queueSave)
  win.on('closed', () => { win = null })

  // Frameless + transparent windows have no chrome to close, so a stray
  // Cmd+W would orphan the app with no way back.
  win.on('close', (e) => {
    if (!global.__veilQuitting) {
      e.preventDefault()
      win.hide()
    }
  })

  return win
}

function queueSave () {
  if (!win || win.isDestroyed()) return
  clearTimeout(saveTimer)
  saveTimer = setTimeout(() => {
    if (!win || win.isDestroyed()) return
    store.writeConfig({ window: { bounds: win.getBounds() } })
  }, 500)
}

const get = () => (win && !win.isDestroyed() ? win : null)

function show ({ focus = true } = {}) {
  const w = get() || create()
  if (focus) w.show()
  else w.showInactive()
  w.setAlwaysOnTop(true, 'screen-saver', 1)
  return w
}

function hide () {
  const w = get()
  if (w) w.hide()
}

function toggle () {
  const w = get()
  if (w && w.isVisible()) hide()
  else show()
  return !!(get() && get().isVisible())
}

function move (dx, dy) {
  const w = get()
  if (!w) return
  const b = w.getBounds()
  w.setBounds({ ...b, x: b.x + dx, y: b.y + dy })
}

function resizeBy (dw, dh) {
  const w = get()
  if (!w) return
  const b = w.getBounds()
  w.setBounds({
    ...b,
    width: Math.max(320, b.width + dw),
    height: Math.max(240, b.height + dh)
  })
}

function setOpacity (value) {
  const next = Math.min(1, Math.max(0.25, Number(value) || 1))
  const w = get()
  if (w) w.setOpacity(next)
  store.writeConfig({ window: { opacity: next } })
  return next
}

function nudgeOpacity (delta) {
  return setOpacity((store.readConfig().window.opacity || 1) + delta)
}

function setClickThrough (on) {
  const w = get()
  if (w) w.setIgnoreMouseEvents(!!on, { forward: true })
  store.writeConfig({ window: { clickThrough: !!on } })
  return !!on
}

function setProtection (on) {
  const w = get()
  if (w) w.setContentProtection(!!on)
  store.writeConfig({ window: { protection: !!on } })
  return !!on
}

function status () {
  const cfg = store.readConfig().window
  const w = get()
  return {
    visible: !!(w && w.isVisible()),
    protection: cfg.protection !== false,
    clickThrough: !!cfg.clickThrough,
    opacity: cfg.opacity ?? 1
  }
}

function send (channel, payload) {
  const w = get()
  if (w) w.webContents.send(channel, payload)
}

module.exports = {
  create,
  get,
  show,
  hide,
  toggle,
  move,
  resizeBy,
  setOpacity,
  nudgeOpacity,
  setClickThrough,
  setProtection,
  status,
  send
}
