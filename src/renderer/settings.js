'use strict'

window.Settings = (() => {
  const panel = document.getElementById('settings')
  const body = document.getElementById('settingsBody')
  const tabs = document.getElementById('tabs')
  const closeBtn = document.getElementById('settingsClose')

  let hooks = {}
  let current = null
  let tab = 'ai'
  let open = false
  let devices = []
  let claudeCodeStatus = null
  let claudeCodeProbe = null
  let whisperStatus = null
  let whisperProbe = null

  const esc = (value) => Markdown.escape(value ?? '')

  /* ---------- accelerator capture ---------- */

  const CODE_KEYS = {
    Minus: '-', Equal: '=', Backslash: '\\', BracketLeft: '[', BracketRight: ']',
    Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/', Backquote: '`',
    Space: 'Space', Enter: 'Return', Escape: 'Escape', Tab: 'Tab',
    Backspace: 'Backspace', Delete: 'Delete',
    ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right'
  }

  // Reads the physical key, not the character: with Shift held, Digit2 reports
  // "@" on a US layout, which is not a valid Electron accelerator.
  function accelFromEvent (event) {
    const mods = []
    if (event.metaKey) mods.push('Command')
    if (event.ctrlKey) mods.push('Control')
    if (event.altKey) mods.push('Alt')
    if (event.shiftKey) mods.push('Shift')

    let key = null
    if (/^Digit\d$/.test(event.code)) key = event.code.slice(5)
    else if (/^Key[A-Z]$/.test(event.code)) key = event.code.slice(3)
    else if (/^F\d{1,2}$/.test(event.code)) key = event.code
    else if (CODE_KEYS[event.code]) key = CODE_KEYS[event.code]

    if (!key || !mods.length) return null
    return [...mods, key].join('+')
  }

  /* ---------- config writing ---------- */

  function nest (path, value) {
    const parts = path.split('.')
    const root = {}
    let node = root
    parts.forEach((part, index) => {
      if (index === parts.length - 1) node[part] = value
      else node = (node[part] = {})
    })
    return root
  }

  async function savePrompts (prompts) {
    await window.veil.setConfig({ prompts })
    await hooks.refresh?.()
    rebuild()
  }

  /* ---------- field builders ---------- */

  const field = (label, control, hint) =>
    `<div class="field"><label class="fieldLabel">${esc(label)}</label>${control}` +
    (hint ? `<p class="fieldHint">${hint}</p>` : '') + '</div>'

  const text = (path, value, placeholder = '') =>
    `<input type="text" data-path="${path}" value="${esc(value)}" placeholder="${esc(placeholder)}" spellcheck="false" />`

  const number = (path, value, min, max) =>
    `<input type="number" data-path="${path}" value="${esc(value)}" min="${min}" max="${max}" />`

  const select = (path, value, options) =>
    `<select data-path="${path}">` + options.map(([v, label]) =>
      `<option value="${esc(v)}"${String(v) === String(value) ? ' selected' : ''}>${esc(label)}</option>`
    ).join('') + '</select>'

  const check = (path, value, label) =>
    `<label class="check"><input type="checkbox" data-path="${path}"${value ? ' checked' : ''} />` +
    `<span>${esc(label)}</span></label>`

  const hotkey = (path, value) =>
    `<input type="text" class="hk" data-hk="${path}" value="${esc(value || '')}" ` +
    'placeholder="click, then press keys" readonly />'

  /* ---------- views ---------- */

  function viewClaudeCode () {
    const cc = current.config.chat.claudeCode
    const found = claudeCodeStatus

    const status = !found
      ? '<p class="fieldHint" style="margin-bottom:14px">Looking for the CLI…</p>'
      : found.ok
        ? `<p class="fieldHint" style="margin-bottom:14px">Found <b>${esc(found.version)}</b> at <b>${esc(found.path)}</b>.` +
          (found.account?.loggedIn
            ? ` Signed in as <b>${esc(found.account.email || found.account.method)}</b> — no API key needed.`
            : ' Using the login it already has — no API key needed.') +
          '</p>'
        : `<p class="fieldHint" style="margin-bottom:14px;color:var(--live)">${esc(found.error)}</p>`

    return [
      status,
      field('Binary path', text('chat.claudeCode.binaryPath', cc.binaryPath, found?.path || 'auto-detect'),
        'Only needed if auto-detection fails. A GUI app does not inherit your shell PATH, so an unusual install location has to be named here.'),
      field('Working directory', text('chat.claudeCode.cwd', cc.cwd, 'temp directory'),
        'Where Claude Code runs. Leave blank unless you want it to see a specific project.'),
      field('Fast start', check('chat.claudeCode.fastStart', cc.fastStart !== false,
        'Skip hooks, MCP servers, plugins and CLAUDE.md'),
        'Measured here: <b>~11s</b> to first token without it, <b>~1.5s</b> with it. Leave it on for live use.'),
      field('Tools', check('chat.claudeCode.allowTools', !!cc.allowTools,
        'Let it read files and run commands'),
        'Off means answers only — it never touches your disk. On makes "fix this" far stronger but slower, and anything needing permission is denied automatically since nobody is there to approve it.'),
      field('Spend cap (USD)', `<input type="number" data-path="chat.claudeCode.maxBudgetUsd" value="${cc.maxBudgetUsd ?? 0}" min="0" max="100" step="0.05" />`,
        'Per ask. <b>0</b> means no cap. Real cost is reported on every answer.')
    ].join('')
  }

  function viewAi () {
    const chat = current.config.chat
    const provider = current.providers.find((p) => p.id === chat.provider) || {}
    const isClaude = provider.kind === 'anthropic'
    const isClaudeCode = provider.kind === 'claudecode'

    return [
      '<p class="sectionLabel">Answers</p>',
      field('Provider', select('chat.provider', chat.provider,
        current.providers.filter((p) => p.chat !== false).map((p) => [p.id, p.label]))),
      isClaudeCode
        ? field('Model', text('chat.model', chat.model, 'whatever Claude Code uses'),
            'An alias such as <b>sonnet</b> or <b>opus</b>, or a full model id. Blank keeps the CLI default.')
        : `<div class="row">
            ${field('Model', text('chat.model', chat.model, provider.model || 'model id'))}
            ${field('Max tokens', number('chat.maxTokens', chat.maxTokens, 256, 128000))}
          </div>`,
      isClaudeCode ? viewClaudeCode() : '',
      !isClaudeCode
        ? field('Base URL', text('chat.baseUrl', chat.baseUrl, provider.baseUrl || ''),
            'Leave blank to use the provider default. Any OpenAI-compatible server works here.')
        : '',
      isClaude || isClaudeCode
        ? field('Effort', select('chat.effort', chat.effort, [
            ['low', 'low — fastest'],
            ['medium', 'medium'],
            ['high', 'high'],
            ['xhigh', 'xhigh'],
            ['max', 'max — slowest, most thorough']
          ]), 'How hard Claude thinks before answering. A prompt preset can override this.')
        : '',
      field('Reasoning', check('chat.showThinking', chat.showThinking !== false,
        'Show the reasoning summary while it thinks')),
      isClaude
        ? field('Declines', check('chat.serverSideFallback', chat.serverSideFallback !== false,
            'Retry on a fallback model if a request is declined'))
        : '',
      field('System prompt',
        `<textarea class="box" data-path="chat.systemPrompt" rows="7" spellcheck="false">${esc(chat.systemPrompt)}</textarea>`,
        'Sets the voice and length of every answer.'),
      `<button class="btn danger" data-act="clearSession">Clear conversation</button>`,
      !provider.vision
        ? '<p class="fieldHint" style="margin-top:12px"><b>This model has no vision support</b> — screenshots will not be sent. Pick a vision model to use the screenshot hotkeys.</p>'
        : ''
    ].join('')
  }

  function viewWhisper () {
    const cfg = current.config.transcription.whisperCpp || {}
    const found = whisperStatus
    const install = current.platform === 'darwin'
      ? 'brew install whisper-cpp'
      : 'see github.com/ggml-org/whisper.cpp for a Windows build'

    const binary = !found
      ? '<p class="fieldHint" style="margin-bottom:12px">Looking for whisper.cpp…</p>'
      : found.binaryPath
        ? `<p class="fieldHint" style="margin-bottom:12px">Engine: <b>${esc(found.binaryPath)}</b></p>`
        : `<p class="fieldHint" style="margin-bottom:12px;color:var(--live)">${esc(found.error)}<br>Install it with <b>${esc(install)}</b>.</p>`

    const models = (found?.models || []).map((m) => {
      const active = found.modelName === m.file
      return `<div class="keyRow">
        <div class="who">${esc(m.label)}${active ? '<em>in use</em>' : ''}</div>
        <span class="mark ${m.installed ? 'set' : 'unset'}" style="flex:1 1 auto">${m.installed ? esc(m.installed) : 'not downloaded'}</span>
        ${m.installed
          ? ''
          : `<button class="btn" data-act="getModel" data-model="${esc(m.file)}">Download</button>`}
      </div>`
    }).join('')

    return [
      binary,
      found?.modelPath
        ? ''
        : '<p class="fieldHint" style="margin-bottom:12px;color:var(--live)">No model yet — download one below. Audio stays on this machine either way.</p>',
      '<p class="sectionLabel">Models</p>',
      `<p class="fieldHint" style="margin-bottom:12px">Bigger is more accurate and slower. <b>small</b> is the usual choice for languages other than English. Saved under ${esc(current.paths.config.replace(/config\.json$/, 'models/'))}</p>`,
      models,
      `<div id="whisperProgress" class="fieldHint" style="margin:8px 0 14px"></div>`,
      field('Model file', text('transcription.whisperCpp.modelPath', cfg.modelPath, found?.modelPath || 'auto'),
        'Blank picks the largest model found.'),
      field('Engine path', text('transcription.whisperCpp.binaryPath', cfg.binaryPath, found?.binaryPath || 'auto')),
      `<div class="row">
        ${field('Threads', number('transcription.whisperCpp.threads', cfg.threads || 0, 0, 32))}
        ${field('Output', select('transcription.whisperCpp.translate', cfg.translate ? 'yes' : 'no',
          [['no', 'original language'], ['yes', 'translate to English']]))}
      </div>`
    ].join('')
  }

  function viewVoice () {
    const voice = current.config.transcription
    const provider = current.providers.find((p) => p.id === voice.provider) || {}
    const ready = current.transcription
    const isLocal = provider.kind === 'whispercpp'

    const deviceOptions = [['', 'System default'], ...devices.map((d) => [d.id, d.label])]
    const macSystemAudio = current.platform === 'darwin'

    return [
      '<p class="sectionLabel">Speech to text</p>',
      ready.ok
        ? `<p class="fieldHint" style="margin-bottom:14px">Ready — <b>${esc(ready.provider)}</b> · ${esc(ready.model)}${ready.local ? ' · nothing leaves this machine' : ''}</p>`
        : `<p class="fieldHint" style="margin-bottom:14px;color:var(--live)">${esc(ready.reason)}</p>`,
      field('Provider', select('transcription.provider', voice.provider,
        current.providers
          .filter((p) => p.transcription || p.id === voice.provider)
          .map((p) => [p.id, p.transcription ? p.label : `${p.label} (no speech API)`]))),
      isLocal
        ? field('Language', text('transcription.language', voice.language, 'auto'),
            'A language code such as <b>id</b> or <b>en</b>. Naming it is about 40% faster than auto-detect, and more reliable on short clips.')
        : `<div class="row">
            ${field('Model', text('transcription.model', voice.model, provider.transcriptionModel || 'whisper-1'))}
            ${field('Language', text('transcription.language', voice.language, 'auto'))}
          </div>`,
      isLocal ? viewWhisper() : field('Base URL', text('transcription.baseUrl', voice.baseUrl, provider.baseUrl || '')),
      '<p class="sectionLabel">Recording</p>',
      field('Chunk length', number('transcription.chunkSeconds', voice.chunkSeconds, 0, 120),
        isLocal
          ? 'Seconds per pass while recording. <b>0</b> transcribes once when you stop — most accurate. <b>15–30</b> gives a rolling transcript during a long call and keeps the model warm.'
          : 'Seconds per upload while recording. <b>0</b> transcribes once when you stop — most accurate. <b>15–30</b> gives a rolling transcript during a long call. Values under 5 are ignored.'),
      '<p class="sectionLabel">Input</p>',
      field('Microphone', select('transcription.inputDeviceId', voice.inputDeviceId, deviceOptions),
        devices.length ? '' : 'Device names appear after you grant microphone access once.'),
      field('System audio', check('transcription.captureSystemAudio', voice.captureSystemAudio,
        'Also record what the other side says'),
      macSystemAudio
        ? 'macOS gives no system-audio tap to apps. Install a loopback driver such as <b>BlackHole</b>, route your meeting output through it, and select it as the microphone above.'
        : 'Windows mixes desktop loopback into the recording. If it fails, the app falls back to the microphone and says so.'),
      `<button class="btn danger" data-act="clearTranscript">Clear transcript</button>`
    ].join('')
  }

  function viewShots () {
    const capture = current.config.capture
    const win = current.config.window
    const needsPermission = current.platform === 'darwin' && current.screenPermission !== 'granted'

    return [
      '<p class="sectionLabel">Screenshots</p>',
      needsPermission
        ? `<div class="field"><p class="fieldHint" style="color:var(--live)">Screen Recording permission is <b>${esc(current.screenPermission)}</b>. Screenshots return an empty frame until it is granted.</p>
           <button class="btn" data-act="openScreenSettings" style="margin-top:8px">Open System Settings</button></div>`
        : '',
      field('Which screen', select('capture.display', capture.display, [
        ['cursor', 'Screen under the pointer'],
        ['primary', 'Primary screen'],
        ...current.displays.map((d) => [d.id, d.label])
      ])),
      `<div class="row">
        ${field('Max size (px)', number('capture.maxDimension', capture.maxDimension, 512, 4096))}
        ${field('Keep last', number('capture.keepLast', capture.keepLast, 1, 20))}
      </div>`,
      field('Region hotkey asks', select('capture.regionPrompt', capture.regionPrompt,
        [['', 'Nothing — just attach it'], ...current.config.prompts.map((p) => [p.id, p.label])]),
      'Longest side is scaled down before upload, which is what you pay for.'),
      '<p class="sectionLabel">Panel</p>',
      field('Opacity', `<input type="number" data-path="window.opacity" value="${win.opacity ?? 1}" min="0.25" max="1" step="0.05" />`),
      field('Capture protection', check('window.protection', win.protection !== false,
        'Hide this panel from screen share and screenshots')),
      field('Mouse', check('window.clickThrough', !!win.clickThrough,
        'Click-through — the panel ignores the mouse entirely')),
      field('Launch', check('window.showOnStart', win.showOnStart !== false,
        'Show the panel when VeilPilot starts')),
      `<p class="fieldHint">Config: <b>${esc(current.paths.config)}</b></p>`
    ].join('')
  }

  function viewPrompts () {
    const prompts = current.config.prompts || []
    const sources = [
      ['screenshot', 'screenshot'],
      ['transcript', 'transcript'],
      ['both', 'both']
    ]
    const efforts = [
      ['', 'default effort'], ['low', 'low'], ['medium', 'medium'],
      ['high', 'high'], ['xhigh', 'xhigh'], ['max', 'max']
    ]

    return [
      '<p class="sectionLabel">Prompt hotkeys</p>',
      '<p class="fieldHint" style="margin-bottom:14px">Each one is a hotkey that captures what it needs and asks straight away. Use <b>{{transcript}}</b> and <b>{{input}}</b> to drop in the live transcript or whatever is typed in the ask box.</p>',
      ...prompts.map((p, index) => `
        <div class="preset" data-index="${index}">
          <div class="presetTop">
            <input class="label" type="text" data-prompt="label" value="${esc(p.label)}" placeholder="Name" />
            ${hotkey(`prompt.${index}`, p.accelerator)}
            <select class="src" data-prompt="source">
              ${sources.map(([v, l]) => `<option value="${v}"${p.source === v ? ' selected' : ''}>${l}</option>`).join('')}
            </select>
          </div>
          <textarea class="box" data-prompt="text" rows="3" spellcheck="false">${esc(p.text)}</textarea>
          <div class="row" style="margin-top:9px">
            <select data-prompt="effort">
              ${efforts.map(([v, l]) => `<option value="${v}"${(p.effort || '') === v ? ' selected' : ''}>${l}</option>`).join('')}
            </select>
            <input type="text" data-prompt="model" value="${esc(p.model || '')}" placeholder="model override" />
            <button class="btn danger" data-act="dropPrompt">Remove</button>
          </div>
        </div>`),
      '<button class="btn" data-act="addPrompt">Add a prompt</button>'
    ].join('')
  }

  function viewKeys () {
    const h = current.config.hotkeys
    const rows = [
      ['toggleOverlay', 'Show / hide'],
      ['shotOnly', 'Attach shot'],
      ['regionShot', 'Region shot'],
      ['recordToggle', 'Transcribe'],
      ['send', 'Focus ask box'],
      ['cancel', 'Stop'],
      ['newSession', 'New session'],
      ['clickThrough', 'Click-through'],
      ['toggleProtection', 'Protection'],
      ['opacityUp', 'Opacity +'],
      ['opacityDown', 'Opacity −'],
      ['moveUp', 'Move up'],
      ['moveDown', 'Move down'],
      ['moveLeft', 'Move left'],
      ['moveRight', 'Move right']
    ]

    return [
      '<p class="sectionLabel">API keys</p>',
      '<p class="fieldHint" style="margin-bottom:14px">Stored encrypted by the OS keystore, never in the config file.</p>',
      ...current.providers.filter((p) => p.needsKey).map((p) => `
        <div class="keyRow">
          <div class="who">${esc(p.label)}<em>${esc(p.kind)}</em></div>
          <input type="password" data-key="${p.id}" placeholder="${current.keys[p.id] ? '•••••••• saved' : 'paste an API key'}" spellcheck="false" />
          <span class="mark ${current.keys[p.id] ? 'set' : 'unset'}">${current.keys[p.id] ? 'saved' : 'none'}</span>
        </div>
        ${p.keyUrl ? `<p class="fieldHint" style="margin:-6px 0 12px 105px"><a href="${esc(p.keyUrl)}" data-ext>${esc(p.keyUrl)}</a></p>` : ''}`),
      '<p class="sectionLabel">Global hotkeys</p>',
      '<p class="fieldHint" style="margin-bottom:14px">Click a box and press the combination. At least one modifier is required. A key another app already owns is reported as unavailable.</p>',
      ...rows.map(([key, label]) => `
        <div class="keyRow">
          <div class="who">${esc(label)}</div>
          ${hotkey(`hotkeys.${key}`, h[key])}
        </div>`),
      '<button class="btn" data-act="quit" style="margin-top:8px">Quit VeilPilot</button>'
    ].join('')
  }

  const VIEWS = { ai: viewAi, voice: viewVoice, shots: viewShots, prompts: viewPrompts, keys: viewKeys }

  function rebuild () {
    if (!current) return
    body.innerHTML = VIEWS[tab]()
    body.scrollTop = 0
    for (const node of tabs.children) {
      node.classList.toggle('is-on', node.dataset.tab === tab)
    }
    probeClaudeCode()
    probeWhisper()
  }

  async function probeWhisper (force = false) {
    const voice = current?.config.transcription
    if (!voice) return
    const provider = current.providers.find((p) => p.id === voice.provider)
    if (provider?.kind !== 'whispercpp') return

    const cfg = voice.whisperCpp || {}
    const key = `${cfg.binaryPath || ''}|${cfg.modelPath || ''}`
    if (!force && whisperProbe === key) return
    whisperProbe = key
    whisperStatus = await window.veil.detectWhisper()
    if (open && tab === 'voice') rebuild()
  }

  // Finding the CLI touches the filesystem, so it runs once per binary-path
  // value rather than on every render. The guard also stops the re-render it
  // triggers from looping.
  async function probeClaudeCode () {
    const chat = current?.config.chat
    if (!chat) return
    const provider = current.providers.find((p) => p.id === chat.provider)
    if (provider?.kind !== 'claudecode') return

    const key = chat.claudeCode.binaryPath || '(auto)'
    if (claudeCodeProbe === key) return
    claudeCodeProbe = key
    claudeCodeStatus = await window.veil.detectClaudeCode()
    if (open && tab === 'ai') rebuild()
  }

  /* ---------- events ---------- */

  tabs.addEventListener('click', (event) => {
    const next = event.target.dataset?.tab
    if (!next) return
    tab = next
    rebuild()
  })

  closeBtn.addEventListener('click', () => close())

  // Saved on change (blur), not on every keystroke: a save round-trip
  // re-renders and would steal the caret mid-word.
  body.addEventListener('change', async (event) => {
    const node = event.target
    const path = node.dataset.path
    if (path) {
      let value = node.type === 'checkbox'
        ? node.checked
        : node.type === 'number' ? Number(node.value) : node.value
      // One select stands in for a boolean, because "original language" reads
      // better than an unlabelled tick box.
      if (path.endsWith('.translate')) value = value === 'yes'
      // Switching provider changes which fields are relevant; a new binary or
      // model path has to be re-probed.
      const structural = path.endsWith('.provider') ||
        path === 'chat.claudeCode.binaryPath' ||
        path.startsWith('transcription.whisperCpp.')
      await window.veil.setConfig(nest(path, value))
      await hooks.refresh?.()
      if (structural) rebuild()
      return
    }

    const promptField = node.dataset.prompt
    if (promptField) {
      const index = Number(node.closest('.preset').dataset.index)
      const prompts = current.config.prompts.map((p, i) =>
        i === index ? { ...p, [promptField]: node.value } : p)
      await savePrompts(prompts)
    }
  })

  body.addEventListener('keydown', async (event) => {
    const node = event.target
    if (!node.dataset.hk) return
    event.preventDefault()

    if (event.key === 'Backspace' || event.key === 'Delete') {
      await writeHotkey(node.dataset.hk, '')
      return
    }
    const accelerator = accelFromEvent(event)
    if (!accelerator) return
    await writeHotkey(node.dataset.hk, accelerator)
  })

  body.addEventListener('focusin', (event) => {
    if (event.target.dataset.hk) event.target.classList.add('is-capturing')
  })
  body.addEventListener('focusout', (event) => {
    if (event.target.dataset.hk) event.target.classList.remove('is-capturing')
  })

  async function writeHotkey (path, accelerator) {
    if (path.startsWith('prompt.')) {
      const index = Number(path.split('.')[1])
      const prompts = current.config.prompts.map((p, i) =>
        i === index ? { ...p, accelerator } : p)
      await savePrompts(prompts)
    } else {
      await window.veil.setConfig(nest(path, accelerator))
      await hooks.refresh?.()
      rebuild()
    }
  }

  body.addEventListener('click', async (event) => {
    const link = event.target.closest('a[data-ext]')
    if (link) {
      event.preventDefault()
      window.veil.openExternal(link.getAttribute('href'))
      return
    }

    const action = event.target.dataset?.act
    if (!action) return

    if (action === 'addPrompt') {
      const prompts = [...current.config.prompts, {
        id: `custom_${Date.now().toString(36)}`,
        label: 'New prompt',
        accelerator: '',
        source: 'screenshot',
        text: 'Jelaskan apa yang ada di layar ini.'
      }]
      await savePrompts(prompts)
    } else if (action === 'dropPrompt') {
      const index = Number(event.target.closest('.preset').dataset.index)
      await savePrompts(current.config.prompts.filter((_p, i) => i !== index))
    } else if (action === 'clearSession') {
      await window.veil.clearSession()
      hooks.toast?.('info', 'Conversation cleared.')
    } else if (action === 'clearTranscript') {
      await window.veil.clearTranscript()
      hooks.toast?.('info', 'Transcript cleared.')
    } else if (action === 'getModel') {
      const file = event.target.dataset.model
      event.target.disabled = true
      event.target.textContent = 'Downloading…'
      const result = await window.veil.downloadWhisperModel(file)
      if (result?.ok) {
        await hooks.refresh?.()
        await probeWhisper(true)
        rebuild()
      } else {
        event.target.disabled = false
        event.target.textContent = 'Download'
      }
    } else if (action === 'openScreenSettings') {
      await window.veil.perms.openScreenSettings()
    } else if (action === 'quit') {
      await window.veil.win.quit()
    }
  })

  body.addEventListener('blur', async (event) => {
    const providerId = event.target.dataset?.key
    if (!providerId) return
    const value = event.target.value.trim()
    if (!value) return
    event.target.value = ''
    await window.veil.setKey(providerId, value)
    await hooks.refresh?.()
    rebuild()
    hooks.toast?.('info', 'API key saved.')
  }, true)

  /* ---------- api ---------- */

  function init (next) {
    hooks = next || {}
    // Written straight into the node: a 488 MB download would otherwise
    // re-render the whole tab a hundred times.
    window.veil.on.whisperProgress(({ file, percent, received, total }) => {
      const node = document.getElementById('whisperProgress')
      if (!node) return
      const mb = (n) => (n / 1048576).toFixed(0)
      node.innerHTML = `Downloading <b>${esc(file)}</b> — ${percent}% (${mb(received)} / ${mb(total)} MB)`
    })
  }

  function render (state) { current = state }

  async function openPanel (state) {
    current = state
    open = true
    panel.hidden = false
    rebuild()
    // Device labels are only available once microphone access exists, so the
    // list is fetched lazily and the view refreshed if it changed.
    const found = await Recorder.devices()
    if (found.length !== devices.length) {
      devices = found
      if (tab === 'voice') rebuild()
    } else {
      devices = found
    }
  }

  function close () {
    open = false
    panel.hidden = true
  }

  return { init, render, open: openPanel, close, isOpen: () => open, rebuild }
})()
