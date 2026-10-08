'use strict'

const frame = document.getElementById('frame')
const veil = document.getElementById('veil')
const sel = document.getElementById('sel')
const size = document.getElementById('size')
const hint = document.getElementById('hint')

let origin = null

// The frozen screenshot is the backdrop, so the selection happens against a
// still frame — nothing moves underneath while you drag.
window.region.image().then((dataUrl) => {
  if (dataUrl) frame.style.backgroundImage = `url("${dataUrl}")`
})

const rectFrom = (a, b) => ({
  x: Math.min(a.x, b.x),
  y: Math.min(a.y, b.y),
  width: Math.abs(a.x - b.x),
  height: Math.abs(a.y - b.y)
})

function paint (rect) {
  sel.style.display = 'block'
  sel.style.left = `${rect.x}px`
  sel.style.top = `${rect.y}px`
  sel.style.width = `${rect.width}px`
  sel.style.height = `${rect.height}px`

  size.style.display = 'block'
  size.textContent = `${Math.round(rect.width)} × ${Math.round(rect.height)}`
  // Keep the readout inside the viewport when dragging toward an edge.
  const below = rect.y + rect.height + 10
  size.style.top = `${below + 24 > window.innerHeight ? Math.max(4, rect.y - 26) : below}px`
  size.style.left = `${Math.min(rect.x, window.innerWidth - 90)}px`
}

window.addEventListener('mousedown', (e) => {
  if (e.button !== 0) return
  origin = { x: e.clientX, y: e.clientY }
  veil.style.display = 'none'
  hint.style.display = 'none'
  paint(rectFrom(origin, origin))
})

window.addEventListener('mousemove', (e) => {
  if (!origin) return
  paint(rectFrom(origin, { x: e.clientX, y: e.clientY }))
})

window.addEventListener('mouseup', (e) => {
  if (!origin) return
  const rect = rectFrom(origin, { x: e.clientX, y: e.clientY })
  origin = null
  window.region.done(rect.width >= 8 && rect.height >= 8 ? rect : null)
})

window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') window.region.done(null)
})

// A right-click is the conventional cancel for a screenshot crosshair.
window.addEventListener('contextmenu', (e) => {
  e.preventDefault()
  window.region.done(null)
})
