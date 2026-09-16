//! Conformance and cross-implementation tests.

#[rustfmt::skip]
mod testdata;

use fast_grapheme_segmenter::{
    count_graphemes, count_graphemes_utf16, grapheme_boundaries, grapheme_boundaries_utf16,
    grapheme_indices, graphemes,
};

/// The official Unicode `GraphemeBreakTest.txt` cases for Unicode 17.
#[test]
fn ucd_grapheme_break_test() {
    for (i, &(input, expected)) in testdata::TEST_CASES.iter().enumerate() {
        let got: Vec<&str> = graphemes(input).collect();
        assert_eq!(got, *expected, "case {}: {:?}", i, input);

        let count = count_graphemes(input);
        assert_eq!(count, expected.len(), "count for case {}: {:?}", i, input);

        let mut offsets = Vec::with_capacity(expected.len());
        let mut bytes = 0;
        for cluster in expected {
            offsets.push(bytes);
            bytes += cluster.len();
        }
        let got_offsets: Vec<usize> = grapheme_indices(input).map(|(i, _)| i).collect();
        assert_eq!(got_offsets, offsets, "indices for case {}: {:?}", i, input);

        let got_bounds: Vec<usize> = grapheme_boundaries(input).collect();
        assert_eq!(
            got_bounds, offsets,
            "boundaries for case {}: {:?}",
            i, input
        );

        assert_eq!(
            graphemes(input).count(),
            expected.len(),
            "iterator count for case {}: {:?}",
            i,
            input
        );

        // The same segmentation through the UTF-16 frontend: the same count,
        // and boundaries at the cumulative UTF-16 lengths of the clusters.
        let units: Vec<u16> = input.encode_utf16().collect();
        assert_eq!(
            count_graphemes_utf16(&units),
            expected.len(),
            "utf16 count for case {}: {:?}",
            i,
            input
        );
        let mut utf16_offsets = Vec::with_capacity(expected.len());
        let mut units_so_far = 0;
        for cluster in expected {
            utf16_offsets.push(units_so_far);
            units_so_far += cluster.encode_utf16().count();
        }
        let got_utf16: Vec<usize> = grapheme_boundaries_utf16(&units).collect();
        assert_eq!(
            got_utf16, utf16_offsets,
            "utf16 boundaries for case {}: {:?}",
            i, input
        );
    }
}

/// The UTF-16 frontend treats unpaired surrogates as U+FFFD (category
/// `Any`): each is its own cluster and never glues to its neighbors.
#[test]
fn utf16_lone_surrogates() {
    use fast_grapheme_segmenter::count_graphemes_utf16;

    // High surrogate with no low in sight, low surrogate with no high.
    let lone_high: Vec<u16> = vec![0xD83C, 'a' as u16];
    assert_eq!(count_graphemes_utf16(&lone_high), 2);
    let lone_low: Vec<u16> = vec![0xDDF0, 'a' as u16];
    assert_eq!(count_graphemes_utf16(&lone_low), 2);

    // A lone surrogate between a base and a combining mark breaks both
    // sides: the mark glues to the surrogate's replacement, not the base.
    let sandwich: Vec<u16> = vec!['a' as u16, 0xD800, 0x0301];
    let bounds: Vec<usize> =
        fast_grapheme_segmenter::grapheme_boundaries_utf16(&sandwich).collect();
    assert_eq!(bounds, [0, 1]);

    // A surrogate pair is one codepoint, not two replacement clusters.
    let pair: Vec<u16> = vec![0xD83C, 0xDDF0, 0xD83C, 0xDDF7];
    assert_eq!(count_graphemes_utf16(&pair), 1);

    assert_eq!(count_graphemes_utf16(&[]), 0);
}

/// The clusters of every test case re-join into the original input, in order.
#[test]
fn clusters_join_back() {
    for &(input, _) in testdata::TEST_CASES {
        let joined = graphemes(input).collect::<String>();
        assert_eq!(joined, input);
    }
}

/// Agreement with `icu_segmenter` (icu4x) over every conformance case.
#[test]
fn matches_icu4x() {
    use icu_segmenter::GraphemeClusterSegmenter;

    let segmenter = GraphemeClusterSegmenter::new();
    for &(input, _) in testdata::TEST_CASES {
        let ours: Vec<&str> = graphemes(input).collect();
        let mut theirs: Vec<&str> = Vec::with_capacity(ours.len());
        let mut prev: Option<usize> = None;
        for b in segmenter.segment_str(input) {
            if let Some(p) = prev.replace(b) {
                theirs.push(&input[p..b]);
            }
        }
        assert_eq!(ours, theirs, "divergence on {:?}", input);
    }
}

/// Agreement with `icu_segmenter` over pseudo-random strings drawn
/// from the codepoint neighborhoods the stateful rules live in.
///
/// A xorshift PRNG keeps this deterministic and dependency-free.
#[test]
fn matches_icu4x_random() {
    use icu_segmenter::GraphemeClusterSegmenter;

    // Ranges chosen to hit every category and rule: ASCII, CR/LF, combining
    // marks, viramas, ZWJ/ZWNJ, prepends, jamo, hangul syllables, kana,
    // pictographics, regional indicators, tag characters and plane 14+ tails.
    const RANGES: &[(u32, u32)] = &[
        (0x0, 0x7F),
        (0x300, 0x370),
        (0x600, 0x605),
        (0x903, 0x94d),
        (0x9cd, 0x9d7),
        (0x1100, 0x11FF),
        (0xAC00, 0xAC1C),
        (0x200B, 0x200D),
        (0x302A, 0x302F),
        (0x3030, 0x3030),
        (0x3297, 0x3299),
        (0xA960, 0xA97C),
        (0xFE00, 0xFE0F),
        (0x1F1E6, 0x1F1FF),
        (0x1F3FB, 0x1F3FF),
        (0x1F400, 0x1F680),
        (0xE0020, 0xE007F),
        (0xE0100, 0xE01EF),
        (0x10A38, 0x10A50),
        (0x11000, 0x110C2),
        (0x11F00, 0x11F42),
    ];

    let segmenter = GraphemeClusterSegmenter::new();
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    for round in 0..20_000u32 {
        let len = (next() % 24) as usize;
        let mut s = String::new();
        for _ in 0..len {
            let (lo, hi) = RANGES[(next() % RANGES.len() as u64) as usize];
            let cp = lo as u64 + next() % (hi as u64 - lo as u64 + 1);
            s.push(char::from_u32(cp as u32).unwrap());
        }
        let ours: Vec<&str> = graphemes(&s).collect();
        let mut theirs: Vec<&str> = Vec::with_capacity(ours.len());
        let mut prev: Option<usize> = None;
        for b in segmenter.segment_str(&s) {
            if let Some(p) = prev.replace(b) {
                theirs.push(&s[p..b]);
            }
        }
        if ours != theirs {
            panic!(
                "divergence in round {} on {:?}: ours {:?}, theirs {:?}",
                round, s, ours, theirs
            );
        }
        assert_eq!(count_graphemes(&s), ours.len(), "count on {:?}", s);
    }
}
