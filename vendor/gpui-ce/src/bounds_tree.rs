use crate::{Bounds, Half, ScaledPixels};
use std::{
    cmp,
    fmt::Debug,
    ops::{Add, Sub},
};

/// The side of a grid cell, in the units of the bounds. Most of a frame's bounds are a few dozen
/// device pixels across, so each reaches into one cell or a few.
const CELL: f32 = 64.;

/// The cells along each axis. Bounds past the last cell, or before the first, are kept in the
/// edge cells.
const CELLS: usize = 128;

/// The end of a cell's list of entries.
const NONE: u32 = u32::MAX;

/// A coordinate the grid can place.
pub(crate) trait GridUnit {
    fn to_f32(&self) -> f32;
}

impl GridUnit for f32 {
    fn to_f32(&self) -> f32 {
        *self
    }
}

impl GridUnit for ScaledPixels {
    fn to_f32(&self) -> f32 {
        self.0
    }
}

/// Assigns draw orders to bounds inserted one after another: each gets one more than the highest
/// order of the bounds it intersects, so primitives that don't overlap share an order and draw in
/// one batch.
///
/// The bounds live in a uniform grid. A cell keeps the highest order of the bounds that cover it
/// whole, since every query reaching into the cell meets those, and a list of the bounds that only
/// reach into part of it.
#[derive(Debug)]
pub(crate) struct BoundsTree<U>
where
    U: Clone + Debug + Default + PartialEq,
{
    /// Allocated on the first insert.
    cells: Vec<Cell>,
    /// Cells changed since the last clear.
    touched: Vec<u32>,
    entries: Vec<Entry<U>>,
    /// Every bounds inserted since the last clear, for queries without area, which the grid
    /// can't answer: they intersect bounds that strictly contain them.
    recorded: Vec<(Bounds<U>, u32)>,
    /// The bounds with the highest order so far: a query that meets them has its answer at once.
    max: Option<(Bounds<U>, u32)>,
    max_order: u32,
    /// Minimum ordering assigned to any subsequent insert. Raised before painting deferred
    /// draws so overlays always sort above the main scene (and their orders can't fall inside a
    /// content-filter order range from the main scene). 0 means no floor.
    order_floor: u32,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    /// The highest order of the bounds covering the whole cell.
    cover: u32,
    /// The highest order of any bounds reaching into the cell.
    max: u32,
    /// The newest entry reaching into part of the cell.
    head: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            cover: 0,
            max: 0,
            head: NONE,
        }
    }
}

#[derive(Debug)]
struct Entry<U>
where
    U: Clone + Debug + Default + PartialEq,
{
    bounds: Bounds<U>,
    order: u32,
    next: u32,
}

impl<U> BoundsTree<U>
where
    U: Clone
        + Debug
        + PartialEq
        + PartialOrd
        + Add<U, Output = U>
        + Sub<Output = U>
        + Half
        + Default
        + GridUnit,
{
    /// Removes every bounds and the order floor.
    pub fn clear(&mut self) {
        for &cell in &self.touched {
            self.cells[cell as usize] = Cell::default();
        }
        self.touched.clear();
        self.entries.clear();
        self.recorded.clear();
        self.max = None;
        self.max_order = 0;
        self.order_floor = 0;
    }

    /// Raise the minimum ordering for subsequent inserts to `floor`. Relative ordering above the
    /// floor is preserved (overlapping inserts still step above one another).
    pub fn set_order_floor(&mut self, floor: u32) {
        self.order_floor = self.order_floor.max(floor);
    }

    /// The highest ordering assigned to any bounds so far (0 if empty).
    pub fn max_order(&self) -> u32 {
        self.max_order
    }

    /// Inserts bounds with an ordering strictly greater than *every* existing bounds (not just
    /// intersecting ones), and returns that ordering. Used for content-filter group boundaries
    /// (which must sort after all previously-painted content so their order range can't collide
    /// with unrelated non-overlapping content that reuses low orderings) and to raise the order
    /// floor before painting deferred draws (so overlays always sort above the main scene).
    pub fn insert_above_all(&mut self, new_bounds: Bounds<U>) -> u32 {
        let ordering = self.max_order + 1;
        self.insert_with_order(new_bounds, ordering);
        ordering
    }

    /// The order floor set by [`Self::set_order_floor`].
    pub(crate) fn order_floor(&self) -> u32 {
        self.order_floor
    }

    /// Returns the highest ordering among bounds that intersect `query`, or 0.
    pub(crate) fn max_intersecting(&mut self, query: &Bounds<U>) -> u32 {
        if let Some((bounds, order)) = &self.max
            && query.intersects(bounds)
        {
            return *order;
        }
        let cells = cell_range(query);
        if cells.empty {
            return self
                .recorded
                .iter()
                .filter(|(bounds, _)| bounds.intersects(query))
                .map(|(_, order)| *order)
                .max()
                .unwrap_or(0);
        }
        let mut found = 0;
        for y in cells.y0..=cells.y1 {
            for x in cells.x0..=cells.x1 {
                let Some(cell) = self.cells.get(y * CELLS + x) else {
                    return found;
                };
                if cell.max <= found {
                    continue;
                }
                found = found.max(cell.cover);
                let mut next = cell.head;
                while next != NONE {
                    let entry = &self.entries[next as usize];
                    if entry.order > found && entry.bounds.intersects(query) {
                        found = entry.order;
                    }
                    next = entry.next;
                }
            }
        }
        found
    }

    /// Inserts bounds that already have an ordering, such as a replayed group of primitives.
    pub(crate) fn insert_with_order(&mut self, new_bounds: Bounds<U>, ordering: u32) {
        if ordering >= self.max_order || self.max.is_none() {
            self.max_order = ordering;
            self.max = Some((new_bounds.clone(), ordering));
        }
        self.recorded.push((new_bounds.clone(), ordering));
        let cells = cell_range(&new_bounds);
        if self.cells.is_empty() {
            self.cells = vec![Cell::default(); CELLS * CELLS];
        }
        let (left, top) = (new_bounds.origin.x.to_f32(), new_bounds.origin.y.to_f32());
        let right = left + new_bounds.size.width.to_f32();
        let bottom = top + new_bounds.size.height.to_f32();
        for y in cells.y0..=cells.y1 {
            for x in cells.x0..=cells.x1 {
                let index = y * CELLS + x;
                let cell = &mut self.cells[index];
                if cell.max == 0 && cell.head == NONE {
                    self.touched.push(index as u32);
                }
                cell.max = cell.max.max(ordering);
                let interior = x > 0 && y > 0 && x < CELLS - 1 && y < CELLS - 1;
                let covers = interior
                    && !cells.empty
                    && left <= x as f32 * CELL
                    && right >= (x + 1) as f32 * CELL
                    && top <= y as f32 * CELL
                    && bottom >= (y + 1) as f32 * CELL;
                if covers {
                    cell.cover = cell.cover.max(ordering);
                } else {
                    let next = cell.head;
                    cell.head = self.entries.len() as u32;
                    self.entries.push(Entry {
                        bounds: new_bounds.clone(),
                        order: ordering,
                        next,
                    });
                }
            }
        }
    }

    /// Inserts bounds into the tree and returns its assigned ordering.
    ///
    /// The ordering is one greater than the maximum ordering of any
    /// existing bounds that intersect with the new bounds.
    pub fn insert(&mut self, new_bounds: Bounds<U>) -> u32 {
        let ordering = (self.max_intersecting(&new_bounds) + 1).max(self.order_floor);
        self.insert_with_order(new_bounds, ordering);
        ordering
    }
}

impl<U> Default for BoundsTree<U>
where
    U: Clone + Debug + Default + PartialEq,
{
    fn default() -> Self {
        BoundsTree {
            cells: Vec::new(),
            touched: Vec::new(),
            entries: Vec::new(),
            recorded: Vec::new(),
            max: None,
            max_order: 0,
            order_floor: 0,
        }
    }
}

/// The cells whose interiors overlap the interior of some bounds, clamped to the grid. Bounds
/// without area take the cell their edge or corner falls in, and are marked `empty`.
struct CellRange {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    empty: bool,
}

fn cell_range<U: GridUnit + Clone + Debug + Default + PartialEq>(bounds: &Bounds<U>) -> CellRange {
    let axis = |start: f32, size: f32| {
        let (low, high) = (start.min(start + size), start.max(start + size));
        let clamp = |cell: f32| cmp::min(cell.max(0.) as usize, CELLS - 1);
        let first = (low / CELL).floor();
        if high > low {
            (clamp(first), clamp((high / CELL).ceil() - 1.), false)
        } else {
            (clamp(first), clamp(first), true)
        }
    };
    let (x0, x1, empty_x) = axis(bounds.origin.x.to_f32(), bounds.size.width.to_f32());
    let (y0, y1, empty_y) = axis(bounds.origin.y.to_f32(), bounds.size.height.to_f32());
    CellRange {
        x0,
        x1,
        y0,
        y1,
        empty: empty_x || empty_y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bounds, Point, Size};
    use rand::{Rng, SeedableRng};

    #[test]
    fn test_insert() {
        let mut tree = BoundsTree::<f32>::default();
        let bounds1 = Bounds {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 10.0,
                height: 10.0,
            },
        };
        let bounds2 = Bounds {
            origin: Point { x: 5.0, y: 5.0 },
            size: Size {
                width: 10.0,
                height: 10.0,
            },
        };
        let bounds3 = Bounds {
            origin: Point { x: 10.0, y: 10.0 },
            size: Size {
                width: 10.0,
                height: 10.0,
            },
        };

        // Insert the bounds into the tree and verify the order is correct
        assert_eq!(tree.insert(bounds1), 1);
        assert_eq!(tree.insert(bounds2), 2);
        assert_eq!(tree.insert(bounds3), 3);

        // Insert non-overlapping bounds and verify they can reuse orders
        let bounds4 = Bounds {
            origin: Point { x: 20.0, y: 20.0 },
            size: Size {
                width: 10.0,
                height: 10.0,
            },
        };
        let bounds5 = Bounds {
            origin: Point { x: 40.0, y: 40.0 },
            size: Size {
                width: 10.0,
                height: 10.0,
            },
        };
        let bounds6 = Bounds {
            origin: Point { x: 25.0, y: 25.0 },
            size: Size {
                width: 10.0,
                height: 10.0,
            },
        };
        assert_eq!(tree.insert(bounds4), 1); // bounds4 does not overlap with bounds1, bounds2, or bounds3
        assert_eq!(tree.insert(bounds5), 1); // bounds5 does not overlap with any other bounds
        assert_eq!(tree.insert(bounds6), 2); // bounds6 overlaps with bounds4, so it should have a different order
    }

    #[test]
    fn test_random_iterations() {
        let max_bounds = 100;
        for seed in 1..=1000 {
            // let seed = 44;
            let mut tree = BoundsTree::default();
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed as u64);
            let mut expected_quads: Vec<(Bounds<f32>, u32)> = Vec::new();

            // Insert a random number of random AABBs into the tree.
            let num_bounds = rng.random_range(1..=max_bounds);
            for _ in 0..num_bounds {
                let min_x: f32 = rng.random_range(-100.0..100.0);
                let min_y: f32 = rng.random_range(-100.0..100.0);
                let width: f32 = rng.random_range(0.0..50.0);
                let height: f32 = rng.random_range(0.0..50.0);
                let bounds = Bounds {
                    origin: Point { x: min_x, y: min_y },
                    size: Size { width, height },
                };

                let expected_ordering = expected_quads
                    .iter()
                    .filter_map(|quad| quad.0.intersects(&bounds).then_some(quad.1))
                    .max()
                    .unwrap_or(0)
                    + 1;
                expected_quads.push((bounds, expected_ordering));

                // Insert the AABB into the tree and collect intersections.
                let actual_ordering = tree.insert(bounds);
                assert_eq!(actual_ordering, expected_ordering);
            }
        }
    }

    /// Every operation against a brute-force model, with bounds from a cell to most of the grid,
    /// on cell boundaries and past the edges.
    #[test]
    fn grid_matches_brute_force() {
        let snap = |value: f32, rng: &mut rand::rngs::StdRng| {
            if rng.random_bool(0.3) {
                (value / CELL).round() * CELL
            } else {
                value
            }
        };
        for seed in 1..=300 {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let mut tree = BoundsTree::<f32>::default();
            let mut model: Vec<(Bounds<f32>, u32)> = Vec::new();
            let mut floor = 0;
            for _ in 0..rng.random_range(1..200) {
                let extent = if rng.random_bool(0.1) { 9000.0 } else { 400.0 };
                let origin = Point {
                    x: snap(rng.random_range(-200.0..9000.0), &mut rng),
                    y: snap(rng.random_range(-200.0..9000.0), &mut rng),
                };
                let size = Size {
                    width: snap(rng.random_range(0.0..extent), &mut rng),
                    height: snap(rng.random_range(0.0..extent), &mut rng),
                };
                let bounds = Bounds { origin, size };
                let max_intersecting = model
                    .iter()
                    .filter_map(|(other, order)| other.intersects(&bounds).then_some(*order))
                    .max()
                    .unwrap_or(0);
                let max_order = model.iter().map(|(_, order)| *order).max().unwrap_or(0);
                match rng.random_range(0..10) {
                    0 => {
                        assert_eq!(tree.max_intersecting(&bounds), max_intersecting);
                    }
                    1 => {
                        let order = tree.insert_above_all(bounds);
                        assert_eq!(order, max_order + 1);
                        model.push((bounds, order));
                    }
                    2 => {
                        let order = rng.random_range(1..50);
                        tree.insert_with_order(bounds, order);
                        model.push((bounds, order));
                    }
                    3 => {
                        floor = floor.max(rng.random_range(0..40));
                        tree.set_order_floor(floor);
                    }
                    _ => {
                        let order = tree.insert(bounds);
                        assert_eq!(order, (max_intersecting + 1).max(floor));
                        model.push((bounds, order));
                    }
                }
                assert_eq!(
                    tree.max_order(),
                    model.iter().map(|(_, order)| *order).max().unwrap_or(0)
                );
            }
            tree.clear();
            assert_eq!(tree.max_order(), 0);
        }
    }
}
