# Writing a control

Most controls follow the same small pattern. This page explains that pattern and the common implementation details. For visual decisions, see [Design](design.md).

## Control structure

- **Every control is a free function generic over `V: ControlHost`.** No structs with builders, no trait objects, no `RootView` anywhere.
- **Take `Palette` by value.** It is `Copy`, and a returned `impl IntoElement` cannot borrow a temporary.
- **State goes to the host, not into the control.** `collapsible` takes `expanded` as a parameter for exactly this reason: whether a section is open usually outlives the view that draws it.
- **Animation is noticed, not announced.** A control asks `ControlState` how far its value is from what it showed last frame — `blend`, `tween` — rather than starting a timer in its click handler. That is what makes a change from a shortcut or a menu move exactly as a click does, and it is why a control that animates takes `view: &V` alongside `cx`.

`ControlState` is the exception, and it is a short list on purpose: which pop-up is open and how far into its fade, what each animated control showed last frame, where things painted, and what is being dragged. Everything a control remembers about itself goes in one table, under a `Tag` built from a tuple — `(id, "pill-x")`, `(id, "shown", &row.id)` — never a `format!` string, and is retired by `animating()` a frame or two after the control stops rendering. If you want to add a field to it, the question to answer first is why the host cannot own that instead, and the second is why it cannot be a record like the rest.

## Common implementation issues

**Building children in a loop.** Build children in a `for` loop, not in `.children(map(..))`, whenever each child needs `cx` for a listener. A closure passed to `map` is `FnMut` and cannot hand `&mut Context` out twice. Reborrow per iteration:

```rust
for item in items {
    let cx: &mut Context<V> = &mut *cx;
    row = row.child(button(item.id, item.label, /* .. */, cx, /* .. */));
}
```

**Floats compared across frames.** A phase or a progress value derived from wall-clock time has to keep its precision. `Duration::as_secs_f32()` on a Unix timestamp lands on a float whose neighbours are 128 seconds apart, so its fractional part is always exactly zero — a spinner built on it sits frozen no matter how many frames it is given. Take the fraction out of the integer milliseconds instead. There is a test for this in `controls.rs`.

**Drags outlive their control.** Use the existing `TrackDrag` mechanism for a draggable control rather than adding a new field. Sliders, splits and the colour pad use the same gesture model over different geometry.

## Comments and tests

- **Comments say why, not what.** `.h(px(26.0))` needs no comment. The reason a marker expires on its own does.
- **Test the logic, not the pixels.** Fuzzy scoring, span sanitising, gamut clamping and geometry are all testable without a window; the drawing is not, so keep drawing thin and put the thinking somewhere a test can reach.

If a bug was invisible in a unit test and obvious on screen, that usually means a calculation was buried inside a paint closure. Lift it out into a named function and test that.

## Checklist

1. Decide it is raised or recessed, and give it `CONTROL_HEIGHT` if it sits in a row with others.
2. Free function, generic over `V: ControlHost`, taking `Palette` by value.
3. If it needs state between frames, ask hard whether the host should own it instead.
4. If it drags, use the existing `TrackDrag` mechanism rather than a new field.
5. Every visible change of state animates, with one of the three durations in [Design › Motion](design.md#motion): `blend` for a two-state look, `tween` for a position or a size, `present` before fading new rows into a list, `snap` for a value the pointer is holding. Nothing animates in on its first frame. Key each part with a tuple, `(id, "part")` or `(id, "part", &row.id)`, not a formatted string.
6. Export it from `lib.rs`.
7. Add a row to the table in [Widgets](widgets.md) and to the list in the [README](../README.md).
8. Show it in `examples/gallery.rs`, in both schemes, and watch its motion once in slow motion (`VAMPIR_SLOW_MOTION=8 tools/gallery.sh launch`).
9. Add a test for whatever part of it is not drawing.
