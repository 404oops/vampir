//! Differential tests and opt-in benchmarks against the PR's original search paths.
use super::{
    Command, SearchResult, command_result, command_results, fuzzy_filter, fuzzy_score, rank_results,
};
use crate::menu::ownership_bench::{baseline_fuzzy_filter, measure};
use std::collections::HashMap;

fn baseline_rank_results(query: &str, results: &[SearchResult]) -> Vec<SearchResult> {
    // Scored on the label alone: it is what the person reads and
    // abbreviates, and the score's length penalty would otherwise let a
    // long detail line push a result down. A result that matches only
    // through its detail or section still qualifies, after every result
    // whose label matched, in the order it came.
    let rank = |result: &SearchResult| -> Option<Option<i32>> {
        if let Some(score) = fuzzy_score(query, &result.label) {
            return Some(Some(score));
        }
        let mut text = result.label.to_string();
        for part in [&result.detail, &result.section].into_iter().flatten() {
            text.push(' ');
            text.push_str(part);
        }
        fuzzy_score(query, &text).map(|_| None)
    };
    type Run<'a> = Vec<(Option<i32>, &'a SearchResult)>;
    let mut sections: Vec<(Option<&str>, Run)> = Vec::new();
    let mut positions: HashMap<Option<&str>, usize> = HashMap::new();
    for result in results {
        let Some(rank) = rank(result) else {
            continue;
        };
        let section = result.section.as_deref();
        let index = *positions.entry(section).or_insert_with(|| {
            let index = sections.len();
            sections.push((section, Vec::new()));
            index
        });
        sections[index].1.push((rank, result));
    }
    // Stable sorts, so ties keep the order the sources were assembled in.
    for (_, run) in &mut sections {
        run.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    }
    sections.sort_by_key(|(_, run)| std::cmp::Reverse(run[0].0));
    sections
        .into_iter()
        .flat_map(|(_, run)| run)
        .map(|(_, result)| result.clone())
        .collect()
}

fn commands(count: usize) -> Vec<Command> {
    (0..count)
        .map(|index| {
            let command = Command::new(
                format!("command-{index}"),
                format!("Open project settings {index}"),
            );
            if index % 2 == 0 {
                command.group("Workspace")
            } else {
                command
            }
        })
        .collect()
}

#[test]
fn direct_command_results_preserve_metadata_and_order() {
    let mut commands = commands(12);
    commands.push(
        Command::new("unicode", "İstanbul café Σίσυφος")
            .group("Editing")
            .shortcut("Ctrl+I"),
    );
    for query in [
        "",
        "  ",
        "ops",
        "zzzz",
        "workspace",
        "İ",
        "café",
        "Σ",
        "o p",
    ] {
        let expected: Vec<_> = baseline_fuzzy_filter(query, &commands)
            .iter()
            .map(command_result)
            .collect();
        let actual = command_results(query, &commands);
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(
                (
                    &actual.id,
                    &actual.label,
                    &actual.detail,
                    &actual.section,
                    &actual.shortcut,
                    &actual.group
                ),
                (
                    &expected.id,
                    &expected.label,
                    &expected.detail,
                    &expected.section,
                    &expected.shortcut,
                    &expected.group
                )
            );
        }
        assert_eq!(
            fuzzy_filter(query, &commands)
                .iter()
                .map(|c| &c.id)
                .collect::<Vec<_>>(),
            expected.iter().map(|r| &r.id).collect::<Vec<_>>()
        );
    }
}

#[test]
fn ranked_results_preserve_sections_ties_and_metadata_matches() {
    let results = [
        SearchResult::new("a", "Open settings").section("Workspace"),
        SearchResult::new("b", "Open settings").section("Other"),
        SearchResult::new("c", "Café résumé")
            .detail("İstanbul Σίσυφος")
            .section("Workspace"),
        SearchResult::new("d", "Empty"),
        SearchResult::new("e", "Archive").detail("Open settings"),
    ];
    for query in [
        "",
        " ",
        "ops",
        "zzzz",
        "workspace",
        "open",
        "İ",
        "Σ",
        "café",
    ] {
        let ids = |rows: Vec<SearchResult>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            ids(rank_results(query, &results)),
            ids(baseline_rank_results(query, &results))
        );
    }
}

#[test]
#[ignore = "timing benchmark; run explicitly without concurrent tests"]
fn pr_search_benchmark() {
    use std::hint::black_box;
    for count in [100, 1000] {
        let commands = commands(count);
        let results: Vec<_> = commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                SearchResult::new(command.id.clone(), command.label.clone())
                    .detail("Recently edited project")
                    .section(format!("Source {}", index % 4))
            })
            .collect();
        for query in ["", "ops", "zzzz"] {
            measure(&format!("command_results_{count}_{query:?}_before"), || {
                let results: Vec<_> = baseline_fuzzy_filter(black_box(query), black_box(&commands))
                    .iter()
                    .map(command_result)
                    .collect();
                black_box(results);
            });
            measure(&format!("command_results_{count}_{query:?}_after"), || {
                black_box(command_results(black_box(query), black_box(&commands)));
            });
            measure(&format!("ranked_results_{count}_{query:?}_before"), || {
                black_box(baseline_rank_results(black_box(query), black_box(&results)));
            });
            measure(&format!("ranked_results_{count}_{query:?}_after"), || {
                black_box(rank_results(black_box(query), black_box(&results)));
            });
        }
    }
}
