'use strict'

const { contextBridge, ipcRenderer } = require('electron')

// Returns an unsubscribe function so the renderer can rebind without leaking
// listeners across settings reloads.
function listener (channel) {
  return (callback) => {
    const wrapped = (_event, payload) => callback(payload)
    ipcRenderer.on(channel, wrapped)
    return () => ipcRenderer.removeListener(channel, wrapped)
  }
}

contextBridge.exposeInMainWorld('veil', {
  bootstrap: () => ipcRenderer.invoke('app:bootstrap'),
  setConfig: (patch) => ipcRenderer.invoke('config:set', patch),
  setKey: (provider, key) => ipcRenderer.invoke('secrets:set', { provider, key }),
  reapplyHotkeys: () => ipcRenderer.invoke('hotkeys:reapply'),

  ask: (payload) => ipcRenderer.invoke('ai:ask', payload),
  cancel: () => ipcRenderer.invoke('ai:cancel'),
  clearSession: () => ipcRenderer.invoke('session:clear'),
  clearTranscript: () => ipcRenderer.invoke('transcript:clear'),

  capture: (mode, promptId) => ipcRenderer.invoke('capture:take', { mode, promptId }),

  transcribe: (bytes, mime) => ipcRenderer.invoke('transcribe:audio', { bytes, mime }),
  reportRecording: (state) => ipcRenderer.send('rec:state', state),

  win: {
    move: (dx, dy) => ipcRenderer.invoke('win:move', { dx, dy }),
    resize: (dw, dh) => ipcRenderer.invoke('win:resize', { dw, dh }),
    opacity: (value) => ipcRenderer.invoke('win:opacity', value),
    clickThrough: (on) => ipcRenderer.invoke('win:clickThrough', on),
    protection: (on) => ipcRenderer.invoke('win:protection', on),
    hide: () => ipcRenderer.invoke('win:hide'),
    quit: () => ipcRenderer.invoke('win:quit')
  },

  perms: {
    screen: () => ipcRenderer.invoke('perms:screen'),
    openScreenSettings: () => ipcRenderer.invoke('perms:openScreenSettings'),
    microphone: () => ipcRenderer.invoke('perms:microphone')
  },

  copy: (text) => ipcRenderer.invoke('clipboard:write', text),
  openExternal: (url) => ipcRenderer.invoke('shell:open', url),

  on: {
    status: listener('status'),
    toast: listener('toast'),
    aiStart: listener('ai:start'),
    aiDelta: listener('ai:delta'),
    aiThinking: listener('ai:thinking'),
    aiNote: listener('ai:note'),
    aiEnd: listener('ai:end'),
    shotAdded: listener('shot:added'),
    transcriptAppend: listener('transcript:append'),
    transcriptCleared: listener('transcript:cleared'),
    sessionCleared: listener('session:cleared'),
    recToggle: listener('rec:toggle'),
    recState: listener('rec:state'),
    focusInput: listener('focus:input'),
    hotkeyErrors: listener('hotkeys:errors')
  }
})

// Used only by the region-selector window.
contextBridge.exposeInMainWorld('region', {
  image: () => ipcRenderer.invoke('region:image'),
  done: (rect) => ipcRenderer.send('region:done', rect)
})
