import fs from 'node:fs';
import init, { MapViewer } from '../pkg/wahoo_map.js';
import { VectorTile } from '@mapbox/vector-tile';
import { createRequire } from 'node:module'; const Pbf = createRequire(import.meta.url)('pbf'); const PbfC = Pbf;
await init({ module_or_path: fs.readFileSync(new URL('../pkg/wahoo_map_bg.wasm', import.meta.url)) });
const v = new MapViewer(new Uint8Array(fs.readFileSync(process.argv[2])));
for (const [z,x,y] of [[10,525,336],[12,2103,1344],[14,8413,5378],[17,67306,43026],[8,131,84]]) {
  const b = v.tile(z,x,y);
  const t = new VectorTile(new PbfC(b));
  const out = Object.entries(t.layers).map(([n,l]) => { let w=0; for(let i=0;i<l.length;i++){ l.feature(i).loadGeometry(); w++; } return `${n}:${w}`; });
  console.log(z,x,y,b.length,out.join(' '));
}
const t = new VectorTile(new PbfC(v.tile(14,8413,5378)));
console.log(JSON.stringify(t.layers.way?.feature(0).properties));




