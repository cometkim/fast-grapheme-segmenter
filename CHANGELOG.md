# fast-grapheme-segmenter

## 0.1.1

### Patch Changes

- [`0bdcd1f`](https://github.com/cometkim/fast-grapheme-segmenter/commit/0bdcd1f026190692f5b1cc27c0b58e4b009f1c26) - Make iterator construction O(1) by arming the one-byte ASCII shortcut lazily at boundaries instead of sampling up to 64 bytes in every `Driver::new`.
  
  To mitigate perf regressions observed in real-world use cases.
  
  The library has prematurely optimized for ASCII-heavy text inputs exceeding a certain size.
  That was the only part where the Rust port differs from the original `unicode-segmenter` JS library.
  
  However, ecosystem investigations reveal that not all call-site use cases operate in ideal conditions.
  
  Re-initializing the iterator for a single segment occurs frequently; this is a classic anti-pattern also observed in the JavaScript ecosystem as well.
  
  It is a misconception to assume that Rust users always handle this gracefully, but they don't.
  In some instances, they already expose the behavior as a public API, making it impossible to fix sensibly.
  
  After the fix, the sampling cost is no longer paid upfront when creating the iterator. The newly improved inner-loop arming heuristic evaluates starting from at least one boundary.

## 0.1.0

### Minor Changes

- [`7396248`](https://github.com/cometkim/fast-grapheme-segmenter/commit/739624812f2f04dc52d9d6559281a1a6be23c31d) - Initial release from CI
