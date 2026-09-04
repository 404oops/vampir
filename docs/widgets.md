# What is in the toolkit

Most entries here are free functions generic over your view. They take a `Palette` by value and a `Context<V>` for callbacks, then return an element. `TextInput` is the main exception because it needs to retain cursor and selection state.

Every control animates its changes of state — the switch slides, the tick draws in, the selection pill moves, a re-sorted row takes its new place — whoever made the change and however. The table does not repeat it; the rules are in [Design › Motion](design.md#motion).

![The controls page of the gallery example](controls.png)

## Controls — `vampir::controls`

| | |
|---|---|
| `caption` | Quiet section label. |
| `button` | The lit button. `ButtonVariant::{Soft, Primary, Danger}`. |
| `icon_button` | Square, takes any element as its icon. `active` gives it the pressed-in look of a toggle. |
| `switch` | Toggle, with an optional label that is part of the hit target. |
| `checkbox` | With an optional label; the whole row is the hit target. The well fills and the tick draws itself in. |
| `radio_group` | Vertical exclusive choices, each able to carry a `detail` line. |
| `segmented` | Recessed track, one raised pill that slides to the chosen option. Two or three short options. |
| `chip` / `chip_group` | Buttons sized to their own labels, in a wrapping row. For a choice with more options than a segmented control can carry but all worth showing: tags, a filter bar, twenty export formats. |
| `spinbox` | Steppers around an editable value field. |
| `text_field` / `text_area` | Recessed wells around a `TextInput`. |
| `search_field` | Field with a magnifier and a clear button. |
| `combo` | Pop-up menu button and its list. `ComboDirection::Up` near a window's bottom. |
| `slider` | `SliderTrack::Continuous` or `Stepped { stops }`, which draws ticks and snaps. |
| `progress_bar` | Determinate. |
| `spinner` | Indeterminate. Only animates while the host is asking for frames — see [Hosting](hosting.md#request-frames-for-animations). |
| `badge` | `BadgeTone::{Neutral, Accent, Danger}`. |
| `separator` | Hairline rule, horizontal or vertical. |
| `scrollbar` | Overlay bar. Put it in a `.relative()` wrapper around the scroll container; it renders nothing while the content fits and fades in when it stops fitting. |

## Containers — `vampir::containers`

`tab_bar` (drag to reorder — the tab rides under the pointer and the others slide aside — with optional close buttons and a raised pill that slides to the active tab; a `Tab` is identified by its label unless `Tab::id` says otherwise, and everything the bar remembers about a tab follows that identity through a reorder), `split_handle` + `split_area`, `collapsible` (unfolds to its measured height), `dialog` + `DialogButton`.

`split_area` is the invisible probe that turns a pointer position into a fraction. Put it inside the element the two panes share; the divider will not work without it.

## Menus — `vampir::menu`

`context_menu` renders the open menu; `MenuItem::action(..).shortcut(..) .checked(..).danger().disabled()` builds the rows, and `MenuItem::header` and `MenuItem::separator` break them up. Open it from wherever the press happened:

```rust
.on_mouse_down(MouseButton::Right, cx.listener(|this, event: &MouseDownEvent, _w, cx| {
    this.control_state_mut().open_menu("row", event.position, row_id.to_string());
    cx.notify();
}))
```

and render it once, near the root:

```rust
.children(vampir::context_menu("row", &self.menu_items(), self.palette, self, cx,
    |host, action, window, cx| host.run_menu_action(action, window, cx)))
```

The `target` string is yours and comes back unchanged through `ControlState::menu_target()` while the callback runs. Build the item list from it each frame so it reflects what was clicked. `menu_button` opens the same menu beneath a button, and pressing the button again closes it.

These are in-window menus drawn by the toolkit. The macOS menu bar is a different thing entirely and belongs to GPUI — see [Shipping a macOS app](macos-apps.md).

## Overlays — `vampir::overlay`

![The command palette open over the fields page](overlays.png)

`Tooltip::text` / `Tooltip::with_shortcut` plug into GPUI's own `.tooltip(..)`, which already owns the hover delay and the placement.

`command_palette` plus `fuzzy_filter` / `fuzzy_score`: the host owns the query input, the filtered list and the highlighted index, because those are also what the arrow keys move and what Enter commits. `command_list` is the palette's rows on their own, for a search field with its results underneath or anywhere else a query has a list of answers; the gallery's Fields page shows one filtering as you type.

## Data — `vampir::data`

![A tab bar, a tree, a table and a split](data.png)

`tree_row` renders one row of an already flattened tree, so it drops straight into a `uniform_list`. `table_header` with `Column` and `SortDirection`.

The flattening is the host's, and it is the step that makes collapsing mean anything: keep a real tree, and walk it into `TreeRow`s each frame, skipping the children of anything closed. Keeping the flattened list *as* the model is the tempting shortcut — it is what gets drawn, after all — but then closing a branch has nothing to hide, because its children were never underneath it. `examples/gallery.rs` shows the walk.

## Colour — `vampir::swatch`

![The colour page, with an Oklch chroma and lightness pad](colour.png)

`hue_slider`, `color_pad` (chroma across, lightness up), `swatch_grid`, `hue_wheel`. Everything is `Oklch`.

## Shortcuts — `vampir::shortcut`

`shortcut_recorder` captures the next chord and hands back a `Chord` with both a `keystroke` for `KeyBinding` and a `display` for a person. Binding it is the host's business.

## Text — `vampir::text_input`

![Text fields, a rich-text area and a shortcut recorder](fields.png)

`TextInput` is a full input: selection, IME composition, undo, clipboard, multi-line wrapping. It is the one thing in the crate that is an entity rather than a function, because a text cursor genuinely is state.

Rich text is a `Highlighter`, a closure from content to `Span`s, re-run on every layout:

```rust
input.update(cx, |input, cx| {
    input.set_highlighter(|text| {
        find_mentions(text).map(|range| Span::new(range).color(accent).bold()).collect()
    }, cx);
});
```

Stale ranges are dropped rather than panicked on, because a highlighter usually runs against text that has since been edited.

## Colour and lighting — `vampir::color`, `vampir::lighting`

`Palette::from_hue(hue, dark)` derives the whole set from one hue. A host with a richer theme of its own writes the twenty-odd fields directly, which is the point of the struct being plain and public.

`lighting` is the vocabulary every control draws itself with — `lit`, `raised`, `recessed`, `panel`, `rim`, `glow`, `shade`. [Design](design.md) explains what each means and when to reach for it.
