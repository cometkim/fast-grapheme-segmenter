//! Fast, lightweight, forward-only segmentation into Unicode extended grapheme clusters,
//! as defined by [UAX #29](https://www.unicode.org/reports/tr29/).
//!
//! This crate does one thing: iterate extended grapheme clusters from the
//! start of a string towards the end, as fast as the rules allow.
//!
//! - Unicode 17.0, follow up-to-date standard.
//! - Extended grapheme clusters only. GB9a, GB9b and GB9c always apply.
//! - `no_std`, no `unsafe`, zero dependencies.
//! - Only ~19 KB of generated tables
//!
//! It is a Rust port of the [unicode-segmenter](https://github.com/cometkim/unicode-segmenter),
//! the JavaScript library this algorithm was originally developed for,
//! replicating the [`Intl.Segmenter`](https://developer.mozilla.org/docs/Web/JavaScript/Reference/Global_Objects/Intl/Segmenter) behavior.
//!
//! And it's highly optmized for simple foward segmentation and counting grapheme clusters in a given text.
//!
//! # Examples
//!
//! ```
//! use fast_grapheme_segmenter::graphemes;
//!
//! let text = "a🇰🇷b";
//! let clusters: Vec<&str> = graphemes(text).collect();
//! assert_eq!(clusters, ["a", "🇰🇷", "b"]);
//! ```
//!
//! Byte offsets of each cluster:
//!
//! ```
//! use fast_grapheme_segmenter::grapheme_indices;
//!
//! let text = "a🇰🇷b";
//! let offsets: Vec<usize> = grapheme_indices(text).map(|(i, _)| i).collect();
//! assert_eq!(offsets, [0, 1, 9]);
//! ```
//!
//! Or just the offsets, without slicing the clusters out:
//!
//! ```
//! use fast_grapheme_segmenter::grapheme_boundaries;
//!
//! let offsets: Vec<usize> = grapheme_boundaries("a🇰🇷b").collect();
//! assert_eq!(offsets, [0, 1, 9]);
//! ```
//!
//! Counting clusters, e.g. for a display-width-aware `strlen`:
//!
//! ```
//! use fast_grapheme_segmenter::count_graphemes;
//!
//! assert_eq!(count_graphemes("🏳️‍🌈👩‍👩‍👧‍👦"), 2);
//! ```

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod tables;

use core::cmp;
use core::str::CharIndices;
use tables::GraphemeCat;

/// The Unicode version whose data this crate's tables encode.
pub const UNICODE_VERSION: (u8, u8, u8) = tables::UNICODE_VERSION;

// Bits of the packed rule state, which summarises everything the boundary
// rules can ask about the text preceding the cursor.
//
// It is a pure function of the codepoints consumed so far, so it carries
// across cluster boundaries and needs no reset.
//
// The bits a rule can ask about are laid out so that each `PAIR_MASK` entry
// selects exactly one of them, which turns every rule into a single
// `state & mask == 0` test.

/// Always set, so that a mask of `ALWAYS` means "never a boundary".
const ALWAYS: u8 = 1 << 0;
/// An odd run of `Regional_Indicator` immediately precedes (GB12, GB13).
const RIS_ODD: u8 = 1 << 1;
/// The last codepoint is a ZWJ that was preceded by `Extended_Pictographic Extend*` (GB11).
const EMOJI_ZWJ: u8 = 1 << 2;
/// The `InCB=Consonant` run in progress contains an `InCB=Linker` (GB9c).
const INCB_LINKED: u8 = 1 << 3;
/// `Extended_Pictographic Extend*` immediately precedes; feeds `EMOJI_ZWJ` on a ZWJ.
const EXTPIC_RUN: u8 = 1 << 4;
/// `InCB=Consonant [InCB=Extend InCB=Linker]*` precedes.
const INCB_RUN: u8 = 1 << 5;

/// Bits that survive consuming an `Extend`: the pictographic run that GB11
/// allows `Extend*` to span.
const EXTEND_KEEP: u8 = ALWAYS | EXTPIC_RUN;

/// The state bit that decides the boundary verdict for a pair of categories.
///
/// A mask of `0` is an unconditional boundary (GB999 and friends)
/// and can never match, while `ALWAYS` is unconditionally not a boundary.
const fn pair_mask(before: GraphemeCat, after: GraphemeCat) -> u8 {
    use GraphemeCat::*;
    match (before, after) {
        (GC_CR, GC_LF) => ALWAYS,                                  // GB3
        (GC_Control | GC_CR | GC_LF, _) => 0,                      // GB4
        (_, GC_Control | GC_CR | GC_LF) => 0,                      // GB5
        (GC_L, GC_L | GC_V | GC_LV | GC_LVT) => ALWAYS,            // GB6
        (GC_LV | GC_V, GC_V | GC_T) => ALWAYS,                     // GB7
        (GC_LVT | GC_T, GC_T) => ALWAYS,                           // GB8
        (_, GC_Extend | GC_ZWJ) => ALWAYS,                         // GB9
        (_, GC_SpacingMark) => ALWAYS,                             // GB9a
        (GC_Prepend, _) => ALWAYS,                                 // GB9b
        (_, GC_InCB_Consonant) => INCB_LINKED,                     // GB9c
        (GC_ZWJ, GC_Extended_Pictographic) => EMOJI_ZWJ,           // GB11
        (GC_Regional_Indicator, GC_Regional_Indicator) => RIS_ODD, // GB12, GB13
        (_, _) => 0,                                               // GB999
    }
}

/// Every pairwise rule evaluated at compile time,
/// indexed by `(before as usize) << 4 | after as usize`.
///
/// There is a boundary between two codepoints exactly
/// when the packed state and the pair's mask share no bit.
const PAIR_MASK: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut before = 0;
    while before < 16 {
        let mut after = 0;
        while after < 16 {
            table[before << 4 | after] = pair_mask(tables::CATS[before], tables::CATS[after]);
            after += 1;
        }
        before += 1;
    }
    table
};

// The state machine works on raw category bytes, which is what the tables
// hand back and what indexes `PAIR_MASK`, so the driver never widens them to the enum.

const CAT_ANY: u8 = GraphemeCat::GC_Any as u8;
const CAT_EXTEND: u8 = GraphemeCat::GC_Extend as u8;
const CAT_PICTOGRAPHIC: u8 = GraphemeCat::GC_Extended_Pictographic as u8;
const CAT_REGIONAL: u8 = GraphemeCat::GC_Regional_Indicator as u8;
const CAT_ZWJ: u8 = GraphemeCat::GC_ZWJ as u8;
const CAT_CONSONANT: u8 = GraphemeCat::GC_InCB_Consonant as u8;

/// State transition on consuming an `Extend` that continues an `InCB=Consonant` run.
///
/// Split out of [`next_state`] because it is the only transition that has to
/// look at the codepoint rather than just its category.
#[inline]
fn extend_in_incb_run(state: u8, ch: char) -> u8 {
    if ch == '\u{200c}' {
        // ZWNJ is `InCB=None`, so it ends the conjunct sequence.
        state & EXTEND_KEEP
    } else if state & INCB_LINKED != 0 || tables::is_incb_linker(ch) {
        state & EXTEND_KEEP | INCB_LINKED | INCB_RUN
    } else {
        // `InCB=Extend`: the run continues but is still unlinked.
        state & EXTEND_KEEP | INCB_RUN
    }
}

/// The packed state after consuming `ch`, whose category is `cat`.
#[inline(always)]
fn next_state(state: u8, cat: u8, ch: char) -> u8 {
    match cat {
        CAT_EXTEND if state & INCB_RUN != 0 => extend_in_incb_run(state, ch),
        CAT_EXTEND => state & EXTEND_KEEP,
        CAT_PICTOGRAPHIC => ALWAYS | EXTPIC_RUN,
        // Flip the parity of the run of regional indicators.
        CAT_REGIONAL => (state & RIS_ODD) ^ (ALWAYS | RIS_ODD),
        // A ZWJ arms GB11 if a pictographic run precedes it, and is itself `InCB=Extend`,
        // so any conjunct sequence in progress carries on.
        CAT_ZWJ => (state & EXTPIC_RUN) >> 2 | state & (ALWAYS | INCB_LINKED | INCB_RUN),
        CAT_CONSONANT => ALWAYS | INCB_RUN,
        // Nothing to remember: only the always-set bit remains.
        _ => ALWAYS,
    }
}

/// External iterator for a string's extended grapheme clusters.
///
/// Created with [`graphemes`] or [`GraphemeSegmentation::graphemes`].
#[derive(Clone, Debug)]
pub struct Graphemes<'a> {
    driver: Driver<'a>,
}

impl<'a> Graphemes<'a> {
    /// Create an iterator over the extended grapheme clusters of `s`.
    #[must_use]
    #[inline]
    pub fn new(s: &'a str) -> Graphemes<'a> {
        Graphemes {
            driver: Driver::new(s),
        }
    }

    /// The remaining, not yet segmented part of the original string.
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &'a str {
        &self.driver.string[self.driver.offset..]
    }
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.driver.string.len() - self.driver.offset;
        (cmp::min(remaining, 1), Some(remaining))
    }

    #[inline]
    fn next(&mut self) -> Option<&'a str> {
        let start = self.driver.offset;
        if start == self.driver.string.len() {
            return None;
        }
        let end = self.driver.next_boundary();
        Some(&self.driver.string[start..end])
    }

    /// Counts by delegating to [`count_graphemes`], skipping the slicing
    /// that feeding each cluster through [`Iterator::next`] would do.
    ///
    /// Restarting the count at the current offset is sound: a cluster head
    /// re-establishes every packed state bit it depends on.
    ///
    /// The only rule that can carry context across a boundary pair is one whose head
    /// category sets those bits itself, or the boundary went through a state-resetting Control.
    ///
    /// The randomized test below checks it.
    #[inline]
    fn count(self) -> usize {
        count_graphemes(&self.driver.string[self.driver.offset..])
    }
}

/// External iterator for a string's extended grapheme clusters and their byte offsets.
///
/// Created with [`grapheme_indices`] or [`GraphemeSegmentation::grapheme_indices`].
#[derive(Clone, Debug)]
pub struct GraphemeIndices<'a> {
    start_offset: usize,
    iter: Graphemes<'a>,
}

impl<'a> GraphemeIndices<'a> {
    /// Create an iterator over the extended grapheme clusters of `s` with their byte offsets.
    #[must_use]
    #[inline]
    pub fn new(s: &'a str) -> GraphemeIndices<'a> {
        GraphemeIndices {
            start_offset: 0,
            iter: Graphemes::new(s),
        }
    }

    /// The remaining, not yet segmented part of the original string.
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &'a str {
        self.iter.as_str()
    }
}

impl<'a> Iterator for GraphemeIndices<'a> {
    type Item = (usize, &'a str);

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }

    #[inline]
    fn next(&mut self) -> Option<(usize, &'a str)> {
        let pre_offset = self.start_offset;
        let cluster = self.iter.next()?;
        self.start_offset += cluster.len();
        Some((pre_offset, cluster))
    }

    #[inline]
    fn count(self) -> usize {
        self.iter.count()
    }
}

/// External iterator over the byte offsets at which a string's extended grapheme clusters start.
///
/// The same segmentation as [`graphemes`] without slicing the clusters out,
/// which is what cursor motion and column arithmetic usually wants.
///
/// Created with [`grapheme_boundaries`] or [`GraphemeSegmentation::grapheme_boundaries`].
#[derive(Clone, Debug)]
pub struct GraphemeBoundaries<'a> {
    driver: Driver<'a>,
}

impl<'a> GraphemeBoundaries<'a> {
    /// Create an iterator over the byte offsets at which the extended grapheme clusters of `s` start.
    #[must_use]
    #[inline]
    pub fn new(s: &'a str) -> GraphemeBoundaries<'a> {
        GraphemeBoundaries {
            driver: Driver::new(s),
        }
    }

    /// The remaining, not yet segmented part of the original string.
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &'a str {
        &self.driver.string[self.driver.offset..]
    }
}

impl<'a> Iterator for GraphemeBoundaries<'a> {
    type Item = usize;

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.driver.string.len() - self.driver.offset;
        (cmp::min(remaining, 1), Some(remaining))
    }

    #[inline]
    fn next(&mut self) -> Option<usize> {
        let start = self.driver.offset;
        if start == self.driver.string.len() {
            return None;
        }
        self.driver.next_boundary();
        Some(start)
    }

    /// Counts by delegating to [`count_graphemes`],
    /// skipping the boundary walk that feeding each offset through [`Iterator::next`] would do.
    #[inline]
    fn count(self) -> usize {
        count_graphemes(&self.driver.string[self.driver.offset..])
    }
}

/// Extension methods for segmenting a `str` into extended grapheme clusters.
pub trait GraphemeSegmentation {
    /// Iterate over the extended grapheme clusters.
    ///
    /// ```
    /// use fast_grapheme_segmenter::GraphemeSegmentation;
    ///
    /// let mut clusters = "a\u{301}b".graphemes();
    /// assert_eq!(clusters.next(), Some("a\u{301}"));
    /// assert_eq!(clusters.next(), Some("b"));
    /// assert_eq!(clusters.next(), None);
    /// ```
    fn graphemes(&self) -> Graphemes<'_>;

    /// Iterate over the extended grapheme clusters with their byte offsets.
    fn grapheme_indices(&self) -> GraphemeIndices<'_>;

    /// Iterate over the byte offsets at which the extended grapheme clusters start,
    /// without slicing the clusters out.
    fn grapheme_boundaries(&self) -> GraphemeBoundaries<'_>;

    /// Count the extended grapheme clusters.
    fn count_graphemes(&self) -> usize;
}

impl GraphemeSegmentation for str {
    #[inline]
    fn graphemes(&self) -> Graphemes<'_> {
        Graphemes::new(self)
    }

    #[inline]
    fn grapheme_indices(&self) -> GraphemeIndices<'_> {
        GraphemeIndices::new(self)
    }

    #[inline]
    fn grapheme_boundaries(&self) -> GraphemeBoundaries<'_> {
        GraphemeBoundaries::new(self)
    }

    #[inline]
    fn count_graphemes(&self) -> usize {
        count_graphemes(self)
    }
}

/// Iterate over the extended grapheme clusters of a string.
///
/// ```
/// use fast_grapheme_segmenter::graphemes;
///
/// assert_eq!(graphemes("🏳️‍🌈!").collect::<Vec<_>>(), ["🏳️‍🌈", "!"]);
/// ```
#[must_use]
#[inline]
pub fn graphemes(s: &str) -> Graphemes<'_> {
    Graphemes::new(s)
}

/// Iterate over the extended grapheme clusters of a string with their byte offsets.
///
/// ```
/// use fast_grapheme_segmenter::grapheme_indices;
///
/// let clusters = grapheme_indices("a🏳️‍🌈").collect::<Vec<_>>();
/// assert_eq!(clusters, [(0, "a"), (1, "🏳️‍🌈")]);
/// ```
#[must_use]
#[inline]
pub fn grapheme_indices(s: &str) -> GraphemeIndices<'_> {
    GraphemeIndices::new(s)
}

/// Iterate over the byte offsets at which the extended grapheme clusters of a string start,
/// without slicing the clusters out.
///
/// ```
/// use fast_grapheme_segmenter::grapheme_boundaries;
///
/// let offsets: Vec<usize> = grapheme_boundaries("a🇰🇷b").collect();
/// assert_eq!(offsets, [0, 1, 9]);
/// ```
#[must_use]
#[inline]
pub fn grapheme_boundaries(s: &str) -> GraphemeBoundaries<'_> {
    GraphemeBoundaries::new(s)
}

/// Decode the codepoint starting at UTF-16 code unit `i`.
///
/// Unpaired surrogates decode as U+FFFD, the replacement character, whose category is `Any`;
/// The same policy icu4x applies to ill-formed input.
///
/// The caller guarantees `i < s.len()`;
/// the return is the codepoint and the number of code units it occupies.
#[inline]
fn decode_utf16_at(s: &[u16], i: usize) -> (char, usize) {
    match s[i] {
        hi @ 0xD800..=0xDBFF => match s.get(i + 1) {
            Some(0xDC00..=0xDFFF) => {
                // Safe by construction: a high surrogate plus a low one.
                let cp = 0x10000 + ((hi as u32 - 0xD800) << 10) + (s[i + 1] as u32 - 0xDC00);
                (char::from_u32(cp).unwrap_or('\u{FFFD}'), 2)
            }
            _ => ('\u{FFFD}', 1),
        },
        0xDC00..=0xDFFF => ('\u{FFFD}', 1),
        u => (char::from_u32(u as u32).unwrap_or('\u{FFFD}'), 1),
    }
}

/// Count the extended grapheme clusters of a potentially ill-formed UTF-16 slice,
/// as JavaScript and the `wasm32` targets store their strings.
///
/// Lone surrogates count as one cluster each, by way of U+FFFD.
/// Offsets in the sibling functions are UTF-16 code units, not bytes.
///
/// ```
/// use fast_grapheme_segmenter::count_graphemes_utf16;
///
/// // "🇰🇷a" - the flag is two surrogate pairs, then one BMP codepoint.
/// let units: Vec<u16> = "🇰🇷a".encode_utf16().collect();
/// assert_eq!(count_graphemes_utf16(&units), 2);
/// ```
#[must_use]
pub fn count_graphemes_utf16(s: &[u16]) -> usize {
    if s.is_empty() {
        return 0;
    }
    let (first, mut i) = decode_utf16_at(s, 0);
    let mut count = 1;
    let mut cat_before = tables::grapheme_category_raw(first);
    let mut state = next_state(ALWAYS, cat_before, first);

    while i < s.len() {
        let (ch, units) = decode_utf16_at(s, i);
        let cat_after = tables::grapheme_category_raw(ch);
        if state & PAIR_MASK[(cat_before as usize) << 4 | cat_after as usize] == 0 {
            count += 1;
        }
        state = next_state(state, cat_after, ch);
        cat_before = cat_after;
        i += units;
    }
    count
}

/// External iterator over the UTF-16 code unit offsets at which the extended grapheme clusters of a slice start.
///
/// Created with [`grapheme_boundaries_utf16`].
#[derive(Clone, Debug)]
pub struct GraphemeBoundariesUtf16<'a> {
    units: &'a [u16],
    /// Code unit offset of the cluster whose start the next `next` yields.
    pos: usize,
    /// Code units consumed so far: always just past the head of the cluster
    /// at `pos`, or `pos` itself before the first call.
    scan: usize,
    cat_before: u8,
    state: u8,
}

impl<'a> Iterator for GraphemeBoundariesUtf16<'a> {
    type Item = usize;

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.units.len() - self.pos;
        (cmp::min(remaining, 1), Some(remaining))
    }

    fn next(&mut self) -> Option<usize> {
        let len = self.units.len();
        let start = self.pos;
        if start == len {
            return None;
        }
        if self.scan == start {
            // Consume the cluster head; nothing can break before it.
            let (ch, units) = decode_utf16_at(self.units, start);
            self.cat_before = tables::grapheme_category_raw(ch);
            self.state = next_state(ALWAYS, self.cat_before, ch);
            self.scan = start + units;
        }
        while self.scan < len {
            let (ch, units) = decode_utf16_at(self.units, self.scan);
            let cat = tables::grapheme_category_raw(ch);
            let boundary =
                self.state & PAIR_MASK[(self.cat_before as usize) << 4 | cat as usize] == 0;
            self.state = next_state(self.state, cat, ch);
            self.cat_before = cat;
            let char_at = self.scan;
            self.scan += units;
            if boundary {
                self.pos = char_at;
                return Some(start);
            }
        }
        // The end of the slice is always a boundary.
        self.pos = len;
        Some(start)
    }
}

/// Iterate over the UTF-16 code unit offsets at which the extended grapheme clusters of a slice start,
/// without decoding the clusters out.
///
/// ```
/// use fast_grapheme_segmenter::grapheme_boundaries_utf16;
///
/// let units: Vec<u16> = "a🇰🇷b".encode_utf16().collect();
/// let offsets: Vec<usize> = grapheme_boundaries_utf16(&units).collect();
/// assert_eq!(offsets, [0, 1, 5]);
/// ```
#[must_use]
#[inline]
pub fn grapheme_boundaries_utf16(s: &[u16]) -> GraphemeBoundariesUtf16<'_> {
    GraphemeBoundariesUtf16 {
        units: s,
        pos: 0,
        scan: 0,
        cat_before: CAT_ANY,
        state: ALWAYS,
    }
}

/// Whether `b` is a printable ASCII byte.
///
/// Printable ASCII is a category-`Any` island:
/// no ASCII codepoint is Extend, ZWJ, SpacingMark, Prepend, Regional_Indicator or InCB=Consonant,
/// so the only rule that could suppress a boundary between two of them is GB3 (CR × LF), and neither CR nor LF is printable.
/// Every adjacent pair is a GB999 boundary, and each byte leaves the packed state at `(Any, ALWAYS)`,
/// which is exactly what the per-codepoint loop would leave behind.
#[inline]
fn is_printable_ascii(b: u8) -> bool {
    b.wrapping_sub(0x20) < 0x5F // 0x20..=0x7E
}

/// Whether every byte of `w` is printable ASCII.
///
/// The standard SWAR zero-byte test, applied to a mask whose byte vanishes
/// for anything below 0x20 and to the XOR with 0x7F whose byte vanishes for DEL;
/// bytes with the high bit set are caught by the mask itself.
#[inline]
fn all_printable_ascii(w: u64) -> bool {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const MSB: u64 = 0x8080_8080_8080_8080;
    let low = w & 0xE0E0_E0E0_E0E0_E0E0;
    let del = w ^ 0x7F7F_7F7F_7F7F_7F7F;
    let bad = (low.wrapping_sub(ONES) & !low) | (del.wrapping_sub(ONES) & !del) | w;
    bad & MSB == 0
}

/// End of the maximal printable-ASCII run starting at `from`.
///
/// Swallows eight bytes at a time while a whole word is printable,
/// then walks the remainder byte by byte.
#[inline]
fn printable_run_end(bytes: &[u8], from: usize) -> usize {
    let mut j = from;
    while j + 8 <= bytes.len() {
        let word = u64::from_le_bytes(bytes[j..j + 8].try_into().unwrap());
        if !all_printable_ascii(word) {
            break;
        }
        j += 8;
    }
    while j < bytes.len() && is_printable_ascii(bytes[j]) {
        j += 1;
    }
    j
}

/// A per-byte printable-ASCII mask for a word:
/// 0x80 set in each byte that is in 0x20..=0x7E.
#[inline]
fn printable_msb_mask(w: u64) -> u64 {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const MSB: u64 = 0x8080_8080_8080_8080;
    let low = w & 0xE0E0_E0E0_E0E0_E0E0;
    let del = w ^ 0x7F7F_7F7F_7F7F_7F7F;
    let bad =
        (low.wrapping_sub(ONES) & !low & MSB) | (del.wrapping_sub(ONES) & !del & MSB) | (w & MSB);
    !bad & MSB
}

/// Find the next run of printable-ASCII bytes of length two or more.
///
/// Returns `(start, end)` of the run, or `(len, len)` when none follows.
///
/// Keeping the run discovery out of the per-codepoint loop is what lets
/// text without such runs count exactly as it did before the bulk path:
/// the char loop stays untouched, and this word-at-a-time scan costs a
/// couple of instructions per eight bytes. 
///
/// It is kept out of line for the same reason: it runs once per run, and letting its machinery bloat
/// the counting function measurably degrades the char loop's codegen.
#[inline(never)]
fn next_run(bytes: &[u8], from: usize) -> (usize, usize) {
    let len = bytes.len();
    let mut j = from;
    while j + 8 <= len {
        let p = printable_msb_mask(u64::from_le_bytes(bytes[j..j + 8].try_into().unwrap()));
        // A byte and its successor both printable inside the word, or the
        // word's last byte pairing up with the byte that follows it.
        let pair = p & p >> 8;
        if pair != 0 {
            let start = j + (pair.trailing_zeros() >> 3) as usize;
            return (start, printable_run_end(bytes, start));
        }
        if p >> 56 != 0 && j + 8 < len && is_printable_ascii(bytes[j + 8]) {
            return (j + 7, printable_run_end(bytes, j + 7));
        }
        j += 8;
    }
    let mut i = j;
    while i + 1 < len {
        if is_printable_ascii(bytes[i]) && is_printable_ascii(bytes[i + 1]) {
            return (i, printable_run_end(bytes, i));
        }
        i += 1;
    }
    (len, len)
}

/// The per-codepoint counting loop over `seg`, carrying the rule state.
///
/// Kept out of line so that both counting paths get the same tight loop:
/// the bulk path only ever calls it on run-free gaps, and letting either
/// path's surroundings influence its codegen is measurable on the other.
#[inline(never)]
fn count_gap(seg: &str, count: &mut usize, cat_before: &mut u8, state: &mut u8) {
    for (_, ch) in seg.char_indices() {
        let cat_after = tables::grapheme_category_raw(ch);
        if *state & PAIR_MASK[(*cat_before as usize) << 4 | cat_after as usize] == 0 {
            *count += 1;
        }
        *state = next_state(*state, cat_after, ch);
        *cat_before = cat_after;
    }
}

/// Whether the head of the string is printable-ASCII-heavy enough that the
/// run-bulk counting path will pay for itself.
///
/// Both counting paths are correct for any input; this only picks one,
/// so a misleading sample can cost speed, never correctness.
///
/// Scanning the gaps between runs word-by-word is cheap, but not free,
/// and text that is mostly non-ASCII has no runs worth swallowing.
fn ascii_heavy(bytes: &[u8]) -> bool {
    let sample = bytes.len().min(64);
    let mut printable = 0;
    let mut j = 0;
    while j + 8 <= sample {
        let w = u64::from_le_bytes(bytes[j..j + 8].try_into().unwrap());
        printable += printable_msb_mask(w).count_ones();
        j += 8;
    }
    while j < sample {
        printable += is_printable_ascii(bytes[j]) as u32;
        j += 1;
    }
    printable >= sample as u32 / 2
}

/// Count the extended grapheme clusters of a string.
///
/// A dedicated loop that never slices the clusters it counts, unlike [`Graphemes::count`].
///
/// ```
/// use fast_grapheme_segmenter::count_graphemes;
///
/// assert_eq!(count_graphemes("🇰🇷가나다"), 4); // one flag, three syllables
/// ```
#[must_use]
pub fn count_graphemes(s: &str) -> usize {
    let bytes = s.as_bytes();
    // The first codepoint starts the first cluster (GB1);
    // nothing can break before it, so it is consumed outright.
    let first = match s.chars().next() {
        Some(ch) => ch,
        None => return 0,
    };
    let mut count = 1;
    let mut cat_before = tables::grapheme_category_raw(first);
    let mut state = next_state(ALWAYS, cat_before, first);
    let mut pos = first.len_utf8();

    if !ascii_heavy(bytes) {
        count_gap(&s[pos..], &mut count, &mut cat_before, &mut state);
        return count;
    }
    while pos < bytes.len() {
        let (start, end) = next_run(bytes, pos);
        count_gap(&s[pos..start], &mut count, &mut cat_before, &mut state);
        if start == end {
            break;
        }
        // The run counts wholesale: every byte is its own cluster,
        // except the first when the pair test against what precedes it suppresses the boundary (GB9b, a Prepend).
        if state & PAIR_MASK[(cat_before as usize) << 4 | CAT_ANY as usize] == 0 {
            count += end - start;
        } else {
            count += end - start - 1;
        }
        pos = end;
        cat_before = CAT_ANY;
        state = ALWAYS;
    }
    count
}

/// Forward-only driver over a contiguous string.
///
/// Every rule context GB9c, GB11 and GB12/GB13 needs is carried in the packed state byte.
#[derive(Clone, Debug)]
struct Driver<'a> {
    iter: CharIndices<'a>,
    /// The string being scanned.
    string: &'a str,
    /// Byte offset of the start of the cluster being scanned.
    offset: usize,
    /// Category of the last consumed codepoint.
    cat_before: u8,
    /// Packed rule state for everything consumed so far.
    state: u8,
    /// Whether the string looked printable-ASCII-heavy enough that the one-byte boundary shortcut below pays for itself.
    bulk: bool,
}

impl<'a> Driver<'a> {
    fn new(s: &'a str) -> Driver<'a> {
        let mut driver = Driver {
            iter: s.char_indices(),
            string: s,
            offset: 0,
            cat_before: CAT_ANY,
            state: ALWAYS,
            bulk: ascii_heavy(s.as_bytes()),
        };
        // Consume the first codepoint; nothing can break before it.
        if let Some((_, ch)) = driver.iter.next() {
            driver.cat_before = tables::grapheme_category_raw(ch);
            driver.state = next_state(ALWAYS, driver.cat_before, ch);
        }
        driver
    }

    /// Advance to the next boundary and return its offset.
    #[inline]
    fn next_boundary(&mut self) -> usize {
        // A printable ASCII byte is always followed by a boundary when
        // the next byte is printable ASCII too (GB999), 
        // and the byte consumed leaves (Any, ALWAYS): the cluster starting at `offset` is a single byte,
        // and the state machine only ever sees the edges of such a run.
        // `iter` sits right behind the char at `offset`, which the shortcut just proved to be one byte.
        // Gated on the head sample for the same reason as the counting paths: on text that mixes scripts,
        // this branch mispredicts once per cluster and costs more than the state machine it skips.
        if self.bulk
            && self.offset + 1 < self.string.len()
            && is_printable_ascii(self.string.as_bytes()[self.offset])
            && is_printable_ascii(self.string.as_bytes()[self.offset + 1])
        {
            self.iter.next();
            self.offset += 1;
            self.cat_before = CAT_ANY;
            self.state = ALWAYS;
            return self.offset;
        }
        loop {
            let (i, ch) = match self.iter.next() {
                Some(next) => next,
                None => {
                    // The end of the string is always a boundary (GB2).
                    self.offset = self.string.len();
                    return self.offset;
                }
            };
            let cat_after = tables::grapheme_category_raw(ch);
            let boundary =
                self.state & PAIR_MASK[(self.cat_before as usize) << 4 | cat_after as usize] == 0;
            self.state = next_state(self.state, cat_after, ch);
            self.cat_before = cat_after;
            if boundary {
                self.offset = i;
                return i;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str) -> Vec<&str> {
        graphemes(s).collect()
    }

    #[test]
    fn empty_and_ascii() {
        assert_eq!(split(""), Vec::<&str>::new());
        assert_eq!(split("abcd"), vec!["a", "b", "c", "d"]);
        assert_eq!(count_graphemes(""), 0);
        assert_eq!(count_graphemes("abcd"), 4);
    }

    #[test]
    fn printable_ascii_runs() {
        // Every adjacent printable-ASCII pair is a boundary (GB999)...
        assert_eq!(count_graphemes("abcdefgh"), 8);
        assert_eq!(count_graphemes("hello world"), 11);
        // ...while controls, CR/LF, TAB and DEL fall through to the rules.
        assert_eq!(split("ab\ncd"), vec!["a", "b", "\n", "c", "d"]);
        assert_eq!(split("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert_eq!(split("a\tb"), vec!["a", "\t", "b"]);
        assert_eq!(split("a\u{7f}b"), vec!["a", "\u{7f}", "b"]);
        assert_eq!(split("a\u{1f}b"), vec!["a", "\u{1f}", "b"]);
        // A combining mark still glues onto the run's last byte (GB9).
        assert_eq!(split("tee\u{301}!"), vec!["t", "e", "e\u{301}", "!"]);
        // A prepend swallows the printable byte that follows it (GB9b),
        // and the bulk path resumes only at the next boundary.
        assert_eq!(split("\u{600}ab"), vec!["\u{600}a", "b"]);
        // Run lengths around the 8-byte word scan and its scalar tail.
        for n in [6, 7, 8, 9, 15, 16, 17, 31] {
            let run: String = "x".repeat(n);
            assert_eq!(count_graphemes(&run), n);
            assert_eq!(count_graphemes(&format!("y{run}")), n + 1);
            assert_eq!(count_graphemes(&format!("{run}\u{301}")), n);
            assert_eq!(count_graphemes(&format!("{run}\n{run}")), 2 * n + 1);
            assert_eq!(split(&run).len(), n);
        }
        // The word scan must not read past the run's end.
        let edge = "1234567\u{e9}890"; // 7 printable, 2-byte é, 3 printable
        assert_eq!(split(edge).len(), 11);
    }

    #[test]
    fn crlf() {
        assert_eq!(split("\r\n\r"), vec!["\r\n", "\r"]);
        assert_eq!(split("\n\r\n\r"), vec!["\n", "\r\n", "\r"]);
    }

    #[test]
    fn regional_indicators() {
        // Pairs of flags, then an unpaired one.
        assert_eq!(
            split("\u{1f1f0}\u{1f1f7}\u{1f1e6}\u{1f1f7}"),
            vec!["\u{1f1f0}\u{1f1f7}", "\u{1f1e6}\u{1f1f7}"]
        );
        assert_eq!(count_graphemes("\u{1f1f0}\u{1f1f7}\u{1f1e6}"), 2);
    }

    #[test]
    fn emoji() {
        // Family with skin tones joined by ZWJ: one cluster (GB9, GB11).
        assert_eq!(
            count_graphemes("\u{1f468}\u{200d}\u{1f467}\u{200d}\u{1f466}"),
            1
        );
        assert_eq!(count_graphemes("\u{1F938}\u{1F3FE}\u{1F3FE}"), 1);
        // Rainbow flag and family, both held together by ZWJ + Extend.
        assert_eq!(
            count_graphemes(
                "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}"
            ),
            2
        );
    }

    #[test]
    fn jamo() {
        // Precomposed syllables and decomposed jamo segment alike.
        assert_eq!(
            count_graphemes("\u{1100}\u{1161}\u{11a8}\u{1102}\u{1161}"),
            2
        );
        assert_eq!(count_graphemes("뎌쉐"), 2);
    }

    #[test]
    fn incb_conjuncts() {
        // GB9c: consonant, virama (linker), consonant stay joined...
        let conjunct = "\u{915}\u{94d}\u{915}";
        assert_eq!(split(conjunct), vec![conjunct]);
        // ...and so does a trailing InCB=Extend between the two.
        let linked = "\u{915}\u{93c}\u{94d}\u{915}";
        assert_eq!(split(linked), vec![linked]);
        // A lone virama still attaches to the preceding cluster (GB9).
        assert_eq!(split("\u{915}\u{94d}"), vec!["\u{915}\u{94d}"]);
        // ZWNJ is InCB=None, so it ends the conjunct run and splits the consonants
        // while joining the cluster it follows (GB9).
        assert_eq!(
            split("\u{915}\u{200c}\u{915}"),
            vec!["\u{915}\u{200c}", "\u{915}"]
        );
    }

    #[test]
    fn prepend() {
        // Multiple prepends stack onto the following cluster (GB9b).
        assert_eq!(
            split("\u{20}\u{600}\u{600}\u{20}"),
            vec!["\u{20}", "\u{600}\u{600}\u{20}"]
        );
        assert_eq!(
            split("\u{600}\u{20}\u{20}"),
            vec!["\u{600}\u{20}", "\u{20}"]
        );
    }

    #[test]
    fn indices_and_as_str() {
        let s = "a\u{301}\r\nb";
        let v: Vec<_> = grapheme_indices(s).collect();
        assert_eq!(v, [(0, "a\u{301}"), (3, "\r\n"), (5, "b")]);

        let offsets: Vec<usize> = grapheme_boundaries(s).collect();
        assert_eq!(offsets, [0, 3, 5]);
        assert_eq!(
            grapheme_boundaries("").collect::<Vec<usize>>(),
            Vec::<usize>::new()
        );

        let mut iter = graphemes("ab");
        assert_eq!(iter.next(), Some("a"));
        assert_eq!(iter.as_str(), "b");
        assert_eq!(iter.next(), Some("b"));
        assert_eq!(iter.as_str(), "");
        assert_eq!(iter.next(), None);

        let mut bounds = grapheme_boundaries("a\u{301}b");
        assert_eq!(bounds.next(), Some(0));
        // The remaining, not yet yielded part starts at the next cluster.
        assert_eq!(bounds.as_str(), "b");
        assert_eq!(bounds.next(), Some(3));
        assert_eq!(bounds.next(), None);
    }

    /// `Iterator::count` restarts the count at the current offset,
    /// which is only sound because a cluster head re-establishes every packed state bit its rules depend on.
    /// This hammers that claim over random strings.
    #[test]
    fn count_after_partial_consumption() {
        const RANGES: &[(u32, u32)] = &[
            (0x0, 0x7F),
            (0x300, 0x370),
            (0x600, 0x605),
            (0x903, 0x94d),
            (0x1100, 0x11FF),
            (0xAC00, 0xAC1C),
            (0x200B, 0x200D),
            (0x3297, 0x3299),
            (0xFE00, 0xFE0F),
            (0x1F1E6, 0x1F1FF),
            (0x1F3FB, 0x1F3FF),
            (0x1F400, 0x1F680),
            (0xE0020, 0xE007F),
        ];

        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for _ in 0..4_000 {
            let len = (next() % 20) as usize;
            let mut s = String::new();
            for _ in 0..len {
                let (lo, hi) = RANGES[(next() % RANGES.len() as u64) as usize];
                let cp = lo as u64 + next() % (hi as u64 - lo as u64 + 1);
                s.push(char::from_u32(cp as u32).unwrap());
            }
            let total = count_graphemes(&s);
            assert_eq!(graphemes(&s).count(), total, "full count on {:?}", s);
            assert_eq!(
                grapheme_boundaries(&s).count(),
                total,
                "full boundary count on {:?}",
                s
            );
            for k in 0..=total {
                let mut clusters = graphemes(&s);
                for _ in 0..k {
                    assert!(clusters.next().is_some(), "under-count on {:?}", s);
                }
                assert_eq!(
                    clusters.count(),
                    total - k,
                    "after {} clusters of {:?}",
                    k,
                    s
                );

                let mut offsets = grapheme_boundaries(&s);
                for _ in 0..k {
                    assert!(offsets.next().is_some(), "under-boundaries on {:?}", s);
                }
                assert_eq!(
                    offsets.count(),
                    total - k,
                    "after {} boundaries of {:?}",
                    k,
                    s
                );
            }
        }
    }

    #[test]
    fn size_hint() {
        let mut iter = graphemes("a\u{301}bc");
        assert_eq!(iter.size_hint(), (1, Some(5)));
        iter.next();
        assert_eq!(iter.size_hint(), (1, Some(2)));
    }

    #[test]
    fn demonic() {
        // Zalgo text is one cluster per base character.
        assert_eq!(
            count_graphemes("Z\u{335}\u{36e}\u{34b}A\u{35c}\u{331}\u{334}L\u{328}\u{332}"),
            3
        );
    }
}
