#!/usr/bin/env python3

"""Generate `src/tables.rs` and `tests/testdata.rs` for fast-grapeme-segmenter.

Downloads the UCD files for `UNICODE_VERSION` (cached under `scripts/ucd/`), parses
the Grapheme_Cluster_Break properties, and emits:

- `src/tables.rs`: direct byte-per-codepoint lookup windows for the hot
  codepoint regions, arithmetic shortcuts for the regions whose category is
  computable, and a packed residual range table for everything rare.
- `tests/testdata.rs`: the official `auxiliary/GraphemeBreakTest.txt` cases.

Every arithmetic shortcut and every table is verified against the parsed UCD
data before anything is written, and the emitted lookup is replayed over the
whole codepoint space, so a future Unicode version that invalidates an
assumption fails loudly here instead of silently mis-segmenting.

Usage: python3 scripts/unicode.py
"""

import os
import re
import sys
import urllib.request

UNICODE_VERSION = (17, 0, 0)

UNICODE_VERSION_NUMBER = "%d.%d.%d" % UNICODE_VERSION

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ucd")

MAX_CODEPOINT = 0x10FFFF

SURROGATES = (0xD800, 0xDFFF)

# Direct-lookup windows for the Grapheme_Cluster_Break category.
#
#   [0, T0_END)            T0, direct
#   [T0_END, T1_LO)        CJK..Vai, all Any but two Extended_Pictographic
#   [T1_LO, T1_HI)         T1, direct
#   [HANGUL_LO, HANGUL_HI) Hangul syllables, LV every 28th and LVT otherwise
#   [VS_LO, VS_HI)         variation selectors, all Extend
#   [T2_LO, T2_HI)         T2, direct (emoji)
#   everything else        residual range table
T0_END = 0x30A0  # ends just past the last combining kana mark
T1_LO = 0xA660
T1_HI = 0xAC00
HANGUL_LO = 0xAC00
HANGUL_HI = 0xD7A4
VS_LO = 0xFE00
VS_HI = 0xFE10
T2_LO = 0x1F000
T2_HI = 0x1FB00

# Bits the residual table reserves for the category, so that a range packs
# into `(start, end << CAT_BITS | category)`.
CAT_BITS = 5

# Codepoints that are the only two non-Any values in the CJK..Vai gap.
GAP_PICTOGRAPHIC = (0x3297, 0x3299)


def fetch(name, url):
    path = os.path.join(CACHE, name)
    if not os.path.exists(path):
        os.makedirs(CACHE, exist_ok=True)
        sys.stderr.write("downloading %s\n" % url)
        urllib.request.urlretrieve(url, path)
    return path


def ucd(rel):
    return fetch(
        rel.replace("/", "_"),
        "https://www.unicode.org/Public/%s/ucd/%s" % (UNICODE_VERSION_NUMBER, rel),
    )


def group(ranges):
    """Sort and merge a list of (lo, hi) codepoint ranges."""
    out = []
    lo = hi = None
    for (start, end) in sorted(set(ranges)):
        if lo is None:
            lo, hi = start, end
        elif start <= hi + 1:
            hi = max(hi, end)
        else:
            out.append((lo, hi))
            lo, hi = start, end
    if lo is not None:
        out.append((lo, hi))
    return out


def parse_properties(path, wanted):
    """Parse a UCD property file into `{name: [(lo, hi), ...]}`."""
    found = {}
    one = re.compile(r"^\s*([0-9A-Fa-f]+)\s*;\s*([\w=]+)(?:\s*;\s*(\w+))?")
    span = re.compile(r"^\s*([0-9A-Fa-f]+)\.\.([0-9A-Fa-f]+)\s*;\s*([\w=]+)(?:\s*;\s*(\w+))?")
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = span.match(line) or one.match(line)
            if not m:
                continue
            if m.re is span:
                lo, hi, prop, value = m.groups()
            else:
                lo, prop, value = m.groups()
                hi = lo
            lo = int(lo, 16)
            hi = int(hi, 16)
            if value is not None:
                prop = "%s=%s" % (prop, value)
            if prop not in wanted:
                continue
            found.setdefault(prop, []).append((lo, hi))
    return {name: group(ranges) for (name, ranges) in found.items()}


def load_grapheme_categories():
    """The Grapheme_Cluster_Break categories, plus `InCB=Consonant` folded in.

    `InCB=Consonant` never overlaps another category, so it becomes a category
    of its own; the pairwise rule table indexes all of them with 4 bits.
    """
    gcb = parse_properties(
        ucd("auxiliary/GraphemeBreakProperty.txt"),
        {"CR", "Control", "Extend", "L", "LF", "LV", "LVT", "Prepend",
         "Regional_Indicator", "SpacingMark", "T", "V", "ZWJ"},
    )
    # The Control category includes Cs (surrogates) in the UCD, but Rust `char`s
    # are scalar values only, so surrogates can never be looked up.
    gcb["Control"] = [
        (lo, hi) for (lo, hi) in gcb["Control"]
        if not (SURROGATES[0] <= lo and hi <= SURROGATES[1])
    ]

    derived = parse_properties(
        ucd("DerivedCoreProperties.txt"),
        {"InCB=Consonant", "InCB=Extend", "InCB=Linker"},
    )
    gcb["InCB_Consonant"] = derived["InCB=Consonant"]

    emoji = parse_properties(ucd("emoji/emoji-data.txt"), {"Extended_Pictographic"})
    gcb["Extended_Pictographic"] = emoji["Extended_Pictographic"]
    return gcb, derived["InCB=Extend"], derived["InCB=Linker"]


def build_catmap(grapheme_table, cats):
    cat_index = {name: i for i, name in enumerate(cats)}
    catmap = bytearray(MAX_CODEPOINT + 1)  # Any == 0
    for (lo, hi, cat) in grapheme_table:
        catmap[lo:hi + 1] = bytes([cat_index[cat]]) * (hi + 1 - lo)
    return catmap, cat_index


def fail(msg):
    raise AssertionError(
        "the lookups in scripts/unicode.py are stale for Unicode %s: %s"
        % (UNICODE_VERSION_NUMBER, msg)
    )


def check_shortcuts(catmap, cat_index, incb_extend, incb_linker):
    """Assert every arithmetic shortcut against the parsed UCD data."""
    # The CJK..Vai gap is resolved with two equality tests.
    gap = {cp for cp in range(T0_END, T1_LO) if catmap[cp] != cat_index["Any"]}
    if gap != set(GAP_PICTOGRAPHIC):
        fail("expected only U+3297 and U+3299 to be non-Any in [%#x, %#x), found %s"
             % (T0_END, T1_LO, sorted(hex(c) for c in gap)))
    for cp in gap:
        if catmap[cp] != cat_index["Extended_Pictographic"]:
            fail("U+%04X is no longer Extended_Pictographic" % cp)

    # Hangul syllables are LV at every 28th codepoint and LVT otherwise.
    for cp in range(HANGUL_LO, HANGUL_HI):
        want = "LV" if (cp - HANGUL_LO) % 28 == 0 else "LVT"
        if catmap[cp] != cat_index[want]:
            fail("U+%04X is not %s" % (cp, want))

    for cp in range(VS_LO, VS_HI):
        if catmap[cp] != cat_index["Extend"]:
            fail("U+%04X is not Extend" % cp)

    # InCB=Extend is derived at runtime as
    # [gcb=Extend gcb=ZWJ] - InCB=Linker - U+200C, so no table is emitted for it.
    # Every InCB=Linker being gcb=Extend or gcb=ZWJ is what lets the state
    # machine skip the linker test for every other category.
    allowed = {cat_index["Extend"], cat_index["ZWJ"]}
    for (label, ranges) in (("InCB=Extend", incb_extend), ("InCB=Linker", incb_linker)):
        for (lo, hi) in ranges:
            for cp in range(lo, hi + 1):
                if catmap[cp] not in allowed:
                    fail("U+%04X is %s but its Grapheme_Cluster_Break is not "
                         "Extend or ZWJ" % (cp, label))
    linkers = {cp for (lo, hi) in incb_linker for cp in range(lo, hi + 1)}
    actual = {cp for (lo, hi) in incb_extend for cp in range(lo, hi + 1)}
    derived = {cp for cp in range(MAX_CODEPOINT + 1)
               if catmap[cp] in allowed and cp not in linkers and cp != 0x200C}
    if actual != derived:
        fail("InCB=Extend is no longer [gcb=Extend gcb=ZWJ] - InCB=Linker - "
             "U+200C: missing=%s extra=%s"
             % ([hex(c) for c in sorted(actual - derived)[:8]],
                [hex(c) for c in sorted(derived - actual)[:8]]))


def build_residual(catmap, cat_index):
    """The ranges outside the direct windows and the arithmetic shortcuts."""
    regions = [
        (HANGUL_HI, VS_LO),
        (VS_HI, T2_LO),
        (T2_HI, MAX_CODEPOINT + 1),
    ]
    residual = []
    for (rlo, rhi) in regions:
        cp = rlo
        while cp < rhi:
            v = catmap[cp]
            if v == cat_index["Any"]:
                cp += 1
                continue
            start = cp
            while cp < rhi and catmap[cp] == v:
                cp += 1
            residual.append((start, cp - 1, v))
    for (_, end, v) in residual:
        assert end >> (32 - CAT_BITS) == 0 and v < (1 << CAT_BITS)
    return residual


def replay_lookup(cp, catmap, cat_index, packed):
    """The lookup exactly as `grapheme_category_raw` will perform it.

    The window branches read the flat category map directly, which is also
    what the emitted byte windows contain, verbatim.
    """
    if cp < T0_END:
        return catmap[cp]
    if cp < T1_LO:
        return (cat_index["Extended_Pictographic"] if cp in GAP_PICTOGRAPHIC
                else cat_index["Any"])
    if cp < T1_HI:
        return catmap[cp]
    if cp < HANGUL_HI:
        return cat_index["LV"] if (cp - HANGUL_LO) % 28 == 0 else cat_index["LVT"]
    if T2_LO <= cp < T2_HI:
        return catmap[cp]
    if VS_LO <= cp < VS_HI:
        return cat_index["Extend"]
    lo, hi = 0, len(packed)
    while lo < hi:
        mid = (lo + hi) // 2
        (start, pk) = packed[mid]
        if cp < start:
            hi = mid
        elif cp > pk >> CAT_BITS:
            lo = mid + 1
        else:
            return pk & ((1 << CAT_BITS) - 1)
    return cat_index["Any"]


def emit_byte_window(f, name, values, per_line=32):
    """A direct-indexed category window, one category byte per codepoint.

    The JavaScript original packs two categories per byte to halve its
    download size, at the cost of a shift-and-mask on every lookup. In Rust
    the table is read-only data whose size is nearly free while a hot-loop
    load is not, so the window stays flat: the lookup is a single indexed
    load with no decode.

    Measured on the wasm artifact too: `wasm-opt` already folds the flat
    tables' zero runs, so packing nets only ~2 KB of the shipped module
    while costing 10-45% on table-heavy counting.
    """
    assert all(0 <= v <= 0xF for v in values), "a category does not fit a byte"
    f.write("const %s: &[u8; %d] = b\"" % (name, len(values)))
    for i in range(0, len(values), per_line):
        if i:
            f.write("\\\n    ")
        f.write("".join("\\x%02x" % v for v in values[i:i + per_line]))
    f.write("\";\n\n")


def emit_tables(path, catmap, cat_index, residual, cats):
    any_ = cat_index["Any"]
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write("""\
// NOTE: generated by scripts/unicode.py from Unicode %s data. Do not edit.
#![allow(non_upper_case_globals)]

/// The version of Unicode the tables below are generated from.
pub const UNICODE_VERSION: (u8, u8, u8) = (%d, %d, %d);

/// `Grapheme_Cluster_Break` property values, plus `InCB=Consonant` folded in
/// as a category of its own since it never overlaps another one.
///
/// The discriminants are the values stored in the tables below and are the
/// index space of the pairwise rule table in `src/lib.rs`.
#[allow(non_camel_case_types)]
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphemeCat {
""" % (UNICODE_VERSION_NUMBER, *UNICODE_VERSION))
        for cat in cats:
            f.write("    GC_%s = %d,\n" % (cat, cat_index[cat]))
        f.write("}\n\nuse self::GraphemeCat::*;\n\n")
        f.write("""\
/// Every `GraphemeCat` in discriminant order, so `CATS[i] as u8 == i`.
#[allow(missing_docs)]
pub const CATS: [GraphemeCat; 16] = [
""")
        f.write("    " + ",\n    ".join("GC_%s" % c for c in cats))
        f.write(""",
];

/// `Grapheme_Cluster_Break` of `c`, as a raw category byte.
///
/// This is the hot path of grapheme cluster segmentation: everything below
/// U+%X (every script whose clusters are non-trivial, plus latin, cyrillic
/// and the kana) and the whole emoji block are a single load, the CJK
/// ideographs and the hangul syllables are arithmetic, and only the rare
/// tail reaches a binary search.
///
/// The windows hold one category byte per codepoint. Unlike the JavaScript
/// original, which packs two per byte to halve its bundle size, the table is
/// read-only data here, and paying a shift-and-mask per lookup to halve it
/// again trades hot-loop work for bytes Rust does not care about — even on
/// the wasm artifact, where `wasm-opt` folds the flat tables' zero runs and
/// packing nets only ~2 KB.
#[inline]
pub fn grapheme_category_raw(c: char) -> u8 {
    let cp = c as u32;
    if cp < 0x%X {
        return grapheme_cat_t0[cp as usize];
    }
    // CJK through Vai: all Any except U+3297/U+3299 (Extended_Pictographic).
    if cp < 0x%X {
        return if cp == 0x3297 || cp == 0x3299 { %d } else { %d };
    }
    if cp < 0x%X {
        return grapheme_cat_t1[cp as usize - 0x%X];
    }
    // Hangul syllables: LV at every 28th from 0xAC00, LVT otherwise.
    if cp < 0x%X {
        return if (cp - 0xAC00).is_multiple_of(28) { %d } else { %d };
    }
    if (0x%X..0x%X).contains(&cp) {
        return grapheme_cat_t2[cp as usize - 0x%X];
    }
    // Variation selectors: all Extend.
    if cp >> 4 == 0x%X {
        return %d;
    }
    grapheme_category_residual(cp)
}

/// Cold half of [`grapheme_category_raw`], kept out of line so the hot half
/// stays small enough to inline.
fn grapheme_category_residual(cp: u32) -> u8 {
    use core::cmp::Ordering::{Equal, Greater, Less};
    match grapheme_cat_residual.binary_search_by(|&(start, packed)| {
        if cp < start {
            Greater
        } else if cp > packed >> %d {
            Less
        } else {
            Equal
        }
    }) {
        core::result::Result::Ok(idx) => (grapheme_cat_residual[idx].1 & %d) as u8,
        core::result::Result::Err(_) => %d,
    }
}

/// The Unicode `Indic_Conjunct_Break=Linker` set.
///
/// Both InCB roles are subsets of `gcb=Extend` and `gcb=ZWJ`, which is what
/// lets the state machine skip this test for every other category.
#[inline]
pub fn is_incb_linker(c: char) -> bool {
    matches!(c, %s)
}

""" % (T0_END,
               T0_END, T1_LO, cat_index["Extended_Pictographic"], any_,
               T1_HI, T1_LO,
               HANGUL_HI, cat_index["LV"], cat_index["LVT"],
               T2_LO, T2_HI, T2_LO,
               VS_LO >> 4, cat_index["Extend"],
               CAT_BITS, (1 << CAT_BITS) - 1, any_,
               " | ".join("'\\u{%X}'%s" % (lo, "" if lo == hi else "..='\\u{%X}'" % hi)
                          for (lo, hi) in incb_linker_ranges)))

        f.write("/// Ranges outside the direct windows, as `(start, end << %d | category)`.\n"
                % CAT_BITS)
        f.write("const grapheme_cat_residual: &[(u32, u32)] = &[\n    ")
        f.write(",\n    ".join("(0x%X, 0x%X)" % (start, end << CAT_BITS | v)
                               for (start, end, v) in residual))
        f.write(",\n];\n\n")

        emit_byte_window(f, "grapheme_cat_t0", catmap[0:T0_END])
        emit_byte_window(f, "grapheme_cat_t1", catmap[T1_LO:T1_HI])
        emit_byte_window(f, "grapheme_cat_t2", catmap[T2_LO:T2_HI])


incb_linker_ranges = []


def parse_break_test(path):
    """The official `GraphemeBreakTest.txt` cases as `(input, clusters)` pairs."""
    cases = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            body = line.split("#")[0].strip()
            if not body:
                continue
            tokens = body.split()
            # ÷ 0020 × 0308 ÷ ... ÷
            assert tokens[0] == "÷" and tokens[-1] == "÷", line
            cps = [int(t, 16) for t in tokens[1:-1:2]]
            ops = tokens[2:-1:2]
            assert len(cps) == len(ops) + 1, line
            if any(SURROGATES[0] <= cp <= SURROGATES[1] for cp in cps):
                continue
            clusters = []
            current = [cps[0]]
            for (op, cp) in zip(ops, cps[1:]):
                if op == "÷":
                    clusters.append(current)
                    current = [cp]
                else:
                    current.append(cp)
            clusters.append(current)
            cases.append((cps, clusters))
    return cases


def emit_testdata(path, cases):
    def escape(cps):
        return "".join("\\u{%x}" % cp for cp in cps)

    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write("// NOTE: generated by scripts/unicode.py from the Unicode %s\n"
                % UNICODE_VERSION_NUMBER)
        f.write("// auxiliary/GraphemeBreakTest.txt. Do not edit.\n\n")
        f.write("/// (input, expected extended grapheme clusters) pairs.\n")
        f.write("pub const TEST_CASES: &[(&str, &[&str])] = &[\n")
        for (cps, clusters) in cases:
            f.write('    ("%s", &[%s]),\n'
                    % (escape(cps),
                       ", ".join('"%s"' % escape(cluster) for cluster in clusters)))
        f.write("];\n")


def main():
    global incb_linker_ranges

    grapheme_cats, incb_extend, incb_linker = load_grapheme_categories()
    incb_linker_ranges = incb_linker

    grapheme_table = []
    for cat, ranges in grapheme_cats.items():
        grapheme_table.extend((lo, hi, cat) for (lo, hi) in ranges)
    grapheme_table.sort(key=lambda r: r[0])
    last = -1
    for (lo, _, _) in grapheme_table:
        if lo <= last:
            fail("Grapheme_Cluster_Break values overlap; InCB=Consonant and "
                 "Extended_Pictographic no longer avoid every other category")
        last = lo

    cats = sorted(list(grapheme_cats.keys()) + ["Any"])
    if len(cats) != 16:
        fail("expected exactly 16 categories for a 4-bit index, got %d: %s"
             % (len(cats), cats))
    catmap, cat_index = build_catmap(grapheme_table, cats)
    if cats[0] != "Any":
        fail("GC_Any must be category 0, the residual table's default")

    check_shortcuts(catmap, cat_index, incb_extend, incb_linker)
    residual = build_residual(catmap, cat_index)
    packed = [(start, end << CAT_BITS | v) for (start, end, v) in residual]

    for cp in range(MAX_CODEPOINT + 1):
        got = replay_lookup(cp, catmap, cat_index, packed)
        if got != catmap[cp]:
            fail("the emitted lookup disagrees with the range table at U+%04X "
                 "(got %d, want %d)" % (cp, got, catmap[cp]))
    window_bytes = T0_END + (T1_HI - T1_LO) + (T2_HI - T2_LO)
    sys.stderr.write(
        "grapheme: %d window bytes + %d residual ranges (%d B), "
        "verified over all %d codepoints\n"
        % (window_bytes, len(residual), len(residual) * 8, MAX_CODEPOINT + 1))

    emit_tables(os.path.join(ROOT, "src", "tables.rs"),
                catmap, cat_index, residual, cats)
    cases = parse_break_test(ucd("auxiliary/GraphemeBreakTest.txt"))
    emit_testdata(os.path.join(ROOT, "tests", "testdata.rs"), cases)
    sys.stderr.write("testdata: %d GraphemeBreakTest cases\n" % len(cases))


if __name__ == "__main__":
    main()
