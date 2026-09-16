/**
 * Dependency-free ES module wrapper around the `fast-grapheme-segmenter`
 * WebAssembly module.
 *
 * All entry points work in UTF-16 code units so nothing is ever transcoded.
 * The string is written into wasm memory as `u16`s and the boundaries come back as `u32` offsets that index
 * the original string directly.
 *
 * ```js
 * import { load } from './grapheme-segmenter.mjs';
 *
 * const seg = await load(await readFile('grapheme-segmenter.wasm'));
 *
 * seg.count('🏳️‍🌈🇰🇷a');      // 3
 * seg.boundaries('a🇰🇷b');   // [0, 1, 5]
 * seg.clusters('a🇰🇷b');    // ['a', '🇰🇷', 'b']
 * ```
 */

/**
 * @param {BufferSource | Promise<BufferSource>} wasm The WebAssembly module bytes
 */
export async function load(wasm) {
  const { instance } = await WebAssembly.instantiate(await wasm, {});
  const e = instance.exports;

  const packed = e.fgs_unicode_version();

  /**
   * Write `s` into freshly allocated wasm memory as UTF-16.
   * Returns the byte pointer and size; callers must re-create any TypedArray
   * views after this, since the allocation may grow memory and detach
   * existing buffers.
   * @param {string} s
   */
  function writeUtf16(s) {
    const bytes = s.length * 2;
    const ptr = e.fgs_alloc(bytes);
    const view = new Uint16Array(e.memory.buffer, ptr, s.length);
    for (let i = 0; i < s.length; i++) view[i] = s.charCodeAt(i);
    return { ptr, bytes };
  }

  /**
   * @param {string} s
   * @returns {number} the number of extended grapheme clusters
   */
  function count(s) {
    const { ptr, bytes } = writeUtf16(s);
    const n = e.fgs_count_utf16(ptr, s.length);
    e.fgs_free(ptr, bytes);
    return n;
  }

  /**
   * @param {string} s
   * @returns {number[]} UTF-16 offsets at which each cluster starts
   */
  function boundaries(s) {
    const { ptr, bytes } = writeUtf16(s);
    // Worst case: every unit starts a cluster (lone surrogates etc.).
    const outBytes = s.length * 4;
    const out = e.fgs_alloc(outBytes);
    const n = e.fgs_boundaries_utf16(ptr, s.length, out, s.length);
    const offsets = Array.from(new Uint32Array(e.memory.buffer, out, n));
    e.fgs_free(out, outBytes);
    e.fgs_free(ptr, bytes);
    return offsets;
  }

  /**
   * @param {string} s
   * @returns {string[]} the extended grapheme clusters
   */
  function clusters(s) {
    const offsets = boundaries(s);
    const out = new Array(offsets.length);
    for (let i = 0; i < offsets.length; i++) {
      out[i] = s.slice(offsets[i], offsets[i + 1] ?? s.length);
    }
    return out;
  }

  return {
    count,
    boundaries,
    clusters,
    /** The Unicode version the segmenter tables encode. */
    unicodeVersion:
      `${(packed >> 16) & 0xff}.${(packed >> 8) & 0xff}.${packed & 0xff}`,
    /** For embedders that hold UTF-8: `Uint8Array` in, count out. */
    countUtf8(bytes) {
      const ptr = e.fgs_alloc(bytes.length);
      new Uint8Array(e.memory.buffer, ptr, bytes.length).set(bytes);
      try {
        return e.fgs_count_utf8(ptr, bytes.length);
      } finally {
        e.fgs_free(ptr, bytes.length);
      }
    },
  };
}
