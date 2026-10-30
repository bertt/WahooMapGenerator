// Translates a Mapsforge/VTM render theme (XML Document) into a MapLibre GL style (spec v8).

// ---- tunable constants ------------------------------------------------------------------
// VTM line widths are px at base scale; MapLibre widths are interpolated over zoom.
const WIDTH_STOPS = [[8, 0.6], [12, 1.0], [16, 2.2], [18, 4]];
const TEXT_SCALE = 0.8;
const DEFAULT_GLYPHS = 'https://demotiles.maplibre.org/font/{fontstack}/{range}.pbf';
// Only these stacks exist on demotiles.maplibre.org.
const FONTS = {
  normal: ['Open Sans Regular', 'Arial Unicode MS Regular'],
  bold: ['Open Sans Semibold'],
  italic: ['Noto Sans Italic'],
  bold_italic: ['Noto Sans Bold'],
};
// Pattern fills (src="patterns/x.png") are approximated by flat colours derived from the file name.
const PATTERN_COLORS = {
  light_green: '#c8e6b4', dark_green: '#2f5a2f',
  wetland: '#b4d7d2', dark_wetland: '#1f4642',
  light_yellow: '#f5efc0', dark_yellow: '#6b6224',
};
const PATTERN_FALLBACK = { light: '#e6e6e6', dark: '#333333', other: '#cccccc' };

// ---- XML helpers -------------------------------------------------------------------------
const tag = (el) => el.localName || el.nodeName;
const attr = (el, name) => {
  const v = el.getAttribute ? el.getAttribute(name) : null;
  return v === null || v === undefined || v === '' ? null : v;
};
const elements = (el) => {
  const out = [];
  for (let n = el.firstChild; n; n = n.nextSibling) if (n.nodeType === 1) out.push(n);
  return out;
};
const descendants = (el) => elements(el).flatMap((c) => [c, ...descendants(c)]);

function root(doc) {
  const r = doc && doc.documentElement;
  if (!r || tag(r) !== 'rendertheme') throw new Error('Invalid theme XML');
  return r;
}

export function listVariants(doc) {
  const r = root(doc);
  const menu = elements(r).find((e) => tag(e) === 'stylemenu');
  const background = attr(r, 'map-background');
  if (!menu) return { defaultId: null, variants: [], background };
  const lang = attr(menu, 'defaultlang');
  const variants = elements(menu).filter((e) => tag(e) === 'layer').map((l) => {
    const names = elements(l).filter((e) => tag(e) === 'name');
    const name = (names.find((n) => attr(n, 'lang') === lang) || names[0]);
    return {
      id: attr(l, 'id'),
      name: (name && attr(name, 'value')) || attr(l, 'id'),
      cats: elements(l).filter((e) => tag(e) === 'cat').map((c) => attr(c, 'id')),
    };
  });
  return { defaultId: attr(menu, 'defaultvalue'), variants, background };
}

// ---- colours & numbers -------------------------------------------------------------------
// VTM uses #AARRGGBB for 8-digit colours.
function color(c, fallback) {
  if (!c) return fallback;
  const m = /^#([0-9a-f]{8})$/i.exec(c);
  if (!m) return c;
  const h = m[1];
  const a = parseInt(h.slice(0, 2), 16) / 255;
  const [r, g, b] = [2, 4, 6].map((i) => parseInt(h.slice(i, i + 2), 16));
  return `rgba(${r},${g},${b},${+a.toFixed(3)})`;
}
const num = (v, d) => { const n = parseFloat(v); return Number.isFinite(n) ? n : d; };
const round = (n) => +n.toFixed(3);
const lineWidth = (w) => ['interpolate', ['linear'], ['zoom'], ...WIDTH_STOPS.flatMap(([z, f]) => [z, round(w * f)])];

function patternColor(src) {
  const name = src.replace(/^.*[\\/]/, '').replace(/\.\w+$/, '');
  if (PATTERN_COLORS[name]) return PATTERN_COLORS[name];
  if (name.startsWith('light_')) return PATTERN_FALLBACK.light;
  if (name.startsWith('dark_')) return PATTERN_FALLBACK.dark;
  return PATTERN_FALLBACK.other;
}

// ---- conditions --------------------------------------------------------------------------
const flatAll = (list) => {
  const flat = list.flatMap((f) => (Array.isArray(f) && f[0] === 'all' ? f.slice(1) : [f]));
  return flat.length === 0 ? null : flat.length === 1 ? flat[0] : ['all', ...flat];
};

function keyCondition(k, values) {
  const absent = values.some((v) => v === '~' || v === '-');
  const wildcard = values.includes('*');
  const vals = values.filter((v) => v !== '~' && v !== '-' && v !== '*');
  if (wildcard) return absent ? null : ['has', k];
  const parts = [];
  if (absent) parts.push(['!', ['has', k]]);
  if (vals.length === 1) parts.push(['==', ['get', k], vals[0]]);
  else if (vals.length > 1) parts.push(['in', ['get', k], ['literal', vals]]);
  return parts.length === 1 ? parts[0] : ['any', ...parts];
}

// Own key/value/closed condition of a rule as a list of conjuncts, plus the keys it defines for children.
function ownCondition(el, keys) {
  const kAttr = attr(el, 'k');
  const v = attr(el, 'v');
  const ks = kAttr ? kAttr.split('|') : keys;
  const conds = [];
  if (ks && (kAttr || v !== null)) {
    const perKey = ks.map((k) => (v === null ? ['has', k] : keyCondition(k, v.split('|'))));
    if (!perKey.includes(null)) conds.push(perKey.length === 1 ? perKey[0] : ['any', ...perKey]);
  }
  const closed = attr(el, 'closed');
  if (closed === 'yes') conds.push(['==', ['geometry-type'], 'Polygon']);
  else if (closed === 'no') conds.push(['!=', ['geometry-type'], 'Polygon']);
  return { conds, keys: ks };
}

// ---- translation -------------------------------------------------------------------------
export function themeToStyle(doc, opts = {}) {
  const r = root(doc);
  const { tileUrl, glyphsUrl = DEFAULT_GLYPHS, minzoom, maxzoom, themeBackground } = opts;
  const menu = listVariants(doc);
  const variantId = opts.variantId ?? menu.defaultId;
  const variant = menu.variants.find((v) => v.id === variantId) || menu.variants.find((v) => v.id === menu.defaultId);
  const enabled = variant ? new Set(variant.cats) : null;
  const active = (el) => { const c = attr(el, 'cat'); return !c || !enabled || enabled.has(c); };

  const defs = new Map();
  const outlines = new Map();
  for (const el of descendants(r)) {
    const id = attr(el, 'id');
    if (!id) continue;
    if (tag(el).startsWith('style-')) defs.set(id, el);
    else if (tag(el) === 'outline-layer') outlines.set(id, el);
  }

  // Attributes of el merged over its `use` chain (own attributes win). The cat of used definitions is ignored.
  const resolve = (el, table = defs) => {
    const seen = new Set();
    const chain = [];
    for (let cur = el; cur && !seen.has(cur); ) {
      seen.add(cur);
      chain.unshift(cur);
      cur = table.get(attr(cur, 'use'));
    }
    const out = {};
    for (const e of chain) for (let i = 0; i < e.attributes.length; i++) out[e.attributes[i].name] = e.attributes[i].value;
    return out;
  };
  // Element is skipped when its own cat or the directly referenced definition's cat is disabled.
  const usable = (el, table = defs) => {
    if (!active(el)) return false;
    const used = attr(el, 'use') && table.get(attr(el, 'use'));
    return !used || active(used);
  };

  const layers = [{
    id: 'background', type: 'background',
    paint: { 'background-color': themeBackground === true ? (menu.background || '#ffffff') : /DARK/.test(variantId || '') ? '#000000' : '#ffffff' },
  }];
  const casings = [];
  let casingAt = -1;
  let counter = 0;
  const ignored = new Map();
  const images = {};
  const ignore = (what) => ignored.set(what, (ignored.get(what) || 0) + 1);

  function emit(kind, sourceLayer, ctx, extra, spec) {
    const zmax = ctx.zmax === Infinity ? null : Math.min(24, ctx.zmax + 1); // VTM zoom-max is inclusive
    if (zmax !== null && ctx.zmin >= zmax) return null;
    const layer = { id: `${kind}:${sourceLayer}:${ctx.label || '*'}:${++counter}`, type: spec.type, source: 'map' };
    if (spec.type !== 'background') layer['source-layer'] = sourceLayer;
    if (ctx.zmin > 0) layer.minzoom = ctx.zmin;
    if (zmax !== null) layer.maxzoom = zmax;
    const filter = flatAll([...ctx.conds, ...extra]);
    if (filter) layer.filter = filter;
    if (spec.layout) layer.layout = spec.layout;
    layer.paint = spec.paint || {};
    return layer;
  }

  function line(el, ctx, a, stroke, width, sourceLayer = 'way') {
    const dash = (a.dasharray || '').split(',').map(parseFloat).filter((n) => Number.isFinite(n) && n >= 0);
    const layout = { 'line-cap': ['butt', 'round', 'square'].includes(a.cap) ? a.cap : 'round', 'line-join': 'round' };
    const paint = { 'line-color': color(stroke, '#000000'), 'line-width': lineWidth(width) };
    if (dash.length > 1) paint['line-dasharray'] = dash.map((d) => round(d / Math.max(width, 1)));
    return emit('line', sourceLayer, ctx, [], { type: 'line', layout, paint });
  }

  function drawLine(el, ctx, a) {
    const width = num(a.width, 1);
    const outId = attr(el, 'outline');
    const outEl = outId && outlines.get(outId);
    if (outEl && active(outEl)) {
      const o = resolve(outEl, outlines);
      const casing = line(el, ctx, { cap: a.cap }, o.stroke || '#000000', width + 2 * num(o.width, 0));
      if (casing) {
        if (casingAt < 0) casingAt = layers.length;
        casings.push(casing);
      }
    }
    const l = line(el, ctx, a, a.stroke, width);
    if (l) layers.push(l);
  }

  function drawArea(el, ctx, a) {
    const fill = a.fill || (a.src && patternColor(a.src));
    if (a.src && !a.fill) ignore('pattern fill approximated');
    if (fill) {
      const l = emit('fill', 'way', ctx, [['==', ['geometry-type'], 'Polygon']], {
        type: 'fill', paint: { 'fill-color': color(fill), 'fill-antialias': false },
      });
      if (l) layers.push(l);
    }
    if (a.stroke) {
      const l = line(el, ctx, {}, a.stroke, num(a['stroke-width'], 1));
      if (l) layers.push(l);
    }
  }

  function drawText(el, ctx, a, sourceLayers, onLine) {
    const k = a.k || 'name';
    const size = num(a.size, 14) * TEXT_SCALE;
    const stroke = a.stroke;
    const layout = {
      'text-field': ['get', k],
      'text-font': FONTS[a.style] || FONTS.normal,
      'text-size': round(size),
      'text-allow-overlap': false,
      'text-optional': true,
    };
    if (onLine) layout['symbol-placement'] = 'line';
    else if (a.dy) layout['text-offset'] = [0, round(num(a.dy, 0) / size)];
    if (a.priority !== undefined) layout['symbol-sort-key'] = num(a.priority, 0);
    const paint = { 'text-color': color(a.fill, '#000000') };
    if (stroke) {
      paint['text-halo-color'] = color(stroke);
      paint['text-halo-width'] = round(num(a['stroke-width'], 0) / 2);
    }
    for (const sl of sourceLayers) {
      const l = emit(onLine ? 'pathtext' : 'caption', sl, ctx, [['has', k]], { type: 'symbol', layout, paint });
      if (l) layers.push(l);
    }
  }

  // Icons are referenced by their src; the host loads the image (see metadata.images).
  function drawSymbol(ctx, a, onLine) {
    if (!a.src) return ignore('symbol without src');
    images[a.src] = { src: a.src, width: num(a['symbol-width'], 20) };
    const layout = { 'icon-image': a.src, 'icon-allow-overlap': true };
    let sourceLayer = 'poi';
    if (onLine) {
      sourceLayer = 'way';
      layout['symbol-placement'] = 'line';
      layout['symbol-spacing'] = Math.max(1, num(a['repeat-gap'], 200));
      layout['icon-rotation-alignment'] = 'map';
    }
    const l = emit(onLine ? 'linesymbol' : 'symbol', sourceLayer, ctx, [], { type: 'symbol', layout });
    if (l) layers.push(l);
  }

  function drawElement(el, parentCtx) {
    const name = tag(el);
    const ctx = {
      ...parentCtx,
      zmin: Math.max(parentCtx.zmin, num(attr(el, 'zoom-min'), 0)),
      zmax: Math.min(parentCtx.zmax, num(attr(el, 'zoom-max'), Infinity)),
    };
    const kinds = ['line', 'area', 'caption', 'pathText', 'circle', 'symbol', 'lineSymbol'];
    if (!kinds.includes(name) || !usable(el)) return;
    const a = resolve(el);
    const nodeOnly = ctx.e === 'node';
    const multi = ctx.e === 'node' ? ['poi'] : ctx.e === 'way' ? ['way'] : ['poi', 'way'];
    if (name === 'symbol' || name === 'lineSymbol') return drawSymbol(ctx, a, name === 'lineSymbol');
    if (name === 'caption') return drawText(el, ctx, a, multi, false);
    if (name === 'circle') {
      for (const sl of multi) {
        const paint = { 'circle-radius': num(a.radius, 2), 'circle-color': color(a.fill, '#000000') };
        if (a.stroke) {
          paint['circle-stroke-color'] = color(a.stroke);
          paint['circle-stroke-width'] = num(a['stroke-width'], 1);
        }
        const l = emit('circle', sl, ctx, [], { type: 'circle', paint });
        if (l) layers.push(l);
      }
      return;
    }
    if (nodeOnly) return;
    if (name === 'line') drawLine(el, ctx, a);
    else if (name === 'area') drawArea(el, ctx, a);
    else drawText(el, ctx, a, ['way'], true);
  }

  const labelOf = (el, ctx, own) => {
    const k = attr(el, 'k'); const v = attr(el, 'v');
    return k || v ? [ctx.label, `${k || (own.keys || []).join('|')}${v ? '=' + v : ''}`].filter(Boolean).join(' ') : ctx.label;
  };

  function walk(parent, ctx) {
    const first = attr(parent, 'select') === 'first' && parent !== r;
    const prior = [];
    let blocked = false;
    for (const el of elements(parent)) {
      if (tag(el) !== 'm') { drawElement(el, ctx); continue; }
      const own = ownCondition(el, ctx.keys);
      if (!active(el)) continue;
      if (blocked) continue;
      const conds = [...ctx.conds, ...own.conds];
      if (first) {
        for (const p of prior) conds.push(['!', flatAll(p)]);
        if (own.conds.length === 0) blocked = true;
        else prior.push(own.conds);
      }
      walk(el, {
        e: attr(el, 'e') || ctx.e,
        keys: own.keys,
        conds,
        zmin: Math.max(ctx.zmin, num(attr(el, 'zoom-min'), 0)),
        zmax: Math.min(ctx.zmax, num(attr(el, 'zoom-max'), Infinity)),
        label: labelOf(el, ctx, own),
      });
    }
  }

  // An empty prior list (always-true sibling) is handled by `blocked`.
  walk(r, { e: 'any', keys: null, conds: [], zmin: 0, zmax: Infinity, label: '' });
  if (casingAt >= 0) layers.splice(casingAt, 0, ...casings);

  const source = { type: 'vector', tiles: tileUrl ? [tileUrl] : [] };
  if (minzoom !== undefined) source.minzoom = minzoom;
  if (maxzoom !== undefined) source.maxzoom = maxzoom;
  return {
    version: 8,
    name: variant ? variant.name : 'theme',
    metadata: {
      source: 'vtm-theme',
      variant: variant ? variant.id : null,
      layerCount: layers.length,
      images,
      ignored: [...ignored].map(([w, n]) => (n > 1 || w === 'symbol' ? `${w} x${n}` : w)),
    },
    sources: { map: source },
    glyphs: glyphsUrl,
    layers,
  };
}
