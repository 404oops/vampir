# The design

This guide explains the visual and interaction decisions behind Vampir. Use it when adding or changing a control so the toolkit continues to feel like a coherent set of parts.

## Light comes from above

Everything reads as a physical surface lit from the top. That is the entire visual thesis, and `lighting` is the whole vocabulary:

- `lit(fill, lift)` — the vertical gradient. Every filled control gets one.
- `raised(dark)` — inner top highlight, outer drop shadow. Buttons, pop-up buttons, active segments, tabs.
- `recessed(dark)` — inner top shadow. Fields, tracks, wells, anything you can type or drag into.
- `panel(dark)` — lit top edge and a wide soft shadow. Floating things.
- `rim(fill, dark)` — a control's border, derived from its own fill.
- `glow(color, alpha, radius)` — focus rings and current-item halos.
- `shade(color, amount)` — toward white or black.
- `faded(shadows, t)` — the same shadows at `t` strength, for fades.

**Raised means you press it. Recessed means you put something in it.** If a new control is neither, work out which it is before drawing it.

Never hard-code a shadow or a gradient. If a recipe does not exist for what you need, add it to `lighting` where the next control can find it.

## Patterns to avoid

These patterns have been tried and do not fit the toolkit's visual language:

- A coloured accent bar down the side of a selected row. Use a lit accent fill across the whole row.
- Underlined text tabs. Tabs are raised surfaces.
- Tiny uppercase micro-captions. Use `caption`, sentence case.
- Monospace for values and labels. Proportional type everywhere; mono is for identifiers, hashes and paths only.
- Dots, pills and chips as status indicators. `badge` exists for counts and states; a status is usually better said in words.
- Uniform rounded cards nested inside uniform rounded cards.
- Hairline dividers between every row of a list.
- Flat, shadowless surfaces.

If a new control genuinely needs one of these patterns, explain the reason and the trade-off in the pull request.

## Metrics

`CONTROL_HEIGHT` (30) and `CONTROL_RADIUS` (6) are shared so a row of mixed controls lines up with no per-control tuning. Use them.

`CHIP_HEIGHT` (26) is the one deliberate exception: a wrapping field of thirty full-height controls reads as a wall rather than as a set of choices.

Nested radii step down by 1 or 2, never up.

Text is 12.5px for controls, 11.5px for secondary text, 14px for a dialog title. Do not introduce a fourth size without a reason you can write down.

## Colour

Only the roles in `Palette`. If a control needs a colour that is not there, either it is one of the existing roles under another name, or the palette is genuinely missing a role — and adding one means adding it to `from_hue` for every hue and both schemes, and checking the result.

**Both schemes always.** A light-mode-only or dark-mode-only path is a bug; the `is_dark` flag exists so one code path covers both.

![The same page in light mode](light.png)

`from_hue` has a test that walks every 15 degrees through both schemes and asserts the results stay in gamut; extend it when you add a role.

## Interaction

- **Select on release, not on press.** Pressing is also how a drag starts, and a drag must not leave a trail of selections behind it. `tab_bar` does this; anything draggable must.
- **A press that never moved is a click.** That is the `moved` flag on every drag.
- **Handle the release that never comes.** A pointer released outside the window sends nothing. A move with no button held is that release.
- **Blocking the mouse blocks hover too.** Anything with `.block_mouse_except_scroll()` or `.occlude()` must forward moves and releases, or drags underneath it stall.
- **Disable, do not remove.** A control at its limit greys out and stays put. Removing it shifts its neighbour under the pointer.
- **Full width is a decision, not a default.** `button` fills its slot, which is right for a dialog footer and wrong for a list of twenty choices. When the options are a set rather than a sequence of actions, use `chip`.
- **Pop-up triggers toggle their pop-up.** Pressing an open menu or pop-up button closes it. Dismiss-on-press-outside must ignore the trigger; otherwise it closes on press and reopens on click. `context_menu` and `combo` both check the trigger before dismissing.
- **Deferred priorities:** context menus 200, command palette 180, dialogs 150, pop-up lists 100. Keep new overlays inside that ordering.

## Motion

Nothing on screen changes in a single frame. A state that flips slides between its two looks; a thing that moves slides to where it is going; a colour that changes crosses over. This is not decoration. A change that lands in one frame has to be noticed after the fact and worked out; a change that is seen happening is understood as it happens, and the eye is carried to where it went.

- **Three speeds, and only three.** `SWITCH_SLIDE` (140ms) for a state flipping: a switch, a tick drawing in, a highlight moving to another row, a chevron turning. `MOVE` (180ms) for something going somewhere: the pill under a segmented control or a tab bar, a row taking its sorted place, a page arriving. `SCHEME_FADE` (240ms) for everything changing at once: light to dark, one hue to another. Pop-ups and menus reveal over `COMBO_REVEAL`. A new control picks one of these; it does not invent a fourth.
- **Ease out to arrive, ease in to leave.** Fast start, settling into the end value, for anything coming in; the reverse for anything going, so both ends of a motion sit against the control rather than drifting from it.
- **Animate the state, not the click.** A control notices its value changed while it renders — `ControlState::transition` and `blend` for two states, `tween` for a continuous value — so a change from a shortcut, a menu item, the command palette, the host's own code or the desktop switching to dark at sunset moves exactly as a click does.
- **Nothing animates in from nowhere.** The first time an element is seen it paints its end state. Rows joining a list that is already up fade in at their place; a list that has just appeared arrives whole, with whatever brought it — `ControlState::present` tells the two apart.
- **A slide that changes its mind bends.** A new target sets off from wherever the value is now, never from either end, so a toggle mid-turn reverses smoothly.
- **The hand is the motion.** A value under the pointer — a slider being dragged, a colour-pad marker, a tab being reordered — snaps to it (`ControlState::snap`). Only a value set from elsewhere glides. A thumb trailing the pointer reads as lag, not as polish; a dragged tab that lags the hand reads as a tab that will not come. The tab rides under the pointer from the point where it was grabbed, the others slide out of its way, and on release it slides the last few pixels home.
- **Cross-fade in sRGB.** `Palette::mix` and the blends inside controls use `color::lerp`, not the gamma-correct `color::mix`: half way between a light and a dark palette in linear light is a washed-out grey brighter than either, and the whole window flashes on its way from one to the other.
- **What can be measured unfolds; what cannot, fades.** A disclosure knows how tall its body was last time it was open, so it grows to that height and what is below moves down with it. A body that has never been open fades in, because a height that is guessed is worse than none.
- **Motion has to stop.** Everything is `Instant`-driven and finishes on its own. `ControlState::animating()` is how the host knows to keep asking for frames, and it is also where the frame is counted — see [Hosting](hosting.md#request-frames-for-animations). Nothing loops except `spinner`, which is indeterminate by definition and therefore the host's decision to keep on screen.
- **Look at it slowed down.** A slide is faster than a screenshot. `VAMPIR_SLOW_MOTION=8 tools/gallery.sh launch` runs the gallery with every animation eight times slower, and the footer says so; see [Contributing](../CONTRIBUTING.md#checks-and-expectations).
