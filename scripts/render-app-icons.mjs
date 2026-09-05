import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { crc32, deflateSync } from 'node:zlib';

const rendererPath = process.argv.slice(2).find(argument => argument !== '--check');
const require = createRequire(rendererPath ? resolve(rendererPath) : import.meta.url);
const { Resvg } = require('@resvg/resvg-js');
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sizes = [16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 256];
const atlasSizes = [...sizes, 512];
const atlasWidth = 512;
const atlasHeight = atlasSizes.reduce((total, size) => total + size, 0);
const source = await readFile(join(root, 'extra/logo/nebula-coverage.svg'), 'utf8');
const catalogSource = await readFile(join(root, 'nebula_settings/src/app_icon.rs'), 'utf8');
const palettePattern = /\w+\s*=>\s*\(\s*"([^"]+)"\s*,\s*"(\d+)"\s*,\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*0x([\dA-Fa-f]{6})\s*,\s*0x([\dA-Fa-f]{6})\s*,\s*0x([\dA-Fa-f]{6})\s*\)/g;
const palettes = [...catalogSource.matchAll(palettePattern)].map(([, key, number, nameZh, nameEn, tile, mark, border]) => ({
  key, number, nameZh, nameEn, tile: `#${tile}`, mark: `#${mark}`, border: `#${border}`,
}));
assert.equal(palettes.length, 25);
assert.equal(new Set(palettes.map(palette => palette.key)).size, 25);
assert.equal(new Set(palettes.map(palette => palette.number)).size, 25);
assert.equal(palettes[0].key, 'titanium');

function pngChunk(kind, payload) {
  const chunk = Buffer.alloc(payload.length + 12);
  chunk.writeUInt32BE(payload.length);
  chunk.write(kind, 4);
  payload.copy(chunk, 8);
  chunk.writeUInt32BE(crc32(chunk.subarray(4, -4)), chunk.length - 4);
  return chunk;
}

function paeth(left, above, upperLeft) {
  const predictor = left + above - upperLeft;
  const distanceLeft = Math.abs(predictor - left);
  const distanceAbove = Math.abs(predictor - above);
  const distanceUpperLeft = Math.abs(predictor - upperLeft);
  if (distanceLeft <= distanceAbove && distanceLeft <= distanceUpperLeft) return left;
  return distanceAbove <= distanceUpperLeft ? above : upperLeft;
}

function encodePng(pixels, width, height) {
  const stride = width * 4;
  const filtered = Buffer.alloc((stride + 1) * height);
  for (let row = 0; row < height; row++) {
    let bestScore = Infinity;
    for (const filter of [0, 1, 2, 4]) {
      const trial = Buffer.alloc(stride + 1);
      trial[0] = filter;
      let score = 0;
      for (let column = 0; column < stride; column++) {
        const offset = row * stride + column;
        const left = column >= 4 ? pixels[offset - 4] : 0;
        const above = row > 0 ? pixels[offset - stride] : 0;
        const upperLeft = row > 0 && column >= 4 ? pixels[offset - stride - 4] : 0;
        const prediction = filter === 4 ? paeth(left, above, upperLeft)
          : filter === 2 ? above : filter === 1 ? left : 0;
        const value = (pixels[offset] - prediction + 256) & 255;
        trial[column + 1] = value;
        score += Math.min(value, 256 - value);
      }
      if (score < bestScore) {
        trial.copy(filtered, row * (stride + 1));
        bestScore = score;
      }
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 6;
  return Buffer.concat([
    Buffer.from('89504e470d0a1a0a', 'hex'), pngChunk('IHDR', header),
    pngChunk('IDAT', deflateSync(filtered, { level: 9 })), pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function opticalSource(size) {
  if (size >= 64) return source;
  const strength = Math.min(1, (64 - size) / 40);
  const extent = 90 + 18 * strength;
  const inset = (128 - extent) / 2;
  const scale = extent * size / (128 * 128);
  const offset = inset * size / 128;
  const stroke = Math.max(1.5, Math.round((10 + 2 * strength) * scale));
  const snap = value => Math.round(value * 2) / 2;
  const local = value => Number(((value - offset) / scale).toFixed(6));
  const tip = snap(offset + 52 * scale);
  const middle = snap(offset + 65 * scale);
  const arm = Math.max(1.5, snap(13 * scale));
  const phase = stroke % 2 === 0 ? 0 : 0.5;
  const baseline = Math.round(offset + 80 * scale - phase) + phase;
  const prompt = `M${local(tip - arm)} ${local(middle - arm)}L${local(tip)} ${local(middle)}L${local(tip - arm)} ${local(middle + arm)}M${local(snap(offset + 69 * scale))} ${local(baseline)}H${local(snap(offset + 86 * scale))}`;
  return source.replace('x="19" y="19" width="90" height="90"', `x="${inset}" y="${inset}" width="${extent}" height="${extent}"`)
    .replace('M39 52L52 65L39 78M69 80H86', prompt)
    .replace('stroke-width="10"', `stroke-width="${stroke / scale}"`);
}

function coverage(size) {
  const factor = size <= 64 ? 8 : 4;
  const sampleWidth = size * factor;
  const rendered = new Resvg(opticalSource(size), {
    font: { loadSystemFonts: false }, fitTo: { mode: 'width', value: sampleWidth },
  }).render();
  const samples = rendered.pixels;
  const result = Buffer.alloc(size * size * 4);
  for (let row = 0; row < size; row++) {
    for (let column = 0; column < size; column++) {
      const sums = [0, 0, 0, 0];
      for (let vertical = 0; vertical < factor; vertical++) {
        for (let horizontal = 0; horizontal < factor; horizontal++) {
          const sample = ((row * factor + vertical) * sampleWidth + column * factor + horizontal) * 4;
          for (let channel = 0; channel < 4; channel++) sums[channel] += samples[sample + channel];
        }
      }
      const offset = (row * size + column) * 4;
      for (let channel = 0; channel < 4; channel++) {
        result[offset + channel] = Math.round(sums[channel] / (factor * factor));
      }
      if (result[offset] + result[offset + 1] + result[offset + 2] === 0) result[offset + 3] = 0;
    }
  }
  return result;
}

function colorize(weights, palette) {
  const colors = [palette.tile, palette.border, palette.mark].map(color =>
    [1, 3, 5].map(offset => Number.parseInt(color.slice(offset, offset + 2), 16)));
  const result = Buffer.alloc(weights.length);
  for (let offset = 0; offset < weights.length; offset += 4) {
    const total = weights[offset] + weights[offset + 1] + weights[offset + 2];
    if (total === 0 || weights[offset + 3] === 0) continue;
    for (let channel = 0; channel < 3; channel++) {
      const mixed = colors.reduce((sum, color, index) => sum + color[channel] * weights[offset + index], 0);
      result[offset + channel] = Math.floor((mixed + Math.floor(total / 2)) / total);
    }
    result[offset + 3] = weights[offset + 3];
  }
  return result;
}

function encodeIco(frames) {
  const header = Buffer.alloc(6 + frames.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(frames.length, 4);
  let offset = header.length;
  for (const [index, frame] of frames.entries()) {
    const size = sizes[index];
    const entry = 6 + index * 16;
    header.writeUInt8(size === 256 ? 0 : size, entry);
    header.writeUInt8(size === 256 ? 0 : size, entry + 1);
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(frame.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += frame.length;
  }
  return Buffer.concat([header, ...frames]);
}

const coverageFrames = new Map(atlasSizes.map(size => [size, coverage(size)]));
const atlas = Buffer.alloc(atlasWidth * atlasHeight * 4);
let atlasTop = 0;
for (const [size, weights] of coverageFrames) {
  for (let row = 0; row < size; row++) {
    weights.copy(atlas, ((atlasTop + row) * atlasWidth) * 4, row * size * 4, (row + 1) * size * 4);
  }
  atlasTop += size;
}
const atlasPng = encodePng(atlas, atlasWidth, atlasHeight);
const outputs = new Map([['extra/logo/nebula-coverage.png', atlasPng]]);
const largeCoverage = coverage(1024);
const aliases = { titanium: 'titanium', 'silver-violet': 'light', 'graphite-violet': 'dark' };
for (const palette of palettes.filter(palette => palette.key in aliases)) {
  const alias = aliases[palette.key];
  const png = encodePng(colorize(largeCoverage, palette), 1024, 1024);
  const ico = encodeIco(sizes.map(size => encodePng(colorize(coverageFrames.get(size), palette), size, size)));
  const svg = source.replace('Nebula icon coverage master', `Nebula ${palette.nameEn.toLowerCase()} app icon`)
    .replace('#FF0000', palette.tile).replace('#00FF00', palette.border).replace('#0000FF', palette.mark);
  outputs.set(`extra/logo/nebula-${alias}.svg`, Buffer.from(svg));
  outputs.set(`extra/logo/nebula-${alias}.png`, png);
  outputs.set(`nebula_app/windows/nebula-${alias}.ico`, ico);
  if (palette.key === 'titanium') {
    outputs.set('extra/logo/nebula.png', png);
    outputs.set('nebula_app/windows/nebula.ico', ico);
  }
}
const payloadBytes = atlasPng.length + outputs.get('nebula_app/windows/nebula.ico').length;
assert.ok(payloadBytes <= 96 * 1024, `Runtime icon payload exceeds 96 KiB: ${payloadBytes}`);
outputs.set('extra/logo/icon-catalog.json', Buffer.from(`${JSON.stringify({
  default: 'titanium', sizes, atlasSizes, atlasWidth, atlasHeight, payloadBytes, palettes,
}, null, 2)}\n`));
for (const [path, content] of outputs) {
  const destination = join(root, path);
  if (process.argv.includes('--check')) {
    assert.deepEqual(await readFile(destination), content, `Regenerate stale icon asset: ${path}`);
  } else {
    await writeFile(destination, content);
  }
}
console.log(`25 palettes; default titanium; ${sizes.length} ICO frames; ${payloadBytes} runtime image bytes (96 KiB budget).`);
console.log(`Coverage atlas: ${atlasPng.length} bytes; supersampling: 8x at 16–64px, 4x above; premultiplied area averaging.`);
