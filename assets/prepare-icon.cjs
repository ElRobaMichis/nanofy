// Package the approved artwork without redrawing it. Run with Node and sharp.
const sharp = require(process.env.NANOFY_SHARP || 'sharp');
const fs = require('fs');
const path = require('path');
async function main() {
  const source = await sharp(path.join(__dirname, 'resonancia-original.png'))
    .extract({left: 184, top: 180, width: 888, height: 888})
    .resize(256, 256).png().toBuffer();
  fs.writeFileSync(path.join(__dirname, 'nanofy.png'), source);
  const sizes = [16, 24, 32, 48, 64, 128, 256];
  const frames = await Promise.all(sizes.map(size => sharp(source).resize(size, size).png().toBuffer()));
  const header = Buffer.alloc(6 + 16 * sizes.length);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = header.length;
  frames.forEach((frame, i) => {
    const p = 6 + 16 * i;
    header[p] = header[p + 1] = sizes[i] % 256;
    header.writeUInt16LE(1, p + 4);
    header.writeUInt16LE(32, p + 6);
    header.writeUInt32LE(frame.length, p + 8);
    header.writeUInt32LE(offset, p + 12);
    offset += frame.length;
  });
  fs.writeFileSync(path.join(__dirname, 'nanofy.ico'), Buffer.concat([header, ...frames]));
  const rgba = await sharp(source).resize(32, 32).ensureAlpha().raw().toBuffer();
  if (rgba.length !== 32 * 32 * 4) throw new Error('Invalid RGBA size');
  fs.writeFileSync(path.join(__dirname, 'nanofy-32.rgba'), rgba);
}
main().catch(error => { console.error(error); process.exit(1); });
