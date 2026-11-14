use crate::{
    AbsoluteLength, App, Bounds, DefiniteLength, Display, Edges, GridTemplate, InlineContent,
    LayoutDirection, Length, Pixels, Point, Position, ResolvedDirection, SharedString, Size, Style,
    UnicodeBidi, VerticalAlign, Window, size,
    util::{
        ceil_to_device_pixel, round_half_toward_zero, round_stroke_to_device_pixel,
        round_to_device_pixel,
    },
};
use std::{
    any::Any,
    cell::{Cell, OnceCell, RefCell},
    fmt::Debug,
    ops::Range,
    rc::Rc,
    sync::Arc,
};
use taffy::{
    BlockContext, Cache, CacheTree, Layout, LayoutBlockContainer, LayoutFlexboxContainer,
    LayoutGridContainer, LayoutInput, LayoutOutput, LayoutPartialTree, RunMode,
    TraversePartialTree, compute_block_layout, compute_cached_layout, compute_flexbox_layout,
    compute_grid_layout, compute_hidden_layout, compute_leaf_layout, compute_root_layout,
    geometry::{Point as TaffyPoint, Rect as TaffyRect, Size as TaffySize},
    prelude::{max_content, min_content},
    style::AvailableSpace as TaffyAvailableSpace,
    tree::NodeId,
};

#[cfg(feature = "stacker")]
type StackSafe<T> = stacksafe::StackSafe<T>;
#[cfg(not(feature = "stacker"))]
type StackSafe<T> = T;

type MeasureFn =
    dyn FnMut(Size<Option<Pixels>>, Size<AvailableSpace>, &mut Window, &mut App) -> Size<Pixels>;
type NodeMeasureFn = StackSafe<Box<MeasureFn>>;

/// What a measured node's size depends on, besides the space it's measured in. When a view
/// renders again, a measured node whose key matches the one its predecessor had keeps that node's
/// layout, so it isn't measured again.
pub(crate) trait MeasureKey: Any {
    /// Whether a node with this key measures the same as the node that had `previous`.
    fn same(&self, previous: &dyn MeasureKey) -> bool;

    /// Takes over what the element of the node with `previous` measured, since this node won't be
    /// measured. Without it, the element's measure function runs once, with the last sizes.
    fn inherit(&self, _previous: &dyn MeasureKey) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any;
}

#[derive(Clone)]
pub(crate) struct LayoutDirectionHandle(Rc<Cell<ResolvedDirection>>);

impl LayoutDirectionHandle {
    pub(crate) fn get(&self) -> ResolvedDirection {
        self.0.get()
    }
}

struct DirectionMetadata {
    authored: LayoutDirection,
    unicode_bidi: UnicodeBidi,
    source_direction: Option<ResolvedDirection>,
    auto_direction_hint: Option<ResolvedDirection>,
    logical_children: Option<Vec<LayoutId>>,
    resolved: ResolvedDirection,
    /// Shared with [`LayoutDirectionHandle`]s, made when the first one is requested.
    handle: OnceCell<Rc<Cell<ResolvedDirection>>>,
}

enum NodeKind {
    Plain,
    Measured(NodeMeasureFn),
    /// Stands in for another view's island. Layout delegates to the island's root, and the
    /// island hangs off this node when positions are read.
    Proxy {
        island: IslandId,
        style: Option<Box<taffy::Style>>,
    },
}

struct Node {
    style: taffy::Style,
    children: Vec<NodeId>,
    parent: Option<LayoutId>,
    cache: Cache,
    layout: Layout,
    kind: NodeKind,
    vertical_align: VerticalAlign,
    display: Display,
    position: Position,
    direction: DirectionMetadata,
    inline_content: Option<Arc<InlineContent>>,
    inline_fragments: Option<Arc<[Bounds<Pixels>]>>,
    /// An absolute position assigned by an inline formatting context instead of by Taffy.
    placed: Option<(Point<f32>, Bounds<Pixels>)>,
    /// Absolute outer origin in device pixels and snapped bounds, valid for `bounds_epoch`.
    bounds: Option<(u64, Point<f32>, Bounds<Pixels>)>,
    /// Whether the node kept the layout of the node at its place in the view's previous render,
    /// because nothing that affects its layout changed.
    inherited: bool,
    /// The key of a measured node. Measured nodes without one are always measured again.
    measure_key: Option<Box<dyn MeasureKey>>,
    /// The known and available sizes of the last measurement.
    last_measure: Option<(Size<Option<Pixels>>, Size<AvailableSpace>)>,
    /// Whether the node's measure function ran since the node was created.
    measured: bool,
}

/// The id of a layout island: the nodes one view (or the window itself) requested.
pub(crate) type IslandId = u32;

/// The window's own island, for nodes requested outside any view. Cleared every frame.
const WINDOW_ISLAND: IslandId = 0;

#[derive(Default)]
struct Island {
    nodes: Vec<Node>,
    /// The nodes of the render before the one in progress, which new nodes inherit layout from.
    previous: Vec<Node>,
    /// Whether the island rendered this frame into a layout different from the last one.
    changed: bool,
    root: Option<LayoutId>,
    /// The node in the parent island whose position this island's root takes.
    anchor: Option<LayoutId>,
    /// The inherited direction this island was last resolved with.
    resolved_with: Option<ResolvedDirection>,
}

/// Keeps a view's island alive. Dropping it (with the view's element state) frees the island
/// at the end of the frame.
pub(crate) struct LayoutIsland {
    id: IslandId,
    released: Rc<RefCell<Vec<IslandId>>>,
}

impl LayoutIsland {
    pub(crate) fn id(&self) -> IslandId {
        self.id
    }
}

impl Drop for LayoutIsland {
    fn drop(&mut self) {
        self.released.borrow_mut().push(self.id);
    }
}

/// Computes element layout with Taffy, a flexbox and grid engine.
///
/// Nodes live in islands. Each view keeps its own island across frames, so a view that didn't
/// change keeps its laid-out nodes, and its parent reaches it through a single proxy node. Nodes
/// requested outside any view live in the window's island, which is cleared every frame. Window
/// code creates nodes with [`Self::request_layout`] or [`Self::request_measured_layout`], calls
/// [`Self::compute_layout`] on a root and reads positions with [`Self::layout_bounds`].
pub struct TaffyLayoutEngine {
    islands: Vec<Island>,
    free_islands: Vec<IslandId>,
    released: Rc<RefCell<Vec<IslandId>>>,
    current: Vec<IslandId>,
    computed_available_spaces: Vec<(LayoutId, Size<AvailableSpace>)>,
    bounds_epoch: u64,
    /// Islands rendered this frame.
    rendered: Vec<IslandId>,
    /// Measured nodes that inherited a layout, which must give their elements the measurement
    /// they would have made.
    replays: Vec<LayoutId>,
}

impl TaffyLayoutEngine {
    /// Creates an engine with no nodes.
    pub fn new() -> Self {
        TaffyLayoutEngine {
            islands: vec![Island::default()],
            free_islands: Vec::new(),
            released: Rc::default(),
            current: vec![WINDOW_ISLAND],
            computed_available_spaces: Vec::new(),
            bounds_epoch: 0,
            rendered: Vec::new(),
            replays: Vec::new(),
        }
    }

    /// Ends a frame: clears the window's island and frees the islands of views that were dropped.
    ///
    /// Ids of nodes in the window's island are invalid afterwards. View islands persist.
    pub fn clear(&mut self) {
        self.islands[WINDOW_ISLAND as usize] = Island::default();
        for id in self.released.borrow_mut().drain(..) {
            self.islands[id as usize] = Island::default();
            self.free_islands.push(id);
        }
        debug_assert_eq!(self.current, [WINDOW_ISLAND]);
        for island in self.rendered.drain(..) {
            self.islands[island as usize].changed = false;
        }
        self.replays.clear();
        self.computed_available_spaces.clear();
        self.bounds_epoch += 1;
    }

    /// Creates an empty island for a view.
    pub(crate) fn create_island(&mut self) -> LayoutIsland {
        let id = self.free_islands.pop().unwrap_or_else(|| {
            self.islands.push(Island::default());
            (self.islands.len() - 1) as IslandId
        });
        LayoutIsland {
            id,
            released: self.released.clone(),
        }
    }

    /// Clears `island` and sends node requests into it until [`Self::end_island`]. Nodes that
    /// match the island's previous nodes keep their layout.
    pub(crate) fn begin_island(&mut self, island: IslandId) {
        let island_state = &mut self.islands[island as usize];
        // Swap the buffers so both keep their capacity across renders.
        std::mem::swap(&mut island_state.previous, &mut island_state.nodes);
        island_state.nodes.clear();
        island_state.root = None;
        island_state.resolved_with = None;
        island_state.changed = true;
        self.rendered.push(island);
        self.current.push(island);
    }

    pub(crate) fn end_island(&mut self) {
        if let Some(island) = self.current.pop() {
            self.islands[island as usize].previous.clear();
        }
    }

    pub(crate) fn set_island_root(&mut self, island: IslandId, root: LayoutId) {
        debug_assert_eq!(root.island(), island);
        let inherited = self.node(root).inherited;
        let island = &mut self.islands[island as usize];
        island.root = Some(root);
        island.changed = !inherited;
    }

    /// Adds a node to the current island that lays out `island`'s root in its place.
    pub(crate) fn request_island(&mut self, island: IslandId) -> LayoutId {
        let id = self.push_node(
            taffy::Style::default(),
            &[],
            NodeKind::Proxy {
                island,
                style: None,
            },
            None,
            &Style::default(),
        );
        self.islands[island as usize].anchor = Some(id);
        id
    }

    /// Hangs `island` off `anchor`, a node laid out at the island root's position and size.
    pub(crate) fn anchor_island(&mut self, island: IslandId, anchor: LayoutId) {
        self.islands[island as usize].anchor = Some(anchor);
    }

    fn root_style(&self, island: IslandId) -> Option<&taffy::Style> {
        let root = self.islands[island as usize].root?;
        Some(&self.node(root).style)
    }

    fn current_island(&self) -> IslandId {
        *self.current.last().unwrap()
    }

    fn node(&self, id: LayoutId) -> &Node {
        &self.islands[id.island() as usize].nodes[id.index()]
    }

    fn node_mut(&mut self, id: LayoutId) -> &mut Node {
        &mut self.islands[id.island() as usize].nodes[id.index()]
    }

    fn push_node(
        &mut self,
        style: taffy::Style,
        children: &[LayoutId],
        kind: NodeKind,
        measure_key: Option<Box<dyn MeasureKey>>,
        source: &Style,
    ) -> LayoutId {
        let island = self.current_island();
        let index = self.islands[island as usize].nodes.len();
        let id = LayoutId::new(island, index);
        for child in children {
            self.node_mut(*child).parent = Some(id);
        }
        self.islands[island as usize].nodes.push(Node {
            style,
            children: children.iter().map(|child| NodeId::from(child.0)).collect(),
            parent: None,
            cache: Cache::new(),
            layout: Layout::new(),
            kind,
            vertical_align: source.vertical_align,
            display: source.display,
            position: source.position,
            direction: DirectionMetadata {
                authored: source.direction,
                unicode_bidi: source.effective_unicode_bidi(),
                source_direction: None,
                auto_direction_hint: None,
                logical_children: None,
                resolved: ResolvedDirection::LeftToRight,
                handle: OnceCell::new(),
            },
            inline_content: None,
            inline_fragments: None,
            placed: None,
            bounds: None,
            inherited: false,
            measure_key,
            last_measure: None,
            measured: false,
        });
        if let Some(previous) = self.islands[island as usize].previous.get(index)
            && self.can_inherit(self.node(id), previous)
        {
            let island = &mut self.islands[id.island() as usize];
            let (node, previous) = (&mut island.nodes[index], &mut island.previous[index]);
            node.inherited = true;
            std::mem::swap(&mut node.cache, &mut previous.cache);
            node.layout = previous.layout;
            node.style.direction = previous.style.direction;
            node.direction.resolved = previous.direction.resolved;
            node.last_measure = previous.last_measure;
            let measured = match (&node.measure_key, &previous.measure_key) {
                (Some(key), Some(previous_key)) => key.inherit(previous_key.as_ref()),
                _ => false,
            };
            if matches!(node.kind, NodeKind::Measured(_)) && !measured {
                self.replays.push(id);
            }
        }
        id
    }

    /// Whether `node` can keep the layout of `previous`, the node at its place in the last render,
    /// because nothing that affects its layout changed.
    fn can_inherit(&self, node: &Node, previous: &Node) -> bool {
        let same_kind = match (&node.kind, &previous.kind) {
            (NodeKind::Plain, NodeKind::Plain) => true,
            (NodeKind::Measured(_), NodeKind::Measured(_)) => {
                match (&node.measure_key, &previous.measure_key) {
                    (Some(key), Some(previous_key)) => key.same(previous_key.as_ref()),
                    _ => false,
                }
            }
            (
                NodeKind::Proxy { island, .. },
                NodeKind::Proxy {
                    island: previous_island,
                    ..
                },
            ) => island == previous_island && !self.islands[*island as usize].changed,
            _ => false,
        };
        let same = same_kind
            && node.children == previous.children
            && node
                .children
                .iter()
                .all(|child| self.node(LayoutId(u64::from(*child))).inherited)
            && node.vertical_align == previous.vertical_align
            && node.display == previous.display
            && node.position == previous.position
            && node.direction.authored == previous.direction.authored
            && node.direction.unicode_bidi == previous.direction.unicode_bidi;
        // Directions are resolved after nodes are created, so compare with the one resolved last.
        same && if node.style.direction == previous.style.direction {
            node.style == previous.style
        } else {
            taffy::Style {
                direction: previous.style.direction,
                ..node.style.clone()
            } == previous.style
        }
    }

    /// Adds a node styled by `style`, with `children` as its child nodes, and returns its id.
    ///
    /// `rem_size` and `scale_factor` resolve the style's lengths. Lengths in `rem` use
    /// `rem_size`; layout is snapped to device pixels using `scale_factor`.
    pub fn request_layout(
        &mut self,
        style: Style,
        rem_size: Pixels,
        scale_factor: f32,
        children: &[LayoutId],
    ) -> LayoutId {
        let taffy_style = style.to_taffy(rem_size, scale_factor);
        self.push_node(taffy_style, children, NodeKind::Plain, None, &style)
    }

    /// Adds a leaf node whose size comes from `measure`, and returns its id.
    ///
    /// Use it for content whose size depends on the space it gets, such as text. Taffy calls
    /// `measure` during [`Self::compute_layout`] with the known width and height, if any, and
    /// the available space along each axis. `measure` returns the content's size in logical
    /// pixels.
    pub fn request_measured_layout(
        &mut self,
        style: Style,
        rem_size: Pixels,
        scale_factor: f32,
        measure: impl FnMut(
            Size<Option<Pixels>>,
            Size<AvailableSpace>,
            &mut Window,
            &mut App,
        ) -> Size<Pixels>
        + 'static,
    ) -> LayoutId {
        self.request_keyed_measured_layout(style, rem_size, scale_factor, None, measure)
    }

    /// [`Self::request_measured_layout`] for a node whose size depends only on `key` and the
    /// space it's measured in. See [`MeasureKey`].
    pub(crate) fn request_keyed_measured_layout(
        &mut self,
        style: Style,
        rem_size: Pixels,
        scale_factor: f32,
        key: Option<Box<dyn MeasureKey>>,
        measure: impl FnMut(
            Size<Option<Pixels>>,
            Size<AvailableSpace>,
            &mut Window,
            &mut App,
        ) -> Size<Pixels>
        + 'static,
    ) -> LayoutId {
        let taffy_style = style.to_taffy(rem_size, scale_factor);
        let measure = Box::new(measure) as Box<MeasureFn>;
        #[cfg(feature = "stacker")]
        let measure = StackSafe::new(measure);
        self.push_node(taffy_style, &[], NodeKind::Measured(measure), key, &style)
    }

    pub(crate) fn set_logical_children(&mut self, node_id: LayoutId, children: &[LayoutId]) {
        let node = self.node_mut(node_id);
        let same = node.children.len() == children.len()
            && node
                .children
                .iter()
                .zip(children)
                .all(|(actual, logical)| u64::from(*actual) == logical.0);
        node.direction.logical_children = (!same).then(|| children.to_vec());
    }

    pub(crate) fn set_direction_text(&mut self, node_id: LayoutId, text: SharedString) {
        self.node_mut(node_id).direction.source_direction =
            ResolvedDirection::from_first_strong(&text);
    }

    pub(crate) fn set_auto_direction_hint(
        &mut self,
        node_id: LayoutId,
        direction: Option<ResolvedDirection>,
    ) {
        self.node_mut(node_id).direction.auto_direction_hint = direction;
    }

    pub(crate) fn island_direction_contribution(
        &self,
        island: IslandId,
    ) -> Option<ResolvedDirection> {
        self.auto_direction_contribution(self.islands[island as usize].root?)
    }

    pub(crate) fn auto_direction_contribution(
        &self,
        node_id: LayoutId,
    ) -> Option<ResolvedDirection> {
        let node_id = self.through_proxy(node_id)?;
        (self.node(node_id).direction.authored == LayoutDirection::Inherit)
            .then(|| self.auto_direction(node_id))
            .flatten()
    }

    pub(crate) fn direction_handle(&self, node_id: LayoutId) -> LayoutDirectionHandle {
        let direction = &self.node(node_id).direction;
        LayoutDirectionHandle(
            direction
                .handle
                .get_or_init(|| Rc::new(Cell::new(direction.resolved)))
                .clone(),
        )
    }

    pub(crate) fn directionality(&self, node_id: LayoutId) -> (ResolvedDirection, UnicodeBidi) {
        let direction = &self.node(node_id).direction;
        (direction.resolved, direction.unicode_bidi)
    }

    /// Resolves a proxy to the root of the island it stands for.
    fn through_proxy(&self, node_id: LayoutId) -> Option<LayoutId> {
        match &self.node(node_id).kind {
            NodeKind::Proxy { island, .. } => self.islands[*island as usize].root,
            _ => Some(node_id),
        }
    }

    fn children_of(&self, node_id: LayoutId) -> impl Iterator<Item = LayoutId> + '_ {
        let node = self.node(node_id);
        let logical = node.direction.logical_children.as_deref();
        let actual = logical
            .is_none()
            .then(|| node.children.iter().map(|id| LayoutId(u64::from(*id))))
            .into_iter()
            .flatten();
        logical.into_iter().flatten().copied().chain(actual)
    }

    fn auto_direction(&self, node_id: LayoutId) -> Option<ResolvedDirection> {
        let direction = &self.node(node_id).direction;
        if let Some(direction) = direction.source_direction.or(direction.auto_direction_hint) {
            return Some(direction);
        }
        self.children_of(node_id)
            .find_map(|child| self.auto_direction_contribution(child))
    }

    fn resolve_directions(
        &mut self,
        root_id: LayoutId,
        inherited: ResolvedDirection,
    ) -> Vec<LayoutId> {
        let mut changed = Vec::new();
        self.resolve_direction_subtree(root_id, inherited, &mut changed);
        changed
    }

    fn resolve_direction_subtree(
        &mut self,
        node_id: LayoutId,
        inherited: ResolvedDirection,
        changed: &mut Vec<LayoutId>,
    ) {
        if let NodeKind::Proxy { island, .. } = self.node(node_id).kind {
            let island = &mut self.islands[island as usize];
            if island.resolved_with == Some(inherited) {
                return;
            }
            island.resolved_with = Some(inherited);
            if let Some(root) = island.root {
                self.resolve_direction_subtree(root, inherited, changed);
            }
            return;
        }

        let resolved = match self.node(node_id).direction.authored {
            LayoutDirection::Inherit => inherited,
            LayoutDirection::LeftToRight => ResolvedDirection::LeftToRight,
            LayoutDirection::RightToLeft => ResolvedDirection::RightToLeft,
            LayoutDirection::Auto => self
                .auto_direction(node_id)
                .unwrap_or(ResolvedDirection::LeftToRight),
        };
        let taffy_direction = match resolved {
            ResolvedDirection::LeftToRight => taffy::style::Direction::Ltr,
            ResolvedDirection::RightToLeft => taffy::style::Direction::Rtl,
        };
        let node = self.node_mut(node_id);
        if node.direction.resolved != resolved {
            changed.push(node_id);
            node.direction.resolved = resolved;
            if let Some(handle) = node.direction.handle.get() {
                handle.set(resolved);
            }
        }
        if node.style.direction != taffy_direction {
            node.style.direction = taffy_direction;
            self.mark_dirty(node_id);
        }

        let children: smallvec::SmallVec<[LayoutId; 8]> = self.children_of(node_id).collect();
        for child_id in children {
            self.resolve_direction_subtree(child_id, resolved, changed);
        }
    }

    /// Clears cached layout of `node_id` and of its ancestors, including through island anchors.
    fn mark_dirty(&mut self, mut node_id: LayoutId) {
        loop {
            let node = self.node_mut(node_id);
            node.cache.clear();
            match node.parent {
                Some(parent) => node_id = parent,
                None => match self.anchor_of_root(node_id) {
                    Some(anchor) => node_id = anchor,
                    None => return,
                },
            }
        }
    }

    fn anchor_of_root(&self, node_id: LayoutId) -> Option<LayoutId> {
        let island = &self.islands[node_id.island() as usize];
        (island.root == Some(node_id))
            .then_some(island.anchor)
            .flatten()
    }

    pub(crate) fn vertical_align(&self, node_id: LayoutId) -> VerticalAlign {
        self.node(node_id).vertical_align
    }

    /// Whether the node kept the layout it had in the view's previous render.
    pub(crate) fn inherited(&self, node_id: LayoutId) -> bool {
        self.node(node_id).inherited
    }

    pub(crate) fn display_and_position(&self, node_id: LayoutId) -> (Display, Position) {
        let node = self.node(node_id);
        (node.display, node.position)
    }

    pub(crate) fn inline_content(&self, node_id: LayoutId) -> Option<Arc<InlineContent>> {
        self.node(node_id).inline_content.clone()
    }

    pub(crate) fn set_inline_content(&mut self, node_id: LayoutId, content: InlineContent) {
        self.node_mut(node_id).inline_content = Some(Arc::new(content));
    }

    pub(crate) fn inline_fragments(&self, node_id: LayoutId) -> Option<Arc<[Bounds<Pixels>]>> {
        self.node(node_id).inline_fragments.clone()
    }

    pub(crate) fn set_inline_fragments(
        &mut self,
        node_id: LayoutId,
        fragments: Arc<[Bounds<Pixels>]>,
    ) {
        self.node_mut(node_id).inline_fragments = Some(fragments);
    }

    /// Places a detached inline box, invalidating cached positions of its descendants.
    pub(crate) fn place_inline(&mut self, node_id: LayoutId, bounds: Bounds<Pixels>, scale: f32) {
        self.bounds_epoch += 1;
        self.node_mut(node_id).placed = Some((
            bounds.origin.map(|position| f32::from(position) * scale),
            bounds,
        ));
    }

    /// Treats any `auto` dimension of the given node's style as filling `size`.
    ///
    /// This is applied to window roots before layout so they behave like the
    /// root element on the web, which stretches to fill the initial containing
    /// block (the viewport) unless given an explicit size. Explicitly styled
    /// dimensions are preserved.
    pub fn stretch_auto_size_to_fill(
        &mut self,
        id: LayoutId,
        size: Size<Pixels>,
        scale_factor: f32,
    ) {
        let style = match &self.node(id).kind {
            NodeKind::Proxy { island, .. } => self.root_style(*island),
            _ => Some(&self.node(id).style),
        };
        let Some(style) = style else { return };
        let stretch_width = style.size.width.is_auto();
        let stretch_height = style.size.height.is_auto();
        if !stretch_width && !stretch_height {
            return;
        }
        let mut style = style.clone();
        if stretch_width {
            style.size.width =
                taffy::style::Dimension::length(round_to_device_pixel(size.width.0, scale_factor));
        }
        if stretch_height {
            style.size.height =
                taffy::style::Dimension::length(round_to_device_pixel(size.height.0, scale_factor));
        }
        let node = self.node_mut(id);
        match &mut node.kind {
            NodeKind::Proxy {
                style: stretched, ..
            } => *stretched = Some(Box::new(style)),
            _ => node.style = style,
        }
        node.cache.clear();
    }

    /// Lays out the subtree under `id` for the given available space.
    ///
    /// Call it on the root node with the space the window offers. It calls each measured node's
    /// `measure` closure as needed. Read the results with [`Self::layout_bounds`] afterwards.
    /// Calling it again with a different available space recomputes the subtree.
    #[cfg_attr(feature = "stacker", stacksafe::stacksafe)]
    pub fn compute_layout(
        &mut self,
        id: LayoutId,
        available_space: Size<AvailableSpace>,
        inherited_direction: ResolvedDirection,
        window: &mut Window,
        cx: &mut App,
    ) {
        let changed_ids = self.resolve_directions(id, inherited_direction);
        for changed_id in changed_ids {
            if changed_id == id {
                continue;
            }
            let Some(&(_, available_space)) = self
                .computed_available_spaces
                .iter()
                .find(|(computed, _)| *computed == changed_id)
            else {
                continue;
            };
            self.compute_resolved_layout(changed_id, available_space, None, window, cx);
        }

        match self
            .computed_available_spaces
            .iter_mut()
            .find(|(computed, _)| *computed == id)
        {
            Some(entry) => entry.1 = available_space,
            None => self.computed_available_spaces.push((id, available_space)),
        }
        self.compute_resolved_layout(id, available_space, None, window, cx);
    }

    /// Lays out `island`'s root at exactly `size`, where its anchor already sits.
    pub(crate) fn layout_island(
        &mut self,
        island: IslandId,
        size: Size<Pixels>,
        inherited_direction: ResolvedDirection,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(root) = self.islands[island as usize].root else {
            return;
        };
        self.islands[island as usize].resolved_with = None;
        self.resolve_directions(root, inherited_direction);
        let available_space = size.map(AvailableSpace::Definite);
        self.compute_resolved_layout(root, available_space, Some(size), window, cx);
    }

    fn compute_resolved_layout(
        &mut self,
        id: LayoutId,
        available_space: Size<AvailableSpace>,
        known_size: Option<Size<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let scale_factor = window.scale_factor();
        let to_device = |space: AvailableSpace| match space {
            AvailableSpace::Definite(pixels) => AvailableSpace::Definite(pixels * scale_factor),
            other => other,
        };
        let available_space: TaffySize<TaffyAvailableSpace> = size(
            to_device(available_space.width),
            to_device(available_space.height),
        )
        .into();
        self.bounds_epoch += 1;

        let mut tree = LayoutTree {
            islands: &mut self.islands,
            window,
            cx,
            scale_factor,
        };
        match known_size {
            None => compute_root_layout(&mut tree, id.into(), available_space),
            Some(known) => {
                let known = TaffySize {
                    width: Some(known.width.0 * scale_factor),
                    height: Some(known.height.0 * scale_factor),
                };
                let output = tree.compute_child_layout(
                    id.into(),
                    LayoutInput {
                        known_dimensions: known,
                        known_dimensions_are_definite: TaffySize {
                            width: true,
                            height: true,
                        },
                        parent_size: known,
                        available_space,
                        run_mode: RunMode::PerformLayout,
                        sizing_mode: taffy::SizingMode::InherentSize,
                        axis: taffy::RequestedAxis::Both,
                        vertical_margins_are_collapsible: taffy::Line::FALSE,
                    },
                );
                let node = tree.node_mut(id.into());
                node.layout.size = output.size;
                node.layout.location = TaffyPoint::ZERO;
            }
        }
        tree.replay_measurements(&mut self.replays);
    }

    // Pixel snapping
    //
    // Painting primitives at non-integer pixel coordinates produces blurry
    // output. Pixel snapping converts layout coordinates into integer
    // device-pixel coordinates so painted edges land exactly on physical
    // pixel boundaries.
    //
    // Non-integer coordinates can arise for several reasons, including:
    //   - flex distribution, percentages, centering, and text measurement
    //     can produce fractional element sizes and positions;
    //   - at fractional scale factors (for example 125% or 150%), integer
    //     logical-pixel values can map to non-integer device-pixel values.
    //
    // We pixel-snap by rounding in device-pixel space, after multiplying
    // by `scale_factor`, so that snapping targets physical pixels. Bounds
    // are divided by `scale_factor` before being returned to GPUI.
    //
    // Midpoints are rounded toward zero. This is a stylistic choice: a
    // 1-logical-pixel line at 150% scale should render as 1 dp rather than
    // 2 dp.
    //
    // Pixel snapping is done in two phases:
    //
    //  1. Pre-layout metric snapping. Before Taffy computes layout, all
    //     authored absolute lengths are rounded in `to_taffy`. This
    //     includes borders, padding, gaps, and explicit sizes.
    //     Custom-measured leaf nodes have their measured sizes rounded up
    //     to integer device-pixel lengths.
    //
    //  2. Post-layout edge snapping. After Taffy resolves the tree, layout
    //     relationships such as flex shares, grid tracks, percentages, and
    //     centering can produce new fractional edge positions. Boxes now
    //     have edges in absolute coordinates, and snapping must decide
    //     where those edges land on the device-pixel grid.
    //
    // Ideally, post-layout snapping would satisfy:
    //
    //  - Edge closure. Two raw layout edges at the same absolute position
    //    should snap to the same pixel column.
    //  - Translation stability. A component's internal geometry should not
    //    change when it moves to a new absolute position.
    //
    // These goals are in tension because rounding is not associative.
    // The simple local schemes make different tradeoffs:
    //
    //  - Absolute edge rounding gives each window coordinate one answer,
    //    so coincident edges always close globally. But a span's snapped
    //    length is `round(far) - round(near)`, which may change by 1 dp
    //    as its absolute origin moves.
    //
    //  - Parent-relative edge rounding rounds each child inside its
    //    parent's coordinate space. This guarantees translation stability,
    //    but a shared edge reached through different parents can
    //    accumulate different rounding, causing non-closure between
    //    cousins.
    //
    //  - Length rounding rounds each width, height, and thickness
    //    independently and then places boxes from those rounded lengths.
    //    Sizes stay stable under translation, but neighboring boxes derive
    //    their shared boundary from different sources, so closure is not
    //    guaranteed.
    //
    // We apply absolute edge rounding for each element's outer box in
    // post-layout rounding to preserve closure. Border and padding widths
    // are not touched by post-layout rounding; they keep their pre-layout
    // rounded value so that they remain stable under translation.
    //
    // This gives both closure and translation stability in the case that
    // all local metrics are integer device-pixel lengths. Pre-layout
    // rounding covers that in most cases. The exception is metrics
    // resolved by layout relationships, such as percentages. Outer box
    // edges will still close globally, and painted border widths are still
    // snapped independently, but the raw content-box origin can carry a
    // 1dp residual into descendants.

    /// Returns the node's bounds in logical pixels, relative to the window.
    ///
    /// Call [`Self::compute_layout`] first. The result is cached until the next layout and is
    /// returned in logical pixels after device-pixel snapping, so neighboring boxes meet exactly.
    pub fn layout_bounds(&mut self, id: LayoutId, scale_factor: f32) -> Bounds<Pixels> {
        self.absolute_layout(id, scale_factor).1
    }

    fn absolute_layout(&mut self, id: LayoutId, scale_factor: f32) -> (Point<f32>, Bounds<Pixels>) {
        let node = self.node(id);
        if let Some(placed) = node.placed {
            return placed;
        }
        if let Some((epoch, origin, bounds)) = node.bounds
            && epoch == self.bounds_epoch
        {
            return (origin, bounds);
        }
        let location = Point::from(node.layout.location);
        let layout_size = node.layout.size;
        let parent = node.parent.or_else(|| self.anchor_of_root(id));
        let origin = match parent {
            Some(parent) => self.absolute_layout(parent, scale_factor).0 + location,
            None => location,
        };
        let far = origin + Point::from(Size::from(layout_size));
        let snapped = Bounds::from_corners(
            origin.map(round_half_toward_zero),
            far.map(round_half_toward_zero),
        );
        let bounds = (snapped / scale_factor).map(Pixels);
        let epoch = self.bounds_epoch;
        self.node_mut(id).bounds = Some((epoch, origin, bounds));
        (origin, bounds)
    }

    /// Returns bounds whose origin and size are snapped in the coordinate space of the parent.
    ///
    /// Text uses this placement so its offset within a containing element remains stable when the
    /// element moves across the device-pixel grid. Other layout boxes continue to use absolute edge
    /// snapping through [`Self::layout_bounds`] so coincident edges still close.
    pub(crate) fn parent_relative_layout_bounds(
        &mut self,
        node_id: LayoutId,
        scale_factor: f32,
    ) -> Bounds<Pixels> {
        let node = self.node(node_id);
        if node.inline_fragments.is_some() {
            return self.layout_bounds(node_id, scale_factor);
        }
        let Some(parent_id) = node.parent else {
            return self.layout_bounds(node_id, scale_factor);
        };
        let local_origin = Point::from(node.layout.location).map(round_half_toward_zero);
        let local_size = Size::from(node.layout.size).map(round_half_toward_zero);
        let parent_bounds = self.layout_bounds(parent_id, scale_factor);
        Bounds::new(
            parent_bounds.origin + (local_origin / scale_factor).map(Pixels),
            (local_size / scale_factor).map(Pixels),
        )
    }
}

/// Taffy's view of every island while a layout computes. Measured leaves get the window and app.
struct LayoutTree<'a> {
    islands: &'a mut [Island],
    window: &'a mut Window,
    cx: &'a mut App,
    scale_factor: f32,
}

impl LayoutTree<'_> {
    fn node(&self, id: NodeId) -> &Node {
        let id = LayoutId(u64::from(id));
        &self.islands[id.island() as usize].nodes[id.index()]
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        let id = LayoutId(u64::from(id));
        &mut self.islands[id.island() as usize].nodes[id.index()]
    }

    fn style(&self, id: NodeId) -> &taffy::Style {
        match &self.node(id).kind {
            NodeKind::Proxy {
                style: Some(style), ..
            } => style,
            NodeKind::Proxy { island, .. } => match self.islands[*island as usize].root {
                Some(root) => &self.node(root.into()).style,
                None => &self.node(id).style,
            },
            _ => &self.node(id).style,
        }
    }

    fn compute_node(
        &mut self,
        node_id: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node_id);
        }
        if let NodeKind::Proxy { island, style } = &self.node(node_id).kind {
            let Some(root) = self.islands[*island as usize].root else {
                return LayoutOutput::HIDDEN;
            };
            // A stretched window root carries its size on the proxy.
            let mut inputs = inputs;
            if let Some(style) = style {
                let size = TaffySize {
                    width: style.size.width.into_option(),
                    height: style.size.height.into_option(),
                };
                inputs.known_dimensions = inputs.known_dimensions.or(size);
            }
            return self.compute_node(root.into(), inputs, block_ctx);
        }
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            let node = tree.node(node_id);
            match (node.style.display, !node.children.is_empty()) {
                (taffy::Display::None, _) => compute_hidden_layout(tree, node_id),
                (taffy::Display::Block, true) => {
                    compute_block_layout(tree, node_id, inputs, block_ctx)
                }
                (taffy::Display::FlowRoot, true) => {
                    compute_block_layout(tree, node_id, inputs, None)
                }
                (taffy::Display::Flex, true) => compute_flexbox_layout(tree, node_id, inputs),
                (taffy::Display::Grid, true) => compute_grid_layout(tree, node_id, inputs),
                (_, false) => tree.compute_leaf(node_id, inputs),
            }
        })
    }

    fn compute_leaf(&mut self, node_id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let scale_factor = self.scale_factor;
        let id = LayoutId(u64::from(node_id));
        let Node {
            style,
            kind,
            direction,
            last_measure,
            measured,
            ..
        } = &mut self.islands[id.island() as usize].nodes[id.index()];
        let NodeKind::Measured(measure) = kind else {
            return compute_leaf_layout(inputs, style, |_, _| 0.0, |_, _| TaffySize::ZERO);
        };
        let unicode_bidi = direction.unicode_bidi;
        let window = &mut *self.window;
        let cx = &mut *self.cx;
        compute_leaf_layout(
            inputs,
            style,
            |_, _| 0.0,
            |known, available| {
                let known = Size {
                    width: known.width.map(|e| Pixels(e / scale_factor)),
                    height: known.height.map(|e| Pixels(e / scale_factor)),
                };
                let from_device = |space: AvailableSpace| match space {
                    AvailableSpace::Definite(pixels) => {
                        AvailableSpace::Definite(Pixels(pixels.0 / scale_factor))
                    }
                    other => other,
                };
                let available: Size<AvailableSpace> = available.into();
                let available = size(from_device(available.width), from_device(available.height));
                *last_measure = Some((known, available));
                *measured = true;
                let size = call_measure(
                    measure,
                    style.direction,
                    unicode_bidi,
                    known,
                    available,
                    window,
                    cx,
                );
                snap_measured_size_to_device_pixels(size, scale_factor).into()
            },
        )
    }
}

/// Calls a measure function in the direction its node lays out in.
fn call_measure(
    measure: &mut NodeMeasureFn,
    direction: taffy::style::Direction,
    unicode_bidi: UnicodeBidi,
    known: Size<Option<Pixels>>,
    available: Size<AvailableSpace>,
    window: &mut Window,
    cx: &mut App,
) -> Size<Pixels> {
    let direction = match direction {
        taffy::style::Direction::Ltr => ResolvedDirection::LeftToRight,
        taffy::style::Direction::Rtl => ResolvedDirection::RightToLeft,
    };
    let previous_direction = std::mem::replace(&mut window.measurement_direction, direction);
    let previous_unicode_bidi =
        std::mem::replace(&mut window.measurement_unicode_bidi, unicode_bidi);
    let size = measure(known, available, window, cx);
    window.measurement_direction = previous_direction;
    window.measurement_unicode_bidi = previous_unicode_bidi;
    size
}

impl LayoutTree<'_> {
    /// Gives the elements of measured nodes that kept their layout, and weren't measured since,
    /// the measurement they made last time.
    fn replay_measurements(&mut self, replays: &mut Vec<LayoutId>) {
        for id in replays.drain(..) {
            let Some(Node {
                style,
                kind: NodeKind::Measured(measure),
                direction,
                last_measure: Some((known, available)),
                measured,
                inherited: true,
                ..
            }) = self.islands[id.island() as usize].nodes.get_mut(id.index())
            else {
                continue;
            };
            if *measured {
                continue;
            }
            *measured = true;
            call_measure(
                measure,
                style.direction,
                direction.unicode_bidi,
                *known,
                *available,
                self.window,
                self.cx,
            );
        }
    }
}

impl TraversePartialTree for LayoutTree<'_> {
    type ChildIter<'a>
        = std::iter::Copied<std::slice::Iter<'a, NodeId>>
    where
        Self: 'a;

    fn child_ids(&self, parent: NodeId) -> Self::ChildIter<'_> {
        self.node(parent).children.iter().copied()
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.node(parent).children.len()
    }

    fn get_child_id(&self, parent: NodeId, index: usize) -> NodeId {
        self.node(parent).children[index]
    }
}

impl LayoutPartialTree for LayoutTree<'_> {
    type CoreContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    type CustomIdent = String;

    fn get_core_container_style(&self, node_id: NodeId) -> Self::CoreContainerStyle<'_> {
        self.style(node_id)
    }

    fn set_unrounded_layout(&mut self, node_id: NodeId, layout: &Layout) {
        self.node_mut(node_id).layout = *layout;
        if let NodeKind::Proxy { island, .. } = self.node(node_id).kind
            && let Some(root) = self.islands[island as usize].root
        {
            self.node_mut(root.into()).layout = Layout {
                location: TaffyPoint::ZERO,
                ..*layout
            };
        }
    }

    fn resolve_calc_value(&self, _val: *const (), _basis: f32) -> f32 {
        0.0
    }

    fn compute_child_layout(&mut self, node_id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        self.compute_node(node_id, inputs, None)
    }
}

impl CacheTree for LayoutTree<'_> {
    fn cache_get(&mut self, node_id: NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.node_mut(node_id).cache.get(input)
    }

    fn cache_store(&mut self, node_id: NodeId, input: &LayoutInput, output: LayoutOutput) {
        self.node_mut(node_id).cache.store(input, output)
    }

    fn cache_clear(&mut self, node_id: NodeId) {
        self.node_mut(node_id).cache.clear();
    }
}

impl LayoutBlockContainer for LayoutTree<'_> {
    type BlockContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;
    type BlockItemStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    fn get_block_container_style(&self, node_id: NodeId) -> Self::BlockContainerStyle<'_> {
        self.style(node_id)
    }

    fn get_block_child_style(&self, child_node_id: NodeId) -> Self::BlockItemStyle<'_> {
        self.style(child_node_id)
    }

    fn compute_block_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        self.compute_node(node_id, inputs, block_ctx)
    }
}

impl LayoutFlexboxContainer for LayoutTree<'_> {
    type FlexboxContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;
    type FlexboxItemStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, node_id: NodeId) -> Self::FlexboxContainerStyle<'_> {
        self.style(node_id)
    }

    fn get_flexbox_child_style(&self, child_node_id: NodeId) -> Self::FlexboxItemStyle<'_> {
        self.style(child_node_id)
    }
}

impl LayoutGridContainer for LayoutTree<'_> {
    type GridContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;
    type GridItemStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    fn get_grid_container_style(&self, node_id: NodeId) -> Self::GridContainerStyle<'_> {
        self.style(node_id)
    }

    fn get_grid_child_style(&self, child_node_id: NodeId) -> Self::GridItemStyle<'_> {
        self.style(child_node_id)
    }
}

/// An id for a node in the layout tree.
///
/// [`Window::request_layout`](crate::Window::request_layout) returns one for each element. Pass
/// the ids of an element's children when you request its layout, and read a node's position
/// with the window's layout bounds methods. An id is valid only in the frame that created it.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct LayoutId(u64);

impl LayoutId {
    fn new(island: IslandId, index: usize) -> Self {
        Self((u64::from(island) << 32) | index as u64)
    }

    pub(crate) fn island(self) -> IslandId {
        (self.0 >> 32) as IslandId
    }

    fn index(self) -> usize {
        self.0 as u32 as usize
    }
}

impl From<LayoutId> for NodeId {
    fn from(layout_id: LayoutId) -> NodeId {
        NodeId::from(layout_id.0)
    }
}

fn snap_measured_size_to_device_pixels(size: Size<Pixels>, scale_factor: f32) -> Size<f32> {
    size.map(|d| ceil_to_device_pixel(d.0.max(0.0), scale_factor))
}

fn border_widths_to_taffy(
    widths: &Edges<AbsoluteLength>,
    rem_size: Pixels,
    scale_factor: f32,
) -> TaffyRect<taffy::style::LengthPercentage> {
    let snap = |w: &AbsoluteLength| {
        taffy::style::LengthPercentage::length(round_stroke_to_device_pixel(
            w.to_pixels(rem_size).0,
            scale_factor,
        ))
    };
    TaffyRect {
        top: snap(&widths.top),
        right: snap(&widths.right),
        bottom: snap(&widths.bottom),
        left: snap(&widths.left),
    }
}

// Converts padding, snapping each auto-sized axis as a proportional pair.
fn padding_to_taffy(
    padding: &Edges<DefiniteLength>,
    rem_size: Pixels,
    scale_factor: f32,
    proportional_horizontal: bool,
    proportional_vertical: bool,
) -> TaffyRect<taffy::style::LengthPercentage> {
    // Snaps the combined length, then divides it by the authored ratio.
    fn snap_absolute_pair(
        first: AbsoluteLength,
        second: AbsoluteLength,
        rem_size: Pixels,
        scale_factor: f32,
    ) -> (f32, f32) {
        let first = first.to_pixels(rem_size).0;
        let second = second.to_pixels(rem_size).0;
        let total = first + second;

        if first < 0.0 || second < 0.0 || total == 0.0 {
            return (
                round_to_device_pixel(first, scale_factor),
                round_to_device_pixel(second, scale_factor),
            );
        }

        let snapped_total = round_to_device_pixel(total, scale_factor);
        let snapped_first = snapped_total * (first / total);

        (snapped_first, snapped_total - snapped_first)
    }

    // Keeps normal per-edge conversion for explicit axes and percentage padding.
    fn convert_pair(
        first: &DefiniteLength,
        second: &DefiniteLength,
        rem_size: Pixels,
        scale_factor: f32,
        proportional: bool,
    ) -> (
        taffy::style::LengthPercentage,
        taffy::style::LengthPercentage,
    ) {
        if proportional
            && let (DefiniteLength::Absolute(first), DefiniteLength::Absolute(second)) =
                (*first, *second)
        {
            let (first, second) = snap_absolute_pair(first, second, rem_size, scale_factor);

            (
                taffy::style::LengthPercentage::length(first),
                taffy::style::LengthPercentage::length(second),
            )
        } else {
            (
                first.to_taffy(rem_size, scale_factor),
                second.to_taffy(rem_size, scale_factor),
            )
        }
    }

    let (left, right) = convert_pair(
        &padding.left,
        &padding.right,
        rem_size,
        scale_factor,
        proportional_horizontal,
    );
    let (top, bottom) = convert_pair(
        &padding.top,
        &padding.bottom,
        rem_size,
        scale_factor,
        proportional_vertical,
    );

    TaffyRect {
        top,
        right,
        bottom,
        left,
    }
}

trait ToTaffy<Output> {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> Output;
}

impl ToTaffy<taffy::style::Style> for Style {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::Style {
        use taffy::style_helpers::{fr, length, minmax, repeat};

        fn to_grid_line(
            placement: &Range<crate::GridPlacement>,
        ) -> taffy::Line<taffy::GridPlacement> {
            taffy::Line {
                start: placement.start.into(),
                end: placement.end.into(),
            }
        }

        fn to_grid_repeat<T: taffy::style::CheapCloneStr>(
            unit: &Option<GridTemplate>,
        ) -> Vec<taffy::GridTemplateComponent<T>> {
            unit.map(|template| {
                match template.min_size {
                    // grid-template-*: repeat(<number>, minmax(0, 1fr));
                    crate::GridTemplateMinSize::Zero => {
                        vec![repeat(
                            template.repeat,
                            vec![minmax(length(0.0_f32), fr(1.0_f32))],
                        )]
                    }
                    // grid-template-*: repeat(<number>, minmax(min-content, 1fr));
                    crate::GridTemplateMinSize::MinContent => {
                        vec![repeat(
                            template.repeat,
                            vec![minmax(min_content(), fr(1.0_f32))],
                        )]
                    }
                    // grid-template-*: repeat(<number>, minmax(0, max-content))
                    crate::GridTemplateMinSize::MaxContent => {
                        vec![repeat(
                            template.repeat,
                            vec![minmax(length(0.0_f32), max_content())],
                        )]
                    }
                }
            })
            .unwrap_or_default()
        }

        taffy::style::Style {
            display: self.display.into(),
            direction: taffy::style::Direction::Ltr,
            overflow: self.overflow.into(),
            scrollbar_width: self.scrollbar_width.to_taffy(rem_size, scale_factor),
            position: self.position.into(),
            inset: self.inset.to_taffy(rem_size, scale_factor),
            size: self.size.to_taffy(rem_size, scale_factor),
            min_size: self.min_size.to_taffy(rem_size, scale_factor),
            max_size: self.max_size.to_taffy(rem_size, scale_factor),
            aspect_ratio: self.aspect_ratio,
            margin: self.margin.to_taffy(rem_size, scale_factor),
            padding: padding_to_taffy(
                &self.padding,
                rem_size,
                scale_factor,
                self.size.width == Length::Auto,
                self.size.height == Length::Auto,
            ),
            border: border_widths_to_taffy(&self.border_widths, rem_size, scale_factor),
            align_items: self.align_items.map(|x| x.into()),
            align_self: self.align_self.map(|x| x.into()),
            align_content: self.align_content.map(|x| x.into()),
            justify_content: self.justify_content.map(|x| x.into()),
            gap: self.gap.to_taffy(rem_size, scale_factor),
            flex_direction: self.flex_direction.into(),
            flex_wrap: self.flex_wrap.into(),
            flex_basis: self.flex_basis.to_taffy(rem_size, scale_factor),
            flex_grow: self.flex_grow,
            flex_shrink: self.flex_shrink,
            grid_template_rows: to_grid_repeat(&self.grid_rows),
            grid_template_columns: to_grid_repeat(&self.grid_cols),
            grid_row: self
                .grid_location
                .as_ref()
                .map(|location| to_grid_line(&location.row))
                .unwrap_or_default(),
            grid_column: self
                .grid_location
                .as_ref()
                .map(|location| to_grid_line(&location.column))
                .unwrap_or_default(),
            ..Default::default()
        }
    }
}

impl ToTaffy<f32> for AbsoluteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> f32 {
        round_to_device_pixel(self.to_pixels(rem_size).0, scale_factor)
    }
}

impl ToTaffy<taffy::style::LengthPercentageAuto> for Length {
    fn to_taffy(
        &self,
        rem_size: Pixels,
        scale_factor: f32,
    ) -> taffy::prelude::LengthPercentageAuto {
        match self {
            Length::Definite(length) => length.to_taffy(rem_size, scale_factor),
            Length::Auto => taffy::prelude::LengthPercentageAuto::auto(),
        }
    }
}

impl ToTaffy<taffy::style::Dimension> for Length {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::prelude::Dimension {
        match self {
            Length::Definite(length) => length.to_taffy(rem_size, scale_factor),
            Length::Auto => taffy::prelude::Dimension::auto(),
        }
    }
}

impl ToTaffy<taffy::style::LengthPercentage> for DefiniteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::LengthPercentage {
        match self {
            DefiniteLength::Absolute(length) => length.to_taffy(rem_size, scale_factor),
            DefiniteLength::Relative(relative) => {
                taffy::style::LengthPercentage::percent(relative.0)
            }
        }
    }
}

impl ToTaffy<taffy::style::LengthPercentageAuto> for DefiniteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::LengthPercentageAuto {
        match self {
            DefiniteLength::Absolute(length) => length.to_taffy(rem_size, scale_factor),
            DefiniteLength::Relative(relative) => {
                taffy::style::LengthPercentageAuto::percent(relative.0)
            }
        }
    }
}

impl ToTaffy<taffy::style::Dimension> for DefiniteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::Dimension {
        match self {
            DefiniteLength::Absolute(length) => length.to_taffy(rem_size, scale_factor),
            DefiniteLength::Relative(relative) => taffy::style::Dimension::percent(relative.0),
        }
    }
}

impl ToTaffy<taffy::style::LengthPercentage> for AbsoluteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::LengthPercentage {
        taffy::style::LengthPercentage::length(self.to_taffy(rem_size, scale_factor))
    }
}

impl ToTaffy<taffy::style::LengthPercentageAuto> for AbsoluteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::LengthPercentageAuto {
        taffy::style::LengthPercentageAuto::length(self.to_taffy(rem_size, scale_factor))
    }
}

impl ToTaffy<taffy::style::Dimension> for AbsoluteLength {
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> taffy::style::Dimension {
        taffy::style::Dimension::length(self.to_taffy(rem_size, scale_factor))
    }
}

impl<T, T2> From<TaffyPoint<T>> for Point<T2>
where
    T: Into<T2>,
    T2: Clone + Debug + Default + PartialEq,
{
    fn from(point: TaffyPoint<T>) -> Point<T2> {
        Point {
            x: point.x.into(),
            y: point.y.into(),
        }
    }
}

impl<T, T2> From<Point<T>> for TaffyPoint<T2>
where
    T: Into<T2> + Clone + Debug + Default + PartialEq,
{
    fn from(val: Point<T>) -> Self {
        TaffyPoint {
            x: val.x.into(),
            y: val.y.into(),
        }
    }
}

impl<T, U> ToTaffy<TaffySize<U>> for Size<T>
where
    T: ToTaffy<U> + Clone + Debug + Default + PartialEq,
{
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> TaffySize<U> {
        TaffySize {
            width: self.width.to_taffy(rem_size, scale_factor),
            height: self.height.to_taffy(rem_size, scale_factor),
        }
    }
}

impl<T, U> ToTaffy<TaffyRect<U>> for Edges<T>
where
    T: ToTaffy<U> + Clone + Debug + Default + PartialEq,
{
    fn to_taffy(&self, rem_size: Pixels, scale_factor: f32) -> TaffyRect<U> {
        TaffyRect {
            top: self.top.to_taffy(rem_size, scale_factor),
            right: self.right.to_taffy(rem_size, scale_factor),
            bottom: self.bottom.to_taffy(rem_size, scale_factor),
            left: self.left.to_taffy(rem_size, scale_factor),
        }
    }
}

impl<T, U> From<TaffySize<T>> for Size<U>
where
    T: Into<U>,
    U: Clone + Debug + Default + PartialEq,
{
    fn from(taffy_size: TaffySize<T>) -> Self {
        Size {
            width: taffy_size.width.into(),
            height: taffy_size.height.into(),
        }
    }
}

impl<T, U> From<Size<T>> for TaffySize<U>
where
    T: Into<U> + Clone + Debug + Default + PartialEq,
{
    fn from(size: Size<T>) -> Self {
        TaffySize {
            width: size.width.into(),
            height: size.height.into(),
        }
    }
}

/// How much space an element is given along one axis when it is laid out.
///
/// Measured elements receive one of these in their `measure` closure. A
/// [`Definite`](Self::Definite) amount is a fixed width or height. The other two ask for the
/// element's natural size, as CSS `min-content` and `max-content` do.
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq)]
pub enum AvailableSpace {
    /// The element has exactly this many logical pixels of space.
    Definite(Pixels),
    /// The element is sized to its smallest possible content, as with CSS `min-content`.
    #[default]
    MinContent,
    /// The element is sized to its largest natural content, as with CSS `max-content`.
    MaxContent,
}

impl AvailableSpace {
    /// Returns a `Size` with both width and height set to `AvailableSpace::MinContent`.
    ///
    /// Use it when an element should be measured at its smallest content size on both axes.
    ///
    /// # Examples
    ///
    /// ```
    /// use gpui::AvailableSpace;
    /// let min_content_size = AvailableSpace::min_size();
    /// assert_eq!(min_content_size.width, AvailableSpace::MinContent);
    /// assert_eq!(min_content_size.height, AvailableSpace::MinContent);
    /// ```
    pub const fn min_size() -> Size<Self> {
        Size {
            width: Self::MinContent,
            height: Self::MinContent,
        }
    }
}

impl From<AvailableSpace> for TaffyAvailableSpace {
    fn from(space: AvailableSpace) -> TaffyAvailableSpace {
        match space {
            AvailableSpace::Definite(Pixels(value)) => TaffyAvailableSpace::Definite(value),
            AvailableSpace::MinContent => TaffyAvailableSpace::MinContent,
            AvailableSpace::MaxContent => TaffyAvailableSpace::MaxContent,
        }
    }
}

impl From<TaffyAvailableSpace> for AvailableSpace {
    fn from(space: TaffyAvailableSpace) -> AvailableSpace {
        match space {
            TaffyAvailableSpace::Definite(value) => AvailableSpace::Definite(Pixels(value)),
            TaffyAvailableSpace::MinContent => AvailableSpace::MinContent,
            TaffyAvailableSpace::MaxContent => AvailableSpace::MaxContent,
        }
    }
}

impl From<Pixels> for AvailableSpace {
    fn from(pixels: Pixels) -> Self {
        AvailableSpace::Definite(pixels)
    }
}

impl From<Size<Pixels>> for Size<AvailableSpace> {
    fn from(size: Size<Pixels>) -> Self {
        Size {
            width: AvailableSpace::Definite(size.width),
            height: AvailableSpace::Definite(size.height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::px;

    fn direction_style(direction: LayoutDirection) -> Style {
        Style {
            direction,
            ..Default::default()
        }
    }

    #[test]
    fn direction_defaults_to_ltr_and_inherits_through_overrides() {
        let mut engine = TaffyLayoutEngine::new();
        let inherited_grandchild = engine.request_layout(
            direction_style(LayoutDirection::Inherit),
            px(16.0),
            1.0,
            &[],
        );
        let rtl_child = engine.request_layout(
            direction_style(LayoutDirection::RightToLeft),
            px(16.0),
            1.0,
            &[inherited_grandchild],
        );
        let root = engine.request_layout(
            direction_style(LayoutDirection::Inherit),
            px(16.0),
            1.0,
            &[rtl_child],
        );

        let _ = engine.resolve_directions(root, ResolvedDirection::LeftToRight);

        assert_eq!(
            engine.direction_handle(root).get(),
            ResolvedDirection::LeftToRight
        );
        assert_eq!(
            engine.direction_handle(rtl_child).get(),
            ResolvedDirection::RightToLeft
        );
        assert_eq!(
            engine.node(rtl_child).style.direction,
            taffy::style::Direction::Rtl
        );
        assert_eq!(
            engine.direction_handle(inherited_grandchild).get(),
            ResolvedDirection::RightToLeft
        );
    }

    #[test]
    fn auto_direction_uses_first_eligible_strong_descendant() {
        let mut engine = TaffyLayoutEngine::new();
        let neutral = engine.request_layout(
            direction_style(LayoutDirection::Inherit),
            px(16.0),
            1.0,
            &[],
        );
        engine.set_direction_text(neutral, "123 …".into());
        let arabic = engine.request_layout(
            direction_style(LayoutDirection::Inherit),
            px(16.0),
            1.0,
            &[],
        );
        engine.set_direction_text(arabic, "مرحبا".into());
        let root = engine.request_layout(
            direction_style(LayoutDirection::Auto),
            px(16.0),
            1.0,
            &[neutral, arabic],
        );

        let _ = engine.resolve_directions(root, ResolvedDirection::LeftToRight);

        assert_eq!(
            engine.direction_handle(root).get(),
            ResolvedDirection::RightToLeft
        );
        assert_eq!(
            engine.direction_handle(neutral).get(),
            ResolvedDirection::RightToLeft
        );
    }

    #[test]
    fn auto_direction_excludes_directional_subtrees_and_falls_back_to_ltr() {
        let mut engine = TaffyLayoutEngine::new();
        let explicit_child = engine.request_layout(
            direction_style(LayoutDirection::RightToLeft),
            px(16.0),
            1.0,
            &[],
        );
        engine.set_direction_text(explicit_child, "مرحبا".into());
        let root = engine.request_layout(
            direction_style(LayoutDirection::Auto),
            px(16.0),
            1.0,
            &[explicit_child],
        );

        let _ = engine.resolve_directions(root, ResolvedDirection::LeftToRight);

        assert_eq!(
            engine.direction_handle(root).get(),
            ResolvedDirection::LeftToRight
        );
        assert_eq!(
            engine.direction_handle(explicit_child).get(),
            ResolvedDirection::RightToLeft
        );
    }

    #[test]
    fn auto_direction_updates_when_source_text_changes() {
        let mut engine = TaffyLayoutEngine::new();
        let text = engine.request_layout(
            direction_style(LayoutDirection::Inherit),
            px(16.0),
            1.0,
            &[],
        );
        let root = engine.request_layout(
            direction_style(LayoutDirection::Auto),
            px(16.0),
            1.0,
            &[text],
        );
        engine.set_direction_text(text, "English".into());
        let _ = engine.resolve_directions(root, ResolvedDirection::LeftToRight);
        assert_eq!(
            engine.direction_handle(root).get(),
            ResolvedDirection::LeftToRight
        );

        engine.set_direction_text(text, "مرحبا".into());
        let changed = engine.resolve_directions(root, ResolvedDirection::LeftToRight);
        assert!(changed.contains(&root));
        assert_eq!(
            engine.direction_handle(root).get(),
            ResolvedDirection::RightToLeft
        );
    }

    #[test]
    fn auto_sized_axes_snap_padding_proportionally() {
        use taffy::style::LengthPercentage;

        let mut style = Style {
            padding: Edges {
                top: Pixels(6.5).into(),
                right: Pixels(5.0).into(),
                bottom: Pixels(6.5).into(),
                left: Pixels(2.5).into(),
            },
            ..Default::default()
        };

        let padding = style.to_taffy(Pixels(16.0), 1.0).padding;
        let expected_left = 7.0 * (2.5 / 7.5);
        assert_eq!(padding.left, LengthPercentage::length(expected_left));
        assert_eq!(padding.right, LengthPercentage::length(7.0 - expected_left));
        assert_eq!(padding.top, LengthPercentage::length(6.5));
        assert_eq!(padding.bottom, LengthPercentage::length(6.5));

        style.size.width = Length::Definite(Pixels(100.0).into());
        let padding = style.to_taffy(Pixels(16.0), 1.0).padding;
        assert_eq!(padding.left, LengthPercentage::length(2.0));
        assert_eq!(padding.right, LengthPercentage::length(5.0));
        assert_eq!(padding.top, LengthPercentage::length(6.5));
        assert_eq!(padding.bottom, LengthPercentage::length(6.5));

        style.size.width = Length::Auto;
        style.padding.top = Pixels(6.75).into();
        style.padding.bottom = Pixels(6.75).into();
        let padding = style.to_taffy(Pixels(16.0), 2.0).padding;
        assert_eq!(padding.top, LengthPercentage::length(13.5));
        assert_eq!(padding.bottom, LengthPercentage::length(13.5));
    }

    #[test]
    fn border_widths_to_taffy_use_stroke_snapping() {
        let border_widths = Edges {
            top: Pixels(0.0).into(),
            right: Pixels(0.4).into(),
            bottom: Pixels(0.5).into(),
            left: Pixels(1.6).into(),
        };
        let taffy_border = border_widths_to_taffy(&border_widths, Pixels(16.0), 1.0);

        assert_eq!(
            taffy_border.top,
            taffy::style::LengthPercentage::length(0.0)
        );
        assert_eq!(
            taffy_border.right,
            taffy::style::LengthPercentage::length(1.0)
        );
        assert_eq!(
            taffy_border.bottom,
            taffy::style::LengthPercentage::length(1.0)
        );
        assert_eq!(
            taffy_border.left,
            taffy::style::LengthPercentage::length(2.0)
        );
    }

    struct Measured {
        key: u32,
        calls: Rc<Cell<usize>>,
    }

    impl MeasureKey for u32 {
        fn same(&self, previous: &dyn MeasureKey) -> bool {
            previous.as_any().downcast_ref::<u32>() == Some(self)
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl crate::IntoElement for Measured {
        type Element = Self;

        fn into_element(self) -> Self {
            self
        }
    }

    impl crate::Element for Measured {
        type RequestLayoutState = ();
        type PrepaintState = ();

        fn id(&self) -> Option<crate::ElementId> {
            None
        }

        fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            window: &mut Window,
            _: &mut App,
        ) -> (LayoutId, ()) {
            let calls = self.calls.clone();
            let layout_id = window.request_keyed_measured_layout(
                Style::default(),
                self.key,
                move |_, _, _, _| {
                    calls.set(calls.get() + 1);
                    crate::size(px(10.), px(10.))
                },
            );
            (layout_id, ())
        }

        fn prepaint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            _: crate::Bounds<Pixels>,
            _: &mut (),
            _: &mut Window,
            _: &mut App,
        ) {
        }

        fn paint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            _: crate::Bounds<Pixels>,
            _: &mut (),
            _: &mut (),
            _: &mut Window,
            _: &mut App,
        ) {
        }
    }

    struct Row {
        keys: Vec<u32>,
        calls: Rc<Cell<usize>>,
    }

    impl crate::Render for Row {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut crate::Context<Self>,
        ) -> impl crate::IntoElement {
            use crate::{ParentElement as _, Styled as _};
            crate::div()
                .flex()
                .flex_wrap()
                .w(px(25.))
                .children(self.keys.iter().map(|key| Measured {
                    key: *key,
                    calls: self.calls.clone(),
                }))
        }
    }

    #[crate::test]
    fn unchanged_nodes_keep_their_layout_when_their_view_renders_again(
        cx: &mut crate::TestAppContext,
    ) {
        let calls = Rc::new(Cell::new(0));
        let window = cx.add_window({
            let calls = calls.clone();
            move |_, _| Row {
                keys: vec![1, 2, 3],
                calls,
            }
        });
        let measured_first = calls.get();
        assert!(measured_first >= 3);

        // Render again without changes: each node only replays its last measurement.
        calls.set(0);
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.run_until_parked();
        assert_eq!(calls.get(), 3);

        // A changed key measures that node like a new one, and its siblings still replay.
        calls.set(0);
        window
            .update(cx, |row, _, cx| {
                row.keys[1] = 20;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            calls.get() > 3,
            "the changed node is measured again: {}",
            calls.get()
        );
    }
}
