# Contributing

Thanks for considering a contribution. Vampir is a small toolkit with a deliberate visual and technical style, and contributions are welcome when they fit that direction or clearly explain why it should change.

## Getting set up

Use the current stable Rust toolchain. The core's locked dependencies require at least Rust 1.89; the gallery's Linux backend requires at least 1.92. On macOS you need Xcode's command line tools; on Linux you need the Wayland/X11/Vulkan headers GPUI itself wants.

```bash
git clone https://github.com/404oops/vampir
cd vampir
cargo run --example gallery
```

The first build may take a while. GPUI — the [gpui-ce](https://github.com/gpui-ce/gpui-ce) release on crates.io, renamed to `gpui` in `Cargo.toml` — is a substantial dependency; later builds benefit from the cache and are much quicker.

If the gallery window opens, you have everything.

## What is where

```
src/
  lib.rs          the public surface — everything is re-exported here
  controls.rs     buttons, fields, sliders, the spinner, scrollbars
  containers.rs   tabs, splits, collapsibles, dialogs
  menu.rs         in-window context menus and menu buttons
  overlay.rs      tooltips, the command palette, fuzzy matching
  data.rs         tree rows and table headers
  text_input.rs   the full text input: selection, IME, undo, wrapping
  swatch.rs       the Oklch colour pickers
  palette.rs      the colour roles, derived from hue and saturation
  theme.rs        hue, saturation and a scheme that follows the desktop; the bound pickers
  lighting.rs     the shadow and gradient recipes everything draws with
  state.rs        ControlState and the ControlHost trait; the host's root handlers
  keyboard.rs     the standard bindings, focus movement, the ring, the Edit menu
  scroll.rs  easing.rs  color.rs  shortcut.rs  typography.rs

examples/gallery.rs   every widget in one window, and a complete host
packaging/macos/      Info.plist and the .app bundle script
packaging/linux/      desktop entry and installer
tools/                drives the gallery from a script; makes the screenshots
docs/                 the prose, split by subject
```

The two files worth reading before anything else are `state.rs`, which is the whole of what the toolkit remembers between frames, and `lighting.rs`, which is the whole of how it looks.

## Before making a change

Read [`docs/design.md`](docs/design.md) if your change is visible, and [`docs/writing-a-control.md`](docs/writing-a-control.md) if you are adding one. Between them they are the design the toolkit has to keep to stay coherent: what is raised and what is recessed, which patterns were tried and rejected, and what a control's shape is.

These documents describe the decisions that keep the toolkit coherent. If a change needs to depart from them, explain the reason and the trade-off in the pull request, especially when it relates to the rejected patterns in the design guide.

## Checks and expectations

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` all pass. CI runs exactly these, then builds the gallery.
- **Both colour schemes.** A light-only or dark-only path is a bug; the `is_dark` flag exists so one code path covers both. Check your change with the System | Light | Dark segment in the gallery's header — the gallery opens in the desktop's scheme, so click the other one — and drag the hue slider across while you are there.
- **Nothing changes in one frame.** A state that flips slides, a thing that moves slides, a colour crosses over — [Design › Motion](docs/design.md#motion) has the three durations and the rules. A slide is faster than a screenshot, so to look at one launch the gallery with `VAMPIR_SLOW_MOTION=8 tools/gallery.sh launch`: every animation runs eight times slower and the footer carries a badge saying so. Frames taken that way are for looking at, never for the docs.
- **Test the logic, not the pixels.** Fuzzy scoring, span sanitising, gamut clamping and geometry are all testable without a window. The drawing is not, so keep drawing thin and put the thinking where a test can reach it. If a bug was invisible to the tests and obvious on screen, that usually means a calculation is buried in a paint closure — lift it out and test it.
- **No new dependencies.** The core has two; the optional `app-icon` feature adds platform-specific icon support. A patch that adds another dependency needs to argue for it.
- A new control is exported from `lib.rs`, added to the table in [`docs/widgets.md`](docs/widgets.md) and the list in the README, and shown in `examples/gallery.rs` — see the next section, which is not optional.

## Every addition ships in the gallery

**Anything added to the toolkit must be added to `examples/gallery.rs` and shown working in a screenshot in the pull request. A PR that adds something the gallery does not show will be rejected.**

That is the whole rule, and it is strict on purpose. The gallery is the only place a reader can see every control beside every other one, so a control that is not in it is a control nobody can judge against the rest. It is also the only manual test the project has: if the gallery cannot exercise a feature, there is no evidence the feature works.

"Shown working" means the screenshot demonstrates the behaviour, not the presence of the widget — a dropdown open, a tree branch expanded, a focus ring on the thing that has focus, a dialog with the keyboard in it. A mock-up, a cropped detail with no context, or a widget sitting inert in a corner does not count. If the addition changes how something looks in both colour schemes, show both.

Take the screenshots with the tool, so they match the ones already in `docs/`:

```bash
tools/gallery.sh launch          # builds the .app and opens it
tools/gallery.sh key cmd-3       # drive it: key, type, click, drag, scroll
tools/gallery.sh shot after.png  # the whole window, at 1x, pointer parked
```

If your addition changes one of the pages the docs already show, regenerate those too with `tools/gallery.sh screenshots` and include them in the same PR.

## Manual testing

```bash
cargo run --example gallery
```

The gallery is the project's manual test suite. Check visible changes there in both colour schemes. If a change cannot be demonstrated in the gallery, that is a problem with the change, not with the gallery: see the section above.

To see it behave like a real application rather than a binary in `target/`:

```bash
packaging/macos/bundle.sh --open
```

See [`docs/macos-apps.md`](docs/macos-apps.md) for why that is a different thing.

## Pull requests

Small and focused beats large and comprehensive. A PR that fixes one thing and explains it is easier to take than one that fixes five.

In the description, say what the change does and why the alternative was worse. Commit messages in the imperative mood, present tense, and a body if the subject line cannot carry the reason on its own.

Bug reports are contributions. So is telling us that a doc page is wrong, or that something was harder to work out than it should have been — the second is the most useful and the least reported.

## Releasing

Set the version in `Cargo.toml`, commit, and push a matching tag: `git tag v0.2.0 && git push --tags`. The release workflow checks that the tag and the version agree, runs the full check suite, and publishes a GitHub release with generated notes.

It then publishes to crates.io. That works because every dependency comes from the registry: `gpui` is [gpui-ce](https://github.com/gpui-ce/gpui-ce), the community edition of GPUI that is synced from Zed and released to crates.io, and the workflow refuses to publish if a `git =` dependency ever comes back. Authentication is crates.io Trusted Publishing — the repository and workflow are registered once on crates.io as a trusted publisher and no token lives in the repository.

When gpui-ce publishes a new release, bump the version in `Cargo.toml`, run the checks and regenerate the screenshots; colours and layout come from GPUI, so a GPUI update is a visual change and gets reviewed like one. The APIs the toolkit needs are all in the release it pins — some of gpui-ce's newer work (a native focus-ring API, Wayland window icons) is on its main branch only, and the toolkit does not depend on it.

## Using code-generation tools

Code-generation tools are fine to use. The contributor remains responsible for running the checks, looking at visual changes in both colour schemes and understanding the result. Please review generated code carefully, especially around layout, state and input handling.

## Licence

By contributing you agree that your work is licensed under the MIT licence, the same as the rest of the project. GPUI is Apache-2.0 and its notice lives in [THIRD_PARTY.md](THIRD_PARTY.md); if you add a dependency under a licence that asks for attribution, add it there too, and the packaging scripts will ship it.
