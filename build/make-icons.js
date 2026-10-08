'use strict'

/* Writes the tray and app icons as PNGs with no image dependencies.
   The mark is a half-silvered square: lower-left filled, upper-right open,
   split by a hairline diagonal — a beamsplitter, which is what a teleprompter
   mirror and this app have in common. */

const fs = require('node:fs')
const path = require('node:path')
const zlib = require('node:zlib')

const OUT = __dirname

/* ---------- PNG encoder ---------- */

const CRC_TABLE = (() => {
  const table = new Int32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c
  }
  return table
})()

function crc32 (buffer) {
  let c = -1
  for (let i = 0; i < buffer.length; i++) c = CRC_TABLE[(c ^ buffer[i]) & 0xff] ^ (c >>> 8)
  return (c ^ -1) >>> 0
}

function chunk (type, data) {
  const length = Buffer.alloc(4)
  length.writeUInt32BE(data.length, 0)
  const tag = Buffer.from(type, 'ascii')
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(Buffer.concat([tag, data])), 0)
  return Buffer.concat([length, tag, data, crc])
}

function encodePng (width, height, rgba) {
  const stride = width * 4
  const raw = Buffer.alloc((stride + 1) * height)
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0 // filter: none
    rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride)
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(width, 0)
  ihdr.writeUInt32BE(height, 4)
  ihdr[8] = 8  // bit depth
  ihdr[9] = 6  // colour type: RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0))
  ])
}

/* ---------- the mark ---------- */

const LAMP = [0xe8, 0xa3, 0x3d]
const VOID = [0x15, 0x13, 0x1c]

// 4x supersampling, so the diagonal and the rounded corners stay clean at 16px.
function drawMark ({ size, inset, radius, fill, stroke, background }) {
  const SS = 4
  // Proportional so the outline reads at 16px and does not go slab-thick at 512.
  const strokeWidth = Math.max(1, size * 0.02) * SS
  const w = size * SS
  const box = { x0: inset * SS, y0: inset * SS, x1: (size - inset) * SS, y1: (size - inset) * SS }
  const r = radius * SS
  const rgba = Buffer.alloc(w * w * 4)

  const insideRounded = (x, y) => {
    if (x < box.x0 || x > box.x1 || y < box.y0 || y > box.y1) return false
    const cx = Math.min(Math.max(x, box.x0 + r), box.x1 - r)
    const cy = Math.min(Math.max(y, box.y0 + r), box.y1 - r)
    const dx = x - cx
    const dy = y - cy
    return dx * dx + dy * dy <= r * r
  }

  const edgeDistance = (x, y) => Math.min(
    x - box.x0, box.x1 - x, y - box.y0, box.y1 - y
  )

  const samples = new Float32Array(w * w * 4)

  for (let y = 0; y < w; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4
      let colour = null
      let alpha = 0

      if (insideRounded(x, y)) {
        const border = edgeDistance(x, y) < strokeWidth
        // The diagonal runs corner to corner of the box.
        const t = (x - box.x0) / (box.x1 - box.x0) + (y - box.y0) / (box.y1 - box.y0)
        const onDiagonal = Math.abs(t - 1) < 0.045
        const lowerLeft = t < 1

        if (border || onDiagonal) { colour = stroke; alpha = 1 } else if (lowerLeft) { colour = fill; alpha = 1 } else if (background) { colour = background; alpha = 1 }
      } else if (background) {
        colour = background
        alpha = 1
      }

      if (colour) {
        samples[i] = colour[0]
        samples[i + 1] = colour[1]
        samples[i + 2] = colour[2]
        samples[i + 3] = alpha * 255
      }
    }
  }

  // Box-filter back down to the target size.
  const out = Buffer.alloc(size * size * 4)
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let r0 = 0, g0 = 0, b0 = 0, a0 = 0
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const i = ((y * SS + sy) * w + (x * SS + sx)) * 4
          const a = samples[i + 3]
          r0 += samples[i] * a
          g0 += samples[i + 1] * a
          b0 += samples[i + 2] * a
          a0 += a
        }
      }
      const o = (y * size + x) * 4
      if (a0 > 0) {
        out[o] = Math.round(r0 / a0)
        out[o + 1] = Math.round(g0 / a0)
        out[o + 2] = Math.round(b0 / a0)
        out[o + 3] = Math.round(a0 / (SS * SS))
      }
    }
  }
  return out
}

function write (name, size, options) {
  const file = path.join(OUT, name)
  fs.writeFileSync(file, encodePng(size, size, drawMark({ size, ...options })))
  return name
}

// Template images are black-with-alpha; macOS recolours them for light and
// dark menu bars itself.
const BLACK = [0, 0, 0]

const written = [
  write('trayTemplate.png', 16, { inset: 2, radius: 2, fill: BLACK, stroke: BLACK }),
  write('trayTemplate@2x.png', 32, { inset: 4, radius: 4, fill: BLACK, stroke: BLACK }),
  write('icon.png', 512, { inset: 112, radius: 40, fill: LAMP, stroke: LAMP, background: VOID })
]

console.log(`[icons] wrote ${written.join(', ')}`)
