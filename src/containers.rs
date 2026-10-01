//! Things that hold other things: a tab bar, a split divider, a collapsible
//! section, a modal dialog, a scrolling area, a card, and the rows and
//! columns a page is laid out in.
//!
//! These take content rather than data. Where a control renders a value, a
//! container renders whatever the host puts inside it, so each one takes
//! elements and returns a bigger element.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, Div, ElementId, FocusHandle, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, PathBuilder, ScrollHandle, SharedString, Window, canvas, deferred, div, point,
    prelude::*, px,
};

use crate::controls::{
    ButtonVariant, CONTROL_RADIUS, WidgetContext, button_focused, caption, scrollbar,
};
use crate::keyboard::{self, Dismiss, Key, Orientation};
use crate::lighting;
use crate::palette::Palette;
use crate::scroll::{ScrollAxis, scroll_fades};
use crate::state::{
    ComboId, ControlHost, ControlState, MOVE, SWITCH_SLIDE, TabDrag, Tag, TrackAxis,
};

// ---- Tab bar ----------------------------------------------------------------

/// One tab.
#[derive(Clone, Debug)]
pub struct Tab {
    /// What this tab *is*, as opposed to where it currently sits. Everything
    /// the bar remembers about a tab between frames — its size along the
    /// bar, how far along its slide it is, whether its label has crossed
    /// over — is keyed by this, so a reorder, which renumbers every tab,
    /// moves the records with the tabs. Defaults to the label; set it when
    /// two tabs could share one.
    pub id: SharedString,
    pub label: SharedString,
    /// Drawn to the right of the label, for a count or a modified dot.
    pub badge: Option<SharedString>,
    pub closable: bool,
}

impl Tab {
    pub fn new(label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            id: label.clone(),
            label,
            badge: None,
            closable: false,
        }
    }

    /// An identity other than the label, for bars where labels can repeat:
    /// two documents called "Untitled", say.
    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = id.into();
        self
    }

    pub fn badge(mut self, badge: impl Into<SharedString>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    pub fn closable(mut self) -> Self {
        self.closable = true;
        self
    }
}

/// How a tab bar uses the space its host gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TabBarLayout {
    /// Tabs keep their natural width and the bar scrolls sideways if needed.
    Horizontal,
    /// Every tab has the same width, keeping close targets aligned while
    /// siblings are removed. Long labels elide; the strip scrolls sideways.
    ///
    /// `width` is clamped to 96..=320: narrower leaves no room for a label
    /// beside the close target, wider stops reading as a tab. A width that
    /// is not finite falls back to 160.
    HorizontalUniform { width: f32 },
    /// Tabs fill the host's chosen width and stack vertically. Constrain the
    /// returned element's height when the list should scroll.
    Vertical,
}

impl TabBarLayout {
    fn vertical(self) -> bool {
        matches!(self, Self::Vertical)
    }

    fn tab_width(self) -> Option<f32> {
        match self {
            Self::HorizontalUniform { width } => Some(if width.is_finite() {
                width.clamp(96.0, 320.0)
            } else {
                160.0
            }),
            _ => None,
        }
    }
}

const TAB_HEIGHT: f32 = 26.0;
const TAB_GAP: f32 = 2.0;
const TAB_PAD: f32 = 2.0;
/// The well's border plus its padding: where the first tab starts inside
/// the scrolled content, and how far the last one ends before its end.
const TAB_INSET: f32 = 1.0 + TAB_PAD;

/// Horizontal tab bar with drag-to-reorder.
///
/// Pressing a tab selects it; dragging one past the midpoint of its
/// neighbour moves it, and the drop arrives at
/// [`ControlHost::tabs_reordered`] once the host ends the drag with
/// [`crate::state::end_drags`]. A press that never moves stays a plain
/// selection, so reordering costs nothing in ordinary use.
#[allow(clippy::too_many_arguments)]
pub fn tab_bar<V: ControlHost>(
    id: ComboId,
    tabs: &[Tab],
    selected: usize,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
    on_close: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    tab_bar_layout(
        id,
        tabs,
        selected,
        TabBarLayout::Horizontal,
        ctx,
        on_select,
        on_close,
    )
}

/// The tab bar with a space policy. Keep the selected index and any reorder
/// in the host exactly as for [`tab_bar`].
#[allow(clippy::too_many_arguments)]
pub fn tab_bar_layout<V: ControlHost>(
    id: ComboId,
    tabs: &[Tab],
    selected: usize,
    layout: TabBarLayout,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
    on_close: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let dark = palette.is_dark;
    let vertical = layout.vertical();
    let uniform_width = layout.tab_width();
    let on_select = Rc::new(on_select);
    let on_close = Rc::new(on_close);
    let scroll = view.control_state().scroll((id, "tab-scroll"));
    // One tab stop for the bar, not one per tab: a tab bar is a single
    // choice, so Tab passes it in one press and the arrows move between
    // tabs — which is what stops a twenty-tab editor swallowing twenty
    // presses on the way to the document. The handle rides the active tab,
    // because that is the option the arrows are pointing at.
    let focus = view.control_state().focus(id, cx);
    let count = tabs.len();
    let mut arrows = (count > 0).then(|| {
        let on_select = on_select.clone();
        cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            let Some(key) = keyboard::key(event) else {
                return;
            };
            let orientation = if vertical {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            };
            if let Some(moved) = keyboard::step(key, orientation, selected, count) {
                cx.stop_propagation();
                on_select(this, moved, window, cx);
                cx.notify();
            }
        })
    });
    let dragging_here: Option<TabDrag> = view
        .control_state()
        .tab_drag()
        .filter(|drag| drag.bar == id)
        .copied();
    let weak = cx.entity().downgrade();

    // Reordered for display while a drag is in flight, so the tabs slide
    // past each other under the pointer instead of jumping on release.
    let order: Vec<usize> = match &dragging_here {
        Some(drag) if drag.moved => {
            let mut order: Vec<usize> = (0..tabs.len()).collect();
            if drag.from < order.len() && drag.to < order.len() {
                let moved = order.remove(drag.from);
                order.insert(drag.to, moved);
            }
            order
        }
        _ => (0..tabs.len()).collect(),
    };

    let state = view.control_state();
    // The first render has no tab bounds yet. Keep this reveal pending until
    // a painted frame can supply them; then leave manual scrolling alone
    // until the selection or layout changes.
    if let Some(tab) = tabs.get(selected)
        && state.tab_reveal_pending(
            id,
            (&tab.id, selected, vertical, uniform_width.map(f32::to_bits)),
        )
        && reveal_tab(state, id, &tab.id, &scroll, vertical)
    {
        state.finish_tab_reveal(
            id,
            (&tab.id, selected, vertical, uniform_width.map(f32::to_bits)),
        );
    }
    // Where each tab will sit in this order, worked out from the sizes they
    // painted at last frame. Knowing it before layout is what lets a tab
    // slide to a new slot rather than appear in it, and lets the raised pill
    // under the active tab follow the same numbers: one pill that moves,
    // rather than a fill that goes out on one tab and comes on on another.
    // Sizes are measured by tab id. Uniform and vertical bars can plan from
    // their requested size before the first paint; a natural-width tab must
    // wait one paint before it can join the slide plan.
    let sizes: Option<Vec<f32>> = tabs
        .iter()
        .map(|tab| {
            state
                .bounds((id, "tab", &tab.id))
                .map(|bounds| {
                    if vertical {
                        f32::from(bounds.size.height)
                    } else {
                        f32::from(bounds.size.width)
                    }
                })
                .or({
                    if vertical {
                        Some(TAB_HEIGHT)
                    } else {
                        uniform_width
                    }
                })
        })
        .collect();
    let places: Option<Vec<f32>> = sizes
        .as_ref()
        .map(|sizes| tab_places(sizes, &order, TAB_GAP));
    // The tab under the hand rides with the pointer, from the point where it
    // was grabbed, and the others slide out of its way; only on release does
    // it settle into its slot. Its offset from where it is laid out comes
    // from the pointer, not from a tween — the hand is the motion.
    let held_slot = dragging_here
        .as_ref()
        .filter(|drag| drag.moved)
        .map(|drag| drag.to);
    let well = state.group(id);
    let held_offset = held_slot.and_then(|slot| {
        let drag = dragging_here.as_ref()?;
        let places = places.as_ref()?;
        let sizes = sizes.as_ref()?;
        let well = well?;
        let size = *sizes.get(*order.get(slot)?)?;
        let (origin, end) = if vertical {
            (f32::from(well.top()), f32::from(well.bottom()))
        } else {
            (f32::from(well.left()), f32::from(well.right()))
        };
        let laid_out = origin + TAB_PAD + places[slot];
        let lowest = origin + TAB_PAD;
        let highest = (end - TAB_PAD - size).max(lowest);
        let wanted = (drag.pointer - drag.grab).clamp(lowest, highest);
        Some(wanted - laid_out)
    });
    let pill = places
        .as_ref()
        .zip(sizes.as_ref())
        .and_then(|(places, sizes)| {
            let slot = order.iter().position(|&index| index == selected)?;
            Some((slot, TAB_PAD + places[slot], *sizes.get(selected)?))
        })
        .map(|(slot, pos, size)| {
            // Under a held active tab the pill is part of what is being
            // carried, so it goes where the hand goes and slides home from
            // there on release.
            match held_offset.filter(|_| held_slot == Some(slot)) {
                Some(offset) => (
                    state.snap((id, "pill-pos"), pos + offset),
                    state.snap((id, "pill-size"), size),
                ),
                None => (
                    state.tween((id, "pill-pos"), pos, MOVE),
                    state.tween((id, "pill-size"), size, MOVE),
                ),
            }
        });
    // The held tab is drawn twice: an invisible copy keeps its place in the
    // flow, and the one that is seen floats over the bar so it is never
    // under a neighbour it is passing.
    let mut floating: Option<AnyElement> = None;

    let mut rendered: Vec<AnyElement> = Vec::with_capacity(tabs.len());
    for (slot, &index) in order.iter().enumerate() {
        let cx: &mut Context<V> = &mut *cx;
        let Some(tab) = tabs.get(index) else { continue };
        let on_select = on_select.clone();
        let on_close = on_close.clone();
        let active = index == selected;
        let tab_arrows = if active { arrows.take() } else { None };
        let click_focus = focus.clone();
        let held = held_slot == Some(slot);
        let weak = weak.clone();
        let fill = palette.control_fill;
        // Keyed by the tab, never by its index: a reorder renumbers every
        // tab, and a slide that belonged to an index would be inherited by
        // whichever tab landed there — every tab setting off from where some
        // other tab was.
        let key = |what: &'static str| Tag::new((id, what, &tab.id));
        let element = |what: &str| ElementId::Name(format!("{id}-{what}-{}", tab.id).into());
        // The label crosses over as the pill arrives under it.
        let on = state.blend(key("tab-on"), active, SWITCH_SLIDE);
        // Laid out in its slot, drawn on its way there — or, for the tab in
        // hand, wherever the hand is.
        let slide = key(if vertical { "tab-y" } else { "tab-x" });
        let offset = match (places.as_ref(), held_offset.filter(|_| held)) {
            (Some(places), Some(offset)) => state.snap(slide, places[slot] + offset) - places[slot],
            (Some(places), None) => {
                let place = places[slot];
                state.tween(slide, place, MOVE) - place
            }
            (None, _) => 0.0,
        };
        if held && let Some(places) = places.as_ref() {
            let float = tab_face(tab, active, palette, layout)
                .absolute()
                .when(vertical, |el| {
                    el.top(px(TAB_PAD + places[slot] + offset))
                        .left(px(TAB_PAD))
                        .right(px(TAB_PAD))
                })
                .when(!vertical, |el| {
                    el.top(px(TAB_PAD))
                        .left(px(TAB_PAD + places[slot] + offset))
                });
            floating = Some(
                float
                    .opacity(0.85)
                    .bg(lighting::lit(
                        if active { fill } else { palette.field_surface },
                        0.08,
                    ))
                    .shadow(lighting::panel(dark))
                    .into_any_element(),
            );
        }
        let tab_id = tab.id.clone();
        rendered.push(
            div()
                .id(element("tab"))
                .h(px(TAB_HEIGHT))
                .flex_none()
                .when_some(uniform_width, |el, width| el.w(px(width)))
                .when(vertical, |el| el.w_full())
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .text_size(px(12.0))
                .font_weight(if active {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .text_color(crate::color::lerp(
                    palette.text_secondary,
                    palette.control_label,
                    on,
                ))
                .whitespace_nowrap()
                .relative()
                .when(vertical, |el| el.top(px(offset)))
                .when(!vertical, |el| el.left(px(offset)))
                // Until the bar has painted once there is no pill to slide,
                // so the active tab draws the fill the pill will take over.
                .when(active && pill.is_none(), |el| {
                    el.bg(lighting::lit(fill, 0.08))
                        .shadow(lighting::raised(dark))
                })
                // The copy in the flow, while the floating one is what shows.
                .when(held && floating.is_some(), |el| el.opacity(0.0))
                .when(active, |el| {
                    el.child(
                        keyboard::ring_for(&focus, CONTROL_RADIUS, palette)
                            .when_some(tab_arrows, |el, arrows| el.on_key_down(arrows)),
                    )
                })
                .when(!active, |el| {
                    el.hover(move |style| style.bg(palette.row_hover))
                })
                // Before the bar has measured itself there is nothing to
                // float, so the tab lifts in place instead.
                .when(held && floating.is_none(), |el| {
                    el.opacity(0.75).shadow(lighting::panel(dark))
                })
                .child(
                    // Reports this tab's bounds so a drag can work out which
                    // slot the pointer is over, and its width so the next
                    // frame knows where every tab will land. The bounds are
                    // where the tab is laid out, not where it is drawn: a
                    // tab mid-slide, or riding under the hand, still holds
                    // its slot, and slots are what a drag compares the
                    // pointer with.
                    canvas(
                        move |bounds, _window, cx| {
                            let mut laid_out = bounds;
                            if vertical {
                                laid_out.origin.y -= px(offset);
                            } else {
                                laid_out.origin.x -= px(offset);
                            }
                            if let Some(host) = weak.upgrade() {
                                host.update(cx, |host, _cx| {
                                    let state = host.control_state_mut();
                                    state.record_slot(id, slot, laid_out);
                                    state.record_bounds((id, "tab", &tab_id), bounds);
                                });
                            }
                        },
                        |_bounds, _state, _window, _cx| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                        let pointer = if vertical {
                            f32::from(event.position.y)
                        } else {
                            f32::from(event.position.x)
                        };
                        // Where in the tab the hand took hold, so it rides
                        // there rather than jumping to sit by its edge.
                        let grab = this
                            .control_state()
                            .slot(id, slot)
                            .map(|bounds| {
                                pointer
                                    - if vertical {
                                        f32::from(bounds.top())
                                    } else {
                                        f32::from(bounds.left())
                                    }
                            })
                            .unwrap_or(0.0);
                        this.control_state_mut().begin_tab_drag(TabDrag {
                            bar: id,
                            axis: if vertical {
                                TrackAxis::Vertical
                            } else {
                                TrackAxis::Horizontal
                            },
                            from: slot,
                            to: slot,
                            moved: false,
                            grab,
                            pressed: pointer,
                            pointer,
                        });
                        cx.notify();
                    }),
                )
                // Selection on release, not on press: pressing is also how a
                // reorder starts, and a drag must not leave a trail of
                // selections behind it.
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _event, window, cx| {
                        // Only a press on this very tab that never became a
                        // drag is a click on it. A release here after a
                        // press elsewhere (the well, a slider, the ✕) is
                        // not.
                        let clicked = this
                            .control_state()
                            .tab_drag()
                            .is_some_and(|drag| drag.bar == id && drag.from == slot && !drag.moved);
                        if clicked {
                            // Clicking a tab puts the keyboard on the bar, so
                            // Tab carries on from the tab just chosen.
                            window.focus(&click_focus, cx);
                            on_select(this, index, window, cx);
                        }
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .when(vertical || uniform_width.is_some(), |el| {
                            el.flex_1().min_w(px(0.0)).overflow_hidden().text_ellipsis()
                        })
                        .child(tab.label.clone()),
                )
                .children(tab_badge(tab, palette))
                .when(tab.closable, move |el| {
                    let on_close = on_close.clone();
                    el.child(
                        tab_close_glyph()
                            .id(element("tab-close"))
                            .hover(move |style| style.bg(palette.row_hover))
                            // A press on the ✕ is not a hold on the tab: it
                            // must neither start a reorder nor, on release,
                            // select a tab that has just been closed.
                            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                cx.stop_propagation()
                            })
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                on_close(this, index, window, cx);
                                cx.notify();
                            })),
                    )
                })
                .into_any_element(),
        );
    }
    let well_probe = {
        let weak = weak.clone();
        canvas(
            move |bounds, _window, cx| {
                if let Some(host) = weak.upgrade() {
                    host.update(cx, |host, _cx| {
                        host.control_state_mut().record_group(id, bounds);
                    });
                }
            },
            |_bounds, _state, _window, _cx| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
    };

    let axis = if vertical {
        ScrollAxis::Vertical
    } else {
        ScrollAxis::Horizontal
    };
    // Tabs leaving the viewport fade into the well rather than being cut
    // mid-glyph. The fades cover only the lane the tabs run in, inset by the
    // well's border and padding, so the recess itself carries on to the
    // edge and says there is more of it; and the well is flat, so its own
    // fill is exactly what a fade has to land on.
    //
    // No scrollbar: a strip one tab deep has no room for an overlay track
    // that would not sit on the tabs and take their presses, which start
    // reorders. The wheel, a trackpad, the arrow keys (the selection is
    // revealed) and the fades already say the strip goes on and move it.
    let fades = div()
        .absolute()
        .when(vertical, |el| {
            el.top_0()
                .bottom_0()
                .left(px(TAB_INSET))
                .right(px(TAB_INSET))
        })
        .when(!vertical, |el| {
            el.left_0()
                .right_0()
                .top(px(TAB_INSET))
                .bottom(px(TAB_INSET))
        })
        .children(scroll_fades(
            state,
            id,
            &scroll,
            axis,
            palette.field_surface,
            palette.field_surface,
        ));

    // The outer element sizes the bar and holds the fades over the
    // viewport. Horizontal wells hug their tabs; a vertical well fills the
    // width the host gave it.
    div()
        .relative()
        .flex()
        .min_w(px(0.0))
        .when(vertical || uniform_width.is_some(), |el| el.w_full())
        .when(vertical, |el| el.h_full().min_h(px(0.0)).flex_col())
        .child(
            div()
                .id(id)
                .flex()
                .flex_grow(1.0)
                .min_w(px(0.0))
                .when(vertical, |el| {
                    el.min_h(px(0.0))
                        .flex_col()
                        .overflow_y_scroll()
                        // Without this a sideways trackpad swipe scrolls
                        // the rail: GPUI maps an x-delta onto y for a
                        // container that only scrolls one way.
                        .restrict_scroll_to_axis()
                })
                // Left unrestricted on purpose: the same mapping is what
                // turns a plain mouse wheel's vertical delta into sideways
                // scrolling, and a strip of tabs has nothing else to
                // scroll.
                .when(!vertical, |el| el.overflow_x_scroll())
                .track_scroll(&scroll)
                .child(
                    div()
                        .relative()
                        .flex()
                        // The well must keep its content height. If it
                        // stretches to the viewport's height, GPUI measures
                        // no overflow and the wheel event goes to the page
                        // behind the rail.
                        .when(vertical, |el| el.w_full().flex_col().flex_none())
                        .when(!vertical, |el| el.items_center())
                        .gap(px(TAB_GAP))
                        .p(px(TAB_PAD))
                        .rounded(px(CONTROL_RADIUS))
                        .bg(palette.field_surface)
                        .border_1()
                        .border_color(palette.field_border)
                        .shadow(lighting::recessed(dark))
                        // Where the well is, for a held tab to know where
                        // its slot is on screen.
                        .child(well_probe)
                        // Under the tabs, so the active label reads through it.
                        .when_some(pill, |el, (pos, size)| {
                            el.child(
                                div()
                                    .absolute()
                                    .when(vertical, |el| {
                                        el.top(px(pos))
                                            .left(px(TAB_PAD))
                                            .right(px(TAB_PAD))
                                            .h(px(size))
                                    })
                                    .when(!vertical, |el| {
                                        el.top(px(TAB_PAD))
                                            .left(px(pos))
                                            .w(px(size))
                                            .h(px(TAB_HEIGHT))
                                    })
                                    .rounded(px(CONTROL_RADIUS))
                                    .bg(lighting::lit(palette.control_fill, 0.08))
                                    .shadow(lighting::raised(dark)),
                            )
                        })
                        .children(rendered)
                        // Last, so the tab in hand paints over every other.
                        .children(floating),
                ),
        )
        .child(fades)
}

/// A tab's face without its behaviour: what both the tab in the bar and the
/// floating copy of a tab being dragged look like.
fn tab_face(tab: &Tab, active: bool, palette: Palette, layout: TabBarLayout) -> Div {
    div()
        .h(px(TAB_HEIGHT))
        .flex_none()
        .when_some(layout.tab_width(), |el, width| el.w(px(width)))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(12.0))
        .font_weight(if active {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            palette.control_label
        } else {
            palette.text_secondary
        })
        .whitespace_nowrap()
        .child(
            div()
                .when(layout.vertical() || layout.tab_width().is_some(), |el| {
                    el.flex_1().min_w(px(0.0)).overflow_hidden().text_ellipsis()
                })
                .child(tab.label.clone()),
        )
        .children(tab_badge(tab, palette))
        .when(tab.closable, |el| el.child(tab_close_glyph()))
}

fn tab_badge(tab: &Tab, palette: Palette) -> Option<Div> {
    tab.badge.clone().map(|badge| {
        div()
            .flex_none()
            .text_size(px(10.5))
            .text_color(palette.text_secondary)
            .child(badge)
    })
}

fn tab_close_glyph() -> Div {
    div()
        .w(px(14.0))
        .h(px(14.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .text_size(px(10.0))
        .child("\u{2715}")
}

fn tab_places(sizes: &[f32], order: &[usize], gap: f32) -> Vec<f32> {
    let mut along = 0.0;
    order
        .iter()
        .map(|&index| {
            let here = along;
            along += sizes[index] + gap;
            here
        })
        .collect()
}

fn reveal_delta(view_start: f32, view_end: f32, tab_start: f32, tab_end: f32) -> f32 {
    if tab_start < view_start {
        view_start - tab_start
    } else if tab_end > view_end {
        view_end - tab_end
    } else {
        0.0
    }
}

fn reveal_tab(
    state: &ControlState,
    bar: ComboId,
    tab_id: &SharedString,
    scroll: &ScrollHandle,
    vertical: bool,
) -> bool {
    let Some(tab) = state.bounds((bar, "tab", tab_id)) else {
        return false;
    };
    let view = scroll.bounds();
    if view.size.width <= px(0.0) || view.size.height <= px(0.0) {
        return false;
    }
    let delta = if vertical {
        reveal_delta(
            f32::from(view.top()),
            f32::from(view.bottom()),
            f32::from(tab.top()),
            f32::from(tab.bottom()),
        )
    } else {
        reveal_delta(
            f32::from(view.left()),
            f32::from(view.right()),
            f32::from(tab.left()),
            f32::from(tab.right()),
        )
    };
    if delta != 0.0 {
        let mut offset = scroll.offset();
        if vertical {
            offset.y += px(delta);
        } else {
            offset.x += px(delta);
        }
        scroll.set_offset(offset);
    }
    true
}

// ---- Split divider ----------------------------------------------------------

/// Draggable divider between two panes.
///
/// It is only the handle: the host owns the fraction and sizes the panes
/// with it. Dragging reports a position along the whole split area, which
/// arrives at [`ControlHost::track_dragged`] as `x` for a vertical divider
/// and `y` for a horizontal one, clamped to `limits`.
///
/// `thickness` is the visible rule; the grab area is wider, because a 1px
/// hit target is a 1px hit target however good the rest of the UI is.
pub fn split_handle<V: ControlHost>(
    id: ComboId,
    fraction: f32,
    vertical: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    const GRAB: f32 = 9.0;
    let dragging = view.control_state().is_dragging(id);
    let axis = if vertical {
        TrackAxis::Horizontal
    } else {
        TrackAxis::Vertical
    };
    let mut rule: gpui::Hsla = crate::color::to_hsla(palette.field_border);
    if dragging {
        rule = crate::color::to_hsla(palette.accent);
    }

    let orientation = if vertical {
        Orientation::Horizontal
    } else {
        Orientation::Vertical
    };

    div()
        .id(id)
        .relative()
        .flex_none()
        .when(vertical, |el| el.w(px(GRAB)).h_full().cursor_col_resize())
        .when(!vertical, |el| el.h(px(GRAB)).w_full().cursor_row_resize())
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(2.0))
        // A divider you can only move with the mouse is a divider a
        // keyboard user cannot move at all, and the panes either side of
        // it are content they may need to see.
        .child(
            keyboard::ring(ElementId::Name(format!("{id}-ring").into()), 2.0, palette).on_key_down(
                cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                    let Some(key) = keyboard::key(event) else {
                        return;
                    };
                    if let Some(moved) = keyboard::nudge(key, orientation, fraction, 0.02) {
                        cx.stop_propagation();
                        let at = if vertical {
                            point(moved, 0.0)
                        } else {
                            point(0.0, moved)
                        };
                        this.track_dragged(id, at, cx);
                        cx.notify();
                    }
                }),
            ),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.control_state_mut().begin_track_drag(id, axis, None);
                if let Some((_, at)) = this.control_state().track_ratio_at(event.position) {
                    this.track_dragged(id, at, cx);
                }
                cx.notify();
            }),
        )
        .child(
            div()
                .when(vertical, |el| el.w(px(1.0)).h_full())
                .when(!vertical, |el| el.h(px(1.0)).w_full())
                .bg(rule),
        )
}

/// The invisible probe a split's container needs, so the divider can turn a
/// pointer position into a fraction of the whole area. Put it inside the
/// element the two panes share, as an absolutely positioned child.
pub fn split_area<V: ControlHost>(id: ComboId, cx: &mut Context<V>) -> impl IntoElement {
    crate::controls::track_probe::<V>(id, cx.entity().downgrade())
}

// ---- Collapsible section ----------------------------------------------------

/// Disclosure section: a header that turns its chevron and reveals the body.
///
/// The host owns `expanded`, because whether a section is open usually
/// outlives the view and belongs with the rest of its settings. The turn of
/// the chevron is animated from [`ControlState`].
#[allow(clippy::too_many_arguments)]
pub fn collapsible<V: ControlHost>(
    id: &'static str,
    title: &str,
    expanded: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    body: impl IntoElement,
    on_toggle: impl Fn(&mut V, bool, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    // Noticed from the value rather than announced by the click, so a
    // section opened by a shortcut or by the host's own code turns its
    // chevron exactly as a click does. The chevron turns from where it was,
    // so a toggle mid-turn reverses smoothly instead of snapping to the
    // other end first.
    let turned = view
        .control_state()
        .blend((id, "disclosure"), expanded, SWITCH_SLIDE);
    // The body's full height, from the last time it painted open, so it can
    // be shown growing to that height rather than appearing at it. Unknown
    // the first time it opens, when it fades in instead.
    let body_key = Tag::new((id, "body"));
    let body_height = view
        .control_state()
        .measured(body_key)
        .map(|bounds| f32::from(bounds.size.height));
    let weak = cx.entity().downgrade();
    let chevron: gpui::Hsla = crate::color::to_hsla(palette.text_secondary);
    let on_toggle = Rc::new(on_toggle);
    let on_toggle_key = on_toggle.clone();
    let header_ring = ElementId::Name(format!("{id}-header-ring").into());

    div()
        .flex()
        .flex_col()
        .child(
            div()
                .id(ElementId::Name(format!("{id}-header").into()))
                .relative()
                .h(px(28.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.text_primary)
                .rounded(px(5.0))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_toggle(this, !expanded, window, cx);
                    cx.notify();
                }))
                .child(
                    div().w(px(12.0)).h(px(12.0)).flex_none().child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, _cx| {
                                // Drawn rather than rotated: gpui has no
                                // transform, so the chevron is rebuilt each
                                // frame at the angle it has reached.
                                let origin = bounds.origin;
                                let angle = turned * std::f32::consts::FRAC_PI_2;
                                let (sin, cos) = angle.sin_cos();
                                let centre = (6.0f32, 6.0f32);
                                let rotate = |x: f32, y: f32| {
                                    point(
                                        origin.x + px(centre.0 + x * cos - y * sin),
                                        origin.y + px(centre.1 + x * sin + y * cos),
                                    )
                                };
                                let mut builder = PathBuilder::stroke(px(1.5));
                                builder.move_to(rotate(-1.8, -3.6));
                                builder.line_to(rotate(1.8, 0.0));
                                builder.line_to(rotate(-1.8, 3.6));
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, chevron);
                                }
                            },
                        )
                        .size_full(),
                    ),
                )
                .child(SharedString::from(title.to_string()))
                // Last, so the ring paints over the chevron and the title. A
                // disclosure has no fill of its own; the ring is the whole of
                // how it shows it has the keyboard.
                .child(
                    keyboard::ring(header_ring, 5.0, palette).on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            let Some(key) = keyboard::key(event) else {
                                return;
                            };
                            // Left closes and right opens, as well as Space
                            // toggling: a disclosure is the one control where
                            // the arrows have an unambiguous meaning, and a
                            // reader who tries them should not have to guess.
                            let wanted = match key {
                                Key::Activate => !expanded,
                                Key::Left => false,
                                Key::Right => true,
                                _ => return,
                            };
                            cx.stop_propagation();
                            if wanted != expanded {
                                on_toggle_key(this, wanted, window, cx);
                                cx.notify();
                            }
                        },
                    )),
                ),
        )
        // The body unfolds under the header as the chevron turns, and folds
        // back up the same way: its full height is known from the last time
        // it was open, so the section grows to it and what is below moves
        // down with it. It fades as it goes, so half a line of text is never
        // the only thing showing.
        .when(turned > 0.01, |el| {
            el.child(
                div()
                    .overflow_hidden()
                    .when_some(body_height, |el, height| el.h(px(height * turned)))
                    .child(
                        div()
                            .relative()
                            .pl(px(18.0))
                            .pb(px(4.0))
                            .opacity(turned)
                            .child(crate::controls::measure_probe(body_key, weak))
                            .child(body),
                    ),
            )
        })
}

// ---- Modal dialog -----------------------------------------------------------

/// One button in a dialog's footer.
pub struct DialogButton<V> {
    pub id: ElementId,
    pub label: SharedString,
    pub variant: ButtonVariant,
    pub enabled: bool,
    #[allow(clippy::type_complexity)]
    pub on_click: Box<dyn Fn(&mut V, &mut Window, &mut Context<V>) + 'static>,
}

impl<V: ControlHost> DialogButton<V> {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        variant: ButtonVariant,
        on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant,
            enabled: true,
            on_click: Box::new(on_click),
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Modal dialog: a scrim over the view and a centred panel with a title, a
/// body and a row of buttons.
///
/// Open it with [`ControlState::open_dialog`]; while `id` is not open this
/// returns nothing. It fades in, takes the keyboard on its primary button —
/// failing one, the first — and keeps Tab among its buttons. A button,
/// Enter, Escape or a press on the scrim hands the keyboard back where it
/// was, runs the button's callback or `on_dismiss`, and closes the dialog,
/// which fades out and is then gone. A callback that opens a dialog of its
/// own keeps that one.
///
/// Buttons go last-is-rightmost, so the confirming one belongs at the end.
#[allow(clippy::too_many_arguments)]
pub fn dialog<V: ControlHost>(
    id: ComboId,
    title: &str,
    width: f32,
    ctx: WidgetContext<'_, '_, '_, V>,
    body: impl IntoElement,
    buttons: Vec<DialogButton<V>>,
    on_dismiss: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Option<impl IntoElement> {
    let WidgetContext { palette, view, cx } = ctx;
    let state = view.control_state();
    let Some((opacity, _)) = state.dialog_fade(id) else {
        state.clear_dialog_trap(id);
        return None;
    };
    let closing = !state.is_dialog_open(id);
    // Identifies this opening, so a button can close the dialog it was in
    // without closing one its callback opened in its place.
    let opening = state.dialog.as_ref().map(|dialog| dialog.opened_at);
    let dark = palette.is_dark;
    let opacity = opacity.clamp(0.0, 1.0);

    // One handle per button, owned here rather than by the buttons, so the
    // dialog can put the keyboard on its default action when it opens and
    // keep Tab cycling among its own buttons while it is up. A modal that
    // lets the keyboard wander off behind its own scrim has stopped being
    // modal.
    let handles: Vec<FocusHandle> = (0..buttons.len())
        .map(|n| state.dialog_button_focus(id, n, cx))
        .collect();
    // The default action is the primary button. Failing one, the first — by
    // convention the safe one, which is the right thing for Enter to do in
    // a dialog that is asking about something destructive.
    let default = dialog_default(buttons.iter().map(|spec| (spec.variant, spec.enabled)));
    let panel_focus = state.focus((id, "panel"), cx).tab_stop(false);
    // Tab is bound to an action and handled at the root; this is how
    // `keyboard::move_focus` finds out there is a dialog to stay inside. A
    // dialog on its way out is not one to stay inside of.
    if closing {
        state.clear_dialog_trap(id);
    } else {
        state.set_dialog_buttons(
            id,
            buttons
                .iter()
                .enumerate()
                .filter_map(|(index, spec)| spec.enabled.then_some(index))
                .collect(),
        );
    }

    let on_dismiss: Press<V> = Rc::new(on_dismiss);
    let dismiss_from_scrim = on_dismiss.clone();
    let dismiss_from_key = on_dismiss.clone();

    let mut footer: Vec<AnyElement> = Vec::with_capacity(buttons.len());
    let mut presses: Vec<Press<V>> = Vec::with_capacity(buttons.len());
    for (n, spec) in buttons.into_iter().enumerate() {
        let cx: &mut Context<V> = &mut *cx;
        let press: Press<V> = Rc::from(spec.on_click);
        presses.push(press.clone());
        footer.push(
            div()
                .flex_none()
                .min_w(px(88.0))
                .child(button_focused(
                    spec.id,
                    &spec.label,
                    spec.variant,
                    spec.enabled && !closing,
                    &handles[n],
                    palette,
                    cx,
                    move |host, window, cx| finish(host, opening, window, cx, &*press),
                ))
                .into_any_element(),
        );
    }

    let weak = cx.entity().downgrade();
    let grab_panel = panel_focus.clone();
    let grab_default = default.and_then(|index| handles.get(index)).cloned();
    let enter_press = default.and_then(|index| presses.get(index)).cloned();

    Some(
        deferred(
            div()
                .id(ElementId::Name(format!("{id}-scrim").into()))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(crate::color::with_alpha(
                    palette.scrim(),
                    palette.scrim().alpha * opacity,
                ))
                .occlude()
                // A press on the scrim dismisses. A press inside the panel
                // must not, so the panel occludes in its own right.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                        if closing {
                            return;
                        }
                        finish(this, opening, window, cx, &*dismiss_from_scrim);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id(ElementId::Name(format!("{id}-panel").into()))
                        .track_focus(&panel_focus)
                        .w(px(width))
                        .max_h(gpui::relative(0.86))
                        .opacity(opacity)
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .p(px(18.0))
                        .rounded(px(10.0))
                        .bg(if dark {
                            palette.soft_fill
                        } else {
                            palette.field_surface
                        })
                        .border_1()
                        .border_color(palette.field_border)
                        .shadow(lighting::faded(lighting::panel(dark), opacity))
                        .occlude()
                        // Enter is the default action — unless a button has
                        // the keyboard, in which case it is that button's,
                        // and its ring has already handled it before this
                        // runs. Tab and Escape are not here: both are bound
                        // to actions and never arrive as keys. Tab goes
                        // through `keyboard::move_focus`; Escape arrives as
                        // `Dismiss`, below.
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key != "enter"
                                || keyboard::key(event) != Some(Key::Activate)
                                || closing
                            {
                                return;
                            }
                            let Some(press) = enter_press.clone() else {
                                return;
                            };
                            cx.stop_propagation();
                            finish(this, opening, window, cx, &*press);
                            cx.notify();
                        }))
                        // A dialog on its way out has already let go of the
                        // keyboard, and leaves Escape to whoever has it now.
                        .on_action(cx.listener(move |this, _: &Dismiss, window, cx| {
                            if closing {
                                cx.propagate();
                                return;
                            }
                            finish(this, opening, window, cx, &*dismiss_from_key);
                            cx.notify();
                        }))
                        .child(
                            // Prepaint is the first moment there is a
                            // `Window` to focus with. Runs while the keyboard
                            // is anywhere but inside the dialog, which is the
                            // frame it opens on — and any later frame the
                            // host has pulled focus out, which is a bug this
                            // quietly corrects.
                            canvas(
                                move |_bounds, window, cx| {
                                    // A closing dialog keeps rendering while
                                    // it fades. The keyboard has already
                                    // been handed back by then, and pulling
                                    // it into a panel about to disappear
                                    // would strand it there.
                                    if closing || grab_panel.contains_focused(window, cx) {
                                        return;
                                    }
                                    if let Some(host) = weak.upgrade() {
                                        host.update(cx, |host, cx| {
                                            let state = host.control_state_mut();
                                            state.restore_menu_focus(window, cx);
                                            if state.dialog_return_focus.is_none() {
                                                state.dialog_return_focus = window.focused(cx);
                                            }
                                        });
                                    }
                                    match &grab_default {
                                        Some(handle) => window.focus(handle, cx),
                                        None => window.focus(&grab_panel, cx),
                                    }
                                },
                                |_bounds, _state, _window, _cx| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.text_primary)
                                .child(SharedString::from(title.to_string())),
                        )
                        .child(div().flex_1().overflow_hidden().child(body))
                        .child(div().flex().justify_end().gap(px(8.0)).children(footer)),
                ),
        )
        .with_priority(150),
    )
}

fn dialog_default(buttons: impl IntoIterator<Item = (ButtonVariant, bool)>) -> Option<usize> {
    let mut first = None;
    for (index, (variant, enabled)) in buttons.into_iter().enumerate() {
        if enabled {
            first.get_or_insert(index);
            if variant == ButtonVariant::Primary {
                return Some(index);
            }
        }
    }
    first
}

/// What every way out of a dialog does, in the order it has to happen in:
/// the keyboard goes back first, so a host that opens something else from
/// here starts from the right place; then the host acts; then the dialog
/// closes — unless the host's action opened a dialog of its own, which a
/// "Rename" that asks a second question is entitled to do.
fn finish<V: ControlHost>(
    host: &mut V,
    opening: Option<Instant>,
    window: &mut Window,
    cx: &mut Context<V>,
    act: &dyn Fn(&mut V, &mut Window, &mut Context<V>),
) {
    host.control_state_mut().restore_dialog_focus(window, cx);
    act(host, window, cx);
    let state = host.control_state_mut();
    if state.dialog.as_ref().map(|dialog| dialog.opened_at) == opening {
        state.close_dialog();
    }
}

type Press<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// How long a dialog takes to arrive and to leave, re-exported so a host can
/// drive its own timers off the same numbers.
pub const DIALOG_ENTER: Duration = crate::easing::MODAL_ENTER;
pub const DIALOG_EXIT: Duration = crate::easing::MODAL_EXIT;

// ---- Surfaces and layout ----------------------------------------------------

/// A card: the lit panel everything else sits on, with a caption naming
/// what is in it. One level of these on the window's ground is the whole
/// hierarchy a page needs; cards inside cards are on the list of patterns
/// the design guide rejects.
pub fn card(palette: Palette, title: &str, body: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .flex_none()
        .p(px(16.0))
        .rounded(px(10.0))
        .bg(lighting::lit(palette.area_surface, 0.04))
        .border_1()
        .border_color(palette.area_border)
        .shadow(lighting::panel(palette.is_dark))
        .child(caption(palette, title))
        .child(body)
}

/// A caption over a control, as a column, so a row of labelled controls
/// lines up along their captions and along their controls.
pub fn labelled(palette: Palette, label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(caption(palette, label))
        .child(control)
}

/// A row of controls, centred on one another and spaced the toolkit's way.
///
/// `flex_none`, because a `div` is a flex item as well as a flex container:
/// a row that could shrink would be squeezed to fit its parent instead of
/// scrolling out of it.
pub fn row() -> Div {
    div().flex().flex_none().items_center().gap(px(10.0))
}

/// A column of rows or cards, spaced the toolkit's way. `flex_none` for the
/// same reason as [`row`].
pub fn column() -> Div {
    div().flex().flex_none().flex_col().gap(px(14.0))
}

/// Content arriving: a page, a panel, a step of a wizard. It fades in and
/// settles up into place over [`MOVE`] each time `key` changes, and paints
/// at rest the first time it is seen. Key it by what is shown — the page,
/// not the tab's position — so reordering the tabs is not a page change.
pub fn arriving(
    id: &'static str,
    key: u64,
    state: &ControlState,
    content: impl IntoElement,
) -> Div {
    let arrived = crate::easing::ease_out_cubic(state.transition(id, key, MOVE));
    div()
        .opacity(arrived)
        .relative()
        .top(px(8.0 * (1.0 - arrived)))
        .child(content)
}

/// A scrolling area on the window's ground, with the edges and the bar a
/// scrolling area needs: content leaving an edge fades into the ground
/// rather than being cut off against it, and an overlay [`scrollbar`]
/// appears when the content stops fitting.
///
/// The ground is a lit gradient, so each fade lands on whatever the ground
/// is at its own height — a single colour would show as a band. Size the
/// result as you would any element; it fills its slot with the scroller,
/// and `content` scrolls inside that.
///
/// ```ignore
/// scroll_area("page", &self.scroll, ScrollAxis::Vertical, WidgetContext::new(palette, self, cx), window, body)
///     .flex_1()
/// ```
#[allow(clippy::too_many_arguments)]
pub fn scroll_area<V: ControlHost>(
    id: &'static str,
    handle: &ScrollHandle,
    axis: ScrollAxis,
    ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    content: impl IntoElement,
) -> Div {
    let palette = ctx.palette;
    let window_height = f32::from(window.viewport_size().height).max(1.0);
    let bounds = handle.bounds();
    let ground_at =
        |y: gpui::Pixels| lighting::ground_at(palette.backdrop, f32::from(y) / window_height);
    let (start, end) = match axis {
        ScrollAxis::Vertical => (ground_at(bounds.top()), ground_at(bounds.bottom())),
        // A fade down the side spans the area's height. It cannot follow the
        // gradient, so it takes the ground at the area's middle.
        ScrollAxis::Horizontal => {
            let middle = ground_at(bounds.center().y);
            (middle, middle)
        }
    };
    div()
        .relative()
        .overflow_hidden()
        .child(
            div()
                .id(id)
                .size_full()
                .flex()
                .flex_col()
                .when(axis == ScrollAxis::Vertical, |el| el.overflow_y_scroll())
                .when(axis == ScrollAxis::Horizontal, |el| el.overflow_x_scroll())
                // Without this a sideways trackpad swipe scrolls a vertical
                // area: GPUI maps an x-delta onto y for a container that only
                // scrolls one way.
                .restrict_scroll_to_axis()
                .track_scroll(handle)
                .child(content),
        )
        .children(scroll_fades(ctx.state(), id, handle, axis, start, end))
        .child(scrollbar(id, handle, axis, ctx))
}

/// Moves `items[from]` to `to`, and moves `selected` with the item it was
/// on: what a host does with [`ControlHost::tabs_reordered`].
///
/// ```ignore
/// fn tabs_reordered(&mut self, bar: ComboId, from: usize, to: usize, cx: &mut Context<Self>) {
///     match bar {
///         "documents" => reorder(&mut self.documents, &mut self.current, from, to),
///         _ => return,
///     }
///     cx.notify();
/// }
/// ```
pub fn reorder<T>(items: &mut Vec<T>, selected: &mut usize, from: usize, to: usize) {
    if from >= items.len() || to >= items.len() {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
    *selected = moved_selection(*selected, from, to);
}

/// Where `selected` ends up after the item at `from` is dropped at `to`.
fn moved_selection(selected: usize, from: usize, to: usize) -> usize {
    if selected == from {
        to
    } else if from < selected && selected <= to {
        selected - 1
    } else if to <= selected && selected < from {
        selected + 1
    } else {
        selected
    }
}

#[cfg(test)]
mod layout_tests {
    use super::{
        ButtonVariant, TabBarLayout, dialog_default, moved_selection, reorder, reveal_delta,
        tab_places,
    };

    #[test]
    fn uniform_tabs_keep_the_next_close_target_in_the_closed_slot() {
        let width = TabBarLayout::HorizontalUniform { width: 158.0 }
            .tab_width()
            .unwrap();
        let before = tab_places(&[width; 4], &[0, 1, 2, 3], 2.0);
        let after = tab_places(&[width; 3], &[0, 1, 2], 2.0);
        assert_eq!(before[1], after[1]);
        assert_eq!(before[1], 160.0);
        assert_eq!(before[2] - before[1], width + 2.0);
    }

    #[test]
    fn constrained_tab_widths_stay_usable() {
        assert_eq!(
            TabBarLayout::HorizontalUniform { width: 12.0 }.tab_width(),
            Some(96.0)
        );
        assert_eq!(
            TabBarLayout::HorizontalUniform { width: 900.0 }.tab_width(),
            Some(320.0)
        );
        assert_eq!(
            TabBarLayout::HorizontalUniform { width: f32::NAN }.tab_width(),
            Some(160.0)
        );
    }

    #[test]
    fn arrow_selection_reveals_only_the_hidden_part_of_a_tab() {
        assert_eq!(reveal_delta(20.0, 120.0, 40.0, 100.0), 0.0);
        assert_eq!(reveal_delta(20.0, 120.0, 8.0, 68.0), 12.0);
        assert_eq!(reveal_delta(20.0, 120.0, 75.0, 146.0), -26.0);
    }

    #[test]
    fn a_dialog_only_defaults_to_an_enabled_button() {
        use ButtonVariant::{Primary, Soft};
        assert_eq!(dialog_default([(Soft, true), (Primary, true)]), Some(1));
        assert_eq!(dialog_default([(Soft, true), (Primary, false)]), Some(0));
        assert_eq!(dialog_default([(Soft, false), (Soft, true)]), Some(1));
        assert_eq!(dialog_default([(Primary, false)]), None);
        assert_eq!(dialog_default([]), None);
    }

    /// The selection stays on the item it was on, wherever that item goes.
    #[test]
    fn a_reorder_keeps_the_selection_on_its_item() {
        let mut items = vec!["a", "b", "c", "d"];
        let mut selected = 2; // "c"
        reorder(&mut items, &mut selected, 0, 3); // "a" to the end
        assert_eq!(items, ["b", "c", "d", "a"]);
        assert_eq!(items[selected], "c");
        reorder(&mut items, &mut selected, 1, 0); // "c" itself to the front
        assert_eq!(items, ["c", "b", "d", "a"]);
        assert_eq!(selected, 0);
        // Out of range: nothing happens.
        reorder(&mut items, &mut selected, 9, 0);
        assert_eq!(items, ["c", "b", "d", "a"]);
    }

    #[test]
    fn a_selection_outside_the_moved_span_does_not_move() {
        assert_eq!(moved_selection(0, 2, 3), 0);
        assert_eq!(moved_selection(3, 0, 1), 3);
        assert_eq!(moved_selection(2, 0, 3), 1);
        assert_eq!(moved_selection(1, 3, 0), 2);
    }
}
