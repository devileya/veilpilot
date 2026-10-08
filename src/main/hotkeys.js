'use strict'

const { globalShortcut } = require('electron')
const store = require('./store')

// Global shortcuts are first-come-first-served per machine: if another app
// already owns a combination, register() fails and we report it rather than
// leaving a dead key the user keeps pressing.
function apply (handlers) {
  globalShortcut.unregisterAll()
  const cfg = store.readConfig()
  const failed = []
  const taken = new Set()

  const bind = (accelerator, fn, label) => {
    if (!accelerator || !fn) return
    if (taken.has(accelerator)) {
      failed.push({ accelerator, label, reason: 'duplicate' })
      return
    }
    taken.add(accelerator)
    try {
      if (!globalShortcut.register(accelerator, fn)) {
        failed.push({ accelerator, label, reason: 'in use by another app' })
      }
    } catch (err) {
      failed.push({ accelerator, label, reason: err.message })
    }
  }

  const h = cfg.hotkeys
  bind(h.toggleOverlay, handlers.toggleOverlay, 'Show / hide')
  bind(h.shotOnly, handlers.shotOnly, 'Attach screenshot')
  bind(h.regionShot, handlers.regionShot, 'Region screenshot')
  bind(h.recordToggle, handlers.recordToggle, 'Start / stop transcribe')
  bind(h.send, handlers.focusInput, 'Focus the ask box')
  bind(h.cancel, handlers.cancel, 'Stop generating')
  bind(h.clickThrough, handlers.clickThrough, 'Click-through')
  bind(h.toggleProtection, handlers.toggleProtection, 'Capture protection')
  bind(h.newSession, handlers.newSession, 'New session')
  bind(h.opacityUp, () => handlers.opacity(0.08), 'Opacity up')
  bind(h.opacityDown, () => handlers.opacity(-0.08), 'Opacity down')
  bind(h.moveUp, () => handlers.move(0, -48), 'Move up')
  bind(h.moveDown, () => handlers.move(0, 48), 'Move down')
  bind(h.moveLeft, () => handlers.move(-48, 0), 'Move left')
  bind(h.moveRight, () => handlers.move(48, 0), 'Move right')

  for (const preset of cfg.prompts || []) {
    bind(preset.accelerator, () => handlers.runPrompt(preset.id), preset.label || preset.id)
  }

  return failed
}

function release () {
  globalShortcut.unregisterAll()
}

module.exports = { apply, release }
