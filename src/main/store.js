'use strict'

const fs = require('node:fs')
const path = require('node:path')
const { app, safeStorage } = require('electron')
const { CONFIG, PROVIDERS } = require('./defaults')

let cache = null
let secrets = null

const configPath = () => path.join(app.getPath('userData'), 'config.json')
const secretsPath = () => path.join(app.getPath('userData'), 'secrets.bin')

function isPlainObject (v) {
  return !!v && typeof v === 'object' && !Array.isArray(v)
}

// Shallow-per-section merge: new defaults appear after an upgrade without
// discarding what the user set. Arrays (prompts) are replaced wholesale so a
// user who deletes a preset doesn't get it resurrected on next launch.
function merge (base, patch) {
  const out = Array.isArray(base) ? base.slice() : { ...base }
  for (const [k, v] of Object.entries(patch || {})) {
    if (isPlainObject(v) && isPlainObject(out[k])) out[k] = merge(out[k], v)
    else if (v !== undefined) out[k] = v
  }
  return out
}

/* A saved config always beats a new default, so changing a default only
   reaches people who have never run the app. Anything that should also reach
   existing installs needs a migration here, indexed by the version it
   upgrades FROM. */
const MIGRATIONS = {
  // 1 -> 2: transcription moved on-device. Only move people who never set a
  // cloud key — for them cloud transcription could not have worked anyway,
  // and anyone who did configure one keeps it.
  1: (cfg) => {
    if (cfg.transcription.provider === 'openai' && !getKey('openai')) {
      cfg.transcription.provider = 'local'
    }
  }
}

function readConfig () {
  if (cache) return cache
  let onDisk = {}
  try {
    onDisk = JSON.parse(fs.readFileSync(configPath(), 'utf8'))
  } catch (err) {
    if (err.code !== 'ENOENT') console.error('[store] config unreadable, using defaults:', err.message)
  }

  cache = merge(CONFIG, onDisk)

  // No version on disk means a fresh install, which starts on the current
  // defaults and needs no migrating.
  const from = Number(onDisk.version) || 0
  let migrated = false
  for (let v = from; v > 0 && v < CONFIG.version; v++) {
    if (!MIGRATIONS[v]) continue
    MIGRATIONS[v](cache)
    migrated = true
  }
  cache.version = CONFIG.version

  if (migrated || (from && from !== CONFIG.version)) {
    try {
      fs.writeFileSync(configPath(), JSON.stringify(cache, null, 2))
    } catch (err) {
      console.error('[store] could not persist migrated config:', err.message)
    }
  }

  return cache
}

function writeConfig (patch) {
  cache = merge(readConfig(), patch)
  try {
    fs.mkdirSync(path.dirname(configPath()), { recursive: true })
    fs.writeFileSync(configPath(), JSON.stringify(cache, null, 2))
  } catch (err) {
    console.error('[store] could not save config:', err.message)
  }
  return cache
}

function readSecrets () {
  if (secrets) return secrets
  secrets = {}
  try {
    const raw = fs.readFileSync(secretsPath())
    const text = raw.subarray(0, 6).toString('utf8') === 'plain:'
      ? raw.subarray(6).toString('utf8')
      : safeStorage.decryptString(raw)
    secrets = JSON.parse(text)
  } catch (err) {
    if (err.code !== 'ENOENT') console.error('[store] secrets unreadable:', err.message)
  }
  return secrets
}

function writeSecrets (next) {
  secrets = next
  const text = JSON.stringify(next)
  try {
    fs.mkdirSync(path.dirname(secretsPath()), { recursive: true })
    if (safeStorage.isEncryptionAvailable()) {
      fs.writeFileSync(secretsPath(), safeStorage.encryptString(text), { mode: 0o600 })
    } else {
      // Linux without a keyring, or a broken Keychain. Still better than
      // dropping the key, but say so loudly rather than pretending.
      console.warn('[store] OS keystore unavailable — API keys saved unencrypted at', secretsPath())
      fs.writeFileSync(secretsPath(), Buffer.concat([Buffer.from('plain:'), Buffer.from(text)]), { mode: 0o600 })
    }
  } catch (err) {
    console.error('[store] could not save API key:', err.message)
  }
}

function getKey (providerId) {
  return readSecrets()[providerId] || ''
}

function setKey (providerId, key) {
  const next = { ...readSecrets() }
  if (key) next[providerId] = key
  else delete next[providerId]
  writeSecrets(next)
}

function keyStatus () {
  const have = readSecrets()
  return Object.fromEntries(Object.keys(PROVIDERS).map((id) => [id, !!have[id]]))
}

// Collapses a config section + its provider preset into one resolved object,
// so callers never have to remember which blanks fall back to what.
function resolve (section) {
  const cfg = readConfig()[section]
  const preset = PROVIDERS[cfg.provider] || PROVIDERS.openai
  const isChat = section === 'chat'
  return {
    providerId: cfg.provider,
    label: preset.label,
    kind: preset.kind,
    vision: !!preset.vision,
    baseUrl: (cfg.baseUrl || preset.baseUrl || '').replace(/\/+$/, ''),
    model: cfg.model || (isChat ? preset.model : preset.transcriptionModel) || '',
    apiKey: getKey(cfg.provider),
    needsKey: preset.needsKey !== false,
    supportsTranscription: !!preset.transcription
  }
}

module.exports = {
  readConfig,
  writeConfig,
  getKey,
  setKey,
  keyStatus,
  resolve,
  configPath,
  secretsPath
}
