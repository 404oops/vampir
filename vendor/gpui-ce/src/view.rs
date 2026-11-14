use crate::{
    AnyElement, AnyEntity, AnyWeakEntity, App, Bounds, ContentMask, Context, Dependencies, Element,
    ElementId, Entity, EntityId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Modifiers, PaintIndex, Pixels, Point, PrepaintStateIndex, Render, RenderOnce,
    ResolvedDirection, Style, StyleRefinement, TextStyle, UnicodeBidi, WeakEntity,
    scene::ReplaySummary,
    taffy::LayoutIsland,
    window::{InputReads, a11y::A11yRecording},
};
use crate::{Empty, Window};
use anyhow::Result;
use refineable::Refineable;
use std::{
    any::{TypeId, type_name},
    fmt,
    ops::Range,
};

/// A type-erased handle to a view.
///
/// It pairs an [`Entity`] with its render function, so a field or collection can hold views of
/// different types. Convert an entity with `AnyView::from` or `.into()`, and recover its type with
/// [`AnyView::downcast`]. An `AnyView` can be a child wherever a view is accepted. Two `AnyView`s
/// compare equal when they refer to the same entity.
///
/// # Example
///
/// ```
/// use gpui::{AnyView, Context, Render, TestApp, Window, div, prelude::*};
///
/// struct First;
/// struct Second;
///
/// impl Render for First {
///     fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
///         div().child("First")
///     }
/// }
///
/// impl Render for Second {
///     fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
///         div().child("Second")
///     }
/// }
///
/// let mut app = TestApp::new();
/// let (first, second) = app.update(|cx| (cx.new(|_| First), cx.new(|_| Second)));
/// let views: Vec<AnyView> = vec![first.into(), second.into()];
///
/// assert!(views[0].clone().downcast::<First>().is_ok());
/// assert!(views[1].clone().downcast::<First>().is_err());
/// ```
#[derive(Clone, Debug)]
pub struct AnyView {
    entity: AnyEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
    view_type_name: &'static str,
}

impl<V: Render> From<Entity<V>> for AnyView {
    fn from(value: Entity<V>) -> Self {
        AnyView {
            entity: value.into_any(),
            render: any_view::render::<V>,
            view_type_name: type_name::<V>(),
        }
    }
}

impl AnyView {
    /// Returns this view as an element with a fixed root layout style.
    ///
    /// GPUI can reuse the rendered subtree while the view's entity, tracked entity and global
    /// reads, and layout environment remain unchanged. Reads made by nested views and deferred
    /// prepaint or paint work are tracked too. A notification to a tracked entity, a tracked
    /// global revision change, or [`Window::refresh`] causes the view to rebuild. Changes to
    /// bounds or inherited layout and paint state can also require fresh output.
    ///
    /// `style` sets the retained root's layout style. Pass the entity directly as a child when it
    /// should keep its natural layout. See [`Entity::cached`] and
    /// [retained view rendering](crate::_documentation::concepts::retained_rendering).
    pub fn cached(self, style: StyleRefinement) -> ViewElement<AnyView> {
        self.into_element().cached(style)
    }

    /// Returns a weak handle to this view. The weak handle doesn't keep the view alive.
    ///
    /// Call [`AnyWeakView::upgrade`] on it to get an `AnyView` back, or `None` once the view has
    /// been dropped.
    pub fn downgrade(&self) -> AnyWeakView {
        AnyWeakView {
            entity: self.entity.downgrade(),
            render: self.render,
            view_type_name: self.view_type_name,
        }
    }

    /// Converts this handle into an [`Entity`] of type `T`.
    ///
    /// Returns `Err` with the unchanged `AnyView` when the view isn't a `T`.
    pub fn downcast<T: 'static>(self) -> Result<Entity<T>, Self> {
        match self.entity.downcast() {
            Ok(entity) => Ok(entity),
            Err(entity) => Err(Self {
                entity,
                render: self.render,
                view_type_name: self.view_type_name,
            }),
        }
    }

    /// Returns the [`TypeId`] of the view's type.
    pub fn entity_type(&self) -> TypeId {
        self.entity.entity_type
    }

    /// Returns the [`EntityId`] of the entity that backs this view.
    pub fn entity_id(&self) -> EntityId {
        self.entity.entity_id()
    }
}

impl PartialEq for AnyView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl Eq for AnyView {}

/// `AnyView` is the type-erased [`View`]. Its render is a function pointer rather than a
/// concrete type, so it takes part in the reactive graph like any other view, with the same
/// identity as its entity.
impl View for AnyView {
    fn entity_id(&self) -> Option<EntityId> {
        Some(self.entity.entity_id())
    }

    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        (self.render)(&self, window, cx)
    }
}

impl<V: 'static + Render> IntoElement for Entity<V> {
    type Element = ViewElement<Entity<V>>;

    fn into_element(self) -> Self::Element {
        ViewElement::with_type_name(self, type_name::<V>())
    }

    #[inline(never)]
    fn into_any_element(self) -> AnyElement {
        self.into_element().into_any()
    }
}

impl IntoElement for AnyView {
    type Element = ViewElement<AnyView>;

    fn into_element(self) -> Self::Element {
        let view_type_name = self.view_type_name;
        ViewElement::with_type_name(self, view_type_name)
    }
}

/// A weak, type-erased handle to a view.
///
/// It does not keep the view alive. Create one with [`AnyView::downgrade`] or convert a
/// [`WeakEntity`].
pub struct AnyWeakView {
    entity: AnyWeakEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
    view_type_name: &'static str,
}

impl AnyWeakView {
    /// Returns a strong [`AnyView`] if the view is still alive, or `None` if it has been dropped.
    pub fn upgrade(&self) -> Option<AnyView> {
        let entity = self.entity.upgrade()?;
        Some(AnyView {
            entity,
            render: self.render,
            view_type_name: self.view_type_name,
        })
    }
}

impl<V: 'static + Render> From<WeakEntity<V>> for AnyWeakView {
    fn from(view: WeakEntity<V>) -> Self {
        AnyWeakView {
            entity: view.into(),
            render: any_view::render::<V>,
            view_type_name: type_name::<V>(),
        }
    }
}

impl PartialEq for AnyWeakView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl std::fmt::Debug for AnyWeakView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyWeakView")
            .field("entity_id", &self.entity.entity_id)
            .finish_non_exhaustive()
    }
}

mod any_view {
    use crate::{AnyElement, AnyView, App, IntoElement, Render, Window};

    pub(crate) fn render<V: 'static + Render>(
        view: &AnyView,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let view = view.clone().downcast::<V>().unwrap();
        // Record the view's Render type name so the accessibility debug dump can
        // attribute nodes to the view that produced them.
        #[cfg(debug_assertions)]
        window
            .a11y
            .view_type_names
            .insert(view.entity_id(), std::any::type_name::<V>());
        view.update(cx, |view, cx| {
            #[cfg(all(
                feature = "hot-patching",
                debug_assertions,
                not(target_family = "wasm")
            ))]
            let element = subsecond::HotFn::current(V::render).call((view, window, cx));
            #[cfg(not(all(
                feature = "hot-patching",
                debug_assertions,
                not(target_family = "wasm")
            )))]
            let element = view.render(window, cx);
            element.into_any_element()
        })
    }
}

/// A value that renders into elements and may carry an entity identity.
///
/// Every [`Entity`] of a [`Render`] type is a `View`, and so is every [`RenderOnce`] component.
/// Implement `View` directly when a component needs both props from its parent and an entity
/// identity. Otherwise, use [`Render`] for an entity-backed view or [`RenderOnce`] for a stateless
/// component.
///
/// When [`View::entity_id`] returns `Some`, that entity is the view's identity. GPUI gives the
/// view its own space for element ids, so state inside it can't collide with its siblings. A
/// [`Context::notify`] on that entity redraws only this view's subtree. When it returns `None`,
/// the view is stateless. GPUI scopes it under its type name, and it has no entity to notify.
pub trait View: 'static + Sized {
    /// Returns the entity that identifies this view, or `None` for a stateless view.
    ///
    /// A view that holds its entity as a field returns the entity's id here. The id becomes the
    /// view's [`ElementId`]. Two views backed by the same entity must not be siblings under one
    /// parent, because their element state would collide. Nesting is fine, since the parent's
    /// path keeps the ids apart.
    fn entity_id(&self) -> Option<EntityId>;

    /// Renders this view into elements. Takes `self` by value, so the view is used up.
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement;
}

/// A stateless component (`RenderOnce`) is a [`View`] with no entity identity.
impl<T: RenderOnce> View for T {
    fn entity_id(&self) -> Option<EntityId> {
        None
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        #[cfg(all(
            feature = "hot-patching",
            debug_assertions,
            not(target_family = "wasm")
        ))]
        let element = subsecond::HotFn::current(<T as RenderOnce>::render).call((self, window, cx));
        #[cfg(not(all(
            feature = "hot-patching",
            debug_assertions,
            not(target_family = "wasm")
        )))]
        let element = RenderOnce::render(self, window, cx);
        element
    }
}

/// An entity that renders itself (`Render`) is a [`View`] keyed by its own id.
impl<T: Render> View for Entity<T> {
    fn entity_id(&self) -> Option<EntityId> {
        Some(Entity::entity_id(self))
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.update(cx, |this, cx| {
            #[cfg(all(
                feature = "hot-patching",
                debug_assertions,
                not(target_family = "wasm")
            ))]
            let element = subsecond::HotFn::current(T::render).call((this, window, cx));
            #[cfg(not(all(
                feature = "hot-patching",
                debug_assertions,
                not(target_family = "wasm")
            )))]
            let element = Render::render(this, window, cx);
            element.into_any_element()
        })
    }
}

impl<T: Render> Entity<T> {
    /// Returns this entity as an element laid out at a fixed root style.
    ///
    /// An entity passed directly as a child already keeps its output while nothing it read has
    /// changed, and sizes itself from its content. Use `cached` when the parent should decide the
    /// view's root style instead: the view is laid out at `style`, and its content is laid out
    /// within the bounds that style produces. See
    /// [retained rendering](crate::_documentation::concepts::retained_rendering).
    #[track_caller]
    pub fn cached(self, style: StyleRefinement) -> ViewElement<Entity<T>> {
        ViewElement::with_type_name(self, type_name::<T>()).cached(style)
    }
}

/// The element that draws a [`View`].
///
/// It wraps the view and runs it through the three element phases. Its id is the view's entity
/// when the view has one. You rarely name this type: entities, [`AnyView`] and derived components
/// all convert into it. Create one with [`ViewElement::new`].
#[doc(hidden)]
pub struct ViewElement<V: View> {
    view: Option<V>,
    entity_id: Option<EntityId>,
    retention: Retention,
    #[cfg(feature = "profiler")]
    view_type_name: &'static str,
    #[cfg(debug_assertions)]
    source: &'static core::panic::Location<'static>,
}

enum Retention {
    /// Rendered every frame, because the view may hold properties its parent passed in.
    None,
    /// Kept while nothing it read changed, sized by its content.
    Natural,
    /// Kept while nothing it read changed, laid out at a fixed root style.
    Fixed(StyleRefinement),
}

impl<V: View> ViewElement<V> {
    /// Wraps `view` as an element. The element takes the view's entity id, if it has one.
    ///
    /// A view wrapped this way renders every frame, since its parent may hand it new
    /// properties without notifying its entity. Entities and [`AnyView`]s placed as children
    /// directly keep their output between frames instead.
    #[track_caller]
    pub fn new(view: V) -> Self {
        let mut element = Self::with_type_name(view, type_name::<V>());
        element.retention = Retention::None;
        element
    }

    /// Wraps a view whose draw time profiling attributes to `view_type_name`,
    /// such as an entity's type rather than [`Entity`]'s.
    #[track_caller]
    #[cfg_attr(not(feature = "profiler"), expect(unused_variables))]
    pub(crate) fn with_type_name(view: V, view_type_name: &'static str) -> Self {
        let entity_id = view.entity_id();
        ViewElement {
            entity_id,
            retention: if entity_id.is_some() {
                Retention::Natural
            } else {
                Retention::None
            },
            #[cfg(feature = "profiler")]
            view_type_name,
            view: Some(view),
            #[cfg(debug_assertions)]
            source: core::panic::Location::caller(),
        }
    }

    /// Lays this view out at `style` instead of sizing it by its content.
    ///
    /// Crate-private on purpose: keeping output is only sound for entity-backed views, where
    /// [`Context::notify`] is the contract that busts it. Reach this through [`Entity::cached`]
    /// or [`AnyView::cached`].
    pub(crate) fn cached(mut self, style: StyleRefinement) -> Self {
        debug_assert!(self.entity_id.is_some(), "only entity views can be cached");
        self.retention = Retention::Fixed(style);
        self
    }

    fn render(&mut self, window: &mut Window, cx: &mut App) -> AnyElement {
        render_view(self.view.take().unwrap(), window, cx)
    }
}

impl<V: View> IntoElement for ViewElement<V> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What a kept view carries between frames: its layout island, what it read, and where its
/// output went in the last frame.
pub(crate) struct ViewState {
    island: LayoutIsland,
    layout: Option<LayoutKey>,
    dependencies: Dependencies,
    /// The pointer and modifiers when the view last drew, if it read them.
    input: Option<InputSnapshot>,
    output: Option<ViewOutput>,
    /// The element state its last render used, such as [`Window::use_keyed_state`], which is kept
    /// alive while the view is reused.
    render_states: Vec<(GlobalElementId, TypeId)>,
}

/// What the view's layout depends on beyond its own state.
#[derive(PartialEq)]
struct LayoutKey {
    text_style: TextStyle,
    rem_size: Pixels,
    scale_factor: f32,
}

/// Where the view is drawn. Output recorded under one key can only be reused under an equal key.
#[derive(PartialEq)]
struct OutputKey {
    bounds: Bounds<Pixels>,
    content_mask: ContentMask<Pixels>,
    direction: ResolvedDirection,
    unicode_bidi: UnicodeBidi,
}

/// The window input a view read while drawing, and what it was then.
pub(crate) struct InputSnapshot {
    reads: InputReads,
    mouse_position: Point<Pixels>,
    modifiers: Modifiers,
    modality: crate::window::InputModality,
}

impl InputSnapshot {
    fn new(reads: InputReads, window: &Window) -> Self {
        Self {
            reads,
            mouse_position: window.mouse_position,
            modifiers: window.modifiers,
            modality: window.last_input_modality,
        }
    }

    fn still_valid(&self, window: &Window) -> bool {
        (!self.reads.contains(InputReads::POINTER) || self.mouse_position == window.mouse_position)
            && (!self.reads.contains(InputReads::MODIFIERS) || self.modifiers == window.modifiers)
            && (!self.reads.contains(InputReads::MODALITY)
                || self.modality == window.last_input_modality)
    }
}

struct ViewOutput {
    key: OutputKey,
    prepaint: Range<PrepaintStateIndex>,
    paint: Range<PaintIndex>,
    /// Whether `paint` belongs to the same frame as `prepaint`.
    painted: bool,
    /// The draw these ranges index into.
    frame: u64,
    /// The accessibility nodes drawn with it, if the tree was being built.
    a11y: Option<A11yRecording>,
    /// What replaying `paint` needs, kept after its first replay.
    replay: Option<ReplaySummary>,
}

impl ViewOutput {
    /// Whether this output has everything the current frame needs.
    fn complete(&self, window: &Window) -> bool {
        self.a11y.is_some() || !window.a11y.is_active()
    }
}

impl ViewState {
    /// Moves this view's prepaint range along with a replayed range of its parent, drawn in
    /// `frame`. Output that wasn't drawn inside that range, in the previous frame, is dropped.
    pub(crate) fn rebase_prepaint(
        &mut self,
        source: &Range<PrepaintStateIndex>,
        destination: &PrepaintStateIndex,
        frame: u64,
    ) {
        if let Some(output) = &mut self.output {
            if output.painted
                && output.frame + 1 == frame
                && PrepaintStateIndex::within(&output.prepaint, source)
            {
                output.prepaint = output.prepaint.start.rebased(source, destination)
                    ..output.prepaint.end.rebased(source, destination);
                output.frame = frame;
                output.painted = false;
            } else {
                self.output = None;
            }
        }
    }

    /// Moves this view's paint range along with a replayed range of its parent, drawn in `frame`.
    pub(crate) fn rebase_paint(
        &mut self,
        source: &Range<PaintIndex>,
        destination: &PaintIndex,
        frame: u64,
    ) {
        if let Some(output) = &mut self.output {
            if output.frame == frame && PaintIndex::within(&output.paint, source) {
                output.paint = output.paint.start.rebased(source, destination)
                    ..output.paint.end.rebased(source, destination);
                output.painted = true;
            } else {
                self.output = None;
            }
        }
    }

    fn reusable(&self, entity_id: EntityId, layout: &LayoutKey, window: &Window, cx: &App) -> bool {
        self.layout.as_ref() == Some(layout)
            && !window.refreshing
            && !window.dirty_views.contains(&entity_id)
            && !cx.dependencies_changed(&self.dependencies, &window.dirty_views)
            && self
                .input
                .as_ref()
                .is_none_or(|input| input.still_valid(window))
            && !window.is_inspector_picking(cx)
            && self
                .output
                .as_ref()
                .is_none_or(|output| output.complete(window))
    }
}

#[inline]
fn render_view<V: View>(view: V, window: &mut Window, cx: &mut App) -> AnyElement {
    view.render(window, cx).into_any_element()
}

impl<V: View> Element for ViewElement<V> {
    type RequestLayoutState = Option<AnyElement>;
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        self.entity_id.map(ElementId::View)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        #[cfg(debug_assertions)]
        return Some(self.source);

        #[cfg(not(debug_assertions))]
        return None;
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Option<AnyElement>) {
        #[cfg(feature = "profiler")]
        let start = window.window_profiler.begin_view();
        let result = match (self.entity_id, &self.retention) {
            (None, _) => window.with_id(ElementId::Name(type_name::<V>().into()), |window| {
                let mut element = self.render(window, cx);
                (element.request_layout(window, cx), Some(element))
            }),
            (Some(entity_id), Retention::None) => window.with_rendered_view(entity_id, |window| {
                let mut element = self.render(window, cx);
                (element.request_layout(window, cx), Some(element))
            }),
            (Some(entity_id), _) => window.with_rendered_view(entity_id, |window| {
                window.with_element_state::<ViewState, _>(global_id.unwrap(), |state, window| {
                    let mut state = state.unwrap_or_else(|| ViewState {
                        island: window.create_layout_island(),
                        layout: None,
                        dependencies: Dependencies::default(),
                        input: None,
                        output: None,
                        render_states: Vec::new(),
                    });
                    let layout = LayoutKey {
                        text_style: window.text_style(),
                        rem_size: window.rem_size(),
                        scale_factor: window.scale_factor(),
                    };
                    let element = if state.reusable(entity_id, &layout, window, cx) {
                        cx.replay_dependencies(&state.dependencies);
                        window.keep_element_states(&state.render_states);
                        None
                    } else {
                        state.layout = Some(layout);
                        state.output = None;
                        Some(self.render_into_island(&mut state, window, cx))
                    };
                    let island = state.island.id();
                    let layout_id = match &self.retention {
                        Retention::Fixed(style) => {
                            let mut root_style = Style::default();
                            root_style.refine(style);
                            let layout_id = window.request_layout(root_style, None, cx);
                            window.anchor_layout_island(island, layout_id);
                            // The parent can't see the content through a fixed node, so it gets
                            // the direction the content would contribute to an `Auto` ancestor.
                            let direction = window.layout_island_direction_contribution(island);
                            window.set_layout_auto_direction_hint(layout_id, direction);
                            layout_id
                        }
                        _ => window.request_layout_island(island),
                    };
                    ((layout_id, element), state)
                })
            }),
        };
        #[cfg(feature = "profiler")]
        window.window_profiler.end_view(
            start,
            self.view_type_name,
            crate::profiler::ViewPhase::RequestLayout,
        );
        result
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        #[cfg(feature = "profiler")]
        let start = window.window_profiler.begin_view();
        let result = match (self.entity_id, &self.retention) {
            (None, _) => window.with_id(ElementId::Name(type_name::<V>().into()), |window| {
                let mut element = element.take().unwrap();
                element.prepaint(window, cx);
                Some(element)
            }),
            (Some(entity_id), Retention::None) => {
                window.set_view_id(entity_id);
                window.with_rendered_view(entity_id, |window| {
                    let mut element = element.take().unwrap();
                    element.prepaint(window, cx);
                    Some(element)
                })
            }
            (Some(entity_id), retention) => {
                let fixed = matches!(retention, Retention::Fixed(_));
                window.set_view_id(entity_id);
                window.with_rendered_view(entity_id, |window| {
                    window.with_element_state::<ViewState, _>(
                        global_id.unwrap(),
                        |state, window| {
                            let mut state = state.unwrap();
                            let key = OutputKey {
                                bounds,
                                content_mask: window.content_mask(),
                                direction: window.resolved_direction(),
                                unicode_bidi: window.resolved_unicode_bidi(),
                            };
                            let mut element = element.take();
                            if element.is_none()
                                && let Some(output) = state.output.as_mut()
                                && output.painted
                                && output.frame + 1 == window.frame_number
                                && output.key == key
                                && output.complete(window)
                            {
                                let start = window.prepaint_index();
                                window.reuse_prepaint(output.prepaint.clone());
                                if let Some(a11y) =
                                    output.a11y.as_ref().filter(|_| window.a11y.is_active())
                                {
                                    window.a11y.replay(a11y);
                                }
                                output.prepaint = start..window.prepaint_index();
                                output.frame = window.frame_number;
                                output.painted = false;
                                return (None, state);
                            }

                            // Drawn somewhere new: render again into the island, which keeps the
                            // size its anchor was given this frame.
                            let rendered_now = element.is_none();
                            let mut element = match element.take() {
                                Some(element) => element,
                                None => self.render_into_island(&mut state, window, cx),
                            };
                            if fixed || rendered_now {
                                window.layout_island(state.island.id(), bounds.size, cx);
                            }
                            let start = window.prepaint_index();
                            let a11y = window.a11y.is_active().then(|| window.a11y.mark());
                            let (_, dependencies) = record(&mut state, window, cx, |window, cx| {
                                element.prepaint(window, cx)
                            });
                            state.dependencies.extend(dependencies);
                            state.output = Some(ViewOutput {
                                key,
                                prepaint: start..window.prepaint_index(),
                                paint: PaintIndex::default()..PaintIndex::default(),
                                painted: false,
                                frame: window.frame_number,
                                a11y: a11y.map(|mark| window.a11y.record(mark)),
                                replay: None,
                            });
                            (Some(element), state)
                        },
                    )
                })
            }
        };
        #[cfg(feature = "profiler")]
        window.window_profiler.end_view(
            start,
            self.view_type_name,
            crate::profiler::ViewPhase::Prepaint,
        );
        result
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Option<AnyElement>,
        element: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        #[cfg(feature = "profiler")]
        let start = window.window_profiler.begin_view();
        match (self.entity_id, &self.retention) {
            (None, _) => window.with_id(ElementId::Name(type_name::<V>().into()), |window| {
                element.as_mut().unwrap().paint(window, cx);
            }),
            (Some(entity_id), Retention::None) => window.with_rendered_view(entity_id, |window| {
                element.as_mut().unwrap().paint(window, cx);
            }),
            (Some(entity_id), _) => window.with_rendered_view(entity_id, |window| {
                window.with_element_state::<ViewState, _>(global_id.unwrap(), |state, window| {
                    let mut state = state.unwrap();
                    let start = window.paint_index();
                    match element {
                        Some(element) => {
                            let ((), dependencies) =
                                record(&mut state, window, cx, |window, cx| {
                                    element.paint(window, cx)
                                });
                            state.dependencies.extend(dependencies);
                        }
                        None => {
                            let output = state.output.as_mut().unwrap();
                            window.reuse_paint(output.paint.clone(), &mut output.replay);
                        }
                    }
                    let output = state.output.as_mut().unwrap();
                    output.paint = start..window.paint_index();
                    output.painted = true;
                    ((), state)
                })
            }),
        }
        #[cfg(feature = "profiler")]
        window.window_profiler.end_view(
            start,
            self.view_type_name,
            crate::profiler::ViewPhase::Paint,
        );
    }
}

impl<V: View> ViewElement<V> {
    /// Renders the view into its island and requests its layout there, recording what it read.
    fn render_into_island(
        &mut self,
        state: &mut ViewState,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let island = state.island.id();
        state.input = None;
        state.dependencies = Dependencies::default();
        let states_start = window.element_states_mark();
        let view = self.view.take().unwrap();
        let (element, dependencies) = record(state, window, cx, |window, cx| {
            window.with_layout_island(island, |window| {
                let mut element = render_view(view, window, cx);
                let root = element.request_layout(window, cx);
                (element, root)
            })
        });
        let (element, root) = element;
        window.set_layout_island_root(island, root);
        state.dependencies = dependencies;
        state.render_states = window.element_states_since(states_start);
        element
    }
}

/// Runs `f`, returning what it read and noting whether it read the pointer or modifiers.
fn record<R>(
    state: &mut ViewState,
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> (R, Dependencies) {
    let outer_reads = window.input_reads.replace(InputReads::empty());
    let result = cx.record_dependencies(|cx| f(window, cx));
    let reads = window.input_reads.get();
    window.input_reads.set(outer_reads | reads);
    if !reads.is_empty() {
        let previous = state
            .input
            .take()
            .map_or(InputReads::empty(), |input| input.reads);
        state.input = Some(InputSnapshot::new(previous | reads, window));
    }
    result
}

/// A view that renders nothing.
///
/// Its `render` returns [`Empty`]. Use it as a placeholder view, for example as the root of a
/// window that has no content yet.
pub struct EmptyView;

impl Render for EmptyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}
