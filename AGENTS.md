# Vampir repository guide

Vampir is a widget toolkit for [GPUI](https://www.gpui.rs). It is a plain library crate with no application inside it: `gpui` and `unicode-segmentation` are the only dependencies, and it builds anywhere GPUI does. `gpui` is the crates.io [gpui-ce](https://github.com/gpui-ce/gpui-ce) release (the community edition of GPUI, Apache-2.0, notice in `THIRD_PARTY.md`) renamed in `Cargo.toml`; do not change it back to a git dependency, that is what makes the crate publishable.

This is a short working guide for contributors and coding tools. The detailed documentation is in [`docs/`](docs/); use the relevant page when you need more context.

| | |
|---|---|
| [`docs/hosting.md`](docs/hosting.md) | Wiring the toolkit into a view: the `ControlHost` impl, pumping drags, asking for frames, ids, themes. |
| [`docs/widgets.md`](docs/widgets.md) | Every widget in the crate and what each is for. |
| [`docs/keyboard.md`](docs/keyboard.md) | Keyboard and focus: what the controls handle themselves, what the host binds, where the ring goes. |
| [`docs/design.md`](docs/design.md) | Why it looks the way it does, and the rules that keep it coherent. |
| [`docs/writing-a-control.md`](docs/writing-a-control.md) | The shape a control has to keep, the traps, the checklist. |
| [`docs/macos-apps.md`](docs/macos-apps.md) | The menu bar, the shortcuts and the bundle — what macOS does not give you for free. |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | How a change gets in, and the bar it has to clear. |

---

## The basic model

**A control is a function of the data it is handed.** It takes a value, a `Palette`, and a `Context<V>`, and returns an element. It owns nothing.

What cannot be a pure function is small, always the same shape, and lives in one place: `ControlState`. That is which pop-up is open and how far into its fade, when each animated control last changed, and whatever is being dragged. Nothing else.

This keeps controls generic over the host view rather than tying them to one application. The host owns application data and callbacks.

**Nothing changes in a single frame.** Every visible change of state animates, with one of the three durations in [design.md › Motion](docs/design.md#motion), and it is noticed from the value during render (`ControlState::blend`, `tween`) rather than started by the click, so a shortcut or a menu item moves a control exactly as the pointer does. A control that animates takes `view: &V`.

---

## Core conventions

Keep these conventions in mind when changing the library. Each is explained in more detail in the linked documentation.

**Shape** — every control is a free function generic over `V: ControlHost`, taking `Palette` by value. No structs with builders, no trait objects, no `RootView` anywhere. State that has to survive a frame belongs to the host, not to the control. → [writing-a-control.md](docs/writing-a-control.md)

**Light comes from above.** Raised means you press it; recessed means you put something in it. Never hard-code a shadow or a gradient — add a recipe to `lighting` instead. → [design.md](docs/design.md#light-comes-from-above)

**Both colour schemes, always.** A light-only or dark-only path is a bug; the `is_dark` flag exists so one code path covers both. Only the roles in `Palette`; adding a role means adding it to `from_hue` for every hue and both schemes. → [design.md](docs/design.md#colour)

**Shared metrics.** `CONTROL_HEIGHT` (30) and `CONTROL_RADIUS` (6), so a row of mixed controls lines up without tuning. `CHIP_HEIGHT` (26) is the one deliberate exception. → [design.md](docs/design.md#metrics)

**Select on release, not on press**, because pressing is also how a drag starts. Handle the release that never comes. Anything that blocks the mouse must forward moves and releases. Disable rather than remove. → [design.md](docs/design.md#interaction)

**Ring the active element, and if it holds options, the current option inside it.** A group is one tab stop; the ring goes on the chosen dot, the active tab, the slider's thumb — never around the container. Clicking an option puts the keyboard on it, so Tab carries on from there. Tab, Shift-Tab, Escape and text editing are the toolkit's to bind (`bind_keys`, `root`); a host binds only its own shortcuts. Anything that takes the keyboard hands it back when it closes, or to the root when there is nowhere to hand it back to. → [keyboard.md](docs/keyboard.md)

**The gallery shows widgets; the crate does the work.** Anything the gallery would have to implement to demonstrate a widget — focus return, key routing, a fade, a theme, a layout helper — belongs in the crate, so the next host gets it too. A host-side pattern in the gallery that runs to more than a few lines is a missing API, not an example. → [hosting.md](docs/hosting.md)

**Ids are `&'static str`,** and are both the element id and the widget's identity in `ControlState`. Two widgets of the same kind in one view must not share one. → [hosting.md](docs/hosting.md#ids)

**Comments say why, not what. Test the logic, not the pixels.** Keep drawing thin and put the thinking somewhere a test can reach. → [writing-a-control.md](docs/writing-a-control.md#comments-and-tests)

**Visual patterns.** A coloured bar down the side of a selected row, underlined text tabs, uppercase micro-captions, monospace labels, status dots, nested rounded cards, a hairline between every row, flat surfaces. See the design guide for the reasoning behind these choices. → [design.md](docs/design.md#patterns-to-avoid)

---

## Working in the repository

```bash
cargo run --example gallery      # every widget, one window
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs exactly those three checks and then builds the gallery. The first build is slow, because GPUI is large.

`examples/gallery.rs` is the closest thing to a manual test suite. A change that is visible should be visible there, in both schemes. It is also the smallest complete host, so it is where host-side patterns get demonstrated rather than only described.

Avoid adding dependencies. The crate currently has two, which keeps it easy to integrate with projects that already use GPUI.

When you add a control, the checklist at the end of [writing-a-control.md](docs/writing-a-control.md#checklist) lists everywhere it has to be registered — `lib.rs`, the docs table, the README list, the gallery and a test.

**Anything added to the toolkit must be added to the gallery and shown working in a screenshot, or the pull request is rejected.** Not a mock-up and not a widget sitting inert: the behaviour, on screen, from the real binary. The next section is how to produce that without a hand on the mouse.

Prose in the docs is one paragraph per line. Do not hard-wrap it at a column; editors soft-wrap, and hard breaks only make diffs noisy.

## Verifying on screen

`tools/gallery.sh` drives the gallery from a script. It is macOS-only — AppleScript for the keyboard, CoreGraphics for the pointer, `screencapture` for the pixels — and it is how every screenshot in `docs/` was made.

```bash
tools/gallery.sh launch                 # builds dist/Vampir gallery.app and opens it
tools/gallery.sh key cmd-3              # keystrokes in GPUI's own syntax: cmd-shift-s, escape, tab, enter
tools/gallery.sh type "sync"            # text
tools/gallery.sh click 120 300          # window-relative points; also right, double
tools/gallery.sh drag 100 300 400 300
tools/gallery.sh scroll 500 400 0 -40   # dx dy, a trackpad-style scroll at a point
tools/gallery.sh shot out.png           # the window at 1x; or a region: shot out.png X Y W H
tools/gallery.sh ink out.png Y0 Y1 X0 X1   # first and last inked column, for measuring alignment
tools/gallery.sh screenshots            # regenerate docs/*.png from a fresh launch
tools/gallery.sh quit
```

A verification pass is: `launch`, drive to the state, `shot`, then *look at the image* — read it back and check the thing you meant to check is there. Compare a before and an after when the change is subtle; `md5 -q` on two shots of the same region tells you whether anything moved at all, and `ink` tells you where.

Things that will otherwise cost you an hour:

- **Keystrokes go to the frontmost window.** Every `key`, `type` and pointer command brings the gallery to the front first and fails loudly if it cannot. Do not send keystrokes any other way.
- **A keystroke that matches a binding never reaches a key listener.** GPUI dispatches it as the action and stops. If you are adding key handling to a control, listen for the action, or make sure nothing binds the key first.
- **Focus on an element that has gone dispatches nothing** — not even the window's own shortcuts. If ⌘K "stopped working", something removed the focused element and did not re-home the keyboard.
- **A double-click is `clickState`, not timing.** `tools/gallery.sh double` sets it; two `click`s in a row are two single clicks.
- **The ring only shows for keyboard input.** After a click, press Tab once to reveal focus before judging where it is.
- **The spinner animates**, so two shots of the Controls page are never byte-identical. Crop it out when comparing.
- **Coordinates are window-relative.** `bounds` prints where the window is; the docs screenshots were taken at the default 980×752.
- **Animations are faster than the shutter.** A `shot` takes over a second from the command to the capture, so it never catches a 140ms slide. To see one half way, `VAMPIR_SLOW_MOTION=8 tools/gallery.sh launch`, then drive the pointer with `target/tools/input` and capture with `screencapture -x -o -R` against `bounds` directly, about a third of a second after the click. The footer shows a "slow motion" badge while it is on; never take a docs screenshot that way.

Check both schemes for anything visual. The gallery opens in whatever scheme the desktop is in, so set the scheme rather than assume it: the System | Light | Dark segment sits at the right of the header, and for a window `W` wide the three are `click $((W - 280)) 67`, `click $((W - 208)) 67` and `click $((W - 136)) 67`.

## Scope

The library is independent of any particular application. App-specific features such as a menu bar, key bindings and windows belong in the host, which is why [macos-apps.md](docs/macos-apps.md) is written as guidance for hosts and demonstrated in the gallery rather than built into the crate. The Edit menu is the one exception (`edit_menu`), because its items are the toolkit's own text actions.
