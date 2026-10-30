// Manual smoke test: node web/scripts/wasm-smoke.mjs <pbf> <z> <x> <y> <out.map>
import fs from 'node:fs';
import init, { generate, compress_lzma, decompress_lzma, MapViewer } from '../pkg/wahoo_map.js';

const [pbf, z, x, y, out] = process.argv.slice(2);
await init({ module_or_path: fs.readFileSync(new URL('../pkg/wahoo_map_bg.wasm', import.meta.url)) });
const fd = fs.openSync(pbf, 'r');
const source = {
  size: fs.fstatSync(fd).size,
  read(offset, len) {
    const buf = new Uint8Array(len);
    const n = fs.readSync(fd, buf, 0, len, offset);
    return buf.subarray(0, n);
  },
};
let last = 0;
const t0 = Date.now();
const gen = generate(source, +z, +x, +y, (pass, done, total) => {
  const pct = Math.floor((done / total) * 10);
  if (pct !== last) { last = pct; console.log(`pass ${pass} ${pct * 10}% ${((Date.now() - t0) / 1000).toFixed(0)}s`); }
});
const map = gen.take_map();
console.log('map bytes', map.length, 'pois', gen.pois, 'ways', gen.ways, ((Date.now() - t0) / 1000).toFixed(0) + 's');
fs.writeFileSync(out, map);
const viewer = new MapViewer(map);
console.log(viewer.info());
console.log('tile 12/2113/1347 bytes', viewer.tile(12, 2113, 1347).length);
