//! Run with `cargo test --lib ownership_benchmark --config 'profile.dev.package.vampir.opt-level=3' -- --ignored --nocapture`.
//! Dependency-free microbenchmarks; excludes GPUI layout, paint, and event dispatch.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::MenuItem;
use crate::overlay::{Command, fuzzy_filter};
use gpui::{SharedString, StyledText};

struct CountingAllocator;
thread_local! {
    static COUNT: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}
fn allocated(size: usize) {
    let _ = COUNT.try_with(|count| {
        if let Some((calls, bytes)) = count.get() {
            count.set(Some((calls + 1, bytes + size)));
        }
    });
}
// Delegates memory management unchanged; only records this thread's allocation requests.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

pub(crate) fn measure(name: &str, mut operation: impl FnMut()) {
    for _ in 0..100 {
        operation();
    }
    let mut samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        let mut iterations = 0;
        while start.elapsed() < Duration::from_millis(100) {
            for _ in 0..100 {
                operation();
            }
            iterations += 100;
        }
        samples.push(start.elapsed().as_nanos() as f64 / iterations as f64);
    }
    samples.sort_by(f64::total_cmp);
    COUNT.with(|count| count.set(Some((0, 0))));
    operation();
    let (allocs, bytes) = COUNT.with(|count| count.replace(None).unwrap());
    println!(
        "{name}: median_ns={:.1} min_ns={:.1} max_ns={:.1} allocs={allocs} bytes={bytes}",
        samples[4], samples[0], samples[8]
    );
}

fn menu(depth: usize, width: usize) -> Vec<MenuItem> {
    (0..width)
        .map(|index| {
            if depth > 0 && index % 4 == 0 {
                MenuItem::submenu(format!("Branch {depth} {index}"), menu(depth - 1, width))
            } else {
                MenuItem::action("benchmark-action", format!("Action {index}"))
            }
        })
        .collect()
}

#[test]
#[ignore = "timing benchmark; run explicitly without concurrent tests"]
fn ownership_benchmark() {
    println!(
        "debug_assertions={}, 9 samples >=100ms each; allocation count from separate single operation",
        cfg!(debug_assertions)
    );
    for (name, depth, width) in [("menu_flat_16", 0, 16), ("menu_nested_56", 2, 8)] {
        let items = menu(depth, width);
        measure(&format!("{name}_before"), || {
            let owned = black_box(&items).to_vec();
            let keys = owned.clone();
            let reachable = super::reachable_indices(&owned);
            let key_reachable = reachable.clone();
            black_box((owned, keys, reachable, key_reachable));
        });
        measure(&format!("{name}_after"), || {
            let items = black_box(&items);
            let reachable = super::reachable_indices(items);
            let commands = super::menu_commands(items, &reachable);
            let key_reachable = reachable.clone();
            black_box((items, commands, reachable, key_reachable));
        });
    }
    for size in [100, 1000] {
        let commands: Vec<_> = (0..size)
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
            .collect();
        for query in ["", "ops", "zzzz"] {
            measure(&format!("search_{size}_{query:?}_before"), || {
                black_box(baseline_fuzzy_filter(
                    black_box(query),
                    black_box(&commands),
                ));
            });
            measure(&format!("search_{size}_{query:?}_after"), || {
                black_box(fuzzy_filter(black_box(query), black_box(&commands)));
            });
        }
    }
    let label = SharedString::from("A longer result label that exceeds inline string storage");
    measure("styled_label_before", || {
        black_box(StyledText::new(black_box(&label).to_string()));
    });
    measure("styled_label_after", || {
        black_box(StyledText::new(black_box(&label).clone()));
    });
    measure("step_callback_before", || {
        let value = black_box(42usize);
        let handler: Box<dyn Fn() -> usize> = Box::new(move || value);
        let handler = Rc::new(handler);
        let key = handler.clone();
        black_box((&handler, &key));
        black_box(handler());
    });
    measure("step_callback_after", || {
        let value = black_box(42usize);
        let handler: Rc<dyn Fn() -> usize> = Rc::new(move || value);
        let key = handler.clone();
        black_box((&handler, &key));
        black_box(handler());
    });
}

// Frozen baseline from 8117a27 for repeatable comparisons.
fn baseline_fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.chars().all(char::is_whitespace) {
        return Some(0);
    }
    let candidate_lower = candidate.to_lowercase();
    let mut haystack = candidate_lower.chars().enumerate();
    let mut score = 0i32;
    let mut last_index: Option<usize> = None;
    let mut previous = None;

    for needle in query.to_lowercase().chars() {
        if needle.is_whitespace() {
            continue;
        }
        loop {
            let (index, ch) = haystack.next()?;
            let starts_word = previous.is_none_or(|previous: char| {
                previous.is_whitespace() || previous == '-' || previous == '_'
            });
            previous = Some(ch);
            if ch != needle {
                continue;
            }
            // Starting a word is the strongest signal that this is the
            // match the person meant.
            score += if starts_word { 12 } else { 2 };
            if last_index == Some(index.saturating_sub(1)) {
                score += 6;
            }
            last_index = Some(index);
            break;
        }
    }
    // Among equally good matches, the shortest candidate is the one that
    // most nearly *is* the query.
    Some(score - (candidate.chars().count() as i32 / 8))
}

/// Commands matching a query, best first. Ties keep their original order,
/// so a host's own ranking survives where the score cannot separate two.
pub(crate) fn baseline_fuzzy_filter(query: &str, commands: &[Command]) -> Vec<Command> {
    let mut scored: Vec<(i32, usize, Command)> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            let haystack = match &command.group {
                Some(group) => format!("{group} {}", command.label),
                None => command.label.to_string(),
            };
            baseline_fuzzy_score(query, &haystack).map(|score| (score, index, command.clone()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, command)| command).collect()
}

#[test]
fn optimized_search_preserves_scores_and_order() {
    let commands = [
        Command::new("open", "Open File"),
        Command::new("grouped", "Settings").group("Workspace"),
        Command::new("unicode", "İstanbul Σίσυφος café"),
        Command::new("tie", "Open File"),
    ];
    for query in ["", "  ", "opf", "ws", "zzzz", "İ", "Σ", "café", "o p"] {
        for command in &commands {
            assert_eq!(
                crate::fuzzy_score(query, &command.label),
                baseline_fuzzy_score(query, &command.label)
            );
        }
        let ids = |results: Vec<Command>| {
            results
                .into_iter()
                .map(|result| result.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(fuzzy_filter(query, &commands)),
            ids(baseline_fuzzy_filter(query, &commands))
        );
    }
}
