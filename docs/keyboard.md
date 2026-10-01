# Keyboard and focus

Every control in the toolkit can be reached and operated from the keyboard, and none of it needs setting up beyond two calls: `vampir::bind_keys(cx)` when the application starts, and `vampir::root(element, self, cx)` on the root element of a view.

## The principle

**Ring whatever is currently active. If that thing has options inside it, ring the option, not the container.**

A radio group is one stop in the tab order, but the ring belongs on the chosen dot. A tab bar is one stop; the ring belongs on the active tab. A slider is one stop; the ring belongs on the thumb. Ringing the container instead tells a reader "you are somewhere in here", which is the one thing they already knew.

This has a second half that is easy to miss: **clicking an option puts the keyboard on it**, so Tab carries on from what was just clicked rather than starting the group again or skipping it entirely. The mouse and the keyboard have to agree about where you are.

## Two kinds of key

They belong to different owners, and the split is the whole design.

**The control's own keys** — Space on a button, arrows inside a group, Left and Right on a disclosure triangle — are what the control *is*, on every platform. The toolkit handles them through `on_key_down` and they work with no setup. A toolkit that made every host bind them would ship inaccessible by default and stay that way in most applications built on it.

**The application's keys** — ⌘K, ⌘S, whatever opens your own palette — are decisions only the host can make. The toolkit ships actions and leaves the binding alone.

`keyboard::key` draws the line: a press carrying ⌘, ⌃, ⌥ or fn is never one of the control's own, because ⌘← is "go to the start of the line" and a slider that treated it as "one step left" would swallow a binding the host had every right to make.

## What the host has to do

Two calls. `bind_keys` binds the keys that are the same in every application — Tab and Shift-Tab, which walk the window; Escape, which closes whatever is open; and the text-editing keymap for `TextInput`, in the macOS dialect or the Ctrl-key one as the platform expects — to the toolkit's own actions, `FocusNext`, `FocusPrevious` and `Dismiss`. `root` puts the handlers for those actions on the root element, where they arrive when no control has taken them itself, along with the mouse handlers the toolkit's drags need and the root focus handle described below:

```rust
// Once, when the application starts:
vampir::bind_keys(cx);

// On the root element of the view. The host's own actions go after it.
vampir::root(div().id("root"), self, cx)
    .on_action(cx.listener(Self::toggle_palette))
    .child(..)
```

`root` is `handle_keys` and `handle_mouse` together; a host that tracks the mouse itself at the root uses `handle_keys` on its own. See [Hosting](hosting.md#drags).

Tab goes through `move_focus` rather than `window.focus_next` directly: it closes any pop-up the keyboard is walking away from, and keeps Tab cycling inside a modal dialog while one is up. That has to happen on the action's side of the binding — in GPUI a keystroke that matches a binding is dispatched as that action and never reaches a key listener, so the dialog cannot intercept Tab itself.

Escape is answered by whatever holds the keyboard: a pop-up list closes and leaves its value alone, a menu or the command palette closes and hands focus back, a dialog runs its dismiss callback. An Escape nothing of the toolkit's was open for is passed on, so a host that listens for `vampir::Dismiss` on the root — after `root` — closes its own overlays with the same key.

Two more things come with it.

**The first Tab after using the mouse shows where the keyboard already is, rather than moving on.** The ring is hidden while the mouse is in charge, so a Tab that moved would look as though it had skipped a control: you clicked one thing and the ring appeared on the next. `handle_keys` notices the mouse taking over, and `move_focus` lets the first Tab after that only reveal.

**Moving the keyboard closes anything a pop-up has open.** A dropdown is a question the view is asking; walking to another control answers it by leaving, and a list still on screen looks as though the next key pressed will land in it. `move_focus` does it, and `ControlState::dismiss_popups` is the same in one call for a host that moves focus some other way.

The application's own keys — ⌘K, ⌘S — are still the host's to bind, after `bind_keys`; a later binding for the same key wins, so a host can even take Escape for itself if it must. Write them with `secondary` for the platform's primary modifier — see [Hosting](hosting.md#key-bindings-and-menus). What the key *does* is usually one call: the gallery's ⌘K is `self.controls.toggle_palette(..)`.

## What each control does

| | |
|---|---|
| `button`, `icon_button`, `chip` | Space or Enter. |
| `checkbox`, `switch` | Space or Enter toggles. The ring is on the box and on the track — the parts with a fill. Both take an optional label that is part of the hit target. |
| `radio_group` | One tab stop. Up and down move the choice and wrap; Home and End reach the ends. Disabled choices are stepped over, never landed on. |
| `segmented`, `tab_bar` | One tab stop. Left and right move the selection and wrap. |
| `combo` | Space, Enter or Down opens; moving the keyboard away closes it. Up and down move the highlight, Enter commits it, Escape closes and leaves the value alone — arrowing through a list is looking, not choosing. |
| `slider` | Arrows move one step, Page ten, Home and End the ends. A stepped track moves one notch. |
| `dialog` | Takes the keyboard when it opens, on the primary button or failing one the first. Tab cycles among its buttons. Enter is the default action, Escape dismisses, and closing hands the keyboard back where it was. |
| `command_palette`, `search_list` | The query field has the keyboard. Up and down move the highlight, Enter picks it; a new query puts the highlight back on the best match. The palette opens with the keyboard in its query, closes on Escape, and hands the keyboard back where it was. |
| `collapsible` | Space toggles, Left closes, Right opens. |
| `split_handle` | Arrows move the divider. A divider only the mouse can move is content a keyboard user cannot reach. |
| `context_menu` | Takes focus when it opens. Up and down move over the enabled rows only, Enter runs one, Escape closes. Focus goes back where it was. |
| `table_header` | Sortable columns are tab stops; Space or Enter sorts. |
| `tree_row` | One tab stop, on the selected row. Up and down walk the flattened rows without wrapping. Right opens a closed branch and then steps into it; left closes an open one and then steps out to the parent. |
| `selectable_row`, `selectable_list` | One tab stop for the list, on its active row. Up and down move without wrapping; Home and End reach the ends; `selectable_list` scrolls the target into view. Shift extends from the anchor; Cmd/Ctrl-click toggles a row; Cmd/Ctrl+A selects all; Enter activates; Escape clears the selection. |
| `color_pad` | Left and right move chroma, up and down move lightness. |
| `swatch_grid` | Each swatch is a tab stop. |
| `spinbox` | Up and down step the value from inside the field, Enter commits a typed one; the steppers are tab stops too. |
| `text_field`, `text_area`, `search_field` | Tab stops. Their editing keys are bound by `bind_keys`; the actions are public for a host that wants them bound differently — see [Hosting](hosting.md#key-bindings-and-menus). |
| `shortcut_recorder` | Space or Enter begins recording; Escape cancels. The next chord is captured even if the host has already bound it, without executing that binding. |

## Keys a field passes on

A single-line `TextInput` has no line to move to, so it passes Up and Down on rather than swallowing them, and an Enter it has nothing to submit to likewise. Whatever holds the field is what those keys mean there: a spin box steps, a search list or a command palette moves its highlight and picks the highlighted row. That is how those are built, and how to build another: listen for `text_input::Up`, `Down` and `Enter` as *actions* on the element that holds the field — a keystroke that matched a binding never arrives as a raw key — and they only fire while the field has the keyboard, which is exactly when they should.

## Focus that outlives its element

GPUI dispatches nothing from a focus handle whose element is no longer in the tree — not even the window's own shortcuts. So anything that removes the focused element has to put the keyboard somewhere live first. A dialog, a menu or the command palette closing hands it back where it was, and when there was nowhere to hand it back to, to the root: `root` gives the root element the handle in `ControlState::root_focus`, so there is always somewhere live. A host switching pages does the same with `ControlState::focus_root`, as the gallery's `show_page` does. A shortcut that "stopped working" is almost always focus left on something that has gone.

## How focus is held

Two mechanisms, because the controls need different things.

**A control with one option** — a button, a chip, a checkbox — gets GPUI's implicit focus handle, which lives in the element's own state and survives for as long as the element keeps its id. Nothing has to store anything.

**A composite** — a radio group, a segmented control, a tab bar, a tree — shares one handle that rides whichever option is current. That handle *has* to outlive any one option: the selection moves, and a handle owned by the option it is drawn on dies the moment that option stops being current, taking the keyboard with it after a single arrow press. So it lives in `ControlState`, keyed by the control's id, and is handed to the current option each frame. `ControlState::focus(id, cx)` is that registry.

This is why a group is one press in the tab order however many options it holds, and why a twenty-tab editor does not swallow twenty presses on the way to the document.

## Why the ring is an element and not a style

The obvious way to draw a focus ring is a box shadow on the control. It does not survive contact with a real layout.

A box shadow paints *behind* the element that owns it. On a filled control that reads as a ring; on a bare one it shows straight through and reads as a solid block of accent over the whole thing. And behind means behind: a neighbour drawn later covers it, and any ancestor that clips — a scrolling tab bar, a tree with hidden overflow — cuts it off.

So the ring is an element. `keyboard::ring` returns an overlay to add as a control's **last** child:

```rust
div().id("save").relative()
    .child(label)
    .child(keyboard::ring("save-ring", CONTROL_RADIUS, palette).on_key_down(activate))
```

Being last, it paints over the control's own content. Being absolutely placed on the control's bounds, there is nothing outside it for a clip to take — the ring is a border, and a border on an absolutely-placed box draws inward. The same recipe then works for a filled control and a bare one, and inside a clipping container as well as outside one.

It is also the tab stop, which buys two more things. Its hitbox covers the control, so clicking anywhere focuses it and the keyboard picks up where the mouse left off. And it costs no layout: the border is on the overlay, so the ring appearing moves nothing.

`keyboard::ring_for` is the same overlay carrying a group's shared handle instead of one of its own, for the current option of a composite.

Both use `focus_visible` rather than `focus`, so the ring appears for keyboard navigation and not after a click. Someone who has just clicked a button does not need to be told which one they clicked.

## Deciding what a key does

The decisions are pure functions in [`keyboard`](../src/keyboard.rs), which is what makes them testable without a window:

- `key(event)` — what a press means, or `None` to leave it alone.
- `step(key, orientation, current, len)` — where it moves in a group. Wraps, because a set of choices has no end to fall off.
- `nudge` / `nudge_stepped` — where it moves along a range. Clamps, because a range does.
- `tree_step(rows, index, key)` — where it moves in a tree. Does not wrap: a tree is content, and arriving back at the top after pressing down at the bottom loses a reader their place rather than saving them a keystroke.
