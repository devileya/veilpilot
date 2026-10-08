'use strict'

const store = require('./store')

const MAX_TURNS = 16
// Images are re-uploaded with every follow-up turn, so old ones are dropped
// from history: the cost is per-request, not per-conversation.
const TURNS_KEEPING_IMAGES = 2

let turns = []
let shots = []
let transcript = []
let shotSeq = 0

function addShot (shot) {
  const id = `shot_${++shotSeq}`
  shots.push({ id, ...shot })
  const keep = store.readConfig().capture.keepLast || 6
  if (shots.length > keep) shots = shots.slice(-keep)
  return id
}

const getShot = (id) => shots.find((s) => s.id === id) || null
const latestShot = () => shots[shots.length - 1] || null

function appendTranscript (text, at = Date.now()) {
  const clean = String(text || '').trim()
  if (!clean) return null
  const line = { at, text: clean }
  transcript.push(line)
  if (transcript.length > 400) transcript = transcript.slice(-400)
  return line
}

const transcriptLines = () => transcript.slice()

function transcriptText ({ limit = 120 } = {}) {
  return transcript.slice(-limit).map((l) => l.text).join('\n')
}

function pushTurn (turn) {
  turns.push(turn)
  if (turns.length > MAX_TURNS) turns = turns.slice(-MAX_TURNS)
}

// Neutral history the provider adapters translate from. Images survive only
// on the newest user turns.
function history () {
  const userIndexes = turns
    .map((t, i) => (t.role === 'user' ? i : -1))
    .filter((i) => i >= 0)
  const imageAllowed = new Set(userIndexes.slice(-TURNS_KEEPING_IMAGES))
  return turns.map((t, i) => ({
    role: t.role,
    text: t.text,
    images: imageAllowed.has(i) ? (t.images || []) : []
  }))
}

function clear () {
  turns = []
  shots = []
  transcript = []
  shotSeq = 0
}

function clearTranscript () {
  transcript = []
}

module.exports = {
  addShot,
  getShot,
  latestShot,
  appendTranscript,
  transcriptLines,
  transcriptText,
  clearTranscript,
  pushTurn,
  history,
  clear,
  turnCount: () => turns.length
}
