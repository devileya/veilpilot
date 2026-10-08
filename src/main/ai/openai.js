'use strict'

const { events, describeError } = require('./sse')

// Covers OpenAI, GLM (Zhipu), Groq, OpenRouter, DeepSeek, Ollama, LM Studio,
// vLLM — anything exposing POST /chat/completions.
function toMessage (turn) {
  const images = turn.images || []
  if (!images.length) return { role: turn.role, content: turn.text }

  return {
    role: turn.role,
    content: [
      { type: 'text', text: turn.text },
      ...images.map((img) => ({
        type: 'image_url',
        image_url: { url: `data:${img.mime};base64,${img.base64}` }
      }))
    ]
  }
}

async function * stream (opts) {
  const { baseUrl, apiKey, model, system, messages, maxTokens, signal } = opts

  const headers = { 'content-type': 'application/json' }
  if (apiKey) headers.authorization = `Bearer ${apiKey}`
  // OpenRouter attributes traffic by these; harmless elsewhere.
  if (/openrouter\.ai/.test(baseUrl)) {
    headers['http-referer'] = 'https://github.com/veilpilot'
    headers['x-title'] = 'VeilPilot'
  }

  const response = await fetch(`${baseUrl}/chat/completions`, {
    method: 'POST',
    headers,
    signal,
    body: JSON.stringify({
      model,
      stream: true,
      max_tokens: maxTokens,
      messages: [
        ...(system ? [{ role: 'system', content: system }] : []),
        ...messages.map(toMessage)
      ]
    })
  })

  if (!response.ok) throw await describeError(response)

  let finishReason = null
  let usage = null

  for await (const chunk of events(response)) {
    if (chunk.usage) usage = chunk.usage
    const choice = chunk.choices && chunk.choices[0]
    if (!choice) continue
    if (choice.finish_reason) finishReason = choice.finish_reason

    const delta = choice.delta || {}
    // Reasoning models on OpenAI-compatible gateways surface chain-of-thought
    // under a non-standard key; show it where the UI shows Claude's thinking.
    const reasoning = delta.reasoning_content || delta.reasoning
    if (reasoning) yield { type: 'thinking', text: reasoning }
    if (typeof delta.content === 'string' && delta.content) {
      yield { type: 'text', text: delta.content }
    }
  }

  yield { type: 'done', stopReason: finishReason, usage, model }
}

module.exports = { stream }
