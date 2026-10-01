// Generates the 1024x1024 Familiar app icon (a friendly white "F" on the accent blob): node gen-icon.js app-icon.png, then
// pnpm dlx @tauri-apps/cli icon app-icon.png -o src-tauri/icons
const zlib = require("zlib");
const fs = require("fs");
const N = 1024;
const raw = Buffer.alloc(N * (N * 4 + 1));
for (let y = 0; y < N; y++) {
  for (let x = 0; x < N; x++) {
    const o = y * (N * 4 + 1) + 1 + x * 4;
    const r = 180;
    const cx = Math.min(Math.max(x, r), N - r);
    const cy = Math.min(Math.max(y, r), N - r);
    let c = [0, 0, 0, 0];
    if ((x - cx) ** 2 + (y - cy) ** 2 <= r * r) {
      c = [82, 105, 187, 255]; // accent #5269bb
      // "F": stem + top bar + shorter middle bar, with rounded ends
      const rr = (x0, y0, x1, y1, k) => {
        const qx = Math.min(Math.max(x, x0 + k), x1 - k);
        const qy = Math.min(Math.max(y, y0 + k), y1 - k);
        return (x - qx) ** 2 + (y - qy) ** 2 <= k * k;
      };
      if (rr(330, 250, 450, 780, 60) || rr(330, 250, 740, 370, 60) || rr(330, 470, 650, 590, 60)) c = [255, 255, 255, 255];
      // two little eyes for friendliness
      if ((x - 600) ** 2 + (y - 700) ** 2 <= 34 ** 2 || (x - 700) ** 2 + (y - 700) ** 2 <= 34 ** 2) c = [255, 255, 255, 255];
    }
    raw.set(c, o);
  }
}
const table = [];
for (let n = 0; n < 256; n++) {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  table[n] = c >>> 0;
}
const crc = (b) => {
  let r = 0xffffffff;
  for (const x of b) r = table[(r ^ x) & 255] ^ (r >>> 8);
  return (r ^ 0xffffffff) >>> 0;
};
const chunk = (t, d) => {
  const l = Buffer.alloc(4);
  l.writeUInt32BE(d.length);
  const td = Buffer.concat([Buffer.from(t), d]);
  const c = Buffer.alloc(4);
  c.writeUInt32BE(crc(td));
  return Buffer.concat([l, td, c]);
};
const ih = Buffer.alloc(13);
ih.writeUInt32BE(N, 0);
ih.writeUInt32BE(N, 4);
ih[8] = 8;
ih[9] = 6;
fs.writeFileSync(
  process.argv[2],
  Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ih), chunk("IDAT", zlib.deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]),
);
