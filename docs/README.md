# Vampir documentation

Vampir is a widget toolkit for [GPUI](https://www.gpui.rs): buttons, fields, pop-ups, menus, tables and a full text input, over a small OKLCH palette. It is a plain library crate with no application inside it, and it depends on nothing but `gpui` and `unicode-segmentation`.

Use the guide that matches what you are doing:

| | |
|---|---|
| [**Hosting**](hosting.md) | Wiring the toolkit into a view: the `ControlHost` impl, pumping drags from the root, asking for frames, and how ids work. Read this first if you are using Vampir. |
| [**Widgets**](widgets.md) | What is in the crate and what each thing is for. |
| [**Keyboard and focus**](keyboard.md) | How every control is reached and operated from the keyboard, what the host has to bind, and where the focus ring goes. |
| [**Design**](design.md) | Why it looks the way it does: light from above, raised versus recessed, the metrics and colour rules, and the patterns that were tried and rejected. Read this before you change how anything looks. |
| [**Writing a control**](writing-a-control.md) | The shape every control has to keep, the borrow-checker traps particular to GPUI, and the checklist for adding one. |
| [**Shipping a macOS app**](macos-apps.md) | The menu bar, the shortcuts and the bundle — the parts macOS does not give you for free, and which the gallery demonstrates. |

[`AGENTS.md`](../AGENTS.md) contains a short summary for coding tools and links back to these pages for the full explanations.

To contribute, see [`CONTRIBUTING.md`](../CONTRIBUTING.md).

## The basic model

Each control is a function of the data it receives. It takes a value, a `Palette` and a `Context<V>`, and returns an element without owning app state.

State that must survive between frames lives in one `ControlState` on the host. This includes open pop-ups, animation timestamps and active drags.

This keeps controls generic over the host view instead of tying them to one application. The host owns the data and callbacks; the control renders and handles its interaction.

## The gallery

```bash
cargo run --example gallery
```

The gallery puts every widget in one window. It is both a practical example of `ControlHost` and a useful manual test for visual changes, including the macOS integration described in [Shipping a macOS app](macos-apps.md).
