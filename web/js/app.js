import { listVariants, themeToStyle } from './theme.js';

const TILE_ZOOM = 8;
const MIN_VIEW_ZOOM = 11;
const SAMPLE_STYLE_URL = 'https://raw.githubusercontent.com/bertt/WahooMapStyles/refs/heads/main/styles/18001/default/vtm-elemnt-enhanced.xml';
const DEFAULT_THEME = 'styles/vtm-elemnt.xml';
const DEFAULT_THEME_URL = 'https://raw.githubusercontent.com/bertt/WahooMapStyles/refs/heads/main/styles/18001/default/vtm-elemnt.xml';
const SAMPLE_URL = 'https://cdn.wahooligan.com/wahoo-maps/23/tiles/8/131/84.map.lzma';
const $ = (id) => document.getElementById(id);

const state = {
  themeDoc: null,
  mapInfo: null,
  baseName: null,
  pbf: null,
  busy: false,
  result: null, // { bytes: Uint8Array, name: string }
};

// ---- helpers -----------------------------------------------------------------------------
const fmtBytes = (n) => (n >= 1e9 ? `${(n / 1e9).toFixed(2)} GB` : n >= 1e6 ? `${(n / 1e6).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1e3))} kB`);
const fmtTime = (ms) => {
  const s = Math.round(ms / 1000);
  return s >= 60 ? `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s` : `${s}s`;
};
const show = (el, on = true) => el.classList.toggle('hidden', !on);
const showError = (id, message) => {
  const el = $(id);
  el.textContent = message || '';
  show(el, !!message);
};

function setProgress(prefix, fraction, text) {
  show($(`${prefix}-progress`));
  $(`${prefix}-fill`).style.width = `${Math.max(0, Math.min(1, fraction)) * 100}%`;
  $(`${prefix}-text`).textContent = text;
}
const hideProgress = (prefix) => show($(`${prefix}-progress`), false);

let sequence = 0;
function rpc(worker, type, args = {}, { transfer = [], onProgress } = {}) {
  return new Promise((resolve, reject) => {
    const id = ++sequence;
    const cleanup = () => {
      worker.removeEventListener('message', onMessage);
      worker.removeEventListener('error', onError);
    };
    const onMessage = ({ data }) => {
      if (data.id !== id) return;
      if ('progress' in data) return onProgress?.(data.progress);
      cleanup();
      if (data.error) reject(new Error(data.error));
      else resolve(data.result);
    };
    const onError = (e) => {
      cleanup();
      reject(new Error(e.message || 'The worker crashed, most likely because it ran out of memory.'));
    };
    worker.addEventListener('message', onMessage);
    worker.addEventListener('error', onError);
    worker.postMessage({ id, type, ...args }, transfer);
  });
}

const newWorker = () => new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
const viewerWorker = newWorker();

function saveBlob(bytes, name) {
  const url = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
  const a = Object.assign(document.createElement('a'), { href: url, download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

// ---- tabs --------------------------------------------------------------------------------
function selectTab(name) {
  for (const b of document.querySelectorAll('#tabs button[data-tab]')) b.classList.toggle('active', b.dataset.tab === name);
  show($('tab-viewer'), name === 'viewer');
  show($('tab-toolbox'), name === 'toolbox');
  if (name === 'viewer') map.resize();
  history.replaceState(null, '', name === 'viewer' ? location.pathname : `#${name}`);
}
for (const b of document.querySelectorAll('#tabs button[data-tab]')) b.addEventListener('click', () => selectTab(b.dataset.tab));
$('howto-open').addEventListener('click', () => $('howto').showModal());
$('howto').addEventListener('click', (e) => { if (e.target === $('howto')) $('howto').close(); });
for (const a of document.querySelectorAll('[data-goto]')) {
  a.addEventListener('click', (e) => {
    e.preventDefault();
    selectTab(a.dataset.goto);
  });
}

// ---- map ---------------------------------------------------------------------------------
// World basemap shown until a map is opened. OpenFreeMap needs no API key; plain OSM tiles are the fallback.
async function getBasemapStyle() {
  try {
    const response = await fetch('https://tiles.openfreemap.org/styles/positron');
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return await response.json();
  } catch {
    return {
      version: 8,
      sources: { osm: { type: 'raster', tiles: ['https://tile.openstreetmap.org/{z}/{x}/{y}.png'], tileSize: 256, maxzoom: 19, attribution: '© OpenStreetMap contributors' } },
      layers: [{ id: 'osm', type: 'raster', source: 'osm' }],
    };
  }
}

const map = new maplibregl.Map({
  container: 'map',
  style: await getBasemapStyle(),
  center: [10, 30],
  zoom: 1.6,
  maxZoom: 19,
  attributionControl: { compact: true, customAttribution: 'Map data © OpenStreetMap contributors' },
});
map.addControl(new maplibregl.NavigationControl({ visualizePitch: false }), 'top-right');
map.addControl(new maplibregl.ScaleControl(), 'bottom-left');
window.__map = map; // handy for debugging and end-to-end tests

maplibregl.addProtocol('mapsforge', async (params) => {
  const [z, x, y] = params.url.replace('mapsforge://', '').split('/').map(Number);
  return { data: await rpc(viewerWorker, 'tile', { z, x, y }) };
});

map.on('zoom', () => show($('zoom-hint'), !!state.mapInfo && map.getZoom() < MIN_VIEW_ZOOM));
map.on('click', showAttributes);

// ---- attributes popup --------------------------------------------------------------------
let popup = null;

const EMPTY_FC = { type: 'FeatureCollection', features: [] };

// Highlight layers live on top of the translated theme; they are re-added after every setStyle.
function ensureSelectionLayers() {
  if (map.getSource('selection')) return;
  map.addSource('selection', { type: 'geojson', data: EMPTY_FC });
  const poly = ['==', ['geometry-type'], 'Polygon'];
  const line = ['==', ['geometry-type'], 'LineString'];
  const point = ['==', ['geometry-type'], 'Point'];
  map.addLayer({ id: 'sel-fill', type: 'fill', source: 'selection', filter: poly, paint: { 'fill-color': '#ff2d95', 'fill-opacity': 0.2 } });
  map.addLayer({ id: 'sel-outline', type: 'line', source: 'selection', filter: poly, paint: { 'line-color': '#ff2d95', 'line-width': 3 } });
  map.addLayer({ id: 'sel-line-casing', type: 'line', source: 'selection', filter: line, layout: { 'line-cap': 'round', 'line-join': 'round' }, paint: { 'line-color': '#ffffff', 'line-width': 9 } });
  map.addLayer({ id: 'sel-line', type: 'line', source: 'selection', filter: line, layout: { 'line-cap': 'round', 'line-join': 'round' }, paint: { 'line-color': '#ff2d95', 'line-width': 5 } });
  map.addLayer({ id: 'sel-point', type: 'circle', source: 'selection', filter: point, paint: { 'circle-radius': 9, 'circle-color': '#ff2d95', 'circle-stroke-color': '#ffffff', 'circle-stroke-width': 3 } });
}

function setSelection(features) {
  if (!map.isStyleLoaded() && !map.getSource('selection')) return;
  ensureSelectionLayers();
  map.getSource('selection').setData({
    type: 'FeatureCollection',
    features: features.map((f) => ({ type: 'Feature', geometry: f.geometry, properties: {} })),
  });
}

map.on('style.load', () => setSelection([]));

function showAttributes(e) {
  if (!state.mapInfo) return;
  const r = 5;
  const found = map.queryRenderedFeatures(
    [[e.point.x - r, e.point.y - r], [e.point.x + r, e.point.y + r]],
    { layers: ['hit-poi', 'hit-line', 'hit-area'] },
  );
  const seen = new Set();
  const features = found.filter((f) => {
    const key = `${f.layer.id}|${JSON.stringify(f.properties)}`;
    return !seen.has(key) && seen.add(key);
  });
  popup?.remove();
  if (!features.length) return;
  setSelection([features[0]]);

  const kinds = { 'hit-poi': 'Point', 'hit-line': 'Line', 'hit-area': 'Area' };
  const root = document.createElement('div');
  root.className = 'feats';
  for (const f of features.slice(0, 12)) {
    const box = document.createElement('div');
    box.className = 'feat';
    if (!root.firstChild) box.classList.add('active');
    box.addEventListener('mouseenter', () => {
      setSelection([f]);
      root.querySelectorAll('.feat.active').forEach((b) => b.classList.remove('active'));
      box.classList.add('active');
    });
    const head = document.createElement('div');
    head.className = 'feat-head';
    const badge = document.createElement('span');
    badge.className = `badge ${f.layer.id}`;
    badge.textContent = kinds[f.layer.id];
    const title = document.createElement('strong');
    const props = f.properties;
    const main = ['highway', 'amenity', 'shop', 'natural', 'waterway', 'landuse', 'building', 'railway', 'leisure', 'place']
      .find((k) => props[k]);
    title.textContent = props.name || (main ? `${main}=${props[main]}` : 'Unnamed feature');
    head.append(badge, title);
    const table = document.createElement('table');
    const entries = Object.entries(props).sort(([a], [b]) => (a === 'name' ? -1 : b === 'name' ? 1 : a.localeCompare(b)));
    for (const [key, value] of entries) {
      const tr = table.insertRow();
      tr.insertCell().textContent = key;
      const cell = tr.insertCell();
      const text = String(value);
      if (/^https?:\/\//.test(text)) {
        const a = document.createElement('a');
        a.href = text;
        a.target = '_blank';
        a.rel = 'noopener';
        a.textContent = text;
        cell.append(a);
      } else {
        cell.textContent = text;
      }
    }
    box.append(head, table);
    root.append(box);
  }
  if (features.length > 12) {
    const more = document.createElement('div');
    more.className = 'feat more';
    more.textContent = `… and ${features.length - 12} more`;
    root.append(more);
  }
  popup = new maplibregl.Popup({ maxWidth: '400px', className: 'attr-popup' }).setLngLat(e.lngLat).setDOMContent(root).addTo(map);
  const current = popup;
  popup.on('close', () => { if (popup === current) setSelection([]); });
}

// ---- styles ------------------------------------------------------------------------------
function parseTheme(text) {
  const doc = new DOMParser().parseFromString(text, 'application/xml');
  if (doc.getElementsByTagName('parsererror').length || !doc.getElementsByTagName('rendertheme').length) {
    throw new Error('This is not a valid render theme (expected an XML file with a <rendertheme> root).');
  }
  return doc;
}

// Reduce the theme's device/trail variants to a Light and a Dark option for the default device.
function lightDarkOptions(menu) {
  const isDark = (v) => /dark/i.test(v.id) || v.cats.some((c) => /dark/i.test(c));
  const norm = (v) => v.cats.map((c) => c.replace(/-?dark/i, '')).join('|');
  const base = menu.variants.find((v) => v.id === menu.defaultId) || menu.variants[0];
  if (!base) return [];
  const same = menu.variants.filter((v) => norm(v) === norm(base));
  const light = same.find((v) => !isDark(v)) || menu.variants.find((v) => !isDark(v));
  const dark = same.find(isDark) || menu.variants.find(isDark);
  const options = [];
  if (light) options.push({ id: light.id, name: 'Light' });
  if (dark) options.push({ id: dark.id, name: 'Dark' });
  return options.length ? options : menu.variants.map((v) => ({ id: v.id, name: v.name || v.id }));
}

function applyTheme(doc, label, meta) {
  const menu = listVariants(doc); // throws on an invalid theme
  themeToStyle(doc, { tileUrl: 'mapsforge://{z}/{x}/{y}' }); // dry run to surface errors early
  state.themeDoc = doc;
  const select = $('variant');
  const options = lightDarkOptions(menu);
  select.replaceChildren(...options.map((o) => new Option(o.name, o.id)));
  select.value = (options.find((o) => o.name === 'Dark') || options[0])?.id ?? '';
  select.disabled = !options.length;
  $('style-name').textContent = label;
  $('style-meta').textContent = meta || '';
  showError('style-error', '');
  applyViewStyle();
}

async function loadDefaultTheme() {
  let text;
  let meta = `Loaded from ${DEFAULT_THEME_URL}`;
  try {
    const response = await fetch(DEFAULT_THEME_URL);
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    text = await response.text();
  } catch {
    text = await (await fetch(DEFAULT_THEME)).text();
    meta = 'Bundled copy (could not reach GitHub)';
  }
  applyTheme(parseTheme(text), 'Wahoo ELEMNT default (18001)', meta);
}

function buildViewStyle() {
  const style = themeToStyle(state.themeDoc, {
    variantId: $('variant').value || undefined,
    tileUrl: 'mapsforge://{z}/{x}/{y}',
    minzoom: MIN_VIEW_ZOOM,
    maxzoom: Math.min(17, state.mapInfo?.maxzoom ?? 17),
  });
  const clear = 'rgba(0,0,0,0)';
  // Invisible layers that make every feature clickable, also those the theme does not draw.
  style.layers.push(
    { id: 'hit-area', type: 'fill', source: 'map', 'source-layer': 'way', filter: ['==', ['geometry-type'], 'Polygon'], paint: { 'fill-color': clear } },
    { id: 'hit-line', type: 'line', source: 'map', 'source-layer': 'way', filter: ['==', ['geometry-type'], 'LineString'], paint: { 'line-color': clear, 'line-width': 10 } },
    { id: 'hit-poi', type: 'circle', source: 'map', 'source-layer': 'poi', paint: { 'circle-color': clear, 'circle-radius': 9 } },
  );
  return style;
}

function applyViewStyle() {
  if (!state.mapInfo || !state.themeDoc) return;
  try {
    map.setStyle(buildViewStyle());
    showError('style-error', '');
  } catch (err) {
    showError('style-error', `Could not apply the theme: ${err.message}`);
  }
}

$('variant').addEventListener('change', applyViewStyle);

for (const button of document.querySelectorAll('#style-mode button')) {
  button.addEventListener('click', async () => {
    document.querySelectorAll('#style-mode button').forEach((b) => b.classList.toggle('active', b === button));
    const mode = button.dataset.mode;
    show($('style-url-row'), mode === 'url');
    show($('style-upload-row'), mode === 'upload');
    if (mode === 'default') {
      try {
        await loadDefaultTheme();
      } catch (err) {
        showError('style-error', err.message);
      }
    }
  });
}

$('style-url-go').addEventListener('click', () => loadStyleUrl($('style-url').value.trim()));
$('style-sample').addEventListener('click', () => {
  document.querySelectorAll('#style-mode button').forEach((b) => b.classList.toggle('active', b.dataset.mode === 'url'));
  show($('style-url-row'), true);
  show($('style-upload-row'), false);
  $('style-url').value = SAMPLE_STYLE_URL;
  loadStyleUrl(SAMPLE_STYLE_URL);
});

async function loadStyleUrl(url) {
  if (!url) return;
  try {
    const response = await fetch(url);
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    applyTheme(parseTheme(await response.text()), 'Custom style (URL)', url);
  } catch (err) {
    showError('style-error', `Could not load the theme (the server must allow cross-origin requests): ${err.message}`);
  }
}

$('style-file').addEventListener('change', async (e) => {
  const file = e.target.files[0];
  if (!file) return;
  try {
    applyTheme(parseTheme(await file.text()), 'Custom style (upload)', file.name);
  } catch (err) {
    showError('style-error', err.message);
  }
});

// ---- viewer: opening a map ---------------------------------------------------------------
async function openMap(buffer, baseName, title) {
  const { info, size } = await rpc(viewerWorker, 'open', { buffer }, { transfer: [buffer] });
  state.mapInfo = info;
  state.baseName = baseName;
  popup?.remove();
  $('map-title').textContent = `${title} · ${fmtBytes(size)} · click a feature for its attributes`;
  show($('toolbar'));
  show($('empty'), false);
  applyViewStyle();
  const [w, s, e, n] = info.bounds;
  // The tile layer starts at MIN_VIEW_ZOOM, so zoom straight to the middle of the map.
  map.jumpTo({ center: [(w + e) / 2, (s + n) / 2], zoom: MIN_VIEW_ZOOM + 1 });
  show($('zoom-hint'), false);
}

const baseNameOf = (name) => name.replace(/[?#].*$/, '').split('/').pop().replace(/\.lzma$/i, '').replace(/\.map$/i, '') || 'map';

async function loadFile(file) {
  if (!file) return;
  showError('load-error', '');
  try {
    setProgress('load', 0.5, `Reading ${file.name}…`);
    await openMap(await file.arrayBuffer(), baseNameOf(file.name), file.name);
    hideProgress('load');
  } catch (err) {
    hideProgress('load');
    showError('load-error', err.message);
  }
}

$('map-file').addEventListener('change', (e) => {
  loadFile(e.target.files[0]);
  e.target.value = '';
});

// Drag & drop a .map / .map.lzma onto the upload box or anywhere on the map.
for (const target of [document.querySelector('label.drop.small'), document.querySelector('.stage')]) {
  for (const type of ['dragenter', 'dragover']) target.addEventListener(type, (e) => { e.preventDefault(); target.classList.add('over'); });
  for (const type of ['dragleave', 'drop']) target.addEventListener(type, (e) => { e.preventDefault(); target.classList.remove('over'); });
  target.addEventListener('drop', (e) => loadFile(e.dataTransfer.files[0]));
}

$('map-url-go').addEventListener('click', () => loadUrl($('map-url').value.trim()));
$('load-sample').addEventListener('click', () => {
  $('map-url').value = SAMPLE_URL;
  loadUrl(SAMPLE_URL);
});

async function loadUrl(url) {
  if (!url) return;
  showError('load-error', '');
  try {
    setProgress('load', 0, 'Downloading…');
    let response;
    try {
      response = await fetch(url);
    } catch {
      throw new Error('Download failed. The server must allow cross-origin requests (CORS); otherwise download the file and upload it instead.');
    }
    if (!response.ok) throw new Error(`Download failed: HTTP ${response.status}`);
    const total = Number(response.headers.get('content-length')) || 0;
    const reader = response.body.getReader();
    const chunks = [];
    let received = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      received += value.length;
      setProgress('load', total ? received / total : 0.5, `Downloading… ${fmtBytes(received)}${total ? ` of ${fmtBytes(total)}` : ''}`);
    }
    const bytes = new Uint8Array(received);
    let offset = 0;
    for (const c of chunks) {
      bytes.set(c, offset);
      offset += c.length;
    }
    setProgress('load', 1, 'Decoding…');
    await openMap(bytes.buffer, baseNameOf(url), url.replace(/[?#].*$/, '').split('/').pop());
    hideProgress('load');
  } catch (err) {
    hideProgress('load');
    showError('load-error', err.message);
  }
}

// ---- viewer: opening a map (downloads live in the Toolbox) ------------------------------
async function compressAndSave(prefix, errorId, name, run) {
  showError(errorId, '');
  const started = performance.now();
  try {
    setProgress(prefix, 0, 'Compressing…');
    const bytes = await run(({ done, total }) =>
      setProgress(prefix, done / total, `Compressing… ${Math.round((done / total) * 100)}% · ${fmtTime(performance.now() - started)}`));
    setProgress(prefix, 1, `Compressed to ${fmtBytes(bytes.length)} in ${fmtTime(performance.now() - started)}.`);
    saveBlob(bytes, name);
  } catch (err) {
    hideProgress(prefix);
    showError(errorId, `Compression failed: ${err.message}`);
  }
}

// ---- toolbox: OSM PBF -> Wahoo map -------------------------------------------------------
const lonOf = (x) => (x / 2 ** TILE_ZOOM) * 360 - 180;
const latOf = (y) => (Math.atan(Math.sinh(Math.PI * (1 - (2 * y) / 2 ** TILE_ZOOM))) * 180) / Math.PI;

function currentTile() {
  const x = Number($('tile-x').value);
  const y = Number($('tile-y').value);
  const ok = $('tile-x').value !== '' && $('tile-y').value !== '' && Number.isInteger(x) && Number.isInteger(y) && x >= 0 && y >= 0 && x < 2 ** TILE_ZOOM && y < 2 ** TILE_ZOOM;
  return ok ? { x, y } : null;
}

function updateTool() {
  const t = currentTile();
  const info = $('tile-info');
  info.classList.toggle('empty', !t);
  if (t) {
    info.innerHTML = `<b>${TILE_ZOOM}/${t.x}/${t.y}</b> — lon ${lonOf(t.x).toFixed(3)} … ${lonOf(t.x + 1).toFixed(3)}, lat ${latOf(t.y + 1).toFixed(3)} … ${latOf(t.y).toFixed(3)}<br>Output file: <code>${t.y}.map.lzma</code>`;
  } else {
    info.textContent = `Enter a column and row (0–${2 ** TILE_ZOOM - 1}), e.g. 131 and 84 for the Netherlands.`;
  }
  $('generate').disabled = state.busy || !t || !state.pbf;
}

function setPbf(file) {
  state.pbf = file || null;
  $('pbf-name').textContent = file ? file.name : 'Drop an .osm.pbf file here or click to browse';
  $('pbf-size').textContent = file ? fmtBytes(file.size) : 'The file never leaves your computer.';
  updateTool();
}

$('pbf-file').addEventListener('change', (e) => setPbf(e.target.files[0]));
$('tile-x').addEventListener('input', updateTool);
$('tile-y').addEventListener('input', updateTool);
const drop = $('pbf-drop');
for (const type of ['dragenter', 'dragover']) drop.addEventListener(type, (e) => { e.preventDefault(); drop.classList.add('over'); });
for (const type of ['dragleave', 'drop']) drop.addEventListener(type, (e) => { e.preventDefault(); drop.classList.remove('over'); });
drop.addEventListener('drop', (e) => setPbf(e.dataTransfer.files[0]));

$('generate').addEventListener('click', async () => {
  const { x, y } = currentTile();
  showError('gen-error', '');
  show($('result'), false);
  state.result = null;
  state.busy = true;
  updateTool();
  const started = performance.now();
  const worker = newWorker();
  try {
    setProgress('gen', 0, 'Starting…');
    const result = await rpc(worker, 'generate', { file: state.pbf, z: TILE_ZOOM, x, y }, {
      onProgress: ({ pass, done, total }) => {
        const label = pass === 1 ? 'Pass 1/2: scanning relations' : 'Pass 2/2: reading features';
        setProgress('gen', ((pass - 1) + done / total) / 2, `${label} — ${fmtBytes(done)} of ${fmtBytes(total)} · ${fmtTime(performance.now() - started)}`);
      },
    });
    worker.terminate(); // frees the large WebAssembly memory of the generator
    state.result = { bytes: result.bytes, name: String(y), tile: `${TILE_ZOOM}/${x}/${y}` };
    setProgress('gen', 1, `Done in ${fmtTime(performance.now() - started)}.`);
    $('result-text').textContent = `Tile ${state.result.tile}: ${result.pois.toLocaleString()} POIs, ${result.ways.toLocaleString()} ways, ${fmtBytes(result.bytes.length)} uncompressed.`;
    hideProgress('res');
    show($('result'));
  } catch (err) {
    worker.terminate();
    hideProgress('gen');
    showError('gen-error', `Conversion failed: ${err.message}`);
  } finally {
    state.busy = false;
    updateTool();
  }
});

$('res-map').addEventListener('click', () => saveBlob(state.result.bytes, `${state.result.name}.map`));

$('res-lzma').addEventListener('click', async () => {
  const { bytes, name } = state.result;
  $('res-lzma').disabled = true;
  const worker = newWorker();
  await compressAndSave('res', 'res-error', `${name}.map.lzma`, (onProgress) => {
    const copy = bytes.slice().buffer;
    return rpc(worker, 'compressBytes', { buffer: copy }, { transfer: [copy], onProgress });
  });
  worker.terminate();
  $('res-lzma').disabled = false;
});

$('res-view').addEventListener('click', async () => {
  const { bytes, name, tile } = state.result;
  showError('res-error', '');
  try {
    await openMap(bytes.slice().buffer, name, `Tile ${tile}`);
    selectTab('viewer');
  } catch (err) {
    showError('res-error', err.message);
  }
});

// ---- start -------------------------------------------------------------------------------
updateTool();
if (location.hash === '#toolbox') selectTab('toolbox');
loadDefaultTheme().catch((err) => showError('style-error', `Could not load the bundled theme: ${err.message}`));
