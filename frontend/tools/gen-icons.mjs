// Generates PWA icons with no image deps: a dark rounded tile with cc-screen's
// cyan "›" chevron motif (the same glyph the tmux status bar uses). Pure Node:
// hand-rolled PNG (RGBA, filter 0) + zlib deflate + a CRC32 table.
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = resolve(here, "../public");
mkdirSync(outDir, { recursive: true });

const BG = [15, 23, 32]; // #0f1720
const FG = [56, 189, 248]; // #38bdf8 cyan accent

const crcTable = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBuf, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

function distToSeg(px, py, ax, ay, bx, by) {
  const dx = bx - ax, dy = by - ay;
  const l2 = dx * dx + dy * dy || 1;
  let t = ((px - ax) * dx + (py - ay) * dy) / l2;
  t = Math.max(0, Math.min(1, t));
  const cx = ax + t * dx, cy = ay + t * dy;
  return Math.hypot(px - cx, py - cy);
}

function makePNG(size) {
  const px = new Uint8Array(size * size * 4);
  const r = size * 0.18; // corner radius
  const half = size * 0.14; // chevron half-thickness
  // chevron ">" points, centred-ish
  const cx = size * 0.46, cy = size * 0.5, w = size * 0.18, h = size * 0.22;
  const ax = cx - w, ay = cy - h, mx = cx + w, my = cy, bx = cx - w, by = cy + h;

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      // rounded-rect mask (full-bleed friendly for maskable)
      const insetX = Math.max(0, r - x) + Math.max(0, x - (size - 1 - r));
      const insetY = Math.max(0, r - y) + Math.max(0, y - (size - 1 - r));
      const inCorner = insetX > 0 && insetY > 0;
      const outside = inCorner && Math.hypot(insetX, insetY) > r;
      const i = (y * size + x) * 4;
      if (outside) {
        px[i] = px[i + 1] = px[i + 2] = px[i + 3] = 0;
        continue;
      }
      const d = Math.min(
        distToSeg(x, y, ax, ay, mx, my),
        distToSeg(x, y, mx, my, bx, by)
      );
      const col = d <= half ? FG : BG;
      px[i] = col[0]; px[i + 1] = col[1]; px[i + 2] = col[2]; px[i + 3] = 255;
    }
  }

  // raw scanlines, each prefixed by filter byte 0
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    Buffer.from(px.buffer, y * size * 4, size * 4).copy(raw, y * (size * 4 + 1) + 1);
  }

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;  // bit depth
  ihdr[9] = 6;  // colour type RGBA
  // 10,11,12 = compression/filter/interlace = 0

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

for (const [name, size] of [
  ["icon-192.png", 192],
  ["icon-512.png", 512],
  ["apple-touch-icon.png", 180],
  ["favicon.png", 48],
]) {
  writeFileSync(resolve(outDir, name), makePNG(size));
  console.log("wrote", name, size);
}
