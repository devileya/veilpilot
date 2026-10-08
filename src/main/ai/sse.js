'use strict'

// Minimal Server-Sent Events reader for the OpenAI-compatible wire format.
async function * events (response) {
  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''

  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })

      let index
      while ((index = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, index).replace(/\r$/, '')
        buffer = buffer.slice(index + 1)
        if (!line.startsWith('data:')) continue
        const payload = line.slice(5).trim()
        if (!payload) continue
        if (payload === '[DONE]') return
        try {
          yield JSON.parse(payload)
        } catch {
          // Providers occasionally emit keep-alive or comment frames that are
          // not JSON. Skipping is correct; throwing would kill the answer.
        }
      }
    }
  } finally {
    reader.cancel().catch(() => {})
  }
}

// Error bodies are JSON on every provider seen so far, but not reliably
// shaped, so dig for the usual fields before falling back to raw text.
async function describeError (response) {
  let body = ''
  try {
    body = await response.text()
  } catch { /* nothing more to learn */ }

  let detail = body.slice(0, 400)
  try {
    const parsed = JSON.parse(body)
    detail = parsed?.error?.message || parsed?.message || parsed?.error || detail
  } catch { /* keep the raw text */ }

  const hint = response.status === 401
    ? ' — check the API key in Settings'
    : response.status === 404
      ? ' — check the base URL and model name in Settings'
      : ''
  return new Error(`${response.status} ${response.statusText}: ${detail}${hint}`)
}

module.exports = { events, describeError }
