import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { DOMParser } from '@xmldom/xmldom';
import { validateStyleMin } from '@maplibre/maplibre-gl-style-spec';
import { listVariants, themeToStyle } from '../js/theme.js';

const parse = (xml) => new DOMParser().parseFromString(xml, 'text/xml');
const theme = (body, menu = '') => parse(`<rendertheme map-background="#FF0000" xmlns="http://opensciencemap.org/rendertheme">${menu}${body}</rendertheme>`);
const OPTS = { tileUrl: 'http://x/{z}/{x}/{y}.pbf', minzoom: 0, maxzoom: 14 };
const draw = (body, menu, o = {}) => themeToStyle(theme(body, menu), { ...OPTS, ...o }).layers.filter((l) => l.type !== 'background');
const MENU = '<stylemenu defaultvalue="A"><layer id="A"><name lang="en" value="Alpha"/><cat id="a"/></layer><layer id="B-DARK"><name lang="en" value="Beta"/><cat id="b"/></layer></stylemenu>';

test('listVariants', () => {
  const r = listVariants(theme('', MENU));
  assert.equal(r.defaultId, 'A');
  assert.equal(r.background, '#FF0000');
  assert.deepEqual(r.variants, [{ id: 'A', name: 'Alpha', cats: ['a'] }, { id: 'B-DARK', name: 'Beta', cats: ['b'] }]);
});

test('invalid root throws', () => {
  assert.throws(() => themeToStyle(parse('<foo/>'), OPTS), /Invalid theme XML/);
});

test('conditions: inherited k, absent markers, wildcard, closed', () => {
  const [a, b, c, d, e] = draw(`
    <m e="way" k="highway"><m v="a|b"><line stroke="#000"/></m></m>
    <m e="way" k="tunnel" v="~|no"><line stroke="#000"/></m>
    <m e="way" k="bridge" v="-"><line stroke="#000"/></m>
    <m e="way" k="name" v="*" closed="yes"><line stroke="#000"/></m>
    <m e="way" k="a|b" closed="no"><line stroke="#000"/></m>`);
  assert.deepEqual(a.filter, ['all', ['has', 'highway'], ['in', ['get', 'highway'], ['literal', ['a', 'b']]]]);
  assert.deepEqual(b.filter, ['any', ['!', ['has', 'tunnel']], ['==', ['get', 'tunnel'], 'no']]);
  assert.deepEqual(c.filter, ['!', ['has', 'bridge']]);
  assert.deepEqual(d.filter, ['all', ['has', 'name'], ['==', ['geometry-type'], 'Polygon']]);
  assert.deepEqual(e.filter, ['all', ['any', ['has', 'a'], ['has', 'b']], ['!=', ['geometry-type'], 'Polygon']]);
});

test('cat filtering skips rules, elements and styles', () => {
  const body = `<style-line id="s" cat="b" stroke="#111"/>
    <m e="way" k="x" v="1" cat="a"><line stroke="#000"/></m>
    <m e="way" k="x" v="2"><line cat="b" stroke="#000"/><line stroke="#222"/><line use="s"/></m>`;
  const filters = (v) => draw(body, MENU, { variantId: v }).map((l) => l.filter[2]);
  assert.deepEqual(filters('A'), ['1', '2']);
  assert.deepEqual(filters('B-DARK'), ['2', '2', '2']);
});

test('select=first negates earlier siblings', () => {
  const ls = draw(`<m e="way" k="highway" v="path" select="first">
    <m k="bicycle" v="yes"><line stroke="#000"/></m>
    <m k="foot" v="yes"><line stroke="#000"/></m>
    <m k="highway" v="path"><line stroke="#000"/></m></m>`);
  assert.equal(ls.length, 3);
  assert.deepEqual(ls[2].filter.slice(0, 2), ['all', ['==', ['get', 'highway'], 'path']]);
  assert.deepEqual(ls[2].filter.slice(3), [['!', ['==', ['get', 'bicycle'], 'yes']], ['!', ['==', ['get', 'foot'], 'yes']]]);
});

test('use merging and precedence', () => {
  const [l] = draw(`<style-line id="base" stroke="#111111" width="2" cap="butt"/>
    <style-line id="mid" use="base" stroke="#222222"/>
    <m e="way" k="a"><line use="mid" width="3"/></m>`);
  assert.equal(l.paint['line-color'], '#222222');
  assert.equal(l.layout['line-cap'], 'butt');
  assert.equal(l.paint['line-width'].at(-1), 12);
});

test('outline casings are inserted before first outline user', () => {
  const ls = draw(`<outline-layer id="o" stroke="#000000" width="1"/>
    <m e="way" k="a"><line stroke="#fff" width="1"/>
    <line outline="o" stroke="#f00" width="2"/><line outline="o" stroke="#0f0" width="3"/></m>`);
  assert.deepEqual(ls.map((l) => l.paint['line-color']), ['#fff', '#000000', '#000000', '#f00', '#0f0']);
  assert.equal(ls[1].paint['line-width'].at(-1), 16);
});

test('zoom ranges intersect and max is inclusive', () => {
  const [l] = draw(`<m e="way" k="a" zoom-min="5" zoom-max="12"><m zoom-min="8" zoom-max="14"><line zoom-min="6" zoom-max="11" stroke="#000"/></m></m>`);
  assert.equal(l.minzoom, 8);
  assert.equal(l.maxzoom, 12);
  assert.equal(draw(`<m e="way" k="a" zoom-min="9" zoom-max="4"><line/></m>`).length, 0);
});

test('dash arrays are relative to line width', () => {
  const [a, b] = draw(`<m e="way" k="a"><line width="3" dasharray="9,3" stroke="#000"/><line width="0.5" dasharray="9,3" stroke="#000"/></m>`);
  assert.deepEqual(a.paint['line-dasharray'], [3, 1]);
  assert.deepEqual(b.paint['line-dasharray'], [9, 3]);
});

test('areas, patterns, captions, node/way handling', () => {
  const s = themeToStyle(theme(`
    <m e="way" k="natural" v="wood"><area src="patterns/light_green.png"/><area fill="#00ff00" stroke="#000" stroke-width="1"/></m>
    <m e="node" k="amenity"><line stroke="#000"/><symbol src="x.svg"/></m>
    <m k="place"><caption k="name" style="bold" size="20" dy="10" priority="2" stroke="#fff" stroke-width="2"/></m>`), OPTS);
  const ls = s.layers.slice(1);
  assert.deepEqual(ls.map((l) => `${l.type}:${l['source-layer']}`), ['fill:way', 'fill:way', 'line:way', 'symbol:poi', 'symbol:way']);
  assert.equal(ls[0].paint['fill-color'], '#c8e6b4');
  const cap = ls[3];
  assert.equal(cap['layout']['text-size'], 16);
  assert.deepEqual(cap.layout['text-offset'], [0, 0.625]);
  assert.equal(cap.paint['text-halo-width'], 1);
  assert.equal(cap.layout['symbol-sort-key'], 2);
  assert.ok(s.metadata.ignored.includes('symbol x1'));
});

test('background colour', () => {
  const bg = (o, m) => themeToStyle(theme('', MENU), { ...OPTS, ...o }).layers[0].paint['background-color'];
  assert.equal(bg({}), '#ffffff');
  assert.equal(bg({ variantId: 'B-DARK' }), '#000000');
  assert.equal(bg({ themeBackground: true }), '#FF0000');
});

const real = parse(readFileSync(new URL('../styles/vtm-elemnt.xml', import.meta.url), 'utf8'));
const { defaultId, variants } = listVariants(real);

test('real theme: every variant translates to a valid style', () => {
  assert.ok(variants.length >= 8);
  for (const v of variants) {
    const s = themeToStyle(real, { ...OPTS, variantId: v.id });
    const ids = s.layers.map((l) => l.id);
    assert.equal(new Set(ids).size, ids.length, v.id);
    for (const l of s.layers) {
      assert.ok(['background', 'fill', 'line', 'symbol', 'circle'].includes(l.type));
      if (l.type !== 'background') assert.ok(['way', 'poi'].includes(l['source-layer']));
      assert.ok((l.minzoom ?? 0) <= (l.maxzoom ?? 24), l.id);
    }
    assert.deepEqual(validateStyleMin(s), [], v.id);
  }
});

test('real theme: default variant has key layers', () => {
  const s = themeToStyle(real, OPTS);
  assert.equal(s.metadata.variant, defaultId);
  const has = (type, re) => s.layers.some((l) => l.type === type && re.test(l.id));
  assert.ok(has('fill', /natural=water/));
  assert.ok(has('line', /highway=/));
  assert.ok(s.layers.some((l) => l.type === 'symbol' && /place/.test(l.id)));
});
