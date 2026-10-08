'use strict'

const sdk = require('@anthropic-ai/sdk')

const Anthropic = sdk.default || sdk

// `fallbacks: "default"` lets the API re-run a policy-declined request on a
// suitable model inside the same call. Paired header must be exactly this one.
const FALLBACK_BETA = 'server-side-fallback-2026-07-01'

function toMessage (turn) {
  const images = turn.images || []
  if (!images.length) return { role: turn.role, content: turn.text }

  // Images before text: Claude reads the prompt against an image it has
  // already seen.
  return {
    role: turn.role,
    content: [
      ...images.map((img) => ({
        type: 'image',
        source: { type: 'base64', media_type: img.mime, data: img.base64 }
      })),
      { type: 'text', text: turn.text }
    ]
  }
}

function mapEvent (event) {
  if (event.type === 'content_block_start' && event.content_block?.type === 'fallback') {
    const to = event.content_block.to?.model
    return { type: 'note', text: `Routed to ${to || 'a fallback model'}` }
  }
  if (event.type !== 'content_block_delta') return null
  if (event.delta?.type === 'text_delta') return { type: 'text', text: event.delta.text }
  if (event.delta?.type === 'thinking_delta') return { type: 'thinking', text: event.delta.thinking }
  return null
}

// A 400 on the beta path means this key or model does not take the fallback
// parameter. Retry once on the plain endpoint instead of failing the answer.
function fallbackRejected (err) {
  if (err?.name === 'AbortError') return false
  // An SDK too old to expose client.beta.messages.stream throws a TypeError.
  if (err instanceof TypeError) return true
  const status = err?.status ?? err?.response?.status
  if (status !== 400 && status !== 404) return false
  return /fallback|beta|unsupported|unexpected/i.test(String(err?.message || ''))
}

async function * stream (opts) {
  const {
    baseUrl, apiKey, model, system, messages, maxTokens,
    effort, showThinking, serverSideFallback, signal
  } = opts

  const client = new Anthropic({
    apiKey,
    ...(baseUrl && !/api\.anthropic\.com$/.test(baseUrl) ? { baseURL: baseUrl } : {})
  })

  const params = {
    model,
    max_tokens: maxTokens,
    messages: messages.map(toMessage),
    // Adaptive is the only thinking mode on current models; 'summarized'
    // matters for a HUD because the default shows a long silent pause.
    thinking: { type: 'adaptive', display: showThinking ? 'summarized' : 'omitted' },
    ...(system ? { system } : {}),
    ...(effort ? { output_config: { effort } } : {})
  }

  for (let attempt = 0; ; attempt++) {
    const useFallback = serverSideFallback && attempt === 0
    let yielded = 0

    try {
      // Built inside the try so a synchronous rejection of the beta
      // parameters also falls through to the plain endpoint.
      const handle = useFallback
        ? client.beta.messages.stream(
          { ...params, betas: [FALLBACK_BETA], fallbacks: 'default' },
          { signal }
        )
        : client.messages.stream(params, { signal })

      for await (const event of handle) {
        const mapped = mapEvent(event)
        if (mapped) {
          yielded++
          yield mapped
        }
      }

      const final = await handle.finalMessage()
      // Check stop_reason before trusting content: a refusal carries the
      // reason in stop_details, not in the text.
      if (final.stop_reason === 'refusal') {
        yield {
          type: 'note',
          text: `Declined (${final.stop_details?.category || 'policy'}). ${final.stop_details?.explanation || ''}`.trim()
        }
      }
      yield {
        type: 'done',
        stopReason: final.stop_reason,
        stopDetails: final.stop_details,
        usage: final.usage,
        model: final.model
      }
      return
    } catch (err) {
      if (useFallback && yielded === 0 && fallbackRejected(err)) {
        console.warn('[anthropic] server-side fallback not accepted, retrying without it')
        continue
      }
      throw err
    }
  }
}

module.exports = { stream }
