'use strict'

/* Microphone capture lives in the renderer because MediaRecorder does. Audio
   bytes go to the main process, which owns the transcription API call. */
window.Recorder = (() => {
  const CANDIDATE_TYPES = [
    'audio/webm;codecs=opus',
    'audio/webm',
    'audio/ogg;codecs=opus',
    'audio/mp4'
  ]

  let config = { inputDeviceId: '', captureSystemAudio: false, chunkSeconds: 0 }
  let hooks = {}
  let mime = ''
  let active = null       // { stream, cleanup }
  let recorder = null
  let chunks = []
  let rollover = null
  let recording = false
  let uploading = 0

  const emit = () => hooks.onState?.({ recording, uploading: uploading > 0 })

  function init (next) { hooks = next || {} }
  function setConfig (next) { config = { ...config, ...next } }

  function pickMime () {
    if (!window.MediaRecorder) return ''
    return CANDIDATE_TYPES.find((type) => MediaRecorder.isTypeSupported(type)) || ''
  }

  async function devices () {
    try {
      const all = await navigator.mediaDevices.enumerateDevices()
      return all
        .filter((d) => d.kind === 'audioinput')
        .map((d, i) => ({ id: d.deviceId, label: d.label || `Input ${i + 1}` }))
    } catch {
      return []
    }
  }

  async function buildStream () {
    const audio = config.inputDeviceId
      ? { deviceId: { exact: config.inputDeviceId } }
      : true
    const mic = await navigator.mediaDevices.getUserMedia({ audio })
    const stopMic = () => mic.getTracks().forEach((t) => t.stop())

    if (!config.captureSystemAudio) return { stream: mic, cleanup: stopMic }

    // Desktop loopback works on Windows. macOS gives Chromium no system-audio
    // tap, so there the honest answer is a virtual device (BlackHole/Loopback)
    // selected as the input — not a silent half-broken recording.
    let context = null
    try {
      const desktop = await navigator.mediaDevices.getUserMedia({
        audio: { mandatory: { chromeMediaSource: 'desktop' } },
        video: { mandatory: { chromeMediaSource: 'desktop' } }
      })
      desktop.getVideoTracks().forEach((t) => t.stop())
      if (!desktop.getAudioTracks().length) throw new Error('no system audio track')

      context = new AudioContext()
      const destination = context.createMediaStreamDestination()
      context.createMediaStreamSource(mic).connect(destination)
      context.createMediaStreamSource(new MediaStream(desktop.getAudioTracks()))
        .connect(destination)

      return {
        stream: destination.stream,
        cleanup: () => {
          stopMic()
          desktop.getTracks().forEach((t) => t.stop())
          context?.close().catch(() => {})
        }
      }
    } catch {
      context?.close().catch(() => {})
      hooks.onError?.('System audio is not available here — recording the microphone only. On macOS, install a loopback device such as BlackHole and select it as the input.')
      return { stream: mic, cleanup: stopMic }
    }
  }

  async function upload (blob) {
    uploading += 1
    emit()
    try {
      const bytes = new Uint8Array(await blob.arrayBuffer())
      const result = await window.veil.transcribe(bytes, blob.type)
      if (result && !result.ok && result.error) hooks.onError?.(result.error)
    } catch (err) {
      hooks.onError?.(err.message || String(err))
    } finally {
      uploading -= 1
      emit()
    }
  }

  function startSegment () {
    recorder = new MediaRecorder(active.stream, mime ? { mimeType: mime } : undefined)
    const segmentType = recorder.mimeType || mime || 'audio/webm'
    chunks = []

    recorder.ondataavailable = (event) => {
      if (event.data && event.data.size) chunks.push(event.data)
    }

    recorder.onstop = () => {
      const parts = chunks
      chunks = []
      const blob = new Blob(parts, { type: segmentType })
      // Sub-2 KB segments are silence or a mis-tap; uploading them wastes a
      // round trip and returns transcription noise.
      if (blob.size > 2048) upload(blob)
      if (recording) startSegment()
      else teardown()
    }

    recorder.onerror = (event) => {
      hooks.onError?.(`Recorder error: ${event.error?.message || 'unknown'}`)
      stop()
    }

    recorder.start()
  }

  function teardown () {
    clearInterval(rollover)
    rollover = null
    recorder = null
    active?.cleanup?.()
    active = null
    emit()
  }

  async function start () {
    if (recording) return
    mime = pickMime()
    try {
      const permission = await window.veil.perms.microphone()
      if (permission === 'denied' || permission === 'restricted') {
        hooks.onError?.('Microphone access is blocked. Grant it in System Settings › Privacy & Security › Microphone.')
        return
      }
      active = await buildStream()
    } catch (err) {
      hooks.onError?.(`Could not open the microphone: ${err.message || err}`)
      return
    }

    recording = true
    startSegment()

    // Rolling chunks: each segment is stopped and restarted so it is a
    // standalone container. Slices from a single start(timeslice) are not
    // independently decodable and the transcription API rejects them.
    const seconds = Number(config.chunkSeconds) || 0
    if (seconds >= 5) {
      rollover = setInterval(() => {
        if (recorder && recorder.state === 'recording') recorder.stop()
      }, seconds * 1000)
    }

    emit()
  }

  function stop () {
    if (!recording) return
    recording = false
    clearInterval(rollover)
    rollover = null
    if (recorder && recorder.state !== 'inactive') recorder.stop()
    else teardown()
    emit()
  }

  const toggle = () => (recording ? stop() : start())
  const state = () => ({ recording, uploading: uploading > 0 })

  return { init, setConfig, toggle, start, stop, state, devices }
})()
