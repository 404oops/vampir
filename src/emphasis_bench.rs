//! Frozen pre-cleanup highlighter for differential tests and opt-in timing.
use super::{Line, matched_ranges};
use std::ops::Range;

fn baseline_matched_ranges(text: &str, query: &str, line: Line) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let mut ranges = Vec::new();
    for word in query.split_whitespace() {
        let needle: Vec<char> = word.chars().collect();
        let starts_word = |start: usize| start == 0 || !chars[start - 1].1.is_alphanumeric();
        let whole = (0..=chars.len().saturating_sub(needle.len()))
            .find(|&start| {
                start + needle.len() <= chars.len()
                    && (line == Line::Label || starts_word(start))
                    && needle
                        .iter()
                        .enumerate()
                        .all(|(offset, &ch)| same(chars[start + offset].1, ch))
            })
            .map(|start| (start..start + needle.len()).collect::<Vec<_>>());
        let positions = whole.or_else(|| {
            if line == Line::Detail {
                return None;
            }
            let mut found = Vec::new();
            let mut from = 0;
            for &ch in &needle {
                let index = (from..chars.len()).find(|&i| same(chars[i].1, ch))?;
                found.push(index);
                from = index + 1;
            }
            Some(found)
        });
        for index in positions.into_iter().flatten() {
            let start = chars[index].0;
            let end = chars.get(index + 1).map_or(text.len(), |next| next.0);
            ranges.push(start..end);
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

#[test]
fn highlighting_matches_the_original_for_unicode_and_partial_matches() {
    for text in [
        "",
        "Open File",
        "Design system",
        "Café résumé",
        "İstanbul Σίσυφος",
        "a🙂b 🙂c",
        "aaaa aaa",
        "Project board · 2 minutes ago",
        "Visited this morning",
    ] {
        for query in [
            "",
            " ",
            "\t\n",
            "opf",
            "opz",
            "sign des",
            "fé su",
            "İ",
            "Σ",
            "🙂b",
            "aaa aa",
            "boa",
            "in",
            "de",
            "board board",
            "🙂z",
        ] {
            for line in [Line::Label, Line::Detail] {
                assert_eq!(
                    matched_ranges(text, query, line),
                    baseline_matched_ranges(text, query, line),
                    "text={text:?}, query={query:?}"
                );
            }
        }
    }
}

#[test]
#[ignore = "timing benchmark; run explicitly without concurrent tests"]
fn cheatsheet_benchmark() {
    use crate::menu::ownership_bench::measure;
    use std::hint::black_box;
    for (name, text, query, line) in [
        ("highlight_empty", "Open project settings", "", Line::Label),
        (
            "highlight_whole",
            "Open project settings",
            "project",
            Line::Label,
        ),
        (
            "highlight_fuzzy",
            "Open project settings",
            "ops",
            Line::Label,
        ),
        (
            "highlight_failed",
            "Open project settings",
            "opz",
            Line::Label,
        ),
        (
            "highlight_overlap",
            "Design system",
            "sign des",
            Line::Label,
        ),
        ("highlight_unicode", "Café résumé", "fé su", Line::Label),
        (
            "highlight_detail",
            "Project board · 2 minutes ago",
            "boa",
            Line::Detail,
        ),
    ] {
        measure(&format!("{name}_before"), || {
            black_box(baseline_matched_ranges(
                black_box(text),
                black_box(query),
                line,
            ));
        });
        measure(&format!("{name}_after"), || {
            black_box(matched_ranges(black_box(text), black_box(query), line));
        });
    }
    let order: Vec<gpui::SharedString> = (0..1000)
        .map(|index| format!("visible-document-identifier-{index:04}").into())
        .collect();
    measure("visible_membership_before", || {
        let visible: std::collections::HashSet<_> = black_box(&order).iter().cloned().collect();
        for id in order.iter().step_by(10) {
            black_box(visible.contains(black_box(id)));
        }
        black_box(visible);
    });
    measure("visible_membership_after", || {
        let visible: std::collections::HashSet<_> = black_box(&order).iter().collect();
        for id in order.iter().step_by(10) {
            black_box(visible.contains(black_box(id)));
        }
        black_box(visible);
    });
}
