'use strict'

/* A small, escape-first markdown renderer. It is deliberately not a full
   CommonMark implementation: answers here are short, and an unclosed fence
   mid-stream must still render as a code block rather than throw. */
window.Markdown = (() => {
  const ENTITIES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }
  const escape = (value) => String(value).replace(/[&<>"']/g, (c) => ENTITIES[c])

  const SENTINEL = '\u0000'

  function inline (text) {
    // Code spans are pulled out first so emphasis never rewrites code.
    const spans = []
    let out = String(text).replace(/`([^`\n]+)`/g, (_m, code) => {
      spans.push(code)
      return `${SENTINEL}${spans.length - 1}${SENTINEL}`
    })

    out = escape(out)
    out = out.replace(
      /\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g,
      (_m, label, href) => `<a href="${escape(href)}" rel="noreferrer">${label}</a>`
    )
    out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    out = out.replace(/(^|[\s(])\*([^*\n]+)\*/g, '$1<em>$2</em>')
    out = out.replace(/(^|[\s(])_([^_\n]+)_/g, '$1<em>$2</em>')
    out = out.replace(/~~([^~\n]+)~~/g, '<s>$1</s>')
    out = out.replace(
      new RegExp(`${SENTINEL}(\\d+)${SENTINEL}`, 'g'),
      (_m, index) => `<code>${escape(spans[Number(index)])}</code>`
    )
    return out
  }

  const BLANK = /^\s*$/
  const FENCE = /^\s*```(\w*)\s*$/
  const FENCE_END = /^\s*```\s*$/
  const HEADING = /^(#{1,6})\s+(.*)$/
  const RULE = /^\s*(-{3,}|\*{3,}|_{3,})\s*$/
  const BULLET = /^\s*[-*+]\s+(.*)$/
  const NUMBER = /^\s*\d+[.)]\s+(.*)$/
  const QUOTE = /^\s*>\s?(.*)$/

  const startsBlock = (line) =>
    BLANK.test(line) || FENCE.test(line) || HEADING.test(line) ||
    BULLET.test(line) || NUMBER.test(line) || QUOTE.test(line)

  function render (source) {
    const lines = String(source || '').split('\n')
    const out = []
    let list = null
    let i = 0

    const flushList = () => {
      if (!list) return
      const items = list.items.map((item) => `<li>${inline(item)}</li>`).join('')
      out.push(`<${list.type}>${items}</${list.type}>`)
      list = null
    }

    const pushItem = (type, content) => {
      if (!list || list.type !== type) {
        flushList()
        list = { type, items: [] }
      }
      list.items.push(content)
    }

    while (i < lines.length) {
      const line = lines[i]

      const fence = line.match(FENCE)
      if (fence) {
        flushList()
        const lang = fence[1]
        const body = []
        i += 1
        while (i < lines.length && !FENCE_END.test(lines[i])) {
          body.push(lines[i])
          i += 1
        }
        i += 1 // closing fence, or past the end while still streaming
        const cls = lang ? ` class="lang-${escape(lang)}"` : ''
        out.push(`<pre><code${cls}>${escape(body.join('\n'))}</code></pre>`)
        continue
      }

      if (BLANK.test(line)) { flushList(); i += 1; continue }

      if (RULE.test(line)) { flushList(); out.push('<hr>'); i += 1; continue }

      const heading = line.match(HEADING)
      if (heading) {
        flushList()
        const level = Math.min(3, heading[1].length)
        out.push(`<h${level}>${inline(heading[2])}</h${level}>`)
        i += 1
        continue
      }

      const bullet = line.match(BULLET)
      if (bullet) { pushItem('ul', bullet[1]); i += 1; continue }

      const numbered = line.match(NUMBER)
      if (numbered) { pushItem('ol', numbered[1]); i += 1; continue }

      const quote = line.match(QUOTE)
      if (quote) {
        flushList()
        const body = [quote[1]]
        i += 1
        while (i < lines.length) {
          const more = lines[i].match(QUOTE)
          if (!more) break
          body.push(more[1])
          i += 1
        }
        out.push(`<blockquote>${inline(body.join(' '))}</blockquote>`)
        continue
      }

      flushList()
      const paragraph = [line]
      i += 1
      while (i < lines.length && !startsBlock(lines[i]) && !RULE.test(lines[i])) {
        paragraph.push(lines[i])
        i += 1
      }
      out.push(`<p>${inline(paragraph.join('\n')).replace(/\n/g, '<br>')}</p>`)
    }

    flushList()
    return out.join('')
  }

  return { render, escape }
})()
