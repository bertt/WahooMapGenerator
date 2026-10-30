// Runs all heavy WebAssembly work off the main thread: generating, decoding, tile serving, compressing.
import init, { generate, compress_lzma, decompress_lzma, is_mapsforge, MapViewer } from '../pkg/wahoo_map.js';

const ready = init();
let viewer = null;
let mapBytes = null;

function fileSource(file) {
  const reader = new FileReaderSync();
  return {
    size: file.size,
    read: (offset, length) => new Uint8Array(reader.readAsArrayBuffer(file.slice(offset, offset + length))),
  };
}

const handlers = {
  generate({ file, z, x, y }, progress) {
    const result = generate(fileSource(file), z, x, y, (pass, done, total) => progress({ pass, done, total }));
    const bytes = result.take_map();
    const summary = { bytes, pois: result.pois, ways: result.ways };
    result.free();
    return [summary, [bytes.buffer]];
  },

  open({ buffer }) {
    let bytes = new Uint8Array(buffer);
    if (!is_mapsforge(bytes)) bytes = decompress_lzma(bytes);
    if (!is_mapsforge(bytes)) throw new Error('This is not a mapsforge map (.map) or LZMA-compressed map (.map.lzma).');
    const next = new MapViewer(bytes);
    viewer?.free();
    viewer = next;
    mapBytes = bytes;
    return [{ info: JSON.parse(viewer.info()), size: bytes.length }, []];
  },

  tile({ z, x, y }) {
    const bytes = viewer.tile(z, x, y);
    return [bytes.buffer, [bytes.buffer]];
  },

  compress(_, progress) {
    const bytes = compress_lzma(mapBytes, (done, total) => progress({ done, total }));
    return [bytes, [bytes.buffer]];
  },

  raw() {
    const bytes = mapBytes.slice();
    return [bytes, [bytes.buffer]];
  },

  // Compresses given bytes without loading them in the viewer (used by the toolbox).
  compressBytes({ buffer }, progress) {
    const bytes = compress_lzma(new Uint8Array(buffer), (done, total) => progress({ done, total }));
    return [bytes, [bytes.buffer]];
  },
};

self.onmessage = async ({ data: { id, type, ...args } }) => {
  try {
    await ready;
    const [result, transfer] = handlers[type](args, (progress) => self.postMessage({ id, progress }));
    self.postMessage({ id, result }, transfer);
  } catch (e) {
    self.postMessage({ id, error: e?.message ?? String(e) });
  }
};
