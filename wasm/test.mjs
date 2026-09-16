/**
 * Node smoke and conformance tests for the wasm module.
 *
 *   ./build.sh && node test.mjs              # raw C ABI module
 *   ./build.sh --bindgen && node test.mjs    # adds the wasm-bindgen pkg
 *
 * When `scripts/unicode.py`'s UCD cache is present,
 * every case of the official Unicode 17 GraphemeBreakTest.txt is replayed through each build.
 */

import * as assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

import { load } from './grapheme-segmenter.mjs';

// --- handcrafted, against the raw module -----------------------------------

const seg = await load(await readFile(new URL('./grapheme-segmenter.wasm', import.meta.url)));

assert.equal(seg.unicodeVersion, '17.0.0', `unicode version: ${seg.unicodeVersion}`);

assert.equal(seg.count(''), 0, 'empty');
assert.equal(seg.count('abcd'), 4, 'ascii');
assert.equal(seg.clusters('ab\ncd').join('|'), 'a|b|\n|c|d', 'newline breaks ascii pairs');
assert.equal(seg.clusters('a\r\nb').join('|'), 'a|\r\n|b', 'crlf');
assert.equal(seg.boundaries('a🇰🇷b').join(','), '0,1,5', 'boundaries are utf16 units');
assert.equal(seg.clusters('🏳️‍🌈🇰🇷a').join('|'), '🏳️‍🌈|🇰🇷|a', 'zwj emoji + flag');
assert.equal(seg.clusters('뎌쉐').join('|'), '뎌|쉐', 'jamo');
assert.equal(seg.clusters('क्क').join('|'),  'क्क', 'devanagari conjunct stays joined');

// Lone surrogates: own cluster, never gluing, pair = one codepoint.
const loneHigh = String.fromCharCode(0xd83c) + 'a';
assert.equal(seg.count(loneHigh), 2, 'lone high surrogate');

const sandwich = 'a' + String.fromCharCode(0xd800) + '\u0301';
assert.equal(seg.boundaries(sandwich).join(','), '0,1', 'lone surrogate breaks both sides');

// --- UCD conformance --------------------------------------------------------

/** Parse GraphemeBreakTest.txt into (input, expectedClusters) pairs. */
async function* ucdCases() {
  const path = new URL('../scripts/ucd/auxiliary_GraphemeBreakTest.txt', import.meta.url);
  if (!existsSync(path)) return;
  const text = await readFile(path, 'utf8');
  for (const line of text.split('\n')) {
    const body = line.split('#')[0].trim();
    if (!body) continue;
    const tokens = body.split(/\s+/);
    const inner = tokens.slice(1, -1); // between the leading and trailing ÷
    const cps = inner.filter((_, i) => i % 2 === 0).map((t) => parseInt(t, 16));
    const ops = inner.filter((_, i) => i % 2 === 1);
    if (cps.some((cp) => cp >= 0xd800 && cp <= 0xdfff)) continue; // valid strings only
    const input = String.fromCodePoint(...cps);
    const expected = [[cps[0]]];
    for (let i = 0; i < ops.length; i++) {
      if (ops[i] === '÷') expected.push([cps[i + 1]]);
      else expected[expected.length - 1].push(cps[i + 1]);
    }
    yield [input, expected.map((c) => String.fromCodePoint(...c))];
  }
}

/** Run the conformance suite over `segmenter`, which provides count and
 *  clusters (derived from boundaries when only those exist). */
async function conformance(name, segmenter) {
  let cases = 0;
  let fails = 0;
  for await (const [input, expected] of ucdCases()) {
    const want = expected.join('\u0000');
    if (segmenter.count(input) !== expected.length || segmenter.clusters(input).join('\u0000') !== want) {
      fails++;
      if (fails <= 3) console.error(`  ${name} FAIL:`, JSON.stringify(input));
    }
    cases++;
  }
  if (cases === 0) {
    console.log(`${name}: UCD cache absent - run scripts/unicode.py once to enable.`);
  } else {
    console.log(`${name} GraphemeBreakTest: ${cases} cases ${fails ? `FAILED (${fails})` : 'passed'}`);
    if (fails) process.exitCode = 1;
  }
}

await conformance('raw', seg);

// The wasm-bindgen build, when present: clusters are derived from the
// boundaries it returns. Note that a Uint32Array's map coerces to numbers,
// so the slicing loop stays plain.
const pkg = new URL('./pkg-node/fast_grapheme_segmenter_wasm.js', import.meta.url);
if (existsSync(pkg)) {
  const require = createRequire(import.meta.url); 
  const m = require(fileURLToPath(pkg));
  assert.equal(m.unicode_version(), '17.0.0', 'bindgen unicode version');
  assert.equal(m.count('🏳️‍🌈🇰🇷a'), 3, 'bindgen count');
  assert.equal(m.boundaries('a🇰🇷b').join(','), '0,1,5', 'bindgen boundaries');
  assert.equal(m.count_utf16([0x61, 0xd83c, 0xddf0, 0xd83c, 0xddf7]), 2, 'bindgen count_utf16');
  await conformance('bindgen', {
    count: (s) => m.count(s),
    clusters: (s) => {
      const b = m.boundaries(s);
      const out = [];
      for (let i = 0; i < b.length; i++) out.push(s.slice(b[i], b[i + 1] ?? s.length));
      return out;
    },
  });
} else {
  console.log('bindgen: pkg-node absent - build with ./build.sh --bindgen to include.');
}
