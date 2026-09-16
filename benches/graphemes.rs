//! Benchmark against `icu_segmenter` (icu4x) over samples of real-world text.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use fast_grapheme_segmenter::count_graphemes;
use fast_grapheme_segmenter::grapheme_boundaries;
use fast_grapheme_segmenter::graphemes;
use icu_segmenter::{GraphemeClusterSegmenter, GraphemeClusterSegmenterBorrowed};
use unicode_segmentation::UnicodeSegmentation;

const ENGLISH: &str = "\
The Universal Declaration of Human Rights sets out, for the first time, \
fundamental human rights to be universally protected. All human beings are \
born free and equal in dignity and rights. They are endowed with reason and \
conscience and should act towards one another in a spirit of brotherhood.";

const RUSSIAN: &str = "\
Все люди рождаются свободными и равными в своем достоинстве и правах. \
Они наделены разумом и совестью и должны поступать в отношении друг друга \
в духе братства. Никто не должен содержаться в рабстве или подневольном \
состоянии; рабство и работорговля запрещаются во всех их видах.";

const ARABIC: &str = "\
يولد جميع الناس أحرارًا متساوين في الكرامة والحقوق. وقد وهبوا عقلًا وضميرًا \
وعليهم أن يعامل بعضهم بعضًا بروح الإخاء. لكل شخص حق التمتع بجميع الحقوق \
والحريات المذكورة في هذا الإعلان، دون أي تمييز.";

const HINDI: &str = "\
सभी मनुष्यों को गौरव और अधिकारों के मामले में जन्मजात स्वतंत्रता प्राप्त है। \
उन्हें विवेक और अंतःकरण प्राप्त है और परस्पर उन्हें भाईचारे के भाव से \
व्यवहार करना चाहिए। किसी के साथ भेदभाव नहीं किया जाएगा।";

const JAPANESE: &str = "\
すべての人間は、生まれながらにして自由であり、かつ、尊厳と権利とについて\
平等である。人間は、理性と良心とを授けられており、互いに同胞の精神を\
もって行動しなければならない。すべての人間は、いかなる差别も受けない。";

const KOREAN: &str = "\
모든 인간은 태어날 때부터 자유로우며 그 존엄과 권리에 있어 동등하다. \
인간은 천부적으로 이성과 양심을 부여받았으며 서로 형제애의 정신으로 \
행동하여야 한다. 모든 사람은 이 선언에 규정된 권리와 자유를 충분히 \
누릴 자격이 있다.";

const MANDARIN: &str = "\
人人生而自由，在尊严和权利上一律平等。他们赋有理性和良心，并应以兄弟\
关系的精神相对待。人人有资格享有本宣言所载的一切权利和自由，人人完全\
平等地有权享受这些权利和自由，不分种族、肤色、性别、语言、宗教。";

const EMOJI: &str = "\
🏳️‍🌈👩‍👩‍👧‍👦👮🏻‍♀️👨🏽‍💻🧑🏾‍🤝‍🧑🏻🇰🇷🇯🇵👨‍👩‍👧‍👧🧑‍🚀🙋🏽\u{1f938}\u{1f3fe}\
🇺🇳🇧🇷🏳️‍⚧️👵🏼 policeman 👨🏻‍🚒 firefighter 🙅🏿‍♂️\u{1f469}\u{200d}\u{1f373}\
🇩🇪🇫🇷🇮🇹 superhero 🦸🏻‍♀️ 🦸🏿‍♂️";

const SOURCE_CODE: &str = r#"
/// State transition on consuming an `Extend` in an InCB run.
fn extend_in_incb_run(state: u8, ch: char) -> u8 {
    if ch == '\u{200c}' {
        state & EXTEND_KEEP
    } else if state & INCB_LINKED != 0 || tables::is_incb_linker(ch) {
        state & EXTEND_KEEP | INCB_LINKED | INCB_RUN
    } else {
        state & EXTEND_KEEP | INCB_RUN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    assert_eq!(count_graphemes("한국어"), 3);
}
"#;

const CORPORA: &[(&str, &str)] = &[
    ("english", ENGLISH),
    ("russian", RUSSIAN),
    ("arabic", ARABIC),
    ("hindi", HINDI),
    ("japanese", JAPANESE),
    ("korean", KOREAN),
    ("mandarin", MANDARIN),
    ("emoji", EMOJI),
    ("source_code", SOURCE_CODE),
];

/// Collect every cluster of `s` from the boundaries `segmenter` yields.
fn icu_collect<'a>(segmenter: &GraphemeClusterSegmenterBorrowed<'_>, s: &'a str) -> Vec<&'a str> {
    let mut clusters = Vec::new();
    let mut prev: Option<usize> = None;
    for b in segmenter.segment_str(s) {
        if let Some(p) = prev.replace(b) {
            clusters.push(&s[p..b]);
        }
    }
    clusters
}

fn segment(c: &mut Criterion) {
    let segmenter = GraphemeClusterSegmenter::new();
    let mut group = c.benchmark_group("segment");
    for (name, text) in CORPORA {
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(
            BenchmarkId::new(*name, "fast-grapheme-segmenter"),
            text,
            |b, s| b.iter(|| black_box(graphemes(black_box(s))).collect::<Vec<_>>()),
        );
        group.bench_with_input(BenchmarkId::new(*name, "icu_segmenter"), text, |b, s| {
            b.iter(|| icu_collect(black_box(&segmenter), black_box(s)))
        });
        group.bench_with_input(
            BenchmarkId::new(*name, "unicode-segmentation"),
            text,
            |b, s| b.iter(|| black_box(s).graphemes(true).collect::<Vec<_>>()),
        );
    }
    group.finish();
}

fn count(c: &mut Criterion) {
    let segmenter = GraphemeClusterSegmenter::new();
    let mut group = c.benchmark_group("count");
    for (name, text) in CORPORA {
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(
            BenchmarkId::new(*name, "fast-grapheme-segmenter"),
            text,
            |b, s| b.iter(|| count_graphemes(black_box(s))),
        );
        group.bench_with_input(BenchmarkId::new(*name, "icu_segmenter"), text, |b, s| {
            b.iter(|| black_box(&segmenter).segment_str(black_box(s)).count())
        });
        group.bench_with_input(
            BenchmarkId::new(*name, "unicode-segmentation"),
            text,
            |b, s| b.iter(|| black_box(s).graphemes(true).count()),
        );
    }
    group.finish();
}

/// Collecting cluster-start offsets: our boundaries iterator against the
/// boundary iterators the other two natively expose.
fn boundaries(c: &mut Criterion) {
    let segmenter = GraphemeClusterSegmenter::new();
    let mut group = c.benchmark_group("boundaries");
    for (name, text) in CORPORA {
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(
            BenchmarkId::new(*name, "fast-grapheme-segmenter"),
            text,
            |b, s| b.iter(|| grapheme_boundaries(black_box(s)).collect::<Vec<_>>()),
        );
        group.bench_with_input(BenchmarkId::new(*name, "icu_segmenter"), text, |b, s| {
            b.iter(|| {
                black_box(&segmenter)
                    .segment_str(black_box(s))
                    .collect::<Vec<_>>()
            })
        });
        group.bench_with_input(
            BenchmarkId::new(*name, "unicode-segmentation"),
            text,
            |b, s| {
                b.iter(|| {
                    black_box(s)
                        .grapheme_indices(true)
                        .map(|(i, _)| i)
                        .collect::<Vec<_>>()
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, segment, count, boundaries);
criterion_main!(benches);
