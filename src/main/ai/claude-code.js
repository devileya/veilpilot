'use strict'

const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const readline = require('node:readline')
const { spawn, execFile } = require('node:child_process')

/* Runs the local `claude` CLI in headless mode and maps its NDJSON stream onto
   the same shape the other adapters yield. The point is auth: this uses the
   login Claude Code already has, so no API key is stored in VeilPilot.

   Verified wire behaviour (CLI 2.1.280):
   - `--input-format stream-json` accepts base64 image blocks directly, so a
     screenshot needs no Read tool and no temp file.
   - It requires `--output-format stream-json`.
   - `{"type":"stream_event","event":...}` wraps raw Anthropic SSE events.
   - `{"type":"result",...}` is terminal and carries usage and real cost.
   - `--safe-mode --strict-mcp-config` skips hooks, MCP, plugins and CLAUDE.md;
     measured ~11s -> ~1.5s to first token. */

let cached = null

function candidatePaths () {
  const home = os.homedir()
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA || ''
    const localAppData = process.env.LOCALAPPDATA || ''
    return [
      path.join(home, 'claude', 'bin', 'claude.exe'),
      path.join(home, '.claude', 'bin', 'claude.exe'),
      path.join(appData, 'npm', 'claude.cmd'),
      path.join(localAppData, 'Programs', 'claude', 'claude.exe'),
      path.join(home, '.local', 'bin', 'claude.exe'),
      path.join(home, '.local', 'bin', 'claude.cmd')
    ]
  }
  return [
    path.join(home, '.local', 'bin', 'claude'),
    path.join(home, '.claude', 'bin', 'claude'),
    path.join(home, '.claude', 'local', 'claude'),
    '/opt/homebrew/bin/claude',
    '/usr/local/bin/claude',
    '/usr/bin/claude'
  ]
}

const executable = (file) => {
  try {
    fs.accessSync(file, fs.constants.X_OK)
    return fs.statSync(file).isFile()
  } catch {
    return false
  }
}

// A GUI app on macOS inherits a minimal PATH that does not include
// ~/.local/bin, which is where the installer puts the binary — so PATH alone
// is not enough to find it.
function resolveBinary (configured) {
  if (configured) {
    if (executable(configured)) return configured
    throw new Error(`Claude Code not found at ${configured}. Fix the path in settings › AI.`)
  }
  if (cached && executable(cached)) return cached

  const exts = process.platform === 'win32' ? ['.cmd', '.exe', ''] : ['']
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    if (!dir) continue
    for (const ext of exts) {
      const file = path.join(dir, `claude${ext}`)
      if (executable(file)) return (cached = file)
    }
  }
  for (const file of candidatePaths()) {
    if (executable(file)) return (cached = file)
  }

  throw new Error('Claude Code CLI not found. Install it, or set the binary path in settings › AI.')
}

const run = (binary, args) => new Promise((resolve) => {
  execFile(binary, args, { timeout: 15000 }, (err, stdout) =>
    resolve({ err, out: String(stdout || '').trim() }))
})

// Finding the binary is not enough: an installed-but-logged-out CLI fails at
// ask time with a confusing error, so the login is checked up front.
async function detect (configured) {
  let binary
  try {
    binary = resolveBinary(configured)
  } catch (err) {
    return { ok: false, error: err.message }
  }

  const [version, auth] = await Promise.all([
    run(binary, ['--version']),
    run(binary, ['auth', 'status'])
  ])

  if (version.err) {
    return { ok: false, path: binary, error: `Could not run it: ${version.err.message}` }
  }

  let account = null
  try {
    const parsed = JSON.parse(auth.out)
    account = {
      loggedIn: !!parsed.loggedIn,
      method: parsed.authMethod || null,
      email: parsed.email || null
    }
  } catch {
    account = null // older CLI without `auth status`; let the ask surface it
  }

  if (account && !account.loggedIn) {
    return {
      ok: false,
      path: binary,
      version: version.out,
      error: 'Claude Code is installed but not signed in. Run `claude auth login` in a terminal, then reopen this panel.'
    }
  }

  return { ok: true, path: binary, version: version.out, account }
}

function buildArgs (opts) {
  const args = [
    '-p',
    '--input-format', 'stream-json',
    '--output-format', 'stream-json',
    '--verbose',
    '--include-partial-messages',
    // Nothing is around to answer a permission prompt, so anything that would
    // ask is denied instead of hanging the panel forever.
    '--permission-prompts', 'none',
    '--tools', opts.allowTools ? 'default' : ''
  ]
  if (opts.fastStart) args.push('--safe-mode', '--strict-mcp-config')
  if (opts.system) args.push('--system-prompt', opts.system)
  if (opts.model) args.push('--model', opts.model)
  if (opts.effort) args.push('--effort', opts.effort)
  if (opts.maxBudgetUsd > 0) args.push('--max-budget-usd', String(opts.maxBudgetUsd))

  // Claude Code owns the conversation: the first ask names a session, every
  // later ask resumes it, so only the newest turn is ever sent.
  if (opts.resumeId) args.push('--resume', opts.resumeId)
  else if (opts.sessionId) args.push('--session-id', opts.sessionId)

  return args
}

function userMessage (turn) {
  const content = (turn.images || []).map((img) => ({
    type: 'image',
    source: { type: 'base64', media_type: img.mime, data: img.base64 }
  }))
  content.push({ type: 'text', text: turn.text })
  return JSON.stringify({ type: 'user', message: { role: 'user', content } })
}

async function * stream (opts) {
  const binary = resolveBinary(opts.binaryPath)
  const turn = opts.messages[opts.messages.length - 1]
  if (!turn) throw new Error('Nothing to send.')

  const child = spawn(binary, buildArgs(opts), {
    cwd: opts.cwd && fs.existsSync(opts.cwd) ? opts.cwd : os.tmpdir(),
    env: process.env,
    windowsHide: true,
    stdio: ['pipe', 'pipe', 'pipe']
  })

  let spawnError = null
  let stderr = ''
  child.on('error', (err) => { spawnError = err })
  child.stderr.on('data', (data) => {
    stderr = (stderr + data.toString()).slice(-4000)
  })

  const abort = () => { try { child.kill('SIGTERM') } catch { /* already gone */ } }
  opts.signal?.addEventListener('abort', abort, { once: true })

  // EPIPE if the CLI exits before reading stdin; the exit code tells the story.
  child.stdin.on('error', () => {})
  child.stdin.write(`${userMessage(turn)}\n`)
  child.stdin.end()

  let streamed = 0
  let sessionId = null

  try {
    const lines = readline.createInterface({ input: child.stdout, crlfDelay: Infinity })

    for await (const line of lines) {
      if (!line.trim()) continue
      let event
      try {
        event = JSON.parse(line)
      } catch {
        continue // non-JSON noise on stdout is not fatal
      }

      if (event.type === 'system' && event.subtype === 'init') {
        sessionId = event.session_id || sessionId
        continue
      }

      if (event.type === 'stream_event') {
        const inner = event.event
        if (inner?.type !== 'content_block_delta') continue
        if (inner.delta?.type === 'text_delta') {
          streamed += 1
          yield { type: 'text', text: inner.delta.text }
        } else if (inner.delta?.type === 'thinking_delta' && opts.showThinking) {
          yield { type: 'thinking', text: inner.delta.thinking }
        }
        continue
      }

      if (event.type === 'result') {
        if (event.is_error) {
          throw new Error(event.result || event.api_error_status || 'Claude Code reported an error.')
        }
        // Belt and braces: if partial messages were suppressed for any reason,
        // the terminal event still carries the whole answer.
        if (!streamed && event.result) yield { type: 'text', text: event.result }

        yield {
          type: 'done',
          stopReason: event.stop_reason,
          usage: event.usage,
          model: Object.keys(event.modelUsage || {})[0] || opts.model || 'claude code',
          costUsd: event.total_cost_usd,
          sessionId: event.session_id || sessionId
        }
        return
      }
    }

    // Fell off the end without a result event.
    if (opts.signal?.aborted) {
      const error = new Error('Aborted')
      error.name = 'AbortError'
      throw error
    }
    if (spawnError) throw new Error(`Could not start Claude Code: ${spawnError.message}`)

    const code = await new Promise((resolve) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      child.once('close', resolve)
    })
    throw new Error(
      `Claude Code exited (${code}) without answering.` +
      (stderr.trim() ? ` ${stderr.trim().split('\n').slice(-3).join(' ')}` : '')
    )
  } finally {
    opts.signal?.removeEventListener('abort', abort)
    if (child.exitCode === null) abort()
  }
}

module.exports = { stream, detect, resolveBinary }
