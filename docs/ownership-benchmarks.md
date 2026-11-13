# Ownership benchmarks

The ownership cleanup removes recursive menu copies, one layer of spinbox callback allocation, text copies during search rendering, and repeated fuzzy-query normalization. `TextInput::text()` keeps its existing signature; toolkit rendering uses the new `shared_text()` accessor instead.

## Reproduce

```bash
cargo test --lib ownership_benchmark --config 'profile.dev.package.vampir.opt-level=3' -- --ignored --nocapture
```

The dependency-free harness is in `src/ownership_bench.rs`, loaded only into library tests. It retains the original fuzzy-search implementation from commit `8117a27` and measures both versions in the same process. Menu measurements compare the original two tree copies plus reachable-index preparation against borrowed rows plus the production `menu_commands` helper. Callback measurements isolate storage, sharing, invocation, and destruction of a captured callback; they exclude GPUI event dispatch. Styled-label measurements construct and drop `StyledText`, excluding layout and painting.

The command optimizes Vampir at level 3 while keeping the existing development-profile dependencies and debug assertions. These are not full release-build or end-to-end GUI measurements. Each operation is warmed up 100 times, then timed in nine samples of at least 100 milliseconds each, in batches of 100 operations. Inputs and outputs pass through `std::hint::black_box`. Allocation requests and requested bytes are counted in a separate single operation on the benchmark thread; reallocation requests count as allocations, and bytes are cumulative requested bytes rather than peak live memory. Timing includes the disabled counter's small allocator-hook overhead in both versions.

Fixtures are synthetic: a flat menu has 16 actions; a nested menu has eight root rows and 56 total rows across three levels. Search has 100 or 1,000 commands, alternating grouped and ungrouped labels, with an empty query, a matching abbreviation (`ops`), and a nonmatching query (`zzzz`). Inputs are created outside the measured operation. The label fixture is longer than inline string storage. These isolate the reviewed costs rather than representing every application's menu or search data.

## Results

Measured on 2026-11-09, Apple M1 Pro, macOS 27.0.1, rustc 1.97.1 (aarch64-apple-darwin), with the gallery closed and no simultaneous build. Medians are per operation. The original code was also measured before editing; the paired results below come from the final harness with its frozen baseline.

| Operation | Before (µs) | After (µs) | Speedup | Allocation requests, before → after | Requested bytes, before → after |
|---|---:|---:|---:|---:|---:|
| Flat menu preparation (16 rows) | 0.492 | 0.218 | 2.26× | 6 → 5 | 2,656 → 864 |
| Nested menu preparation (56 total rows) | 1.354 | 0.350 | 3.87× | 17 → 4 | 8,224 → 416 |
| 100 commands, empty query | 16.152 | 1.430 | 11.29× | 208 → 1 | 58,073 → 9,600 |
| 100 commands, matching query | 28.658 | 22.405 | 1.28× | 408 → 259 | 61,263 → 59,771 |
| 100 commands, unmatched query | 23.979 | 19.427 | 1.23× | 400 → 251 | 7,635 → 6,044 |
| 1,000 commands, empty query | 157.870 | 16.198 | 9.75× | 2,011 → 1 | 499,497 → 96,000 |
| 1,000 commands, matching query | 289.297 | 229.169 | 1.26× | 4,011 → 2,512 | 532,387 → 516,945 |
| 1,000 commands, unmatched query | 235.024 | 196.371 | 1.20× | 4,000 → 2,501 | 77,835 → 61,394 |
| Styled label construction | 0.273 | 0.046 | 5.98× | 3 → 1 | 280 → 152 |
| Callback storage and invocation | 0.051 | 0.032 | 1.60× | 2 → 1 | 40 → 24 |

Nine-sample timing ranges (minimum–maximum, µs per operation):

| Operation | Before range | After range |
|---|---:|---:|
| Flat menu preparation (16 rows) | 0.487–0.508 | 0.216–0.222 |
| Nested menu preparation (56 total rows) | 1.342–1.377 | 0.348–0.358 |
| 100 commands, empty query | 16.108–16.232 | 1.423–1.451 |
| 100 commands, matching query | 28.541–28.801 | 22.190–24.167 |
| 100 commands, unmatched query | 23.255–24.546 | 19.320–20.265 |
| 1,000 commands, empty query | 157.042–162.040 | 16.037–16.433 |
| 1,000 commands, matching query | 287.836–295.110 | 227.549–240.384 |
| 1,000 commands, unmatched query | 233.593–237.184 | 194.359–197.712 |
| Styled label construction | 0.272–0.275 | 0.045–0.047 |
| Callback storage and invocation | 0.047–0.052 | 0.026–0.032 |

Wall-clock microbenchmarks on a desktop have scheduler and thermal noise. Treat the medians as approximate, and use the allocation counts as evidence of the ownership changes. The improvements apply to these preparation operations, not to whole-frame time or FPS. No separate timing claim is made for `shared_text()`.

## Correctness and on-screen checks

The ordinary tests compare the new fuzzy scorer and filter against the frozen implementation, including Unicode, whitespace, grouped matches, unmatched queries, and equal-score ordering. Existing menu tests exercise the same compact command snapshot used by the event handlers. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `cargo test --no-default-features --lib` pass. The gallery builds and was exercised for search filtering and command activation, nested menu keyboard navigation and activation, and spinbox changes in light and dark schemes. Verification captures are in `target/ownership-review/`.

## Practices behind the changes

Measure representative workloads before optimizing, compare several cases, and distinguish a microbenchmark from application latency; see the [Rust Performance Book's benchmarking guidance](https://nnethercote.github.io/perf-book/benchmarking.html). Here that means measuring both matching and nonmatching searches and reporting allocation requests separately from elapsed time.

Distinguish copying data from sharing ownership. The [standard library's `Rc` documentation](https://doc.rust-lang.org/std/rc/index.html) explains that cloning an `Rc` shares an existing allocation; it does not copy the inner value. Keep shared ownership when independent event handlers need the same callback, but allocate the callback directly in one `Rc` rather than boxing it first. Similarly, GPUI's `StyledText::new` accepts `Into<SharedString>` in the pinned dependency, so it can retain an inexpensive shared-string clone.

Borrow when possible and own only when needed. [`Cow`](https://doc.rust-lang.org/std/borrow/enum.Cow.html) expresses exactly that distinction for the fuzzy-search haystack: an ungrouped command borrows its label; a grouped command owns the combined text. Event handlers need owned data that survives render, so menu handlers capture action IDs and submenu identities instead of borrowing transient data or copying the whole tree.

Look for repeated allocation in frequently called paths before replacing useful abstractions. The [Rust Performance Book's allocation guidance](https://nnethercote.github.io/perf-book/heap-allocations.html) discusses allocation rate and reference-counted sharing. The empty-query path can return commands in their existing order without building haystacks or sorting; nonempty filtering normalizes the query once. The benchmark uses [`black_box`](https://doc.rust-lang.org/std/hint/fn.black_box.html) to discourage compiler elimination of measured work, with the standard library's best-effort limitation.

## Cheat sheet follow-up

Three additional changes follow the checklist: `matched_ranges` writes whole-match byte ranges directly, appends fuzzy matches with rollback on failure, and merges ranges in place; `tree_row` constructs navigation targets in one iterator pipeline; `ListSelection::retain_visible` borrows IDs in its temporary membership set instead of cloning them. Public signatures and dependencies stay the same. The optional pedantic/nursery lint audit was used for review without adopting blanket lint rules or making broad style changes.

Rerun the new measurements with:

```bash
cargo test --lib cheatsheet_benchmark --config 'profile.dev.package.vampir.opt-level=3' -- --ignored --nocapture
```

The same M1 Pro, compiler settings, allocator counter, warmup, and nine-sample protocol described above were used with the gallery closed. The baseline highlighter is frozen in `src/emphasis_bench.rs`; a differential test checks 288 combinations of text, query, and label/detail behavior. The membership benchmark isolates construction of the temporary set and 100 lookups among 1,000 long IDs, rather than timing the complete selection update. Tree navigation is covered by existing logic tests and a gallery keyboard check; no separate timing claim is made for that change.

| Operation | Before median (µs) | After median (µs) | Allocation requests | Requested bytes |
|---|---:|---:|---:|---:|
| Highlight: empty query | 0.1660 | 0.0036 | 3 → 0 | 672 → 0 |
| Highlight: whole word | 0.5413 | 0.3835 | 9 → 6 | 1,032 → 784 |
| Highlight: fuzzy abbreviation | 0.5766 | 0.4964 | 7 → 5 | 848 → 752 |
| Highlight: failed abbreviation | 0.5372 | 0.5198 | 5 → 5 | 720 → 752 |
| Highlight: overlapping words | 0.5259 | 0.3178 | 10 → 6 | 792 → 544 |
| Highlight: Unicode words | 0.5533 | 0.3956 | 9 → 6 | 752 → 656 |
| Highlight: detail line | 0.3876 | 0.3016 | 7 → 5 | 1,176 → 1,088 |
| Membership set: 1,000 IDs | 43.2145 | 23.5970 | 1 → 1 | 51,208 → 18,440 |

A failed abbreviation is a tradeoff: the temporary vector count is lower, but its speculative byte ranges are larger than the old character indices, so this fixture requests 32 extra bytes with the same allocation count. Its small timing difference should not be treated as a reliable speedup. Successful matches and membership construction show clearer improvements. These remain microbenchmarks, not application frame timings.

Follow-up verification: 167 tests pass, strict Clippy and formatting pass, and the gallery builds. Search highlighting was checked in both schemes, tree keyboard navigation was exercised, and captures were inspected in `target/cheatsheet-review/`. Selection retention is covered by its existing filtering/re-homing test. The raw benchmark output is `target/cheatsheet-review/benchmarks.txt`.

## Rebased PR and GPUI-CE 0.3.0

The cleanup was rebased onto PR #7 at `c6b466bf8a99f6bbd052ada777c691393152089e`. Measurements below supersede the earlier GPUI-CE 0.2.2 timings for this PR. All pairs use the same GPUI-CE 0.3.0 dependencies and the same optimized-Vampir/development-dependency protocol; no builds or gallery were running during timing. Allocation counts include reallocations and bytes are cumulative requested bytes, not retained or peak memory. The full measurements are also in `docs/ownership-benchmarks-gpui-0.3.json`.

The existing menu/filter/highlighting baselines retain the old algorithms, which were also present at the PR head; the new ranked-result baseline is copied from that PR head. Command-result cases include converting matches to the final `SearchResult` representation. Ranked-result fixtures alternate four sections and include detail text, testing both label matches and metadata fallback. Each unchanged-query cache case compares rebuilding the final command results with looking up an already-warm cache of 1,000 commands using the same query; it isolates skipped matching work and is not a whole-frame speedup. Cache invalidation after a changed query or replacement command list is tested.

Reproduce all four opt-in benchmark suites:

```bash
cargo test --lib --features command-bar-window benchmark --config 'profile.dev.package.vampir.opt-level=3' -- --ignored --nocapture --test-threads=1
```

The ranked-result suite was repeated after eliminating the empty-query sorting pass:

```bash
cargo test --lib --features command-bar-window pr_search_benchmark --config 'profile.dev.package.vampir.opt-level=3' -- --ignored --nocapture
```

| Case | Before median (µs) | After median (µs) | Allocations | Requested bytes |
|---|---:|---:|---:|---:|
| `unchanged_query_""` | 12.2164 | 0.0105 | 1 → 0 | 160,000 → 0 |
| `unchanged_query_"ops"` | 76.1341 | 0.0106 | 1,011 → 0 | 225,949 → 0 |
| `menu_flat_16` | 0.8280 | 0.2058 | 6 → 5 | 3,168 → 864 |
| `menu_nested_56` | 1.5653 | 0.3445 | 17 → 4 | 10,016 → 416 |
| `search_100_""` | 14.1775 | 1.4927 | 208 → 1 | 58,073 → 9,600 |
| `search_100_"ops"` | 25.7718 | 18.5374 | 408 → 258 | 61,263 → 21,691 |
| `search_100_"zzzz"` | 21.9223 | 18.4246 | 400 → 251 | 7,635 → 6,044 |
| `search_1000_""` | 137.6068 | 16.3415 | 2,011 → 1 | 499,497 → 96,000 |
| `search_1000_"ops"` | 284.4147 | 203.9311 | 4,011 → 2,511 | 532,387 → 206,449 |
| `search_1000_"zzzz"` | 235.5818 | 199.7413 | 4,000 → 2,501 | 77,835 → 61,394 |
| `styled_label` | 0.2784 | 0.0446 | 3 → 1 | 288 → 160 |
| `step_callback` | 0.0501 | 0.0283 | 2 → 1 | 40 → 24 |
| `highlight_empty` | 0.1651 | 0.0036 | 3 → 0 | 672 → 0 |
| `highlight_whole` | 0.5260 | 0.3775 | 9 → 6 | 1,032 → 784 |
| `highlight_fuzzy` | 0.5777 | 0.4915 | 7 → 5 | 848 → 752 |
| `highlight_failed` | 0.5369 | 0.5363 | 5 → 5 | 720 → 752 |
| `highlight_overlap` | 0.5180 | 0.3157 | 10 → 6 | 792 → 544 |
| `highlight_unicode` | 0.5271 | 0.3867 | 9 → 6 | 752 → 656 |
| `highlight_detail` | 0.3868 | 0.2939 | 7 → 5 | 1,176 → 1,088 |
| `visible_membership` | 42.6961 | 23.3185 | 1 → 1 | 51,208 → 18,440 |
| `command_results_100_""` | 17.6083 | 1.6392 | 209 → 1 | 74,073 → 16,000 |
| `ranked_results_100_""` | 8.2913 | 7.9073 | 22 → 22 | 32,316 → 32,316 |
| `command_results_100_"ops"` | 29.0813 | 19.1242 | 409 → 258 | 77,263 → 28,091 |
| `ranked_results_100_"ops"` | 18.1171 | 14.8748 | 222 → 123 | 35,006 → 34,709 |
| `command_results_100_"zzzz"` | 22.5430 | 18.3696 | 400 → 251 | 7,635 → 6,044 |
| `ranked_results_100_"zzzz"` | 47.4239 | 32.1367 | 700 → 203 | 25,610 → 8,252 |
| `command_results_1000_""` | 163.7004 | 18.5427 | 2,012 → 1 | 659,497 → 160,000 |
| `ranked_results_1000_""` | 74.1197 | 70.5881 | 34 → 34 | 312,988 → 312,988 |
| `command_results_1000_"ops"` | 297.0309 | 193.3979 | 4,012 → 2,511 | 692,387 → 270,449 |
| `ranked_results_1000_"ops"` | 165.5926 | 134.6746 | 2,034 → 1,035 | 340,878 → 337,881 |
| `command_results_1000_"zzzz"` | 225.6826 | 190.3403 | 4,000 → 2,501 | 77,835 → 61,394 |
| `ranked_results_1000_"zzzz"` | 477.3808 | 321.3489 | 7,000 → 2,003 | 265,010 → 82,952 |

The flat-menu baseline had substantial scheduler noise (minimum 0.5277 µs, maximum 13.7824 µs); do not treat its median as a precise speedup. The JSON includes every sample range. Failed highlight matches still carry the previously documented 32-byte temporary-buffer tradeoff. The native-view placement cache and platform cleanup are not timed by these microbenchmarks.

Additional best-practice changes in this update: borrowed matching until final result materialization; one normalized query and a reusable metadata buffer in ranked search; stable tie behavior with an allocation-free sort of uniquely indexed command scores; bounded native-command-bar caching with explicit invalidation; fewer native positioning/show calls when bounds/DPI/viewport are unchanged; native-view ownership and cleanup on all three backends; an interruptible X11 worker whose lifetime ends with its owner; fallible thread startup; narrower example/helper visibility; and a stored reveal duration instead of constructing `ControlState` every scrim frame. The command bar now inherits the host's slow-motion scale. Public toolkit signatures are preserved, apart from the additive `TextInput::shared_text()` accessor. No dependencies or vendored backend changes were added.
