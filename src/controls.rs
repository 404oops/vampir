//! The controls: buttons, a switch, a spin box, text field and area, pop-up
//! menus, sliders and overlay scrollbars.
//!
//! Every one is a free function generic over the host view, taking a
//! [`Palette`] for colour and a `Context<V>` for its callbacks, and
//! returning an element. Nothing is a struct with a builder, because
//! nothing here holds state between frames beyond what [`ControlState`]
//! already holds.

use gpui::{
    Anchor, Context, Div, ElementId, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, PathBuilder, ScrollHandle, SharedString, Window, anchored, canvas, deferred,
    div, point, prelude::*, px,
};
use std::rc::Rc;

use crate::easing::{ease_out_cubic, lerp_f32};
use crate::keyboard::{self, Dismiss, Key, Orientation};
use crate::lighting;
use crate::menu::VIEWPORT_MARGIN;
use crate::overlay::Hint;
use crate::palette::Palette;
use crate::scroll::{SCROLLBAR_THICKNESS, ScrollAxis, ScrollDrag, THUMB_THICKNESS};
use crate::state::{ComboId, ControlHost, ControlState, MOVE, SWITCH_SLIDE, Tag, TrackAxis};
use crate::text_input::TextInput;

/// What a control that animates is handed besides its own data: the palette
/// to draw with, the host it reads its state from, and the GPUI context it
/// makes listeners with. One parameter where there were three, built for
/// the call with [`WidgetContext::new`] and never held between frames.
///
/// A control that draws other controls hands each of them a
/// [`WidgetContext::reborrow`]: the context holds the `&mut` GPUI context,
/// so it cannot be given away twice.
pub struct WidgetContext<'v, 'c, 'app, V: ControlHost + 'v> {
    pub palette: Palette,
    pub view: &'v V,
    pub cx: &'c mut Context<'app, V>,
}

impl<'v, 'c, 'app, V: ControlHost> WidgetContext<'v, 'c, 'app, V> {
    pub fn new(palette: Palette, view: &'v V, cx: &'c mut Context<'app, V>) -> Self {
        Self { palette, view, cx }
    }

    /// The host's control state, borrowed for as long as the host is rather
    /// than for as long as this context is, so a control can hold it while
    /// it uses `cx`.
    pub fn state(&self) -> &'v ControlState {
        self.view.control_state()
    }

    /// The same context, lent out for one more control.
    pub fn reborrow(&mut self) -> WidgetContext<'v, '_, 'app, V> {
        WidgetContext {
            palette: self.palette,
            view: self.view,
            cx: &mut *self.cx,
        }
    }
}

/// Shared metrics: every field, pop-up and button is this tall and this
/// round, so a row of mixed controls lines up without per-control tuning.
pub const CONTROL_HEIGHT: f32 = 30.0;
pub const CONTROL_RADIUS: f32 = 6.0;

// ---- Repeated visual constants -----------------------------------------------

pub const DISABLED_OPACITY: f32 = 0.5;

const PRESS_SHADE_DELTA: f32 = -0.06;

const CAPTION_TEXT_SIZE: f32 = 12.0;
const BUTTON_TEXT_SIZE: f32 = 12.5;
const BUTTON_PADDING_H: f32 = 12.0;

const SWITCH_TRACK_H: f32 = 26.0;
const SWITCH_TRACK_RADIUS: f32 = 13.0;
const SWITCH_LABEL_TEXT_SIZE: f32 = 12.5;
const SWITCH_TRACK_DISABLED: f32 = 0.45;
const SWITCH_TRACK_OPACITY_DARK: f32 = 0.5;
const SWITCH_TRACK_OPACITY_LIGHT: f32 = 0.58;

const STEP_BUTTON_SIZE: f32 = 22.0;
const STEP_BUTTON_RADIUS: f32 = 3.0;
const STEP_BUTTON_TEXT_SIZE: f32 = 13.0;
const SPINBOX_WIDTH: f32 = 118.0;
const SPINBOX_PAD_H: f32 = 4.0;
const SPINBOX_TEXT_SIZE: f32 = 12.0;

const FOCUSED_GLOW_OPACITY: f32 = 0.35;
const FOCUSED_GLOW_BLUR: f32 = 3.0;

const TEXT_FIELD_PAD_H: f32 = 9.0;

const TEXT_AREA_MIN_H: f32 = 40.0;
const TEXT_AREA_LINE_HEIGHT: f32 = 17.0;
const TEXT_AREA_PAD_H: f32 = 10.0;
const TEXT_AREA_PAD_V: f32 = 8.0;

const COMBO_PAD_L: f32 = 10.0;
const COMBO_PAD_R: f32 = 28.0;
const COMBO_TEXT_SIZE: f32 = 12.5;
const COMBO_ROW_PAD_H: f32 = 9.0;
const COMBO_BODY_LIT: f32 = 0.05;
const COMBO_ROW_SELECTED_LIT: f32 = 0.08;

const CHEVRON_STROKE_W: f32 = 1.5;
const CHEVRON_CHIP_INSET_R: f32 = 5.0;
const CHEVRON_CHIP_SIZE: f32 = 18.0;
const CHEVRON_CHIP_LIT: f32 = 0.12;

const SPIN_BUTTON_DISABLED: f32 = 0.45;

// ---- Captions ---------------------------------------------------------------

/// Section label: a quiet, medium-weight line that names a region without
/// competing with the content in it.
pub fn caption(palette: Palette, text: &str) -> impl IntoElement {
    div()
        .flex_none()
        .text_size(px(CAPTION_TEXT_SIZE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(palette.text_secondary)
        .whitespace_nowrap()
        .child(SharedString::from(text))
}

/// Text that fades its new words in when it changes — a status line, a
/// "last action", a count. Words cannot be interpolated, so the change of
/// value is what is noticed, and the new text arrives over the old rather
/// than replacing it mid-glance. Style it as any `div`: colour, size.
pub fn fading_text(id: &'static str, text: impl Into<SharedString>, state: &ControlState) -> Div {
    use std::hash::{Hash, Hasher};
    let text = text.into();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let arrived = ease_out_cubic(state.transition(id, hasher.finish(), SWITCH_SLIDE));
    div().opacity(arrived).child(text)
}

// ---- Buttons ----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonVariant {
    /// Tinted secondary button. Most buttons are this.
    Soft,
    /// The single call to action in a group.
    Primary,
    /// Destructive.
    Danger,
}

impl ButtonVariant {
    /// (resting fill, hover fill, label).
    fn colors(self, palette: Palette) -> (gpui::Rgba, gpui::Rgba, gpui::Rgba) {
        match self {
            ButtonVariant::Primary => (
                palette.primary_fill,
                palette.primary_fill_hover,
                palette.primary_label,
            ),
            ButtonVariant::Danger => (
                palette.danger_fill,
                palette.danger_fill_hover,
                palette.danger_label,
            ),
            ButtonVariant::Soft => (
                palette.soft_fill,
                palette.soft_fill_hover,
                palette.soft_label,
            ),
        }
    }
}

/// Button lit from above: gradient fill, a highlight along the top edge, a
/// darker rim and a soft shadow. Fills its width, so put it in a sized slot.
pub fn button<V: ControlHost>(
    id: impl Into<ElementId>,
    text: &str,
    variant: ButtonVariant,
    enabled: bool,
    palette: Palette,
    cx: &mut Context<V>,
    on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    build_button(
        id.into(),
        text,
        variant,
        enabled,
        None,
        palette,
        cx,
        Rc::new(on_click),
    )
}

/// A [`button`] whose focus handle the caller owns.
///
/// For a button something else needs to reach: a dialog putting the
/// keyboard on its default action when it opens, or cycling Tab among its
/// own buttons. An ordinary button keeps its handle to itself, which is what
/// lets it be a plain function of its inputs.
#[allow(clippy::too_many_arguments)]
pub fn button_focused<V: ControlHost>(
    id: impl Into<ElementId>,
    text: &str,
    variant: ButtonVariant,
    enabled: bool,
    focus: &FocusHandle,
    palette: Palette,
    cx: &mut Context<V>,
    on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    build_button(
        id.into(),
        text,
        variant,
        enabled,
        Some(focus),
        palette,
        cx,
        Rc::new(on_click),
    )
}

type Press<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

#[allow(clippy::too_many_arguments)]
fn build_button<V: ControlHost>(
    id: ElementId,
    text: &str,
    variant: ButtonVariant,
    enabled: bool,
    focus: Option<&FocusHandle>,
    palette: Palette,
    cx: &mut Context<V>,
    on_click: Press<V>,
) -> impl IntoElement {
    let dark = palette.is_dark;
    let (fill, fill_hover, label_color) = variant.colors(palette);
    let label: SharedString = SharedString::from(text);

    // The click and the key press are one action arriving two ways, and
    // must not become two implementations of it.
    let pressed = on_click.clone();
    let activate = cx.listener(move |this, event: &KeyDownEvent, window, cx| {
        if keyboard::key(event) == Some(Key::Activate) {
            cx.stop_propagation();
            pressed(this, window, cx);
            cx.notify();
        }
    });
    let ring: gpui::AnyElement = match focus {
        Some(focus) => keyboard::ring_for(focus, CONTROL_RADIUS, palette)
            .on_key_down(activate)
            .into_any_element(),
        None => keyboard::ring(
            ElementId::Name(format!("{id}-ring").into()),
            CONTROL_RADIUS,
            palette,
        )
        .on_key_down(activate)
        .into_any_element(),
    };

    div()
        .id(id)
        .h(px(CONTROL_HEIGHT))
        .w_full()
        .px(px(BUTTON_PADDING_H))
        .flex()
        .items_center()
        .justify_center()
        .relative()
        .rounded(px(CONTROL_RADIUS))
        .bg(lighting::lit(fill, 0.08))
        .border_1()
        .border_color(lighting::rim(fill, dark))
        .shadow(lighting::raised(dark))
        .text_size(px(BUTTON_TEXT_SIZE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(label_color)
        .whitespace_nowrap()
        .overflow_hidden()
        .when(!enabled, |el| el.opacity(DISABLED_OPACITY))
        .when(enabled, move |el| {
            el.cursor_pointer()
                .hover(move |style| style.bg(lighting::lit(fill_hover, 0.1)))
                // Pressed: no gradient at all, so the control reads as
                // Pushed flat into the surface rather than merely darker.
                .active(move |style| {
                    style.bg(lighting::lit(lighting::shade(fill, PRESS_SHADE_DELTA), 0.0))
                })
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_click(this, window, cx);
                }))
        })
        .child(label)
        // Last, so it paints over the label as well as the fill.
        .when(enabled, |el| el.child(ring))
}

// ---- Switch -----------------------------------------------------------------

/// Toggle switch.
///
/// The track colour and knob position cross over [`SWITCH_SLIDE`] with
/// [`ease_out_cubic`]. The instant of the toggle is recorded in
/// [`crate::ControlState`], so a first paint or a remount shows the end
/// state without animating: only a real toggle slides.
#[allow(clippy::too_many_arguments)]
pub fn switch<V: ControlHost>(
    id: impl Into<ElementId>,
    checked: bool,
    label: Option<&str>,
    enabled: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_toggle: impl Fn(&mut V, bool, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    const KNOB: f32 = 20.0;
    const TRACK_W: f32 = 46.0;
    const PAD: f32 = 3.0;
    let left_off = PAD;
    let left_on = TRACK_W - KNOB - PAD;

    let palette = ctx.palette;
    let id: ElementId = id.into();
    let track_off = palette.field_border;
    let track_on = palette.accent;
    let track_target = if checked { track_on } else { track_off };
    let track_source = if checked { track_off } else { track_on };
    let left_target = if checked { left_on } else { left_off };
    let left_source = if checked { left_off } else { left_on };
    // The on state is the accent at reduced strength: at full saturation a
    // switch shouts louder than the setting it controls.
    let track_opacity = if !enabled {
        SWITCH_TRACK_DISABLED
    } else if checked {
        if palette.is_dark {
            SWITCH_TRACK_OPACITY_DARK
        } else {
            SWITCH_TRACK_OPACITY_LIGHT
        }
    } else {
        1.0
    };

    // The change is noticed here, from the value, rather than announced by
    // the click handler — so a switch flipped from a menu, a shortcut or the
    // command palette slides exactly as one flipped by the pointer does.
    let transition = ctx
        .state()
        .transition(&id, u64::from(checked), SWITCH_SLIDE);
    let eased = ease_out_cubic(transition);
    let cx: &mut Context<V> = &mut *ctx.cx;
    let track_color = crate::color::lerp(track_source, track_target, eased);
    let left = lerp_f32(left_source, left_target, eased);

    let ring_id = ElementId::Name(format!("{id}-ring").into());
    let label: Option<SharedString> = label.map(std::convert::Into::into);
    let on_toggle = Rc::new(on_toggle);
    let toggled = on_toggle.clone();

    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(7.0))
        .flex_none()
        // The label is part of the hit target, as it is on a checkbox. A
        // switch that only answers to its 46px track is one most people
        // miss on the first try.
        .when(enabled, |el| {
            el.cursor_pointer()
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_toggle(this, !checked, window, cx);
                    cx.notify();
                }))
        })
        .child(
            div()
                .w(px(TRACK_W))
                .h(px(SWITCH_TRACK_H))
                .flex_none()
                .relative()
                // The track is the switch's fill, and it lives here rather
                // than on a child so the ring has something solid to sit
                // around.
                .rounded(px(SWITCH_TRACK_RADIUS))
                .bg(crate::color::with_alpha(track_color, track_opacity))
                .child(
                    div()
                        .absolute()
                        .top(px(PAD))
                        .left(px(left))
                        .w(px(KNOB))
                        .h(px(KNOB))
                        .rounded_full()
                        .bg(palette.field_surface)
                        .border_1()
                        .border_color(palette.field_border_strong),
                )
                .when(enabled, |el| {
                    el.child(
                        keyboard::ring(ring_id, SWITCH_TRACK_RADIUS, palette).on_key_down(
                            ctx.cx
                                .listener(move |this, event: &KeyDownEvent, window, cx| {
                                    if keyboard::key(event) == Some(Key::Activate) {
                                        cx.stop_propagation();
                                        toggled(this, !checked, window, cx);
                                        cx.notify();
                                    }
                                }),
                        ),
                    )
                }),
        )
        .children(label.map(|label| {
            div()
                .text_size(px(SWITCH_LABEL_TEXT_SIZE))
                .text_color(palette.text_primary)
                .whitespace_nowrap()
                .when(!enabled, |el| el.opacity(DISABLED_OPACITY))
                .child(label)
        }))
}

// ---- Spin box ---------------------------------------------------------------

/// Spin box: an editable value field between decrement and increment
/// buttons. The value is a real text input, so it can also be typed; Enter
/// commits what was typed, clamped to the range.
///
/// `edit_input` is the host's [`TextInput`] for the value; `value` is the
/// committed number the steppers work from. The box writes each new value
/// into the field before reporting it, so the host only has to store it.
#[allow(clippy::too_many_arguments)]
pub fn spinbox<V: ControlHost>(
    id_prefix: &'static str,
    value: i32,
    min: i32,
    max: i32,
    enabled: bool,
    edit_input: &Entity<TextInput>,
    palette: Palette,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, i32, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    edit_input.update(cx, |input, _cx| {
        input.restyle(palette);
    });
    // The box draws its input, not its value, so a step writes the number
    // into the field before the host hears about it.
    let edit = edit_input.clone();
    let commit: Commit<V> = Rc::new(move |this, value, window, cx| {
        edit.update(cx, |input, cx| input.set_text(&value.to_string(), cx));
        on_change(this, value, window, cx);
    });
    let dec = commit.clone();
    let key_step = commit.clone();
    let typed = commit.clone();
    let typed_from = edit_input.clone();
    let inc = commit;

    type Step<V> = Box<dyn Fn(&mut V, &mut Window, &mut Context<V>) + 'static>;
    let step_button = |id: ElementId,
                       glyph: &'static str,
                       active: bool,
                       cx: &mut Context<V>,
                       handler: Step<V>| {
        let step_ring = ElementId::Name(format!("{id:?}-ring").into());
        div()
            .id(id)
            .w(px(STEP_BUTTON_SIZE))
            .h(px(STEP_BUTTON_SIZE))
            .rounded(px(STEP_BUTTON_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(STEP_BUTTON_TEXT_SIZE))
            .text_color(palette.text_secondary)
            .when(!enabled, |el| el.opacity(SPIN_BUTTON_DISABLED))
            // A stepper at its limit stays visible but stops responding:
            // removing it would shift the other one under the pointer.
            .when(enabled && active, move |el| {
                let handler = Rc::new(handler);
                let key_handler = handler.clone();
                el.relative()
                    .cursor_pointer()
                    .hover(move |style| style.bg(palette.soft_fill))
                    .active(move |style| style.bg(palette.soft_fill_hover))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        handler(this, window, cx);
                    }))
                    .child(
                        keyboard::ring(step_ring, STEP_BUTTON_RADIUS, palette).on_key_down(
                            cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                                if keyboard::key(event) == Some(Key::Activate) {
                                    cx.stop_propagation();
                                    key_handler(this, window, cx);
                                    cx.notify();
                                }
                            }),
                        ),
                    )
            })
            .child(glyph)
    };

    div()
        .w(px(SPINBOX_WIDTH))
        .h(px(CONTROL_HEIGHT))
        .flex_none()
        .rounded(px(CONTROL_RADIUS))
        .bg(palette.field_surface)
        .border_1()
        .border_color(palette.field_border)
        .shadow(lighting::recessed(palette.is_dark))
        .when(!enabled, |el| el.opacity(DISABLED_OPACITY))
        .flex()
        .items_center()
        .justify_between()
        .px(px(SPINBOX_PAD_H))
        // Up and down step the value from inside the field, which is what
        // every spin box has done with them for as long as there have been
        // spin boxes. They arrive as the field's own actions: a single-line
        // field has no line to move to, so it passes them up — and a
        // keystroke that matched a binding never reaches a raw key listener
        // in GPUI in any case.
        .when(enabled, |el| {
            let step_down = key_step.clone();
            el.on_action(
                cx.listener(move |this, _: &crate::text_input::Up, window, cx| {
                    if value < max {
                        key_step(this, (value + 1).min(max), window, cx);
                        cx.notify();
                    }
                }),
            )
            .on_action(
                cx.listener(move |this, _: &crate::text_input::Down, window, cx| {
                    if value > min {
                        step_down(this, (value - 1).max(min), window, cx);
                        cx.notify();
                    }
                }),
            )
            // Enter commits what was typed. Something that is not a number
            // goes back to the value it replaced, so the field never shows a
            // number the host does not have.
            .on_action(cx.listener(
                move |this, _: &crate::text_input::Enter, window, cx| {
                    let text = typed_from.read(cx).text();
                    match text.trim().parse::<i32>() {
                        Ok(entered) => typed(this, entered.clamp(min, max), window, cx),
                        Err(_) => typed_from.update(cx, |input, cx| {
                            input.set_text(&value.to_string(), cx);
                        }),
                    }
                    cx.notify();
                },
            ))
        })
        .child(step_button(
            ElementId::Name(format!("{id_prefix}-dec").into()),
            "\u{2212}",
            value > min,
            cx,
            Box::new(move |this, window, cx| {
                dec(this, (value - 1).max(min), window, cx);
            }),
        ))
        .child(
            div()
                .flex_1()
                .px(px(SPINBOX_PAD_H))
                .text_size(px(SPINBOX_TEXT_SIZE))
                .text_color(palette.text_primary)
                .child(edit_input.clone()),
        )
        .child(step_button(
            ElementId::Name(format!("{id_prefix}-inc").into()),
            "+",
            value < max,
            cx,
            Box::new(move |this, window, cx| {
                inc(this, (value + 1).min(max), window, cx);
            }),
        ))
}

/// A spin box's new value on its way to the host.
type Commit<V> = Rc<dyn Fn(&mut V, i32, &mut Window, &mut Context<V>)>;

// ---- Text field and area ----------------------------------------------------

/// Recessed well, plus the accent glow while focused.
fn well_shadows(palette: Palette, focused: bool) -> Vec<gpui::BoxShadow> {
    let mut shadows = lighting::recessed(palette.is_dark);
    if focused {
        shadows.push(lighting::glow(
            palette.accent,
            FOCUSED_GLOW_OPACITY,
            FOCUSED_GLOW_BLUR,
        ));
    }
    shadows
}

/// Single-line text field: a recessed well around a [`TextInput`].
///
/// The chrome takes the press itself and forwards it, so clicking the
/// padding either side of the text still places the caret instead of
/// missing the input entirely. An input styled with
/// [`InputStyle::from_palette`](crate::InputStyle::from_palette) is kept in
/// step with `palette` here, so a theme crossing over carries the text.
pub fn text_field<V: ControlHost>(
    input: &Entity<TextInput>,
    palette: Palette,
    window: &Window,
    cx: &mut Context<V>,
) -> impl IntoElement {
    input.update(cx, |input, _cx| {
        input.restyle(palette);
    });
    let focused = input.read(cx).focus_handle.is_focused(window);
    let click_input = input.clone();
    div()
        .h(px(CONTROL_HEIGHT))
        .w_full()
        .px(px(TEXT_FIELD_PAD_H))
        .flex()
        .items_center()
        .rounded(px(CONTROL_RADIUS))
        .bg(palette.field_surface)
        .border_1()
        .border_color(if focused {
            palette.accent
        } else {
            palette.field_border
        })
        .shadow(well_shadows(palette, focused))
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_host, event: &MouseDownEvent, window, cx| {
                click_input.update(cx, |input, cx| {
                    input.handle_chrome_click(event.position, window, cx)
                });
            }),
        )
        .child(input.clone())
}

/// Multi-line text area in the same recessed well.
///
/// With `height` it is fixed; without, it fills its flex slot down to a 40px
/// floor, so an area in a resizable panel grows with the panel instead of
/// clipping.
pub fn text_area<V: ControlHost>(
    input: &Entity<TextInput>,
    height: Option<f32>,
    enabled: bool,
    palette: Palette,
    window: &Window,
    cx: &mut Context<V>,
) -> impl IntoElement {
    input.update(cx, |input, _cx| {
        input.restyle(palette);
    });
    let focused = input.read(cx).focus_handle.is_focused(window);
    let click_input = input.clone();
    div()
        .id(ElementId::Name(
            format!("text-area-{}", input.entity_id()).into(),
        ))
        .when_some(height, |el, h| el.h(px(h)))
        .when(height.is_none(), |el| {
            el.flex_1().min_h(px(TEXT_AREA_MIN_H))
        })
        .line_height(px(TEXT_AREA_LINE_HEIGHT))
        .w_full()
        .px(px(TEXT_AREA_PAD_H))
        .py(px(TEXT_AREA_PAD_V))
        .rounded(px(CONTROL_RADIUS))
        .bg(palette.area_surface)
        .border_1()
        .border_color(if focused {
            palette.accent
        } else {
            palette.area_border
        })
        .shadow(well_shadows(palette, focused))
        .when(!enabled, |el| el.opacity(DISABLED_OPACITY))
        .overflow_y_scroll()
        // A sideways gesture must not scroll this. GPUI maps an x-delta onto
        // y for a container that only scrolls vertically, so without this a
        // horizontal trackpad swipe scrolls the text area up and down.
        .restrict_scroll_to_axis()
        .when(enabled, |el| {
            el.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_host, event: &MouseDownEvent, window, cx| {
                    click_input.update(cx, |input, cx| {
                        input.handle_chrome_click(event.position, window, cx)
                    });
                }),
            )
        })
        .child(input.clone())
}

// ---- Pop-up menu ------------------------------------------------------------

/// The list's metrics: each row's height, the gap between rows, the list's
/// padding and border, and the most of it that shows before it scrolls.
/// Named because [`combo_placement`] works from them as well as the styles.
const COMBO_ROW_HEIGHT: f32 = 28.0;
const COMBO_ROW_GAP: f32 = 2.0;
const COMBO_LIST_PADDING: f32 = 5.0;
const COMBO_LIST_BORDER: f32 = 1.0;
const COMBO_LIST_MAX_HEIGHT: f32 = 240.0;

/// Where a pop-up's list goes when it opens.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ComboPlacement {
    /// The list's top edge relative to the button's, before the window's
    /// edges have their say.
    top: f32,
    /// How far the list starts scrolled, so the chosen row is in the part
    /// of it that shows.
    scroll: f32,
}

/// Places the list so that row `chosen` lies centred over the button, the
/// way a native pop-up opens: the pointer is already on the current value,
/// and every other value is one row away.
///
/// A list taller than it may be scrolls, and a row past the part that shows
/// cannot lie on the button unless the list starts scrolled to it. So it
/// does, by the least that brings the row fully into view, and the list's
/// top moves up by the same amount so the row stays where it was put.
fn combo_placement(chosen: usize, count: usize) -> ComboPlacement {
    let pitch = COMBO_ROW_HEIGHT + COMBO_ROW_GAP;
    // The row's edges within the list's scrolling content, padding included.
    let row_top = COMBO_LIST_PADDING + chosen as f32 * pitch;
    let row_bottom = row_top + COMBO_ROW_HEIGHT;
    let content = 2.0 * COMBO_LIST_PADDING + (count as f32 * pitch - COMBO_ROW_GAP).max(0.0);
    let furthest = (content - COMBO_LIST_MAX_HEIGHT).max(0.0);
    let scroll = (row_bottom - COMBO_LIST_MAX_HEIGHT).clamp(0.0, furthest);
    let top = (CONTROL_HEIGHT - COMBO_ROW_HEIGHT) / 2.0 - COMBO_LIST_BORDER - row_top + scroll;
    ComboPlacement { top, scroll }
}

/// Opens `id`'s list on `chosen`, scrolled as [`combo_placement`] asks, so
/// the row that lands on the button is one the list is actually showing.
fn open_list(state: &mut ControlState, id: ComboId, chosen: usize, count: usize) {
    state.open_combo(id, chosen);
    let placement = combo_placement(chosen, count);
    state
        .scroll(id)
        .set_offset(point(px(0.0), px(-placement.scroll)));
}

/// Pop-up menu: a raised button showing the current option, with an accent
/// chevron chip, and a floating list when open.
///
/// The list opens over the button with the current option lying on it, and
/// is shifted back inside the window if that would put it over an edge, so
/// a pop-up near the bottom of a window needs nothing special.
///
/// `id` is both the element id and the pop-up's identity in
/// [`crate::ControlState`], so no two pop-ups in one view may share it.
#[allow(clippy::too_many_arguments)]
pub fn combo<V: ControlHost>(
    id: ComboId,
    current_index: usize,
    options: &[String],
    width: Option<f32>,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let palette = ctx.palette;
    let view = ctx.view;
    let cx: &mut Context<V> = &mut *ctx.cx;
    let dark = palette.is_dark;
    let state = view.control_state();
    let is_open = state.is_combo_open(id);
    let list_scroll = state.scroll(id);
    // The list keeps rendering while it fades back out after a close.
    // Reveal progress: opacity, plus the short drift down into place a menu
    // makes, so the two things that drop out of a click arrive the same
    // way.
    let fade = state.combo_fade(id);
    let showing = fade.is_some();
    let reveal = fade.map_or(1.0, |(reveal, _)| reveal);
    let drift = -4.0 * (1.0 - reveal);

    let display: SharedString = options
        .get(current_index)
        .cloned()
        .unwrap_or_default()
        .into();
    let options_owned: Vec<String> = options.to_vec();
    let count = options.len();
    // The list lies over the row that was current when it opened — that
    // rather than `current_index`, which a pick changes under a list still
    // fading out.
    let placement = combo_placement(state.combo_opened_on(id).unwrap_or(current_index), count);
    // Where the button was last frame, in window coordinates: what the list
    // is laid over, whatever the button is nested inside.
    let button = state.track(id);
    let weak = cx.entity().downgrade();
    let keyboard_at = if is_open {
        state.combo_highlight_or(current_index)
    } else {
        current_index
    };
    let on_select = Rc::new(on_select);
    let key_select = on_select.clone();
    // In dark mode the button is a tinted fill; in light mode it is the same
    // white as a field, so a row of fields and pop-ups reads as one surface.
    let body_fill = if dark {
        palette.soft_fill
    } else {
        palette.field_surface
    };
    let chip_fill = palette.control_fill;
    let chevron_color: gpui::Hsla = crate::color::to_hsla(palette.control_label);

    div()
        .relative()
        .when_some(width, |el, w| el.w(px(w)).flex_none())
        .when(width.is_none(), |el| el.w_full())
        // Records where the button is, so the list can be laid over it.
        .child(
            canvas(
                move |bounds, _window, cx| {
                    if let Some(host) = weak.upgrade() {
                        host.update(cx, |host, _cx| {
                            host.control_state_mut().record_track(id, bounds);
                        });
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .id(ElementId::Name(format!("{id}-toggle").into()))
                .h(px(CONTROL_HEIGHT))
                .w_full()
                .pl(px(COMBO_PAD_L))
                .pr(px(COMBO_PAD_R))
                .flex()
                .items_center()
                .rounded(px(CONTROL_RADIUS))
                .bg(lighting::lit(body_fill, COMBO_BODY_LIT))
                .border_1()
                .border_color(if is_open {
                    palette.accent
                } else {
                    lighting::rim(body_fill, dark)
                })
                .shadow(lighting::raised(dark))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(palette.text_primary)
                .whitespace_nowrap()
                .overflow_hidden()
                .hover(move |style| style.bg(lighting::lit(body_fill, 0.09)))
                .relative()
                // Focus stays on the button while its list is open, which is
                // what a native pop-up does: the list is a view of the
                // button's value, not a place the keyboard goes.
                .child(
                    keyboard::ring(
                        ElementId::Name(format!("{id}-ring").into()),
                        CONTROL_RADIUS,
                        palette,
                    )
                    // Once `bind_keys` has run, Escape arrives as this action
                    // and never as a key. With no list open it is the host's.
                    .on_action(cx.listener(move |this, _: &Dismiss, _window, cx| {
                        if !this.control_state().is_combo_open(id) {
                            cx.propagate();
                            return;
                        }
                        this.control_state_mut().close_combo();
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            let Some(key) = keyboard::key(event) else {
                                return;
                            };
                            if !this.control_state().is_combo_open(id) {
                                // Down opens as well as Enter, so a reader who knows
                                // there is a list under here can get at it the way
                                // they would anywhere else.
                                if matches!(key, Key::Activate | Key::Down) {
                                    cx.stop_propagation();
                                    open_list(this.control_state_mut(), id, current_index, count);
                                    cx.notify();
                                }
                                return;
                            }
                            cx.stop_propagation();
                            match key {
                                // Escape leaves the value alone: arrowing through a
                                // list is looking, not choosing.
                                Key::Dismiss => {
                                    this.control_state_mut().close_combo();
                                }
                                Key::Activate => {
                                    let chosen =
                                        this.control_state().combo_highlight_or(current_index);
                                    this.control_state_mut().close_combo();
                                    key_select(this, chosen, window, cx);
                                }
                                key => {
                                    let at = this.control_state().combo_highlight_or(current_index);
                                    if let Some(moved) =
                                        keyboard::step(key, Orientation::Vertical, at, count)
                                    {
                                        this.control_state_mut().highlight_combo(moved);
                                    }
                                }
                            }
                            cx.notify();
                        },
                    )),
                )
                .child(display)
                .child(chevron_chip(chip_fill, chevron_color, dark))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    let state = this.control_state_mut();
                    // The list's own press-outside already closed this
                    // pop-up for this very click, the button being outside
                    // the list. Don't reopen it. The marker expires by
                    // itself, because the release that would clear it may
                    // land on an occluding surface and never arrive.
                    if state.take_combo_dismissal() == Some(id) {
                        cx.notify();
                        return;
                    }
                    if state.is_combo_open(id) {
                        state.close_combo();
                    } else {
                        open_list(state, id, current_index, count);
                    }
                    cx.notify();
                })),
        )
        // Anchored in window coordinates rather than hung under the button
        // in the tree, because `anchored` is what knows where the window's
        // edges are and shifts the list back inside them.
        .when_some(button.filter(|_| showing), |el, button| {
            el.child(
                deferred(
                    anchored()
                        .position(point(button.origin.x, button.origin.y + px(placement.top)))
                        .anchor(Anchor::TopLeft)
                        .offset(point(px(0.0), px(drift)))
                        .snap_to_window_with_margin(px(VIEWPORT_MARGIN))
                        .child(
                            div()
                                .id(ElementId::Name(format!("{id}-popup").into()))
                                .w(button.size.width)
                                .opacity(reveal)
                                .rounded(px(CONTROL_RADIUS + 1.0))
                                .bg(body_fill)
                                .border_1()
                                .border_color(lighting::rim(body_fill, dark))
                                .shadow(lighting::panel(dark))
                                // Clipped to its own rounding, so the edge fades
                                // below do not square off the corners.
                                .overflow_hidden()
                                .relative()
                                // A list on its way out takes no clicks and lets
                                // them through to whatever is beneath it.
                                .when(is_open, |el| {
                                    el.occlude().on_mouse_down_out(cx.listener(
                                        move |this, _event, _window, cx| {
                                            let state = this.control_state_mut();
                                            state.combo_pressed_outside(id);
                                            cx.notify();
                                        },
                                    ))
                                })
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("{id}-popup-list").into()))
                                        .max_h(px(COMBO_LIST_MAX_HEIGHT))
                                        .p(px(COMBO_LIST_PADDING))
                                        .flex()
                                        .flex_col()
                                        .gap(px(COMBO_ROW_GAP))
                                        .overflow_y_scroll()
                                        .restrict_scroll_to_axis()
                                        .track_scroll(&list_scroll)
                                        .children(options_owned.into_iter().enumerate().map(
                                            |(index, option)| {
                                                let on_select = on_select.clone();
                                                // While the list is open the highlight
                                                // follows the keyboard; the rest of the
                                                // time it marks what is chosen.
                                                let highlighted = index == keyboard_at;
                                                div()
                                                    .id(ElementId::NamedInteger(
                                                        format!("{id}-option").into(),
                                                        index as u64,
                                                    ))
                                                    .h(px(COMBO_ROW_HEIGHT))
                                                    .flex_none()
                                                    .w_full()
                                                    .px(px(COMBO_ROW_PAD_H))
                                                    .flex()
                                                    .items_center()
                                                    .rounded(px(CONTROL_RADIUS - 1.0))
                                                    .text_size(px(COMBO_TEXT_SIZE))
                                                    .text_color(if highlighted {
                                                        palette.control_label
                                                    } else {
                                                        palette.text_primary
                                                    })
                                                    .when(highlighted, |elem| {
                                                        elem.bg(lighting::lit(
                                                            palette.control_fill,
                                                            COMBO_ROW_SELECTED_LIT,
                                                        ))
                                                    })
                                                    .when(!highlighted, |elem| {
                                                        elem.hover(move |style| {
                                                            style.bg(palette.row_hover)
                                                        })
                                                    })
                                                    .overflow_hidden()
                                                    .child(SharedString::from(option))
                                                    .when(is_open, |elem| {
                                                        let on_select = on_select.clone();
                                                        elem.cursor_pointer().on_click(cx.listener(
                                                            move |this, _event, window, cx| {
                                                                this.control_state_mut()
                                                                    .close_combo();
                                                                on_select(this, index, window, cx);
                                                                cx.notify();
                                                            },
                                                        ))
                                                    })
                                            },
                                        )),
                                )
                                // A long list fades at the edge that has more past
                                // it, rather than slicing a row in half.
                                .children(crate::scroll::scroll_fades(
                                    state,
                                    id,
                                    &list_scroll,
                                    crate::scroll::ScrollAxis::Vertical,
                                    body_fill,
                                    body_fill,
                                )),
                        ),
                )
                .with_priority(100),
            )
        })
}

/// The up/down chevrons on a pop-up button, drawn rather than set in a font
/// so they land on the same pixels whatever the UI face is.
fn chevron_chip(fill: gpui::Rgba, chevron: gpui::Hsla, dark: bool) -> impl IntoElement {
    div()
        .absolute()
        .right(px(CHEVRON_CHIP_INSET_R))
        .top(px((CONTROL_HEIGHT - 2.0 - CHEVRON_CHIP_SIZE) / 2.0))
        .w(px(CHEVRON_CHIP_SIZE))
        .h(px(CHEVRON_CHIP_SIZE))
        .rounded(px(CONTROL_RADIUS - 2.0))
        .bg(lighting::lit(fill, CHEVRON_CHIP_LIT))
        .shadow(lighting::raised(dark))
        .child(
            canvas(
                |_bounds, _window, _cx| {},
                move |bounds, _state, window, _cx| {
                    let origin = bounds.origin;
                    let mut builder = PathBuilder::stroke(px(CHEVRON_STROKE_W));
                    builder.move_to(point(origin.x + px(5.5), origin.y + px(7.5)));
                    builder.line_to(point(origin.x + px(9.0), origin.y + px(4.0)));
                    builder.line_to(point(origin.x + px(12.5), origin.y + px(7.5)));
                    builder.move_to(point(origin.x + px(5.5), origin.y + px(10.5)));
                    builder.line_to(point(origin.x + px(9.0), origin.y + px(14.0)));
                    builder.line_to(point(origin.x + px(12.5), origin.y + px(10.5)));
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, chevron);
                    }
                },
            )
            .size_full(),
        )
}

// ---- Sliders ----------------------------------------------------------------

/// How a slider's track behaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SliderTrack {
    /// Any value between the ends.
    #[default]
    Continuous,
    /// `stops` evenly spaced positions, marked with ticks, the first and
    /// last at the ends of the track. A value of 2 gives a two-position
    /// slider; anything below 2 falls back to continuous.
    Stepped { stops: u32 },
}

impl SliderTrack {
    /// The stop count, if this track has enough of them to snap to. The
    /// snapping itself happens in [`crate::ControlState::track_ratio_at`],
    /// so a drag stays quantised after the pointer has left the track.
    pub fn stops(self) -> Option<u32> {
        match self {
            SliderTrack::Stepped { stops } if stops >= 2 => Some(stops),
            _ => None,
        }
    }
}

/// Horizontal slider: a track, an accent fill up to `ratio` (0..=1) and a
/// round handle. With [`SliderTrack::Stepped`] it also draws a tick at each
/// stop and snaps to them.
///
/// The slider owns its drag. A press anywhere on the track jumps there and
/// starts one; every position, on press and while dragging, arrives at
/// [`ControlHost::track_dragged`] as the `x` of its point. The host only
/// has to pump the drag from wherever it tracks the pointer, with
/// [`crate::state::continue_drags`], and end it with
/// [`crate::state::end_drags`].
pub fn slider<V: ControlHost>(
    id: ComboId,
    ratio: f32,
    track: SliderTrack,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let ratio = ratio.clamp(0.0, 1.0);
    let stops = track.stops();
    let weak = cx.entity().downgrade();
    // A value set from elsewhere — a click on the track, a preset, a
    // shortcut — glides to its place. A drag does not: the hand is the
    // motion, and a thumb trailing the pointer reads as lag.
    let state = view.control_state();
    let shown = if state.is_dragging(id) {
        state.snap((id, "ratio"), ratio)
    } else {
        state.tween((id, "ratio"), ratio, MOVE)
    };

    // The keyboard lives on the thumb, not on the box around it. The thumb
    // is the current value — the thing the arrows move — and it has a fill,
    // so the ring reads as a ring instead of washing through the control.
    // There is exactly one of it per slider, so its own element state holds
    // the handle perfectly well and no shared one is needed.
    let arrows = cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
        let Some(key) = keyboard::key(event) else {
            return;
        };
        let moved = match stops {
            Some(stops) => keyboard::nudge_stepped(key, Orientation::Horizontal, ratio, stops),
            None => keyboard::nudge(key, Orientation::Horizontal, ratio, 0.05),
        };
        if let Some(moved) = moved {
            cx.stop_propagation();
            this.track_dragged(id, point(moved, 0.0), cx);
            cx.notify();
        }
    });

    div()
        .id(id)
        .h(px(28.0))
        .flex_1()
        .px(px(6.0))
        .flex()
        .items_center()
        .relative()
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.control_state_mut()
                    .begin_track_drag(id, TrackAxis::Horizontal, stops);
                if let Some((_, at)) = this.control_state().track_ratio_at(event.position) {
                    this.track_dragged(id, at, cx);
                }
                // Dragging hands the keyboard to the thumb, so the arrows
                // carry on from wherever the pointer let go.
                window.focus_next(cx);
                cx.notify();
            }),
        )
        .child(
            div()
                .flex_1()
                .h(px(5.0))
                .rounded(px(2.5))
                .bg(palette.field_border)
                .relative()
                .child(track_probe(id, weak))
                .children(stops.map(|stops| tick_marks(stops, palette)))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(gpui::relative(shown))
                        .rounded(px(2.5))
                        .bg(palette.accent)
                        .opacity(if palette.is_dark { 0.45 } else { 0.55 }),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            keyboard::ring(
                                ElementId::Name(format!("{id}-thumb").into()),
                                7.0,
                                palette,
                            )
                            .on_key_down(arrows),
                        )
                        .absolute()
                        .top(px(-4.5))
                        .left(gpui::relative(shown))
                        .ml(px(-7.0))
                        .w(px(14.0))
                        .h(px(14.0))
                        .rounded_full()
                        .bg(palette.field_surface)
                        .border_1()
                        .border_color(palette.field_border_strong),
                ),
        )
}

/// An invisible layer that records its own bounds into the control state as
/// it paints. Every draggable track needs this: a drag reads positions
/// against the track long after the pointer has left it.
pub(crate) fn track_probe<V: ControlHost>(
    id: ComboId,
    weak: gpui::WeakEntity<V>,
) -> impl IntoElement {
    canvas(
        move |bounds, _window, cx| {
            if let Some(host) = weak.upgrade() {
                host.update(cx, |host, _cx| {
                    host.control_state_mut().record_track(id, bounds);
                });
            }
        },
        |_bounds, _state, _window, _cx| {},
    )
    .absolute()
    .size_full()
}

/// Ticks under a stepped track: one short mark per stop, the end ones
/// pulled inside the track so they are not clipped by its rounded caps.
fn tick_marks(stops: u32, palette: Palette) -> impl IntoElement {
    let last = (stops - 1) as f32;
    div()
        .absolute()
        .inset_0()
        .children((0..stops).map(move |stop| {
            let at = stop as f32 / last;
            div()
                .absolute()
                .top(px(7.0))
                .left(gpui::relative(at))
                .ml(px(-0.5))
                .w(px(1.0))
                .h(px(4.0))
                .bg(palette.field_border_strong)
                .opacity(0.7)
        }))
}

// ---- Checkbox ---------------------------------------------------------------

/// Checkbox with an optional label to its right. The whole row is the hit
/// target, label included, because a 14px square is a poor one.
#[allow(clippy::too_many_arguments)]
pub fn checkbox<V: ControlHost>(
    id: impl Into<ElementId>,
    checked: bool,
    label: Option<&str>,
    enabled: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_toggle: impl Fn(&mut V, bool, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    const BOX: f32 = 15.0;
    let dark = ctx.palette.is_dark;
    let palette = ctx.palette;
    let tick: gpui::Hsla = crate::color::to_hsla(palette.control_label);
    let label: Option<SharedString> = label.map(std::convert::Into::into);
    let on_toggle = Rc::new(on_toggle);
    let id: ElementId = id.into();
    let box_id = id.clone();
    // Noticed from the value, so a box ticked by a shortcut or by the host's
    // own code crosses over exactly as a click does. The well fills and
    // rises while the tick draws itself in; unticking runs it backwards.
    let on = ctx.state().blend((&box_id, "state"), checked, SWITCH_SLIDE);
    let cx: &mut Context<V> = &mut *ctx.cx;
    let fill = lighting::lit_mix(palette.field_surface, palette.control_fill, 0.1, on);
    let border = crate::color::lerp(
        palette.field_border_strong,
        lighting::rim(palette.control_fill, dark),
        on,
    );

    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(7.0))
        .flex_none()
        .when(!enabled, |el| el.opacity(0.5))
        .when(enabled, |el| {
            let clicked = on_toggle.clone();
            el.cursor_pointer()
                .on_click(cx.listener(move |this, _event, window, cx| {
                    clicked(this, !checked, window, cx);
                    cx.notify();
                }))
        })
        .child(
            // The box carries the ring rather than the row: it is the
            // control, and the row is mostly label.
            div()
                .relative()
                .w(px(BOX))
                .h(px(BOX))
                .flex_none()
                .rounded(px(4.0))
                // Unchecked reads as an empty well, checked as a filled
                // control: the state is legible from the depth alone,
                // before the tick is even resolved.
                .bg(fill)
                .border_1()
                .border_color(border)
                .shadow(if on >= 0.5 {
                    lighting::raised(dark)
                } else {
                    lighting::recessed(dark)
                })
                .when(on > 0.01, move |el| {
                    el.child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, _cx| {
                                // The tick grows out from the box's centre
                                // and fades in as it comes.
                                let origin = bounds.origin;
                                let scale = 0.6 + 0.4 * on;
                                let at = |x: f32, y: f32| {
                                    point(
                                        origin.x + px(7.5 + (x - 7.5) * scale),
                                        origin.y + px(7.5 + (y - 7.5) * scale),
                                    )
                                };
                                let mut tick = tick;
                                tick.alpha *= on;
                                let mut builder = PathBuilder::stroke(px(1.75));
                                builder.move_to(at(3.5, 7.5));
                                builder.line_to(at(6.2, 10.5));
                                builder.line_to(at(11.5, 4.5));
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, tick);
                                }
                            },
                        )
                        .size_full(),
                    )
                })
                // Last, so it paints over the tick and the fill alike.
                .when(enabled, |el| {
                    let toggled = on_toggle.clone();
                    el.child(
                        keyboard::ring(
                            ElementId::Name(format!("{box_id}-ring").into()),
                            4.0,
                            palette,
                        )
                        .on_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, window, cx| {
                                if keyboard::key(event) == Some(Key::Activate) {
                                    cx.stop_propagation();
                                    toggled(this, !checked, window, cx);
                                    cx.notify();
                                }
                            },
                        )),
                    )
                }),
        )
        .children(label.map(|label| {
            div()
                .text_size(px(12.5))
                .text_color(palette.text_primary)
                .whitespace_nowrap()
                .child(label)
        }))
}

// ---- Segmented control ------------------------------------------------------

/// Segmented control: a recessed track with one raised segment for the
/// current choice. Use it instead of a pop-up when there are two or three
/// options and they are worth showing at once.
#[allow(clippy::too_many_arguments)]
pub fn segmented<V: ControlHost>(
    id: &'static str,
    options: &[String],
    selected: usize,
    enabled: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let palette = ctx.palette;
    let view = ctx.view;
    let cx: &mut Context<V> = &mut *ctx.cx;
    let dark = palette.is_dark;
    let on_select = Rc::new(on_select);
    let fill = palette.control_fill;
    let state = view.control_state();
    let focus = state.focus(id, cx);
    let count = options.len();
    let weak = cx.entity().downgrade();
    let mut arrows = (enabled && count > 0).then(|| {
        let on_select = on_select.clone();
        cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            let Some(key) = keyboard::key(event) else {
                return;
            };
            if let Some(moved) = keyboard::step(key, Orientation::Horizontal, selected, count) {
                cx.stop_propagation();
                on_select(this, moved, window, cx);
                cx.notify();
            }
        })
    });

    // The selection is one raised pill that slides from option to option,
    // the way the platform's own segmented controls move theirs. Its place
    // comes from where the options painted last frame, so on the very first
    // frame there is nothing to slide and the selected option draws its own
    // fill instead — which looks the same, and is what the pill takes over.
    let pill = state
        .group(id)
        .zip(state.slot(id, selected))
        .map(|(group, slot)| {
            // Both probes report padding boxes, and an absolute child is
            // positioned against its parent's, so the difference is the
            // pill's `left` exactly.
            let x = f32::from(slot.origin.x) - f32::from(group.origin.x);
            (
                state.tween((id, "pill-x"), x, MOVE),
                state.tween((id, "pill-w"), f32::from(slot.size.width), MOVE),
            )
        });

    // Built up front rather than inside `.children(..)`: each segment needs
    // `cx` to make its listener, and a closure passed to `map` cannot hand
    // the same `&mut` out more than once.
    let mut segments: Vec<gpui::AnyElement> = Vec::with_capacity(options.len());
    for (index, option) in options.iter().enumerate() {
        // Fresh reborrow per segment: the listener closure takes `cx` by
        // move, and the loop needs it again next time round.
        let cx: &mut Context<V> = &mut *cx;
        let on_select = on_select.clone();
        let active = index == selected;
        let click_focus = focus.clone();
        // The label crosses over at the same time as the pill arrives.
        let on = state.blend((id, "segment-on", index), active, SWITCH_SLIDE);
        // The selected segment is the one with a fill, so it is the one the
        // ring can sit on — and it is the option the arrows are pointing at.
        let segment_arrows = if active { arrows.take() } else { None };
        segments.push(
            div()
                .id(ElementId::NamedInteger(
                    format!("{id}-segment").into(),
                    index as u64,
                ))
                .relative()
                .child(slot_probe(id, index, weak.clone()))
                .when(active && enabled, |el| {
                    el.child(
                        keyboard::ring_for(&focus, CONTROL_RADIUS - 2.0, palette)
                            .when_some(segment_arrows, |el, arrows| el.on_key_down(arrows)),
                    )
                })
                .h_full()
                .flex_1()
                .px(px(10.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(CONTROL_RADIUS - 2.0))
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
                .overflow_hidden()
                .when(active && pill.is_none(), |el| {
                    el.bg(lighting::lit(fill, 0.09))
                        .shadow(lighting::raised(dark))
                })
                .when(enabled && !active, move |el| {
                    el.cursor_pointer()
                        .hover(move |style| style.bg(palette.row_hover))
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            window.focus(&click_focus, cx);
                            on_select(this, index, window, cx);
                            cx.notify();
                        }))
                })
                .child(SharedString::from(option.clone()))
                .into_any_element(),
        );
    }

    div()
        .id(id)
        .relative()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(palette.field_surface)
        .border_1()
        .border_color(palette.field_border)
        .shadow(lighting::recessed(dark))
        .when(!enabled, |el| el.opacity(0.5))
        .child(group_probe(id, weak))
        // Under the labels, so they read through it.
        .when_some(pill, |el, (x, width)| {
            el.child(
                div()
                    .absolute()
                    .top(px(2.0))
                    .bottom(px(2.0))
                    .left(px(x))
                    .w(px(width))
                    .rounded(px(CONTROL_RADIUS - 2.0))
                    .bg(lighting::lit(fill, 0.09))
                    .shadow(lighting::raised(dark)),
            )
        })
        .children(segments)
}

/// Records one option's bounds into `ControlState::slot_bounds`, so the
/// selection pill knows where to slide to. Paints nothing.
fn slot_probe<V: ControlHost>(
    id: ComboId,
    slot: usize,
    weak: gpui::WeakEntity<V>,
) -> impl IntoElement {
    canvas(
        move |bounds, _window, cx| {
            if let Some(host) = weak.upgrade() {
                host.update(cx, |host, _cx| {
                    host.control_state_mut().record_slot(id, slot, bounds);
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
}

/// Remembers an element's bounds in [`ControlState::measured`] under `tag`,
/// for anything that has to know a size before it can be laid out to it.
pub(crate) fn measure_probe<V: ControlHost>(
    tag: Tag,
    weak: gpui::WeakEntity<V>,
) -> impl IntoElement {
    canvas(
        move |bounds, _window, cx| {
            if let Some(host) = weak.upgrade() {
                host.update(cx, |host, _cx| {
                    host.control_state_mut().measure(tag, bounds);
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
}

/// Records a group's own bounds into `ControlState::group_bounds`; the
/// options' bounds only mean something relative to these.
fn group_probe<V: ControlHost>(id: ComboId, weak: gpui::WeakEntity<V>) -> impl IntoElement {
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
}

// ---- Icon button ------------------------------------------------------------

/// Square button around whatever element the host draws: a glyph, an SVG, a
/// rendered image. `active` gives it the pressed-in look of a toggle that is
/// on.
///
/// An icon has no words, so the button takes the words it would have had
/// as `label`, and shows them as its tooltip: `"Undo"`, or
/// `Hint::new("Favourite").shortcut(display("secondary-d"))` to show the
/// accelerator after them. That is the button's one tooltip — GPUI allows
/// one per element.
#[allow(clippy::too_many_arguments)]
pub fn icon_button<V: ControlHost>(
    id: impl Into<ElementId>,
    icon: impl IntoElement,
    label: impl Into<Hint>,
    size: f32,
    active: bool,
    enabled: bool,
    palette: Palette,
    cx: &mut Context<V>,
    on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let dark = palette.is_dark;
    let on_click = Rc::new(on_click);
    let id: ElementId = id.into();
    let icon_id = id.clone();
    let fill = if active {
        palette.control_fill
    } else {
        palette.soft_fill
    };

    div()
        .id(id)
        .w(px(size))
        .h(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CONTROL_RADIUS))
        .bg(lighting::lit(fill, if active { 0.04 } else { 0.08 }))
        .border_1()
        .border_color(lighting::rim(fill, dark))
        .text_color(if active {
            palette.control_label
        } else {
            palette.text_secondary
        })
        .when(active, |el| el.shadow(lighting::recessed(dark)))
        .when(!active, |el| el.shadow(lighting::raised(dark)))
        .when(!enabled, |el| el.opacity(0.5))
        .when(enabled, move |el| {
            let pressed = on_click.clone();
            el.cursor_pointer()
                .hover(move |style| style.bg(lighting::lit(palette.soft_fill_hover, 0.1)))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_click(this, window, cx);
                }))
                .relative()
                .child(
                    keyboard::ring(
                        ElementId::Name(format!("{icon_id}-ring").into()),
                        CONTROL_RADIUS,
                        palette,
                    )
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            if keyboard::key(event) == Some(Key::Activate) {
                                cx.stop_propagation();
                                pressed(this, window, cx);
                                cx.notify();
                            }
                        },
                    )),
                )
        })
        .child(icon)
        .tooltip(label.into().tooltip(palette))
}

/// A text character as an icon, for [`icon_button`]: "↺", "★", "+". It
/// takes the button's own colour, so an active toggle's glyph is lit with
/// the rest of it.
pub fn glyph(character: &'static str) -> Div {
    div().text_size(px(13.0)).child(character)
}

// ---- Progress and rules -----------------------------------------------------

/// Determinate progress bar: the same recessed track as a slider, filled to
/// `fraction` (0..=1) in the accent colour.
pub fn progress_bar(fraction: f32, palette: Palette) -> impl IntoElement {
    let fraction = fraction.clamp(0.0, 1.0);
    div()
        .h(px(6.0))
        .w_full()
        .flex_none()
        .rounded(px(3.0))
        .bg(palette.field_border)
        .shadow(lighting::recessed(palette.is_dark))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(gpui::relative(fraction))
                .rounded(px(3.0))
                .bg(lighting::lit(palette.accent, 0.12)),
        )
}

/// Hairline rule. Horizontal by default; `vertical` for a column divider.
pub fn separator(vertical: bool, palette: Palette) -> impl IntoElement {
    let mut color: gpui::Hsla = crate::color::to_hsla(palette.field_border);
    color.alpha = 0.9;
    div()
        .flex_none()
        .bg(color)
        .when(vertical, |el| el.w(px(1.0)).h_full())
        .when(!vertical, |el| el.h(px(1.0)).w_full())
}

// ---- Overlay scrollbar ------------------------------------------------------

/// Interactive overlay scrollbar: a draggable thumb, and a track click that
/// jumps. Put it inside a `.relative()` wrapper around the scroll container.
/// It renders nothing while the content fits.
///
/// The thumb is faint until hovered, because an overlay bar is a hint about
/// position rather than a control competing with the content.
pub fn scrollbar<V: ControlHost>(
    id: &'static str,
    handle: &ScrollHandle,
    axis: ScrollAxis,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> gpui::AnyElement {
    use crate::scroll::{apply_scroll_drag, scrollbar_geometry};
    let WidgetContext { palette, view, cx } = ctx;

    let Some(geometry) = scrollbar_geometry(handle, axis) else {
        return div().absolute().into_any_element();
    };
    // A bar that appears because the content just grew fades in rather than
    // switching on beside it.
    let shown = view
        .control_state()
        .tween_from(("scrollbar", id, "shown"), 0.0, 1.0, SWITCH_SLIDE);
    let mut thumb_color: gpui::Hsla = crate::color::to_hsla(palette.text_secondary);
    thumb_color.alpha = 0.24;

    let drag_handle = handle.clone();
    let track = div()
        .id(ElementId::Name(format!("scrollbar-{id}").into()))
        .absolute()
        .opacity(shown)
        // Block clicks and hover from bleeding into rows under the track,
        // but let wheel deltas pass so the strip is not a scroll dead zone.
        .block_mouse_except_scroll()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                let Some(geometry) = scrollbar_geometry(&drag_handle, axis) else {
                    return;
                };
                let position = match axis {
                    ScrollAxis::Vertical => f32::from(event.position.y),
                    ScrollAxis::Horizontal => f32::from(event.position.x),
                };
                let within = position - geometry.track_start - geometry.thumb_pos;
                let grab = if (0.0..=geometry.thumb_len).contains(&within) {
                    within
                } else {
                    // Track click: centre the thumb on the pointer.
                    geometry.thumb_len / 2.0
                };
                let drag = ScrollDrag {
                    handle: drag_handle.clone(),
                    axis,
                    grab,
                };
                apply_scroll_drag(&drag, position);
                this.control_state_mut().begin_scroll_drag(drag);
                cx.notify();
            }),
        )
        // Blocking the mouse also truncates hover at this strip: while the
        // pointer is inside it the host is not hovered, so the host's own
        // drag tracking sees neither the moves nor the release. Forward
        // both, the way an occluding modal surface has to.
        .on_mouse_move(
            cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                this.forwarded_mouse_move(event, window, cx);
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _event, window, cx| {
                this.forwarded_mouse_up(window, cx);
            }),
        );

    let thumb = div()
        .absolute()
        .rounded_full()
        .bg(thumb_color)
        .hover(move |style| {
            let mut hovered = thumb_color;
            hovered.alpha = 0.50;
            style.bg(hovered)
        });

    match axis {
        ScrollAxis::Vertical => track
            .top_0()
            .bottom(px(geometry.track_end_inset))
            .right_0()
            .w(px(SCROLLBAR_THICKNESS))
            .child(
                thumb
                    .top(px(geometry.thumb_pos))
                    .right(px(3.0))
                    .w(px(THUMB_THICKNESS))
                    .h(px(geometry.thumb_len)),
            )
            .into_any_element(),
        ScrollAxis::Horizontal => track
            .left_0()
            .right(px(geometry.track_end_inset))
            .bottom_0()
            .h(px(SCROLLBAR_THICKNESS))
            .child(
                thumb
                    .left(px(geometry.thumb_pos))
                    .bottom(px(3.0))
                    .h(px(THUMB_THICKNESS))
                    .w(px(geometry.thumb_len)),
            )
            .into_any_element(),
    }
}

// ---- Chips ------------------------------------------------------------------

/// Shorter than [`CONTROL_HEIGHT`], because a wrapping field of thirty of
/// them at full control height reads as a wall rather than a set of choices.
pub const CHIP_HEIGHT: f32 = 26.0;

/// Which chips in a group are on.
#[derive(Clone, Copy, Debug)]
pub enum ChipSelection<'a> {
    /// One at a time, or none.
    One(Option<usize>),
    /// Any number, as a mask running parallel to the options. A mask shorter
    /// than the options leaves the rest off.
    Many(&'a [bool]),
}

impl ChipSelection<'_> {
    fn holds(&self, index: usize) -> bool {
        match self {
            ChipSelection::One(selected) => *selected == Some(index),
            ChipSelection::Many(mask) => mask.get(index).copied().unwrap_or(false),
        }
    }
}

/// A chip: a button sized to its own label rather than to its container.
///
/// Use these when a choice has more options than a [`segmented`] control can
/// carry but they are all worth showing at once — a set of tags, a filter
/// bar, twenty export formats. Lay them out in a wrapping row:
///
/// ```ignore
/// div().flex().flex_wrap().gap(px(6.0)).children(chips)
/// ```
///
/// [`chip_group`] does exactly that for a plain list of labels; reach for
/// `chip` directly when each one needs something of its own, such as a value
/// to send rather than an index.
#[allow(clippy::too_many_arguments)]
pub fn chip<V: ControlHost>(
    id: impl Into<ElementId>,
    label: &str,
    selected: bool,
    enabled: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let dark = palette.is_dark;
    let text: SharedString = SharedString::from(label);
    let on_click = Rc::new(on_click);
    let id: ElementId = id.into();
    let chip_id = id.clone();
    // Selected takes the accent; the rest stay quiet, so one chip reads out
    // of a field of them at a glance. The accent washes in rather than
    // switching on, whoever toggled the chip.
    let on = view
        .control_state()
        .blend((&chip_id, "state"), selected, SWITCH_SLIDE);
    let mix = |a, b| crate::color::lerp(a, b, on);
    let fill = mix(palette.soft_fill, palette.control_fill);
    let fill_hover = mix(palette.soft_fill_hover, palette.control_fill);
    let label_color = mix(palette.soft_label, palette.control_label);
    let border = mix(lighting::rim(palette.soft_fill, dark), palette.accent);

    div()
        .id(id)
        .h(px(CHIP_HEIGHT))
        // The point of the whole control: no `w_full`, so the chip is as
        // wide as its label and its neighbours sit beside it.
        .flex_none()
        .px(px(11.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CONTROL_RADIUS))
        .bg(lighting::lit(fill, 0.08))
        .border_1()
        .border_color(border)
        .shadow(lighting::raised(dark))
        .text_size(px(12.0))
        .font_weight(if selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .text_color(label_color)
        .whitespace_nowrap()
        .when(!enabled, |el| el.opacity(0.5))
        .when(enabled, move |el| {
            let pressed = on_click.clone();
            el.cursor_pointer()
                .hover(move |style| style.bg(lighting::lit(fill_hover, 0.11)))
                .active(move |style| style.bg(lighting::lit(lighting::shade(fill, -0.06), 0.0)))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_click(this, window, cx);
                }))
                .relative()
                // Each chip is its own tab stop rather than the group being
                // one: chips toggle independently, so they are a row of
                // checkboxes wearing a different coat, not a single choice.
                .child(
                    keyboard::ring(
                        ElementId::Name(format!("{chip_id}-ring").into()),
                        CONTROL_RADIUS,
                        palette,
                    )
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            if keyboard::key(event) == Some(Key::Activate) {
                                cx.stop_propagation();
                                pressed(this, window, cx);
                                cx.notify();
                            }
                        },
                    )),
                )
        })
        .child(text)
}

/// A wrapping row of [`chip`]s, each sized to its label.
///
/// Single or multiple selection, depending on the [`ChipSelection`] handed
/// in; either way the callback reports the chip that was clicked and the
/// host decides what that means.
#[allow(clippy::too_many_arguments)]
pub fn chip_group<V: ControlHost>(
    id: &'static str,
    options: &[String],
    selected: ChipSelection<'_>,
    enabled: bool,
    mut ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let on_select = Rc::new(on_select);

    let mut chips: Vec<gpui::AnyElement> = Vec::with_capacity(options.len());
    for (index, option) in options.iter().enumerate() {
        let on_select = on_select.clone();
        chips.push(
            chip(
                ElementId::NamedInteger(format!("{id}-chip").into(), index as u64),
                option,
                selected.holds(index),
                enabled,
                ctx.reborrow(),
                move |this, window, cx| on_select(this, index, window, cx),
            )
            .into_any_element(),
        );
    }

    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(6.0))
        .children(chips)
}

// ---- Radio group ------------------------------------------------------------

/// One choice in a [`radio_group`].
#[derive(Clone, Debug)]
pub struct Choice {
    pub label: SharedString,
    /// A second line under the label. Use it when the choice needs a reason,
    /// not to restate the label at greater length.
    pub detail: Option<SharedString>,
    pub enabled: bool,
}

impl Choice {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            enabled: true,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// Vertical list of mutually exclusive choices, each a dot and a label.
///
/// Reach for it over [`segmented`] when the choices need explaining, or when
/// there are more than about three: a segmented control sized for prose
/// stops looking like one control.
#[allow(clippy::too_many_arguments)]
pub fn radio_group<V: ControlHost>(
    id: &'static str,
    choices: &[Choice],
    selected: usize,
    enabled: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let palette = ctx.palette;
    let view = ctx.view;
    let cx: &mut Context<V> = &mut *ctx.cx;
    let dark = palette.is_dark;
    let on_select = Rc::new(on_select);
    // The group's handle, which sits on whichever choice is current. It has
    // to outlive any one choice: the selection moves, and a handle owned by
    // the dot it is drawn on would die with it after a single arrow press.
    let focus = view.control_state().focus(id, cx);

    // Disabled choices are stepped over rather than landed on and refused.
    // An arrow that appears to do nothing reads as a broken control.
    let live: Vec<usize> = choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| choice.enabled)
        .map(|(index, _)| index)
        .collect();
    let here = live
        .iter()
        .position(|index| *index == selected)
        .unwrap_or(0);
    let mut arrows = (enabled && !live.is_empty()).then(|| {
        let on_select = on_select.clone();
        cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            let Some(key) = keyboard::key(event) else {
                return;
            };
            if let Some(moved) = keyboard::step(key, Orientation::Vertical, here, live.len()) {
                cx.stop_propagation();
                on_select(this, live[moved], window, cx);
                cx.notify();
            }
        })
    });

    let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(choices.len());
    for (index, choice) in choices.iter().enumerate() {
        let cx: &mut Context<V> = &mut *cx;
        let on_select = on_select.clone();
        let active = index == selected;
        let clickable = enabled && choice.enabled;
        let detail = choice.detail.clone();
        let click_focus = focus.clone();
        // The core grows into the chosen dot and shrinks out of the one it
        // left, so the choice is seen to move rather than to reappear.
        let on = view
            .control_state()
            .blend((id, "choice-on", index), active, SWITCH_SLIDE);
        // Only the current choice carries the handle and the arrow keys.
        let dot_arrows = if active { arrows.take() } else { None };
        rows.push(
            div()
                .id(ElementId::NamedInteger(
                    format!("{id}-choice").into(),
                    index as u64,
                ))
                .flex()
                .items_start()
                .gap(px(8.0))
                .py(px(4.0))
                .when(!clickable, |el| el.opacity(0.5))
                .when(clickable, move |el| {
                    el.cursor_pointer()
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            // Clicking hands the keyboard to the group too,
                            // so Tab carries on from the option just picked
                            // rather than skipping the group altogether.
                            window.focus(&click_focus, cx);
                            on_select(this, index, window, cx);
                            cx.notify();
                        }))
                })
                .child(
                    // The dot is a recessed well with a raised core, so the
                    // chosen one reads at a glance from its depth. It also
                    // carries the ring: it *is* the option, and it has a
                    // fill for the ring to sit around.
                    div()
                        .id(ElementId::NamedInteger(
                            format!("{id}-dot").into(),
                            index as u64,
                        ))
                        .relative()
                        .when(active, |el| {
                            el.child(
                                keyboard::ring_for(&focus, 7.5, palette)
                                    .when_some(dot_arrows, |el, arrows| el.on_key_down(arrows)),
                            )
                        })
                        .mt(px(2.0))
                        .w(px(15.0))
                        .h(px(15.0))
                        .flex_none()
                        .rounded_full()
                        .bg(palette.field_surface)
                        .border_1()
                        .border_color(crate::color::lerp(
                            palette.field_border_strong,
                            palette.accent,
                            on,
                        ))
                        .shadow(lighting::recessed(dark))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(on > 0.01, |el| {
                            let core = 7.0 * (0.4 + 0.6 * on);
                            el.child(
                                div()
                                    .w(px(core))
                                    .h(px(core))
                                    .rounded_full()
                                    .opacity(on)
                                    .bg(lighting::lit(palette.control_fill, 0.12)),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(palette.text_primary)
                                .child(choice.label.clone()),
                        )
                        .children(detail.map(|detail| {
                            div()
                                .text_size(px(11.5))
                                .text_color(palette.text_secondary)
                                .child(detail)
                        })),
                )
                .into_any_element(),
        );
    }

    div().id(id).flex().flex_col().gap(px(2.0)).children(rows)
}

// ---- Search field -----------------------------------------------------------

/// Text field with a magnifier and, once there is something to clear, a
/// clear button.
///
/// The input is the host's, so filtering happens through its `on_change`
/// like any other field; this only adds the chrome that makes it read as a
/// search rather than as a name.
pub fn search_field<V: ControlHost>(
    id: &'static str,
    input: &Entity<TextInput>,
    palette: Palette,
    window: &Window,
    cx: &mut Context<V>,
) -> impl IntoElement {
    input.update(cx, |input, _cx| {
        input.restyle(palette);
    });
    let focused = input.read(cx).focus_handle.is_focused(window);
    let has_text = !input.read(cx).content.is_empty();
    let glyph: gpui::Hsla = crate::color::to_hsla(palette.text_secondary);
    let click_input = input.clone();
    let clear_input = input.clone();
    let key_clear = input.clone();

    div()
        .h(px(CONTROL_HEIGHT))
        .w_full()
        .pl(px(28.0))
        .pr(px(if has_text { 26.0 } else { 9.0 }))
        .relative()
        .flex()
        .items_center()
        .rounded(px(CONTROL_RADIUS))
        .bg(palette.field_surface)
        .border_1()
        .border_color(if focused {
            palette.accent
        } else {
            palette.field_border
        })
        .shadow(well_shadows(palette, focused))
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_host, event: &MouseDownEvent, window, cx| {
                click_input.update(cx, |input, cx| {
                    input.handle_chrome_click(event.position, window, cx)
                });
            }),
        )
        .child(
            div()
                .absolute()
                .left(px(8.0))
                .top(px((CONTROL_HEIGHT - 2.0 - 14.0) / 2.0))
                .w(px(14.0))
                .h(px(14.0))
                .child(
                    canvas(
                        |_bounds, _window, _cx| {},
                        move |bounds, _state, window, _cx| {
                            let origin = bounds.origin;
                            let mut builder = PathBuilder::stroke(px(1.3));
                            // A ring approximated by a dodecagon: at 10px
                            // across, a circle and this are the same
                            // picture, and this needs no curve support.
                            let (cx_, cy, r) = (origin.x + px(6.0), origin.y + px(6.0), 4.3f32);
                            for step in 0..=12 {
                                let angle = step as f32 * std::f32::consts::TAU / 12.0;
                                let at = point(cx_ + px(r * angle.cos()), cy + px(r * angle.sin()));
                                if step == 0 {
                                    builder.move_to(at);
                                } else {
                                    builder.line_to(at);
                                }
                            }
                            builder.move_to(point(origin.x + px(9.2), origin.y + px(9.2)));
                            builder.line_to(point(origin.x + px(13.0), origin.y + px(13.0)));
                            if let Ok(path) = builder.build() {
                                window.paint_path(path, glyph);
                            }
                        },
                    )
                    .size_full(),
                ),
        )
        .child(input.clone())
        .when(has_text, |el| {
            el.child(
                div()
                    .id(ElementId::Name(format!("{id}-clear").into()))
                    .relative()
                    .child(
                        keyboard::ring(
                            ElementId::Name(format!("{id}-clear-ring").into()),
                            8.0,
                            palette,
                        )
                        .on_key_down(cx.listener(
                            move |_host, event: &KeyDownEvent, _window, cx| {
                                if keyboard::key(event) == Some(Key::Activate) {
                                    cx.stop_propagation();
                                    key_clear.update(cx, |input, cx| input.set_text("", cx));
                                    cx.notify();
                                }
                            },
                        )),
                    )
                    .absolute()
                    .right(px(6.0))
                    .top(px((CONTROL_HEIGHT - 2.0 - 16.0) / 2.0))
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(11.0))
                    .text_color(palette.text_secondary)
                    .hover(move |style| style.bg(palette.row_hover))
                    .on_click(cx.listener(move |_host, _event, _window, cx| {
                        clear_input.update(cx, |input, cx| input.set_text("", cx));
                        cx.notify();
                    }))
                    .child("\u{2715}"),
            )
        })
}

// ---- Spinner and badges -----------------------------------------------------

/// How far through its turn a spinner is, from a duration since the epoch.
///
/// The fraction has to be taken out of the integer milliseconds rather than
/// off the end of a float. A Unix timestamp is around 1.76e9, and `f32`
/// steps by 128 at that magnitude, so `Duration::as_secs_f32() % 1.0` is
/// always exactly zero: every spinner drawn from it sits frozen at angle
/// zero, no matter how many frames it is given.
fn spinner_phase(since_epoch: std::time::Duration) -> f32 {
    (since_epoch.as_millis() % 1000) as f32 / 1000.0
}

/// Indeterminate busy indicator: an arc that turns once a second.
///
/// It animates by being repainted, so a host must keep asking for frames
/// while one is on screen. That is deliberate: a spinner nobody can see
/// should not be holding the display link open.
pub fn spinner(size: f32, palette: Palette) -> impl IntoElement {
    let color: gpui::Hsla = crate::color::to_hsla(palette.accent);
    // A wall-clock phase, so several spinners on one screen turn together
    // rather than each from its own start.
    let phase = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(spinner_phase)
        .unwrap_or(0.0);
    let start = phase * std::f32::consts::TAU;
    let radius = size / 2.0 - 1.5;

    div().w(px(size)).h(px(size)).flex_none().child(
        canvas(
            |_bounds, _window, _cx| {},
            move |bounds, _state, window, _cx| {
                let origin = bounds.origin;
                let centre = point(origin.x + px(size / 2.0), origin.y + px(size / 2.0));
                let mut builder = PathBuilder::stroke(px(1.8));
                // Three quarters of a ring: the gap is what makes the
                // rotation visible at all.
                let steps = 18;
                for step in 0..=steps {
                    let angle = start + (step as f32 / steps as f32) * std::f32::consts::TAU * 0.75;
                    let at = point(
                        centre.x + px(radius * angle.cos()),
                        centre.y + px(radius * angle.sin()),
                    );
                    if step == 0 {
                        builder.move_to(at);
                    } else {
                        builder.line_to(at);
                    }
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size_full(),
    )
}

/// What a [`badge`] is saying.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BadgeTone {
    /// A plain count or label.
    #[default]
    Neutral,
    /// Something the accent colour already means elsewhere in the view.
    Accent,
    /// Something wrong.
    Danger,
}

/// Small pill for a count, a state or a tag.
pub fn badge(text: &str, tone: BadgeTone, palette: Palette) -> impl IntoElement {
    let (fill, label) = match tone {
        BadgeTone::Neutral => (palette.soft_fill, palette.soft_label),
        BadgeTone::Accent => (palette.control_fill, palette.control_label),
        BadgeTone::Danger => (palette.danger_fill, palette.danger_label),
    };
    div()
        .flex_none()
        .h(px(17.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(8.5))
        .bg(lighting::lit(fill, 0.07))
        .border_1()
        .border_color(lighting::rim(fill, palette.is_dark))
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(label)
        .whitespace_nowrap()
        .child(SharedString::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn spinner_phase_walks_the_second() {
        assert_eq!(spinner_phase(Duration::from_millis(0)), 0.0);
        assert_eq!(spinner_phase(Duration::from_millis(250)), 0.25);
        assert_eq!(spinner_phase(Duration::from_millis(999)), 0.999);
        // Wraps rather than climbing, so the arc turns instead of winding up.
        assert_eq!(spinner_phase(Duration::from_millis(1250)), 0.25);
    }

    /// The bug this replaced: at a real Unix timestamp `as_secs_f32()` lands
    /// on a float whose neighbours are 128 seconds apart, so its fractional
    /// part is always exactly zero and the spinner never moves.
    #[test]
    fn spinner_phase_still_moves_at_a_real_timestamp() {
        let now = Duration::from_millis(1_757_000_000_500);
        assert_eq!(now.as_secs_f32() % 1.0, 0.0, "the trap this avoids");
        assert_eq!(spinner_phase(now), 0.5);
    }

    #[test]
    fn spinner_phase_is_distinct_across_a_frame() {
        let a = spinner_phase(Duration::from_millis(1_757_000_000_000));
        let b = spinner_phase(Duration::from_millis(1_757_000_000_016));
        assert_ne!(a, b);
    }

    /// The chosen row's vertical centre relative to the button's top, once
    /// the list is placed and scrolled as `placement` says.
    fn chosen_row_centre(placement: ComboPlacement, chosen: usize) -> f32 {
        placement.top
            + COMBO_LIST_BORDER
            + COMBO_LIST_PADDING
            + chosen as f32 * (COMBO_ROW_HEIGHT + COMBO_ROW_GAP)
            - placement.scroll
            + COMBO_ROW_HEIGHT / 2.0
    }

    #[test]
    fn the_chosen_row_lands_centred_on_the_button() {
        for (chosen, count) in [(0, 1), (0, 4), (3, 4), (2, 7), (7, 20), (15, 20), (19, 20)] {
            let placement = combo_placement(chosen, count);
            assert_eq!(
                chosen_row_centre(placement, chosen),
                CONTROL_HEIGHT / 2.0,
                "row {chosen} of {count}"
            );
        }
    }

    #[test]
    fn a_list_that_fits_does_not_start_scrolled() {
        for chosen in 0..7 {
            assert_eq!(combo_placement(chosen, 7).scroll, 0.0, "row {chosen}");
        }
    }

    #[test]
    fn a_long_list_scrolls_only_as_far_as_the_chosen_row_needs() {
        let pitch = COMBO_ROW_HEIGHT + COMBO_ROW_GAP;
        // Rows that show unscrolled leave the list alone...
        assert_eq!(combo_placement(6, 20).scroll, 0.0);
        // ...one past them scrolls just enough to bring it fully into view...
        let row_bottom = COMBO_LIST_PADDING + 15.0 * pitch + COMBO_ROW_HEIGHT;
        assert_eq!(
            combo_placement(15, 20).scroll,
            row_bottom - COMBO_LIST_MAX_HEIGHT
        );
        // ...and never past the end of the list, even for a row it lacks.
        let content = 2.0 * COMBO_LIST_PADDING + 20.0 * pitch - COMBO_ROW_GAP;
        let furthest = content - COMBO_LIST_MAX_HEIGHT;
        assert!(combo_placement(19, 20).scroll <= furthest);
        assert_eq!(combo_placement(40, 20).scroll, furthest);
    }
}
