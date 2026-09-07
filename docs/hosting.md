# Hosting the toolkit

This page covers the responsibilities of the view that renders Vampir controls. Most controls are simple functions. The host provides one `ControlState`, puts the toolkit's handlers on its root, and asks for frames while something animates.

## Set up a host

Three things, once:

```rust
use gpui::{Context, Point, Window, div, prelude::*};
use vampir::{ControlHost, ControlState};

struct Editor {
    controls: ControlState,
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
    fn tabs_reordered(&mut self, bar: vampir::ComboId, from: usize, to: usize, cx: &mut Context<Self>) {
        match bar {
            "documents" => vampir::reorder(&mut self.documents, &mut self.current, from, to),
            _ => return,
        }
        cx.notify();
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.controls.palette();
        let body = self.body(palette, window, cx);
        if self.controls.animating() {
            window.request_animation_frame();
        }
        vampir::root(div().id("root"), self, cx)
            .size_full()
            .font_family(vampir::ui_font())
            .bg(vampir::ground(palette.backdrop))
            .child(body)
    }
}
```

Only the first two methods of `ControlHost` are required. `track_dragged` and `tabs_reordered` have empty defaults, and you add them when you add the widget that needs them. `vampir::reorder` moves the item and keeps the selection on the item it was on.

`vampir::root` is `handle_keys` and `handle_mouse` together. It puts the toolkit's key handlers on the root (see [Keyboard and focus](keyboard.md#what-the-host-has-to-do)), the mouse handlers its drags need, and the root focus handle the keyboard falls back to when whatever had it has gone. Put `vampir::handle_mouse` on every `.occlude()`d surface of your own as well: an occluding panel swallows the move stream a drag underneath it depends on.

## Drags

A slider handle, a split divider, a tab or a scrollbar thumb is let go of long after the pointer has left it, so the toolkit tracks the gesture from the root, and `handle_mouse` is all that takes. It also handles the release that never comes: a pointer released outside the window sends nothing, and a move with no button held is taken as that release.

A host that tracks drags of its own at the root has two things to do. Call `vampir::mouse_moved(self, event, cx)` from its own mouse-move handler and `vampir::end_drags(self, cx)` from its mouse-up, instead of using `handle_mouse`. And override `forwarded_mouse_move` and `forwarded_mouse_up` on `ControlHost` to route into the same place: a surface that blocks the mouse, such as a scrollbar track, forwards its moves and releases through those, and the defaults only know about the toolkit's drags.

## Request frames for animations

Ask `ControlState::animating()` **after** your page has been built, not before. A switch or a disclosure notices its own state change while it renders — that is what lets a change made by a menu item or a shortcut slide exactly as a click does — so a check made before the controls run cannot see the slide one of them is about to start, and the frame that starts it never asks for the frame that would continue it.

Fades and slides are `Instant`-driven, so they finish on their own and stop asking for frames. The theme crossing over, a dialog arriving or leaving, a menu or a palette revealing are all counted. Fold `ControlState::animating()` into whatever decides:

```rust
if self.controls.animating() || self.my_own_transitions_running() {
    window.request_animation_frame();
}
```

`spinner` is the exception: it is indeterminate, so it never finishes and never stops needing frames. Ask for them only while one is actually on screen — `request_animation_frame` holds the display link open, and a spinner nobody can see should not be costing 60Hz. The gallery scopes it to the one page that has a spinner on it:

```rust
if self.controls.animating() || page == Page::Controls {
    window.request_animation_frame();
}
```

Do not drive hover from an `Instant`. Scrolling a list re-fires hover on every row and will pin the display link at 60Hz. Use GPUI's `.hover()`.

`animating()` is also the end of a frame's bookkeeping: it counts the frame, and retires the records of anything that has not rendered for two of them — what each control showed, where each value was on its way to, where each track and tab painted. That is what lets a row that leaves a list and comes back count as new, it is what keeps `ControlState` the size of the screen rather than of the session, and it is why it wants to be called exactly once per render, after everything else.

## Animating your own state

The same machinery the controls use is there for the host:

- **A two-state look:** `self.controls.blend((id, "on"), on, SWITCH_SLIDE)` is 0 to 1, eased, from wherever it was when `on` last changed.
- **A value going somewhere:** `self.controls.tween("key", target, MOVE)` glides from its current value to the target and is at the target the first time it is asked. `table_row` uses it to slide a row to its sorted place rather than redealing the table; `tween_angle` does the same for degrees, the short way round.
- **Content arriving:** `vampir::arriving("page", page as u64, &self.controls, body)` fades the body in and settles it up into place each time the key changes. The gallery wraps each page in one.
- **Text changing:** `vampir::fading_text("status", text, &self.controls)` fades new words in over the old ones, because words cannot be interpolated. The gallery's footer is one.
- **A scheme crossing over** is the theme's job; see below.

Key everything by what it *is* rather than where it is — a row by its id, not its index — or a row shifting down one place inherits the animation of whatever was there before. Build the key as a tuple, `(id, "shown", &row.id)`, not with `format!`: see [Ids](#ids).

## Ids

`ComboId` is `&'static str`. It is both the element id and the widget's identity in `ControlState`, so **two widgets of the same kind in one view must not share one**. There is no enum to keep in step, which is the point.

The same string is what comes back to `track_dragged` and `tabs_reordered`, so the `match` in those methods is the whole dispatch table for your drags.

Everything a control remembers between frames — `blend`, `tween`, `present`, its focus handle, where it painted — is filed under a `Tag`, which is the hash of whatever it was built from: a `&str`, a `SharedString`, an `ElementId`, or a tuple of them. Name a moving part with a tuple, `(id, "pill-x")` or `(id, "shown", &row.id)`, and building the key costs nothing; a `format!` string costs two allocations per part per frame and buys nothing over it, since text hashes as text whichever type carries it. One tag can hold one record of each kind at once, so a list may be `present`, have a keyboard row and have painted somewhere all under its own id.

## Themes

Every `ControlState` carries a `Theme`: one hue, and a `Scheme` that is `System`, `Light` or `Dark`. `self.controls.palette()` is the palette to hand every control this frame, and it follows the theme rather than jumping to it. A scheme change crosses over through `Palette::mix` over `SCHEME_FADE`; a hue change glides the short way round; a hue under the hand on its own slider sits under the hand. Every control is a function of the palette it is handed, so one cross-fade at the root is every colour on screen crossing over, whoever made the change.

Start from the system rather than from a guess. `self.controls.observe_appearance(window, cx)` in the view's constructor reads whether the desktop is light or dark and keeps following it. A window that opens dark on a light desktop looks broken before it looks like a choice. `Scheme::System` means the desktop's choice, `Light` and `Dark` are the user's, and picking `System` again hands control back.

```rust
// In the constructor:
self.controls.observe_appearance(window, cx);

// Anywhere:
self.controls.theme.scheme = Scheme::Dark;
self.controls.theme.hue = 200.0;

// In render, the toolkit's own pickers, bound to the theme:
vampir::scheme_picker("scheme", WidgetContext::new(palette, self, cx))   // System | Light | Dark
vampir::hue_picker("hue", WidgetContext::new(palette, self, cx))         // a slider the toolkit reads itself
```

The window's own ground is `palette.backdrop`, lit with `vampir::ground(palette.backdrop)` on the root; `scroll_area` fades scrolling content into it at the right height. `vampir::ui_font()` is the system's interface face, for `.font_family` on the root.

A host with a richer theme of its own ignores all of this. `Palette` is a plain public struct of `Copy` colour roles: build one with `Palette::from_hue` or field by field and pass it, **by value**, into every control. See [Design](design.md#colour) for what the roles mean and when adding one is justified.

Text inputs hold their own colours, because they are GPUI entities rather than functions. Style them with `InputStyle::from_palette(palette, size)`, and the field that holds one — `text_field`, `text_area`, `search_field`, `spinbox` — keeps it in step with whatever palette it is handed, so a theme crossing over carries the text with it. A style written out by hand is left alone. A highlighter that wants the accent uses `Span::accent()`, which reads the colour when the text paints rather than when the closure was written.

## Overlays

Which pop-up is open lives in `ControlState`, and so do the two overlays a host opens itself:

- `self.controls.open_dialog("confirm")` opens the `dialog` with that id. It takes the keyboard, and its buttons, Enter, Escape and the scrim hand the keyboard back and close it before the host's callback runs.
- `self.controls.toggle_palette("commands", &self.query, window, cx)` is what the shortcut for the `command_palette` does; `open_palette` and `close_palette` are the two halves. The palette puts the keyboard in the query, and gives it back where it was when it closes.

A host that removes whatever has focus — switching pages, say — calls `self.controls.focus_root(window, cx)` first. GPUI dispatches nothing from a handle that is no longer in the tree, not even the root's own shortcuts, so a shortcut that "stopped working" is almost always focus left on something that has gone.

## Key bindings and menus

The keys that are the same in every application are the toolkit's. `vampir::bind_keys(cx)`, once at start-up, binds Tab, Shift-Tab and Escape to the toolkit's `FocusNext`, `FocusPrevious` and `Dismiss` actions, and the text-editing keymap (`Backspace`, `SelectAll`, `Copy`, …) in the `"TextInput"` context, in the dialect the platform expects. `vampir::root` puts the handlers on the root; see [Keyboard and focus](keyboard.md#what-the-host-has-to-do). `standard_bindings()` is the list on its own, for a host that wants to leave some out.

Which key opens your command palette is your application's business, not a widget's, so everything else the toolkit leaves to you: it ships the actions and binds none of them. `shortcut_recorder` hands back a `Chord` with a `keystroke` ready for `KeyBinding`. Bind your own after `bind_keys`; a later binding for the same key wins.

Write bindings with `secondary` for the platform's primary modifier — it is ⌘ on macOS and Ctrl everywhere else, so `"secondary-k"` is one binding that is right on all three. Label shortcuts with `vampir::display("secondary-k")`, which renders `⌘K` or `Ctrl+K` as the platform writes it; a `⌘` shown on a Windows machine is a symbol with no key behind it.

On macOS the menu bar is not optional. `vampir::edit_menu()` is the Edit menu, wired to the text input's actions with the OS actions macOS needs to route them to the focused text; the rest of the bar is yours. See [Shipping a macOS app](macos-apps.md).
