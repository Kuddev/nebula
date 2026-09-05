import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { crc32, deflateSync, inflateSync } from 'node:zlib';

const require = createRequire(process.argv[2] ? resolve(process.argv[2]) : import.meta.url);
const { Resvg } = require('@resvg/resvg-js');
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

function optimizePng(png) {
  const chunks = [];
  const data = [];
  for (let offset = 8; offset < png.length;) {
    const length = png.readUInt32BE(offset);
    const chunk = png.subarray(offset, offset + length + 12);
    chunks.push(chunk);
    if (chunk.toString('ascii', 4, 8) === 'IDAT') data.push(chunk.subarray(8, -4));
    offset += chunk.length;
  }
  const compressed = deflateSync(inflateSync(Buffer.concat(data)), { level: 9 });
  const replacement = Buffer.alloc(compressed.length + 12);
  replacement.writeUInt32BE(compressed.length);
  replacement.write('IDAT', 4);
  compressed.copy(replacement, 8);
  replacement.writeUInt32BE(crc32(replacement.subarray(4, -4)), replacement.length - 4);
  let inserted = false;
  return Buffer.concat([png.subarray(0, 8), ...chunks.flatMap(chunk => {
    if (chunk.toString('ascii', 4, 8) !== 'IDAT') return [chunk];
    if (inserted) return [];
    inserted = true;
    return [replacement];
  })]);
}

function render(source, size) {
  const opticalSource = size <= 24
    ? source.replace('x="19" y="19" width="90" height="90"', 'x="14" y="14" width="100" height="100"')
      .replace('stroke-width="10"', 'stroke-width="11"')
    : source;
  return optimizePng(new Resvg(opticalSource, {
    font: { loadSystemFonts: false },
    fitTo: { mode: 'width', value: size },
  }).render().asPng());
}

function encodeIco(source) {
  const frames = sizes.map(size => ({ size, png: render(source, size) }));
  const header = Buffer.alloc(6 + frames.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(frames.length, 4);
  let offset = header.length;

  for (const [index, { size, png }] of frames.entries()) {
    const entry = 6 + index * 16;
    header.writeUInt8(size === 256 ? 0 : size, entry);
    header.writeUInt8(size === 256 ? 0 : size, entry + 1);
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(png.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += png.length;
  }

  return Buffer.concat([header, ...frames.map(frame => frame.png)]);
}

let totalIcoBytes = 0;
for (const variant of ['light', 'dark', 'titanium']) {
  const source = await readFile(join(root, 'extra/logo', `nebula-${variant}.svg`), 'utf8');
  const png = render(source, 1024);
  const ico = encodeIco(source);
  await writeFile(join(root, 'extra/logo', `nebula-${variant}.png`), png);
  await writeFile(join(root, 'nebula_app/windows', `nebula-${variant}.ico`), ico);

  if (variant === 'light') {
    await writeFile(join(root, 'extra/logo/nebula.png'), png);
    await writeFile(join(root, 'nebula_app/windows/nebula.ico'), ico);
  }

  totalIcoBytes += ico.length;
  console.log(`${variant}: 1024px PNG; ICO frames ${sizes.join(', ')}px; ${ico.length} bytes`);
}
if (totalIcoBytes > 64 * 1024) throw new Error(`Icon resource budget exceeded: ${totalIcoBytes} bytes`);
console.log(`All three ICOs: ${totalIcoBytes} bytes (64 KiB budget); default: silver violet`);
