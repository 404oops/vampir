# Hosting the toolkit

This page covers the responsibilities of the view that renders Vampir controls. Most controls are simple functions, but the host must provide shared state, route drag events and request frames for animations.

## Set up a host

Four things, once:

```rust
use gpui::{Context, Point, Window, prelude::*};
use vampir::{ControlHost, ControlState, Palette};

struct Editor {
    controls: ControlState,
    palette: Palette,
    // ... the rest of the app
}

impl ControlHost for Editor {
    fn control_state(&self) -> &ControlState { &self.controls }
    fn control_state_mut(&mut self) -> &mut ControlState { &mut self.controls }

    // Only if you use sliders, splits or the colour pad.
    fn track_dragged(&mut self, id: vampir::ComboId, at: Point<f32>, cx: &mut Context<Self>) {
        match id {
            "zoom" => self.set_zoom(at.x, cx),
            "sidebar-split" => self.sidebar_fraction = at.x,
            _ => {}
        }
    }

    // Only if you use the tab bar.
    fn tabs_reordered(&mut self, _bar: vampir::ComboId, from: usize, to: usize, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        cx.notify();
    }

    // Only if you track drags of your own at the root. Scrollbar tracks
    // block the mouse, so they forward through these.
    fn forwarded_mouse_move(&mut self, event: &gpui::MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        self.root_mouse_move(event, cx);
    }
    fn forwarded_mouse_up(&mut self, _w: &mut Window, cx: &mut Context<Self>) {
        self.root_mouse_up(cx);
    }
}
```

Only the first two methods are required. The rest have empty defaults, and you add them when you add the widget that needs them.

## Forward drag events

The pointer can leave a small control as soon as a drag begins. The host therefore continues tracking the gesture from the root:

```rust
fn root_mouse_move(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
    // A release outside the window never arrives. A move with no button
    // held is that release, so end everything exactly as it would have.
    if !event.dragging() {
        if self.controls.dragging_anything() { self.root_mouse_up(cx); }
        return;
    }
    if vampir::continue_drags(self, event.position, cx) {
        cx.notify();
    }
}

fn root_mouse_up(&mut self, cx: &mut Context<Self>) {
    vampir::end_drags(self, cx);   // applies a finished tab reorder too
    cx.notify();
}
```

Attach that mouse-move handler to the root **and to every `.occlude()`d surface**. An occluding panel swallows the move stream a drag underneath it depends on.

## Request frames for animations

Ask `ControlState::animating()` **after** your page has been built, not before. A switch or a disclosure notices its own state change while it renders — that is what lets a change made by a menu item or a shortcut slide exactly as a click does — so a check made before the controls run cannot see the slide one of them is about to start, and the frame that starts it never asks for the frame that would continue it.

Fades and slides are `Instant`-driven, so they finish on their own and stop asking for frames. Fold `ControlState::animating()` into whatever decides:

```rust
if self.controls.animating() || self.my_own_transitions_running() {
    window.request_animation_frame();
}
```

`spinner` is the exception: it is indeterminate, so it never finishes and never stops needing frames. Ask for them only while one is actually on screen — `request_animation_frame` holds the display link open, and a spinner nobody can see should not be costing 60Hz. The gallery scopes it to the one page that has a spinner on it:

```rust
if self.controls.animating() || dialog_animating || page == Page::Controls {
    window.request_animation_frame();
}
```

Do not drive hover from an `Instant`. Scrolling a list re-fires hover on every row and will pin the display link at 60Hz. Use GPUI's `.hover()`.

`animating()` is also the end of a frame's bookkeeping: it counts the frame, and retires the records of anything that has not rendered for two of them. That is what lets a row that leaves a list and comes back count as new, and it is why it wants to be called exactly once per render, after everything else.

## Animating your own state

The same machinery the controls use is there for the host, and the gallery leans on it for the things only a host can know about:

- **A two-state look:** `self.controls.blend(&id, on, SWITCH_SLIDE)` is 0 to 1, eased, from wherever it was when `on` last changed. The footer's "last action" fades its new text in with it.
- **A value going somewhere:** `self.controls.tween("key", target, MOVE)` glides from its current value to the target and is at the target the first time it is asked. The gallery's table renders each row `.relative().top(px(tween(row) - place))`, so a re-sort slides rows rather than redealing them; `tween_angle` does the same for degrees, the short way round.
- **A scheme crossing over:** the gallery keeps `dark: bool` and `hue: f64` as the truth, tweens a darkness and a shown hue from them, and builds the palette from those — `Palette::mix(light, dark, darkness)` when the darkness is between 0 and 1. Every control is a function of the palette it is handed, so one cross-fade at the root is every colour on screen crossing over, and the text inputs are re-styled whenever the shown pair changes rather than when the truth does.
- **A page arriving:** `transition(&id, page as u64, MOVE)` gives progress since the page changed; the gallery wraps the body in `.opacity(t).relative().top(px(8.0 * (1.0 - t)))`.

Key everything by what it *is* rather than where it is — a row by its id, not its index — or a row shifting down one place inherits the animation of whatever was there before.

## Ids

`ComboId` is `&'static str`. It is both the element id and the widget's identity in `ControlState`, so **two widgets of the same kind in one view must not share one**. There is no enum to keep in step, which is the point.

The same string is what comes back to `track_dragged` and `tabs_reordered`, so the `match` in those methods is the whole dispatch table for your drags.

## Themes

A host with no theme of its own gets the whole palette from one number:

```rust
let palette = Palette::from_hue(268.0, /* dark */ true);
```

Start from the system rather than from a guess. The window knows whether the OS is in light or dark mode, and it says when that changes:

```rust
fn system_dark(window: &Window) -> bool {
    matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

// In the view's constructor. Keep the Subscription in a field, or it stops.
let dark = system_dark(window);
let appearance = window.observe_window_appearance({
    let this = cx.weak_entity();
    move |window, cx| {
        let dark = system_dark(window);
        this.update(cx, |this, cx| if this.follow_system { this.dark = dark; cx.notify(); }).ok();
    }
});
```

The gallery does exactly this: it opens in the scheme the desktop is using, follows the desktop until someone picks Light or Dark by hand, and offers "System" to hand control back. A window that opens dark on a light desktop looks broken before it looks like a choice.

`Palette` is a plain public struct of `Copy` colour roles, so a host with a richer theme writes the fields directly instead. Either way it is passed **by value** into every control; see [Design](design.md#colour) for what the roles mean and when adding one is justified.

Text inputs are the one thing that holds its own colours, because they are GPUI entities rather than functions. Re-style them when the theme changes rather than on every frame:

```rust
if self.styled_for != (self.hue.to_bits(), self.dark) {
    self.styled_for = (self.hue.to_bits(), self.dark);
    self.restyle_inputs(palette, cx);
}
```

## Key bindings and menus

The toolkit ships actions but binds nothing: which key opens your command palette is your application's business, not a widget's. `TextInput` exports its actions (`Backspace`, `SelectAll`, `Copy`, …) for you to bind in the `"TextInput"` context, and `shortcut_recorder` hands back a `Chord` with a `keystroke` ready for `KeyBinding`.

Write bindings with `secondary` for the platform's primary modifier — it is ⌘ on macOS and Ctrl everywhere else, so `"secondary-k"` is one binding that is right on all three. The text-editing keymap is the one place the platforms genuinely disagree (⌥ moves by word on a Mac, Ctrl does elsewhere), and `examples/gallery.rs` shows the two lists. Label shortcuts with `vampir::display("secondary-k")`, which renders `⌘K` or `Ctrl+K` as the platform writes it; a `⌘` shown on a Windows machine is a symbol with no key behind it.

On macOS the menu bar is not optional — see [Shipping a macOS app](macos-apps.md).
