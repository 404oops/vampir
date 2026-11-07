# Retained view rendering

A view keeps what it drew between frames. When the window draws again, GPUI
skips the `render` call of each view where nothing it read has changed, and
draws that view's previous output instead: its layout, the state its elements
kept from prepaint, its painted primitives and its accessibility nodes. An
[`Entity`] or [`AnyView`] passed as a child is kept this way; there's nothing to
turn on.

This page explains what a kept view depends on, where its output can be reused,
and how layout is carried between renders. [The
frame](crate::_documentation::concepts::the_frame) describes the phases this
work skips.

## What a view depends on

While a view renders, prepaints and paints, GPUI records what it reads:

- the entities it reads or updates, including through a context;
- the globals it reads;
- the pointer position, modifiers and input modality, when it asks the window
  for them;
- the text style, rem size and scale factor it inherits.

A view nested in another adds its reads to its parent's, so a parent that was
kept also covers the views inside it. The view renders again on the next frame
when its own entity or an entity it read is notified with
[`Context::notify`], when a global it read is updated, when an input it read
changes, or when its inherited text style, rem size or scale factor changes.
[`Window::refresh`] renders every view.

A change that GPUI can't see doesn't render a view again. Mutating an entity
without calling `notify`, or a value behind an `Rc<RefCell<_>>` or an atomic,
leaves a kept view showing what it drew before. Notify the view's entity after
such a change. The [performance
guide](crate::_documentation::guides::performance#reuse-a-view-that-didnt-change)
has a test that shows both cases.

## Where output is reused

A view's previous output is drawn again only where it was drawn last frame: at
the same bounds, inside the same content mask, with the same text direction and
Unicode bidirectional mode. A view that moved or changed size renders again,
even when nothing it read changed. Output is also rebuilt when a screen reader
connects and the view drew without the accessibility tree, and on every frame
while the inspector is picking an element.

Reusing output costs a check of each recorded dependency, and a copy of the
view's recorded prepaint state and primitives into the new frame. Both grow with
what the view drew, not with what its `render` call would have done. A view
notified every frame gains nothing from being kept.

## Choose how a view is laid out

An entity passed as a child is laid out by its content, in the parent's layout.
Give it a fixed root style with [`Entity::cached`] or [`AnyView::cached`] when
the parent should decide its size instead:

```rust
use gpui::{Context, Entity, IntoElement, Render, StyleRefinement, Window, div, prelude::*, px};

struct Sidebar;

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Navigation")
    }
}

fn sized_by_content(sidebar: Entity<Sidebar>) -> impl IntoElement {
    div().child(sidebar)
}

fn sized_by_parent(sidebar: Entity<Sidebar>) -> impl IntoElement {
    div().child(sidebar.cached(StyleRefinement::default().w(px(240.)).h_full()))
}
```

With `cached`, the parent lays out a box with the style you pass, and the view's
content is laid out inside the size that box gets. The content can't change the
box's size, so choose a style that sizes the box without its content, such as a
fixed width or `size_full`.

## How layout carries over

Each view keeps its own layout nodes between frames, and its parent reaches them
through one node. A kept view's nodes stay as they were laid out. When a view
renders again, each node whose style and children match the node at its place in
the previous render keeps that node's computed layout. Text whose content, style
and fonts are unchanged keeps its measurement too, so it isn't shaped again. A
custom element that measures itself with [`Window::request_measured_layout`] is
measured again whenever its view renders.

## Check a change against a full render

`randomized_element_tree_matches_full_render` draws seeded trees of elements and
views, applies random mutations, and compares each frame with one drawn from
scratch. Run it after changing how views are kept or invalidated:

```sh
cargo test -p gpui-ce --lib randomized_element_tree
```
