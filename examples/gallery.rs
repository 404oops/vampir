//! Every control in the toolkit, in one window.
//!
//! ```bash
//! cargo run --example gallery
//! ```
//!
//! It is also the smallest complete host: one struct, one `ControlHost`
//! impl, one `Render`. The hue slider and the light/dark segment in the
//! header rebuild the whole `Palette` from a single number, so everything
//! re-themes as you drag.

use std::time::Instant;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding, Menu, MenuItem as OsMenuItem,
    MouseButton, MouseDownEvent, MouseMoveEvent, OsAction, Point, ScrollHandle, SharedString,
    Subscription, SystemMenuType, TitlebarOptions, Window, WindowAppearance, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, size,
};
use gpui_ce_platform::application;

use vampir::{
    AppIcon, BadgeTone, ButtonVariant, CONTROL_HEIGHT, ChipSelection, Choice, Chord, Column,
    ComboDirection, ComboId, Command, ControlHost, ControlState, DialogButton, InputStyle,
    MAX_CHROMA, MenuItem, Oklch, Palette, ScrollAxis, SliderTrack, SortDirection, Span,
    TABLE_ROW_HEIGHT, Tab, TextInput, Tooltip, TreeRow, badge, button, caption, checkbox,
    chip_group, collapsible, color_pad, combo, command_list, command_palette, context_menu, dialog,
    fuzzy_filter, hue_slider, hue_wheel, icon_button, lit, menu_button, modal_opacity,
    progress_bar, radio_group, scrollbar, search_field, segmented, separator, shortcut_recorder,
    slider, spinbox, spinner, split_area, split_handle, swatch_grid, switch, tab_bar, table_cell,
    table_header, text_area, text_field, text_input as ti, tree_row,
};

// The gallery is also the smallest complete *macOS* host, so it carries the
// actions a real app has to own: the menu bar routes through these, and so
// do the shortcuts the menu bar advertises.
/// The icon this application shows in the dock, the task bar and its window.
const ICON: AppIcon = AppIcon::default_icon();

actions!(
    gallery,
    [
        Dismiss,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        TogglePalette,
        UseSystemScheme,
        UseLight,
        UseDark,
        ShowControls,
        ShowFields,
        ShowData,
        ShowColour,
        MinimizeWindow,
        ZoomWindow,
        CloseWindow,
        FocusNext,
        FocusPrevious,
    ]
);

// ---- The pages --------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Controls,
    Fields,
    Data,
    Colour,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Controls => "Controls",
            Page::Fields => "Fields",
            Page::Data => "Data",
            Page::Colour => "Colour",
        }
    }
}

/// A node of the Data page's file tree.
///
/// The host owns a real tree and flattens it for the toolkit each frame;
/// `TreeRow` is only what is on screen. Keeping the flattened list *as* the
/// model — the tempting shortcut, since it is what gets rendered — means
/// collapsing a branch has nothing to hide, because its children were never
/// underneath it in the first place.
struct FileNode {
    id: &'static str,
    label: &'static str,
    detail: &'static str,
    expanded: bool,
    children: Vec<FileNode>,
}

impl FileNode {
    fn leaf(id: &'static str, label: &'static str, detail: &'static str) -> Self {
        Self {
            id,
            label,
            detail,
            expanded: false,
            children: Vec::new(),
        }
    }

    fn branch(
        id: &'static str,
        label: &'static str,
        detail: &'static str,
        expanded: bool,
        children: Vec<FileNode>,
    ) -> Self {
        Self {
            id,
            label,
            detail,
            expanded,
            children,
        }
    }

    /// Appends this node, and the ones under it if it is open, to the
    /// flattened list the toolkit draws.
    fn flatten_into(&self, depth: usize, rows: &mut Vec<TreeRow>) {
        let row = if self.children.is_empty() {
            TreeRow::leaf(self.id, self.label, depth)
        } else {
            TreeRow::branch(self.id, self.label, depth, self.expanded)
        };
        rows.push(row.detail(self.detail));
        if self.expanded {
            for child in &self.children {
                child.flatten_into(depth + 1, rows);
            }
        }
    }

    /// Opens or closes the node with this id, wherever it is in the tree.
    fn set_expanded(&mut self, id: &str, expanded: bool) -> bool {
        if self.id == id {
            self.expanded = expanded;
            return true;
        }
        self.children
            .iter_mut()
            .any(|child| child.set_expanded(id, expanded))
    }
}

/// One row of the table on the Data page.
///
/// Two fields for each sortable column: the string a reader sees, and the
/// key a sort compares. They are not the same thing, and a table that sorts
/// by the displayed string gets sizes and dates wrong.
struct FileRow {
    name: &'static str,
    size: &'static str,
    bytes: u64,
    changed: &'static str,
    /// How long ago, in seconds, so "Changed" sorts chronologically.
    age: u64,
}

impl FileRow {
    fn new(
        name: &'static str,
        size: &'static str,
        bytes: u64,
        changed: &'static str,
        age: u64,
    ) -> Self {
        Self {
            name,
            size,
            bytes,
            changed,
            age,
        }
    }
}

/// A list the arrow keys might be moving through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListWithKeyboard {
    Palette,
    Search,
}

// ---- The host ---------------------------------------------------------------

struct Gallery {
    controls: ControlState,
    /// The time scale every animation is running at, when it is not one.
    slow_motion: Option<f32>,
    focus: FocusHandle,
    scroll: ScrollHandle,

    // Theme.
    hue: f64,
    dark: bool,
    /// Whether `dark` is the system's choice rather than the user's. The
    /// gallery starts out following the OS and keeps following it until
    /// someone picks a scheme by hand; "System" hands control back.
    follow_system: bool,
    /// Keeps the appearance observer alive for as long as the view is.
    _appearance: Subscription,
    /// The theme the text inputs were last styled for — the hue and the
    /// darkness as shown, bit for bit, since both animate. They hold their
    /// own colours, so the host hands them new ones when this stops matching.
    styled_for: (u64, u32),

    // Tabs, in the order they are currently arranged. Dragging one reorders
    // this, which is why the page is an enum rather than an index.
    pages: Vec<Page>,
    page: usize,

    // Controls page.
    switched: bool,
    checks: [bool; 2],
    routine: usize,
    scheme: usize,
    tags: Vec<bool>,
    volume: f32,
    steps: f32,
    takes: i32,
    device: usize,

    // Fields page.
    name: Entity<TextInput>,
    notes: Entity<TextInput>,
    query: Entity<TextInput>,
    /// What the search field searches, and which result the keyboard is on.
    search_items: Vec<Command>,
    search_index: usize,
    takes_edit: Entity<TextInput>,
    chord: Option<Chord>,
    chord_focus: FocusHandle,

    // Data page.
    files: Vec<Tab>,
    file: usize,
    tree: Vec<FileNode>,
    picked: Option<SharedString>,
    sort: (SharedString, SortDirection),
    split: f32,
    details_open: bool,
    last_action: SharedString,

    // Colour page.
    colour: Oklch,

    /// True while the focus ring is hidden because the mouse is in charge.
    /// The first Tab after a click shows where the keyboard already is
    /// rather than moving past it.
    ring_hidden: bool,

    // Overlays.
    dialog_enter: Option<Instant>,
    dialog_exit: Option<Instant>,
    palette_open: bool,
    /// Where the keyboard was before the palette took it. Closing puts it
    /// back: focus left on a field that no longer exists reaches nothing,
    /// and ⌘K would have to be re-earned with a click.
    palette_return: Option<FocusHandle>,
    palette_query: Entity<TextInput>,
    palette_index: usize,
    commands: Vec<Command>,
}

impl ControlHost for Gallery {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }

    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }

    /// One implementation for every draggable track in the gallery; the id
    /// says which one moved.
    fn track_dragged(&mut self, id: ComboId, at: Point<f32>, cx: &mut Context<Self>) {
        match id {
            "theme-hue" => self.hue = (at.x as f64 * 359.9).clamp(0.0, 359.9),
            "volume" => self.volume = at.x,
            "steps" => self.steps = at.x,
            "split" => self.split = at.x.clamp(0.2, 0.8),
            "swatch-hue" => self.colour.hue = (at.x as f64 * 359.9).clamp(0.0, 359.9),
            "colour-pad" => {
                self.colour.chroma = at.x as f64 * MAX_CHROMA;
                // The pad paints lightness bottom to top; the drag arrives
                // top-down like every other track.
                self.colour.lightness = (1.0 - at.y as f64).clamp(0.0, 1.0);
            }
            _ => {}
        }
        cx.notify();
    }

    fn tabs_reordered(&mut self, bar: ComboId, from: usize, to: usize, cx: &mut Context<Self>) {
        let (list, selected) = match bar {
            "pages" => (&mut self.pages as &mut dyn Reorder, &mut self.page),
            "files" => (&mut self.files as &mut dyn Reorder, &mut self.file),
            _ => return,
        };
        list.move_item(from, to);
        // Keep the selection on the same item it was on before the move.
        *selected = shifted(*selected, from, to);
        self.note(format!("Moved a tab from {from} to {to}"), cx);
    }

    fn forwarded_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.root_mouse_move(event, cx);
    }

    fn forwarded_mouse_up(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.root_mouse_up(cx);
    }
}

/// Lets `tabs_reordered` move either list without knowing its element type.
trait Reorder {
    fn move_item(&mut self, from: usize, to: usize);
}

impl<T> Reorder for Vec<T> {
    fn move_item(&mut self, from: usize, to: usize) {
        if from < self.len() && to < self.len() {
            let item = self.remove(from);
            self.insert(to, item);
        }
    }
}

/// Where `selected` ends up after the item at `from` is dropped at `to`.
fn shifted(selected: usize, from: usize, to: usize) -> usize {
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

impl Focusable for Gallery {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- Construction -----------------------------------------------------------

const SORTS: [&str; 4] = ["Name", "Date modified", "Size", "Kind"];
const TAGS: [&str; 7] = [
    "Design", "Research", "Bug", "Docs", "Idea", "Urgent", "Archive",
];
const SYNC: [&str; 3] = ["Off", "Auto", "On"];

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The window already knows whether the OS is in light or dark mode;
        // that is the starting scheme, and it stays in step with the system
        // until the user chooses one.
        let dark = system_dark(window);
        let palette = Palette::from_hue(268.0, dark);
        let style = input_style;

        let appearance = window.observe_window_appearance({
            let this = cx.weak_entity();
            move |window, cx| {
                let dark = system_dark(window);
                this.update(cx, |this, cx| {
                    if this.follow_system && this.dark != dark {
                        this.dark = dark;
                        cx.notify();
                    }
                })
                .ok();
            }
        });

        let name = cx.new(|cx| TextInput::new(cx, "Untitled project", false, style(palette, 12.5)));
        let notes = cx.new(|cx| {
            let mut input = TextInput::new(
                cx,
                "Anything worth remembering...",
                true,
                style(palette, 12.5),
            );
            input.content = "Ship the empty state first. Check #contrast in light mode, and ask \
                             Sam where the #copy for the welcome screen lives."
                .into();
            input
        });
        let query = cx.new(|cx| TextInput::new(cx, "Search", false, style(palette, 12.5)));
        let takes_edit = cx.new(|cx| {
            let mut input = TextInput::new(cx, "", false, style(palette, 12.5));
            input.content = "24".into();
            input
        });
        let palette_query =
            cx.new(|cx| TextInput::new(cx, "Type a command", false, style(palette, 14.0)));

        // Rich text is a closure from content to spans, re-run on layout.
        notes.update(cx, |input, cx| {
            let accent = palette.accent;
            input.set_highlighter(
                move |text| {
                    hashtags(text)
                        .into_iter()
                        .map(|range| Span::new(range).color(accent).bold())
                        .collect()
                },
                cx,
            );
        });

        // `VAMPIR_SLOW_MOTION=8 tools/gallery.sh launch` runs every fade and
        // slide eight times slower, which is how a screenshot can catch one
        // half way; see CONTRIBUTING.md.
        let mut controls = ControlState::new();
        let slow_motion = std::env::var("VAMPIR_SLOW_MOTION")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|scale| *scale > 0.0 && *scale != 1.0);
        if let Some(scale) = slow_motion {
            controls.set_time_scale(scale);
        }

        let this = Self {
            controls,
            slow_motion,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            hue: 268.0,
            dark,
            follow_system: true,
            _appearance: appearance,
            styled_for: (
                268.0f64.to_bits(),
                if dark { 1.0f32 } else { 0.0 }.to_bits(),
            ),
            pages: vec![Page::Controls, Page::Fields, Page::Data, Page::Colour],
            page: 0,
            switched: true,
            checks: [true, false],
            routine: 0,
            scheme: 1,
            tags: vec![true, false, true, false, false, false, false],
            volume: 0.72,
            steps: 0.5,
            takes: 24,
            device: 1,
            name,
            notes,
            query,
            search_items: vec![
                Command::new("report", "report.pdf").group("Documents"),
                Command::new("budget", "budget.csv").group("Documents"),
                Command::new("logo", "logo.png").group("Images"),
                Command::new("banner", "banner.jpg").group("Images"),
                Command::new("readme", "readme.md"),
                Command::new("appearance", "Appearance").group("Settings"),
                Command::new("keys", "Keyboard shortcuts").group("Settings"),
                Command::new("notify", "Notifications").group("Settings"),
            ],
            search_index: 0,
            takes_edit,
            chord: Some(Chord {
                keystroke: "secondary-shift-s".into(),
                display: vampir::display("secondary-shift-s"),
            }),
            chord_focus: cx.focus_handle(),
            files: vec![
                Tab::new("Overview").closable(),
                Tab::new("Details").badge("3").closable(),
                Tab::new("History").closable(),
            ],
            file: 0,
            tree: vec![
                FileNode::branch(
                    "documents",
                    "Documents",
                    "2",
                    true,
                    vec![
                        FileNode::leaf("report", "report.pdf", "4.1 MB"),
                        FileNode::leaf("budget", "budget.csv", "18 KB"),
                    ],
                ),
                FileNode::branch(
                    "images",
                    "Images",
                    "4",
                    false,
                    vec![
                        FileNode::leaf("logo", "logo.png", "82 KB"),
                        FileNode::leaf("banner", "banner.jpg", "1.4 MB"),
                        FileNode::leaf("icon", "icon.svg", "6 KB"),
                        FileNode::leaf("shot", "screenshot.png", "308 KB"),
                    ],
                ),
                FileNode::leaf("readme", "readme.md", "2 KB"),
            ],
            picked: Some("report".into()),
            sort: ("changed".into(), SortDirection::Descending),
            split: 0.42,
            details_open: true,
            last_action: "—".into(),
            colour: Oklch::new(268.0, 0.11, 0.62),
            ring_hidden: true,
            dialog_enter: None,
            dialog_exit: None,
            palette_open: false,
            palette_return: None,
            palette_query,
            palette_index: 0,
            commands: vec![
                Command::new("new", "New document")
                    .group("File")
                    .shortcut(vampir::display("secondary-n")),
                Command::new("duplicate", "Duplicate").group("File"),
                Command::new("export", "Export as PDF")
                    .group("File")
                    .shortcut(vampir::display("secondary-e")),
                Command::new("theme", "Toggle light and dark")
                    .group("View")
                    .shortcut(vampir::display("secondary-l")),
                Command::new("hue", "Shift the accent hue").group("View"),
                // Flips a switch from somewhere that is not the switch, which
                // is the case that has to animate just the same.
                Command::new("sync", "Toggle sync while you work").group("View"),
                Command::new("settings", "Open settings")
                    .group("App")
                    .shortcut(vampir::display("secondary-,")),
            ],
        };
        window.focus(&this.focus, cx);
        this
    }

    // ---- Root gestures ----

    fn root_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // A release outside the window never arrives. A move with no button
        // held is that release, so end everything exactly as it would have.
        if !event.dragging() {
            if self.controls.dragging_anything() {
                self.root_mouse_up(cx);
            }
            return;
        }
        if vampir::continue_drags(self, event.position, cx) {
            cx.notify();
        }
    }

    fn root_mouse_up(&mut self, cx: &mut Context<Self>) {
        vampir::end_drags(self, cx);
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.show_palette(false, window, cx);
        } else if self.dialog_enter.is_some() && self.dialog_exit.is_none() {
            self.dialog_exit = Some(Instant::now());
            // Escape arrives here as the host's action, not at the dialog, so
            // the dialog cannot hand the keyboard back itself.
            self.controls.restore_dialog_focus(window, cx);
        } else {
            self.controls.close_menu();
            self.controls.close_combo();
        }
        cx.notify();
    }

    // ---- Menu and shortcut actions ------------------------------------------
    //
    // Each of these is reachable two ways — a menu item and a key binding —
    // and macOS expects both to land in the same place, so they are methods
    // rather than closures buried in the menu definition.

    fn toggle_palette(&mut self, _: &TogglePalette, window: &mut Window, cx: &mut Context<Self>) {
        self.show_palette(!self.palette_open, window, cx);
    }

    /// Opens or closes the command palette.
    ///
    /// Opening puts the keyboard in the query field. A palette that opens
    /// without focus is a box you have to click before you can type into,
    /// which is the one thing nobody reaching for ⌘K wants to do.
    fn show_palette(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = open;
        self.palette_index = 0;
        if open {
            self.controls.dismiss_popups();
            self.palette_return = window.focused(cx);
            let query = self.palette_query.read(cx).focus_handle.clone();
            window.focus(&query, cx);
            self.palette_query
                .update(cx, |input, cx| input.set_text("", cx));
        } else {
            // Back where it came from, or the root — anywhere real. The
            // query field is about to stop existing, and focus left on it
            // goes nowhere, so the next ⌘K would never arrive.
            let back = self
                .palette_return
                .take()
                .unwrap_or_else(|| self.focus.clone());
            window.focus(&back, cx);
        }
        cx.notify();
    }

    fn use_system_scheme(
        &mut self,
        _: &UseSystemScheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.follow_system = true;
        self.dark = system_dark(window);
        cx.notify();
    }

    fn use_light(&mut self, _: &UseLight, _window: &mut Window, cx: &mut Context<Self>) {
        self.follow_system = false;
        self.dark = false;
        cx.notify();
    }

    fn use_dark(&mut self, _: &UseDark, _window: &mut Window, cx: &mut Context<Self>) {
        self.follow_system = false;
        self.dark = true;
        cx.notify();
    }

    /// Tabs can be dragged into any order, so a page is found by identity
    /// rather than by the index it happened to start at.
    fn show_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.pages.iter().position(|candidate| *candidate == page) else {
            return;
        };
        if index != self.page {
            // The keyboard may be on something inside the page that is about
            // to go. GPUI dispatches nothing from a handle that is no longer
            // in the tree — not even the root's own shortcuts — so it is
            // re-homed to the root, which is always there and is one Tab
            // from the first control.
            window.focus(&self.focus, cx);
        }
        self.page = index;
        cx.notify();
    }

    // Moving focus is the one keyboard job the toolkit leaves to the host:
    // which key walks the window is a window-wide decision, and no single
    // control is in a position to make it. Everything else — Space on a
    // button, arrows inside a group — the controls handle themselves.
    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal_focus(window, cx) {
            return;
        }
        vampir::move_focus(self, window, cx, false);
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal_focus(window, cx) {
            return;
        }
        vampir::move_focus(self, window, cx, true);
    }

    /// The commands the query currently matches, best first.
    fn palette_matches(&self, cx: &App) -> Vec<Command> {
        let query = self.palette_query.read(cx).text();
        fuzzy_filter(&query, &self.commands)
    }

    /// Runs a command and closes the palette. Clicking a row and pressing
    /// Enter on one arrive here the same way.
    fn run_command(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.show_palette(false, window, cx);
        match id.as_ref() {
            "theme" => {
                self.follow_system = false;
                self.dark = !self.dark;
            }
            "hue" => self.hue = (self.hue + 47.0) % 360.0,
            "sync" => self.switched = !self.switched,
            _ => {}
        }
        self.note(format!("Ran {id}"), cx);
    }

    /// The palette's own keys: up and down move the highlight, Enter runs it.
    ///
    /// They arrive as the query field's actions rather than as raw keys. A
    /// keystroke that matches a binding is dispatched as that action and
    /// never reaches a key listener; a single-line field has no use for Up,
    /// Down or an Enter with nothing to submit to, so it passes them on, and
    /// they land here. The toolkit leaves them to the host because the host
    /// owns the query, the list and the highlight — but a palette where Enter
    /// does nothing is not a palette.
    fn palette_up(&mut self, _: &ti::Up, window: &mut Window, cx: &mut Context<Self>) {
        self.list_move(vampir::Key::Up, window, cx);
    }

    fn palette_down(&mut self, _: &ti::Down, window: &mut Window, cx: &mut Context<Self>) {
        self.list_move(vampir::Key::Down, window, cx);
    }

    fn palette_enter(&mut self, _: &ti::Enter, window: &mut Window, cx: &mut Context<Self>) {
        match self.list_with_keyboard(window, cx) {
            Some(ListWithKeyboard::Palette) => {
                let matches = self.palette_matches(cx);
                let here = self.palette_index.min(matches.len().saturating_sub(1));
                if let Some(command) = matches.get(here) {
                    let id = command.id.clone();
                    self.run_command(id, window, cx);
                }
            }
            Some(ListWithKeyboard::Search) => {
                let results = self.search_results(cx);
                let here = self.search_index.min(results.len().saturating_sub(1));
                if let Some(result) = results.get(here) {
                    let id = result.id.clone();
                    self.open_result(id, cx);
                }
            }
            None => cx.propagate(),
        }
    }

    fn list_move(&mut self, key: vampir::Key, window: &mut Window, cx: &mut Context<Self>) {
        let (count, index) = match self.list_with_keyboard(window, cx) {
            Some(ListWithKeyboard::Palette) => {
                (self.palette_matches(cx).len(), &mut self.palette_index)
            }
            Some(ListWithKeyboard::Search) => {
                (self.search_results(cx).len(), &mut self.search_index)
            }
            None => {
                cx.propagate();
                return;
            }
        };
        let here = (*index).min(count.saturating_sub(1));
        if let Some(moved) = vampir::step(key, vampir::Orientation::Vertical, here, count) {
            *index = moved;
            cx.notify();
        }
    }

    /// Which list the keyboard is in: the palette's while it is open, the
    /// search field's while that field has focus, or neither. The field
    /// passes Up, Down and Enter up because it has no use for them; this is
    /// where they find out what they mean.
    fn list_with_keyboard(&self, window: &Window, cx: &App) -> Option<ListWithKeyboard> {
        if self.palette_open {
            Some(ListWithKeyboard::Palette)
        } else if self.query.read(cx).focus_handle.is_focused(window) {
            Some(ListWithKeyboard::Search)
        } else {
            None
        }
    }

    /// The first Tab after using the mouse shows where the keyboard already
    /// is instead of moving on.
    ///
    /// The ring is hidden while the mouse is in charge, so a Tab that moved
    /// would look as though it had skipped a control: you clicked one thing
    /// and the ring appeared on the next. Returns whether it handled the
    /// press.
    fn reveal_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A dropdown is a question; moving to another control walks away
        // from it, and it should not stay on screen looking answerable.
        self.controls.dismiss_popups();
        if !self.ring_hidden || window.focused(cx).is_none() {
            self.ring_hidden = false;
            return false;
        }
        self.ring_hidden = false;
        cx.notify();
        true
    }

    fn minimize(&mut self, _: &MinimizeWindow, window: &mut Window, _cx: &mut Context<Self>) {
        window.minimize_window();
    }

    fn zoom(&mut self, _: &ZoomWindow, window: &mut Window, _cx: &mut Context<Self>) {
        window.zoom_window();
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

    /// Hands every text input the current theme's colours, and the notes
    /// highlighter the current accent.
    fn restyle_inputs(&mut self, palette: Palette, cx: &mut Context<Self>) {
        for (input, size) in [
            (self.name.clone(), 12.5),
            (self.notes.clone(), 12.5),
            (self.query.clone(), 12.5),
            (self.takes_edit.clone(), 12.5),
            (self.palette_query.clone(), 14.0),
        ] {
            input.update(cx, |input, _cx| input.style = input_style(palette, size));
        }
        let accent = palette.accent;
        self.notes.update(cx, |input, cx| {
            input.set_highlighter(
                move |text| {
                    hashtags(text)
                        .into_iter()
                        .map(|range| Span::new(range).color(accent).bold())
                        .collect()
                },
                cx,
            );
        });
    }

    fn note(&mut self, what: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.last_action = what.into();
        cx.notify();
    }
}

/// Byte ranges of every `#tag` in the text, for the notes highlighter.
fn hashtags(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let mut end = i + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > i + 1 {
                ranges.push(i..end);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    ranges
}

fn input_style(palette: Palette, size: f32) -> InputStyle {
    InputStyle {
        text_color: palette.text_primary,
        placeholder_color: palette.text_secondary,
        selection_color: palette.accent,
        cursor_color: palette.accent,
        font_size: size,
    }
}

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// A labelled control, laid out as a column so a row of them lines up.
fn field(palette: Palette, label: &str, control: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(caption(palette, label))
        .child(control)
}

/// A row of controls. `flex_none` because a `div` is a flex container: a
/// child that can shrink would be squeezed to fit the window instead of
/// scrolling out of it.
fn row() -> gpui::Div {
    div().flex().flex_none().items_center().gap(px(10.0))
}

fn column() -> gpui::Div {
    div().flex().flex_none().flex_col().gap(px(14.0))
}

/// A panel: the lit surface everything else sits on.
fn card(palette: Palette, title: &str, body: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .flex_none()
        .p(px(16.0))
        .rounded(px(10.0))
        .bg(lit(palette.area_surface, 0.04))
        .border_1()
        .border_color(palette.area_border)
        .shadow(vampir::panel(palette.is_dark))
        .child(caption(palette, title))
        .child(body)
}

/// Anything with a hover tooltip. `icon_button` returns a plain element, so
/// the tooltip hangs off a wrapper with its own id.
fn tipped(
    id: &'static str,
    palette: Palette,
    text: &'static str,
    control: impl IntoElement,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .child(control)
        .tooltip(Tooltip::text(text, palette))
}

fn glyph(character: &'static str, palette: Palette) -> impl IntoElement {
    div()
        .text_size(px(13.0))
        .text_color(palette.text_primary)
        .child(character)
}

// ---- Render -----------------------------------------------------------------

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The scheme and the hue cross over rather than cut. Both are
        // noticed here from their values, so a change from the menu bar, a
        // shortcut, the command palette or the desktop switching to dark at
        // sunset all arrive the same way: every colour on screen is mixed
        // between the palette it had and the one it is getting.
        let dark_t = self.controls.tween(
            "scheme-dark",
            if self.dark { 1.0 } else { 0.0 },
            vampir::SCHEME_FADE,
        );
        // While the hue slider is being dragged the colours sit under the
        // hand; a hue set any other way glides.
        let hue = f64::from(if self.controls.is_dragging("theme-hue") {
            self.controls.snap("scheme-hue", self.hue as f32)
        } else {
            self.controls
                .tween_angle("scheme-hue", self.hue as f32, vampir::MOVE)
        });
        let palette = if dark_t <= 0.0 {
            Palette::from_hue(hue, false)
        } else if dark_t >= 1.0 {
            Palette::from_hue(hue, true)
        } else {
            Palette::mix(
                Palette::from_hue(hue, false),
                Palette::from_hue(hue, true),
                dark_t,
            )
        };
        if self.styled_for != (hue.to_bits(), dark_t.to_bits()) {
            self.styled_for = (hue.to_bits(), dark_t.to_bits());
            self.restyle_inputs(palette, cx);
        }
        let (dialog_opacity, dialog_animating) = modal_opacity(self.dialog_enter, self.dialog_exit);
        if !dialog_animating && self.dialog_exit.is_some() {
            self.dialog_enter = None;
            self.dialog_exit = None;
        }
        let page = self.pages[self.page];

        let backdrop =
            vampir::color::oklch_to_color(0.955 + (0.285 - 0.955) * f64::from(dark_t), 0.012, hue);
        // The root's gradient, sampled at a window y: what a fade over the
        // page has to match at its edge.
        let (surface_top, surface_bottom) = vampir::lighting::lit_stops(backdrop, 0.035);
        let window_height = f32::from(window.viewport_size().height).max(1.0);
        let page_surface_at = move |y: gpui::Pixels| {
            vampir::color::lerp(
                surface_top,
                surface_bottom,
                (f32::from(y) / window_height).clamp(0.0, 1.0),
            )
        };

        let body = match page {
            Page::Controls => self.page_controls(palette, cx),
            Page::Fields => self.page_fields(palette, window, cx),
            Page::Data => self.page_data(palette, cx),
            Page::Colour => self.page_colour(palette, cx),
        };
        // A new page fades in and settles up into place rather than
        // replacing the old one in a single frame. Keyed by the page, not
        // the tab's position, so reordering the tabs is not a page change.
        let arrived = vampir::ease_out_cubic(self.controls.transition(
            &gpui::ElementId::Name("page".into()),
            page as u64,
            vampir::MOVE,
        ));
        let body = div()
            .opacity(arrived)
            .relative()
            .top(px(8.0 * (1.0 - arrived)))
            .child(body);

        // Asked *after* the page is built, on purpose. A control notices its
        // own state change while it renders, and a slide it starts there is
        // invisible to a check made before it ran — the frame that starts
        // the slide would never ask for the one that continues it. Fades and
        // slides stop asking when they finish. The spinner never does, so
        // the controls page keeps the clock running and no other page pays
        // for it.
        if self.controls.animating() || dialog_animating || page == Page::Controls {
            window.request_animation_frame();
        }

        div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui_font())
            .text_size(px(12.5))
            .bg(lit(backdrop, 0.035))
            .text_color(palette.text_primary)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::palette_up))
            .on_action(cx.listener(Self::palette_down))
            .on_action(cx.listener(Self::palette_enter))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .on_action(cx.listener(Self::use_system_scheme))
            .on_action(cx.listener(Self::use_light))
            .on_action(cx.listener(Self::use_dark))
            .on_action(cx.listener(Self::minimize))
            .on_action(cx.listener(Self::zoom))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(|this, _: &ShowControls, window, cx| {
                this.show_page(Page::Controls, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowFields, window, cx| {
                this.show_page(Page::Fields, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ShowData, window, cx| {
                    this.show_page(Page::Data, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ShowColour, window, cx| {
                this.show_page(Page::Colour, window, cx)
            }))
            .on_any_mouse_down(cx.listener(|this, _event: &MouseDownEvent, _window, _cx| {
                // The mouse takes over: the ring goes away, and the next Tab
                // brings it back where it is rather than one place along.
                this.ring_hidden = true;
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                this.root_mouse_move(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| this.root_mouse_up(cx)),
            )
            .child(self.header(palette, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("page")
                            .size_full()
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            // Without this a sideways trackpad swipe scrolls
                            // the page vertically: GPUI maps an x-delta onto
                            // y for a container that only scrolls one way.
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.scroll)
                            .p(px(20.0))
                            .child(body),
                    )
                    // Content leaving the top of the page passes under the
                    // tabs rather than being cut off against them. The root
                    // is a lit gradient, so each fade lands on whatever that
                    // gradient is at its own edge — a single colour would
                    // show as a band.
                    .children(vampir::scroll_fades(
                        &self.controls,
                        "page",
                        &self.scroll,
                        ScrollAxis::Vertical,
                        page_surface_at(self.scroll.bounds().top()),
                        page_surface_at(self.scroll.bounds().bottom()),
                    ))
                    .child(scrollbar(
                        "page",
                        &self.scroll.clone(),
                        ScrollAxis::Vertical,
                        palette,
                        self,
                        cx,
                    )),
            )
            .child(self.footer(palette, cx))
            .children(self.render_dialog(palette, dialog_opacity, cx))
            .children(self.render_command_palette(palette, window, cx))
            .children(context_menu(
                "row",
                &self.row_menu(),
                palette,
                self,
                cx,
                |this, action, _window, cx| {
                    let target = this.controls.menu_target().unwrap_or("").to_string();
                    this.controls.close_menu();
                    this.note(format!("{action} → {target}"), cx);
                },
            ))
            .children(context_menu(
                "view-menu",
                &self.view_menu(),
                palette,
                self,
                cx,
                |this, action, window, cx| {
                    this.controls.close_menu();
                    match action {
                        "system" => this.set_scheme(0, window, cx),
                        "light" => this.set_scheme(1, window, cx),
                        "dark" => this.set_scheme(2, window, cx),
                        "palette" => this.show_palette(true, window, cx),
                        _ => {}
                    }
                    cx.notify();
                },
            ))
    }
}

// ---- Chrome -----------------------------------------------------------------

impl Gallery {
    fn header(&mut self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let schemes = owned(&["System", "Light", "Dark"]);

        let tabs: Vec<Tab> = self
            .pages
            .iter()
            .map(|page| Tab::new(page.title()))
            .collect();

        // One row: the pages on the left, the theme controls on the right.
        // No title — the window already has one, and a heading that repeats
        // it is a line of chrome between the reader and the controls.
        //
        // The page below pads itself by 20 on every side, so the row takes
        // the same 20 above and nothing below: the title bar, the row and
        // the first panel then sit at one even spacing, instead of the row
        // hugging the title bar and leaving a gulf under itself.
        row()
            .flex_none()
            .px(px(20.0))
            .pt(px(20.0))
            .child(tab_bar(
                "pages",
                &tabs,
                self.page,
                palette,
                self,
                cx,
                |this, index, _window, cx| {
                    this.page = index;
                    cx.notify();
                },
                |_this, _index, _window, _cx| {},
            ))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(150.0))
                    .child(hue_slider("theme-hue", self.hue, palette, self, cx)),
            )
            .child(div().w(px(216.0)).child(segmented(
                "scheme",
                &schemes,
                self.scheme_index(),
                true,
                palette,
                self,
                cx,
                |this, index, window, cx| {
                    this.set_scheme(index, window, cx);
                },
            )))
            .child(menu_button("view-menu", "View", true, palette, self, cx))
    }

    fn footer(&mut self, palette: Palette, _cx: &mut Context<Self>) -> impl IntoElement {
        // A new note fades in rather than replacing the old one mid-glance.
        // Text cannot be interpolated, so the change of value is what is
        // noticed, and the words arrive over it.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&self.last_action, &mut hasher);
        let arrived = vampir::ease_out_cubic(self.controls.transition(
            &gpui::ElementId::Name("last-action".into()),
            std::hash::Hasher::finish(&hasher),
            vampir::SWITCH_SLIDE,
        ));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(separator(false, palette))
            .child(
                row()
                    .px(px(20.0))
                    .py(px(10.0))
                    .child(caption(palette, "Last action"))
                    .child(
                        div()
                            .opacity(arrived)
                            .text_color(palette.text_secondary)
                            .child(self.last_action.clone()),
                    )
                    .child(div().flex_1())
                    // Says so when the gallery is running slowed down, so a
                    // screenshot taken that way cannot pass for the real thing.
                    .children(self.slow_motion.map(|scale| {
                        let label = format!("slow motion ×{scale}");
                        badge(&label, BadgeTone::Danger, palette).into_any_element()
                    })),
            )
    }
}

// ---- Pages ------------------------------------------------------------------

impl Gallery {
    fn page_controls(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let sorts = owned(&SORTS);
        let sync = owned(&SYNC);
        let tags = owned(&TAGS);
        let choices = [
            Choice::new("Save automatically").detail("A copy is kept every time the file changes"),
            Choice::new("Ask first")
                .detail("A prompt each time, so nothing is written by surprise"),
            Choice::new("Never save"),
            Choice::new("Managed by your administrator").disabled(),
        ];

        column()
            .child(card(
                palette,
                "Buttons",
                column()
                    .child(
                        row()
                            .child(div().w(px(112.0)).child(button(
                                "cancel",
                                "Cancel",
                                ButtonVariant::Soft,
                                true,
                                palette,
                                cx,
                                |this, _window, cx| this.note("Cancel", cx),
                            )))
                            .child(div().w(px(112.0)).child(button(
                                "save",
                                "Save",
                                ButtonVariant::Primary,
                                true,
                                palette,
                                cx,
                                |this, _window, cx| this.note("Save", cx),
                            )))
                            .child(div().w(px(112.0)).child(button(
                                "delete",
                                "Delete",
                                ButtonVariant::Danger,
                                true,
                                palette,
                                cx,
                                |this, _window, cx| this.note("Delete", cx),
                            )))
                            .child(div().w(px(112.0)).child(button(
                                "disabled",
                                "Disabled",
                                ButtonVariant::Soft,
                                false,
                                palette,
                                cx,
                                |_this, _window, _cx| {},
                            ))),
                    )
                    .child(
                        row()
                            .child(tipped(
                                "tip-undo",
                                palette,
                                "Undo",
                                icon_button(
                                    "undo",
                                    glyph("↺", palette),
                                    CONTROL_HEIGHT,
                                    false,
                                    true,
                                    palette,
                                    cx,
                                    |this, _window, cx| this.note("Undo", cx),
                                ),
                            ))
                            .child(tipped(
                                "tip-redo",
                                palette,
                                "Redo",
                                icon_button(
                                    "redo",
                                    glyph("↻", palette),
                                    CONTROL_HEIGHT,
                                    false,
                                    true,
                                    palette,
                                    cx,
                                    |this, _window, cx| this.note("Redo", cx),
                                ),
                            ))
                            .child(
                                div()
                                    .id("tip-star")
                                    .flex_none()
                                    .child(icon_button(
                                        "star",
                                        glyph("★", palette),
                                        CONTROL_HEIGHT,
                                        self.switched,
                                        true,
                                        palette,
                                        cx,
                                        |this, _window, cx| {
                                            this.switched = !this.switched;
                                            cx.notify();
                                        },
                                    ))
                                    .tooltip(Tooltip::with_shortcut(
                                        "Favourite",
                                        vampir::display("secondary-d"),
                                        palette,
                                    )),
                            )
                            .child(separator(true, palette))
                            .child(badge("12", BadgeTone::Neutral, palette))
                            .child(badge("New", BadgeTone::Accent, palette))
                            .child(badge("Failed", BadgeTone::Danger, palette))
                            .child(separator(true, palette))
                            .child(spinner(16.0, palette)),
                    ),
            ))
            .child(card(
                palette,
                "Toggles",
                row()
                    .gap(px(24.0))
                    .child(switch(
                        "watch",
                        self.switched,
                        Some("Sync while you work"),
                        true,
                        palette,
                        self,
                        cx,
                        |this, on, _window, cx| {
                            this.switched = on;
                            cx.notify();
                        },
                    ))
                    .child(checkbox(
                        "notify",
                        self.checks[0],
                        Some("Email me about changes"),
                        true,
                        palette,
                        self,
                        cx,
                        |this, on, _window, cx| {
                            this.checks[0] = on;
                            cx.notify();
                        },
                    ))
                    .child(checkbox(
                        "compress",
                        self.checks[1],
                        Some("Show hidden files"),
                        true,
                        palette,
                        self,
                        cx,
                        |this, on, _window, cx| {
                            this.checks[1] = on;
                            cx.notify();
                        },
                    ))
                    .child(checkbox(
                        "locked",
                        true,
                        Some("Locked"),
                        false,
                        palette,
                        self,
                        cx,
                        |_this, _on, _window, _cx| {},
                    )),
            ))
            .child(
                row()
                    .items_start()
                    .child(div().flex_1().child(card(
                        palette,
                        "Choices",
                        column().child(radio_group(
                            "routine",
                            &choices,
                            self.routine,
                            true,
                            palette,
                            self,
                            cx,
                            |this, index, _window, cx| {
                                this.routine = index;
                                cx.notify();
                            },
                        )),
                    )))
                    .child(
                        div().flex_1().child(card(
                            palette,
                            "Pickers",
                            column()
                                .child(field(
                                    palette,
                                    "Sync",
                                    segmented(
                                        "auto",
                                        &sync,
                                        self.scheme,
                                        true,
                                        palette,
                                        self,
                                        cx,
                                        |this, index, _window, cx| {
                                            this.scheme = index;
                                            cx.notify();
                                        },
                                    ),
                                ))
                                .child(field(
                                    palette,
                                    "Sort by",
                                    combo(
                                        "device",
                                        self.device,
                                        &sorts,
                                        None,
                                        ComboDirection::Down,
                                        palette,
                                        self,
                                        cx,
                                        |this, index, _window, cx| {
                                            this.device = index;
                                            cx.notify();
                                        },
                                    ),
                                ))
                                .child(field(
                                    palette,
                                    "Items per page",
                                    spinbox(
                                        "takes",
                                        self.takes,
                                        5,
                                        100,
                                        true,
                                        &self.takes_edit,
                                        palette,
                                        cx,
                                        |this, value, _window, cx| {
                                            this.takes = value;
                                            // The spinbox draws its input, not
                                            // its value: the host keeps them
                                            // in step.
                                            this.takes_edit.update(cx, |input, cx| {
                                                input.set_text(&value.to_string(), cx);
                                            });
                                            cx.notify();
                                        },
                                    ),
                                )),
                        )),
                    ),
            )
            .child(card(
                palette,
                "Tags",
                chip_group(
                    "tags",
                    &tags,
                    ChipSelection::Many(&self.tags),
                    true,
                    palette,
                    self,
                    cx,
                    |this, index, _window, cx| {
                        if let Some(flag) = this.tags.get_mut(index) {
                            *flag = !*flag;
                        }
                        cx.notify();
                    },
                ),
            ))
            .child(card(
                palette,
                "Ranges",
                column()
                    .child(field(
                        palette,
                        "Opacity",
                        slider(
                            "volume",
                            self.volume,
                            SliderTrack::Continuous,
                            palette,
                            self,
                            cx,
                        ),
                    ))
                    .child(field(
                        palette,
                        "Grid size",
                        slider(
                            "steps",
                            self.steps,
                            SliderTrack::Stepped { stops: 5 },
                            palette,
                            self,
                            cx,
                        ),
                    ))
                    .child(field(
                        palette,
                        "Uploading",
                        progress_bar(self.volume, palette),
                    )),
            ))
            .into_any_element()
    }

    /// What the search field currently matches, best first.
    fn search_results(&self, cx: &App) -> Vec<Command> {
        let query = self.query.read(cx).text();
        fuzzy_filter(&query, &self.search_items)
    }

    /// Picks a search result. Clicking a row and pressing Enter on one arrive
    /// here the same way.
    fn open_result(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let label = self
            .search_items
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.label.clone())
            .unwrap_or(id);
        self.note(format!("Opened {label}"), cx);
    }

    fn page_fields(
        &mut self,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let results = self.search_results(cx);
        let highlighted = self.search_index.min(results.len().saturating_sub(1));
        column()
            .child(card(
                palette,
                "Text",
                column()
                    .child(field(
                        palette,
                        "Project name",
                        text_field(&self.name, palette, window, cx),
                    ))
                    .child(field(
                        palette,
                        "Notes — #tags are highlighted by a Highlighter closure",
                        text_area(&self.notes, Some(96.0), true, palette, window, cx),
                    ))
                    .child(field(
                        palette,
                        "Search — the list filters as you type; ↑↓ and Enter pick",
                        column()
                            .gap(px(6.0))
                            .child(search_field("search", &self.query, palette, window, cx))
                            .child(command_list(
                                "search-results",
                                &results,
                                highlighted,
                                palette,
                                self,
                                cx,
                                |this, id, _window, cx| this.open_result(id, cx),
                            )),
                    )),
            ))
            .child(card(
                palette,
                "Shortcut",
                column()
                    .child(field(
                        palette,
                        "Quick open — click, then press a chord",
                        shortcut_recorder(
                            "chord",
                            self.chord.as_ref(),
                            &self.chord_focus,
                            palette,
                            self,
                            cx,
                            |this, chord, _window, cx| {
                                this.chord = Some(chord);
                                cx.notify();
                            },
                        ),
                    ))
                    .child(
                        row()
                            .child(div().w(px(140.0)).child(button(
                                "open-dialog",
                                "Open a dialog",
                                ButtonVariant::Soft,
                                true,
                                palette,
                                cx,
                                |this, _window, cx| {
                                    this.dialog_exit = None;
                                    this.dialog_enter = Some(Instant::now());
                                    cx.notify();
                                },
                            )))
                            .child(div().w(px(160.0)).child(button(
                                "open-palette",
                                "Command palette",
                                ButtonVariant::Soft,
                                true,
                                palette,
                                cx,
                                |this, window, cx| this.show_palette(true, window, cx),
                            ))),
                    ),
            ))
            .into_any_element()
    }
}

impl Gallery {
    fn page_data(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let columns = [
            Column::new("name", "Name"),
            Column::new("size", "Size").width(80.0).numeric(),
            Column::new("changed", "Changed").width(110.0),
        ];
        // Sorting compares keys, not the strings on screen: "18 KB" sorts
        // before "4.1 MB" as text and after it as a size, and "yesterday"
        // has no alphabetical relationship to "last week" at all. A header
        // that reports a column and a direction is only half of a sortable
        // table; this is the other half, and it belongs to the host.
        let mut rows = [
            FileRow::new("report.pdf", "4.1 MB", 4_299_161, "2 minutes ago", 120),
            FileRow::new("budget.csv", "18 KB", 18_432, "yesterday", 86_400),
            FileRow::new("Images", "310 MB", 325_058_560, "last week", 604_800),
        ];
        let direction = self.sort.1;
        // Not `column`: that names the layout helper this page uses below.
        let sort_column = self.sort.0.clone();
        rows.sort_by(|a, b| {
            let ordering = match sort_column.as_ref() {
                "size" => a.bytes.cmp(&b.bytes),
                "changed" => a.age.cmp(&b.age),
                _ => a.name.cmp(b.name),
            };
            match direction {
                SortDirection::Ascending => ordering,
                SortDirection::Descending => ordering.reverse(),
            }
        });

        let mut tree_rows = Vec::new();
        // Flattened fresh each frame, skipping the children of anything
        // closed. This is the step that makes collapsing mean something.
        let mut tree: Vec<TreeRow> = Vec::new();
        for node in &self.tree {
            node.flatten_into(0, &mut tree);
        }
        for (index, row) in tree.iter().enumerate() {
            let selected = self.picked.as_ref() == Some(&row.id);
            let menu_target = row.id.clone();
            tree_rows.push(
                // The right-click listener belongs to the row rather than to
                // the tree, because the menu has to be about what was
                // actually clicked. Hanging it on the container instead only
                // ever knows about the selection, so right-clicking one row
                // opens a menu aimed at another.
                div()
                    .flex_none()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            // Selecting on the way is what every file list
                            // does, and it keeps the highlight and the menu
                            // saying the same thing.
                            this.picked = Some(menu_target.clone());
                            this.controls
                                .open_menu("row", event.position, menu_target.clone());
                            cx.notify();
                        }),
                    )
                    .child(tree_row(
                        "tree",
                        &tree,
                        index,
                        selected,
                        palette,
                        self,
                        cx,
                        |this, id, _window, cx| {
                            this.picked = Some(id);
                            cx.notify();
                        },
                        |this, id, expanded, _window, cx| {
                            for node in &mut this.tree {
                                if node.set_expanded(&id, expanded) {
                                    break;
                                }
                            }
                            cx.notify();
                        },
                    ))
                    .into_any_element(),
            );
        }

        let controls = &self.controls;
        let table = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .child(table_header(
                "table",
                &columns,
                Some((&self.sort.0, self.sort.1)),
                palette,
                cx,
                |this, id, direction, _window, cx| {
                    this.sort = (id, direction);
                    cx.notify();
                },
            ))
            .children(rows.iter().enumerate().map(|(slot, file)| {
                // Laid out from the same `columns` the header was given, so
                // the two grids cannot drift. Doing it by hand instead —
                // widths here, padding there — is exactly how a table ends
                // up not lining up with its own header.
                let values = [file.name, file.size, file.changed];
                // A re-sort slides each row to its new place rather than
                // redealing the table: keyed by the file, so the row is seen
                // to be the same row somewhere else.
                let place = slot as f32 * TABLE_ROW_HEIGHT;
                let offset =
                    controls.tween(format!("table-row-{}", file.name), place, vampir::MOVE) - place;
                div()
                    .relative()
                    .top(px(offset))
                    .h(px(TABLE_ROW_HEIGHT))
                    .flex()
                    .flex_none()
                    .items_center()
                    .children(columns.iter().zip(values).enumerate().map(
                        |(index, (column, value))| {
                            table_cell(
                                column,
                                div()
                                    .text_color(if index == 0 {
                                        palette.text_primary
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child(value),
                            )
                        },
                    ))
            }));

        column()
            .child(card(
                palette,
                "Tabs — drag one to reorder it",
                tab_bar(
                    "files",
                    &self.files,
                    self.file,
                    palette,
                    self,
                    cx,
                    |this, index, _window, cx| {
                        this.file = index;
                        cx.notify();
                    },
                    |this, index, _window, cx| {
                        if this.files.len() > 1 {
                            this.files.remove(index);
                            this.file = this.file.min(this.files.len() - 1);
                        }
                        cx.notify();
                    },
                ),
            ))
            .child(card(
                palette,
                "Tree and table — right-click a row, sort a column, drag the divider",
                div()
                    .flex()
                    .relative()
                    .h(px(180.0))
                    .child(split_area("split", cx))
                    .child(
                        div()
                            .id("tree")
                            .w(gpui::relative(self.split))
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .children(tree_rows),
                    )
                    .child(split_handle("split", self.split, true, palette, self, cx))
                    .child(table),
            ))
            .child(collapsible(
                "details",
                "Details",
                self.details_open,
                palette,
                self,
                cx,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .pt(px(8.0))
                    .text_color(palette.text_secondary)
                    .child("A collapsible section keeps its open state in the host, because")
                    .child("whether a section is open usually outlives the view."),
                |this, expanded, _window, cx| {
                    this.details_open = expanded;
                    cx.notify();
                },
            ))
            .into_any_element()
    }

    fn page_colour(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let wheel = hue_wheel(0.62, 0.11);
        let roles: [(&str, gpui::Rgba); 8] = [
            ("accent", palette.accent),
            ("control_fill", palette.control_fill),
            ("soft_fill", palette.soft_fill),
            ("primary_fill", palette.primary_fill),
            ("danger_fill", palette.danger_fill),
            ("field_surface", palette.field_surface),
            ("area_surface", palette.area_surface),
            ("field_border", palette.field_border),
        ];

        column()
            .child(
                row()
                    .items_start()
                    .child(
                        div().flex_1().child(card(
                            palette,
                            "Pick a colour",
                            column()
                                .child(field(
                                    palette,
                                    "Hue",
                                    hue_slider("swatch-hue", self.colour.hue, palette, self, cx),
                                ))
                                .child(field(
                                    palette,
                                    "Chroma and lightness",
                                    color_pad("colour-pad", self.colour, 140.0, palette, self, cx),
                                )),
                        )),
                    )
                    .child(
                        div().flex_1().child(card(
                            palette,
                            "Presets",
                            column()
                                .child(swatch_grid(
                                    "swatches",
                                    &wheel,
                                    Some(self.colour),
                                    26.0,
                                    palette,
                                    cx,
                                    |this, colour, _window, cx| {
                                        this.colour = colour;
                                        cx.notify();
                                    },
                                ))
                                .child(
                                    row().child(
                                        div()
                                            .h(px(56.0))
                                            .flex_1()
                                            .rounded(px(8.0))
                                            .border_1()
                                            .border_color(palette.field_border)
                                            .bg(lit(self.colour.to_rgba(), 0.05)),
                                    ),
                                )
                                .child(caption(
                                    palette,
                                    "Everything here is Oklch, so a hue sweep never goes garish.",
                                )),
                        )),
                    ),
            )
            .child(card(
                palette,
                "Palette roles at this hue",
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(10.0))
                    .children(roles.iter().map(|(name, colour)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .w(px(100.0))
                            .child(
                                div()
                                    .h(px(34.0))
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(palette.field_border)
                                    .bg(lit(*colour, 0.05)),
                            )
                            .child(caption(palette, name))
                    })),
            ))
            .into_any_element()
    }
}

// ---- Overlays ---------------------------------------------------------------

impl Gallery {
    fn row_menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::header(self.controls.menu_target().unwrap_or("Item").to_string()),
            MenuItem::action("open", "Open").shortcut("⏎"),
            MenuItem::action("rename", "Rename").shortcut("F2"),
            MenuItem::separator(),
            MenuItem::action("pin", "Pin to top").checked(true),
            MenuItem::action("reveal", "Show in folder").disabled(),
            MenuItem::separator(),
            MenuItem::action("delete", "Delete").shortcut("⌫").danger(),
        ]
    }

    /// 0 System, 1 Light, 2 Dark — the order the segmented control and the
    /// View menu both list them in.
    fn scheme_index(&self) -> usize {
        if self.follow_system {
            0
        } else if self.dark {
            2
        } else {
            1
        }
    }

    fn set_scheme(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_system = index == 0;
        self.dark = if self.follow_system {
            system_dark(window)
        } else {
            index == 2
        };
        cx.notify();
    }

    fn view_menu(&self) -> Vec<MenuItem> {
        let scheme = self.scheme_index();
        vec![
            MenuItem::header("Colour scheme"),
            MenuItem::action("system", "System").checked(scheme == 0),
            MenuItem::action("light", "Light").checked(scheme == 1),
            MenuItem::action("dark", "Dark").checked(scheme == 2),
            MenuItem::separator(),
            MenuItem::action("palette", "Command palette…")
                .shortcut(vampir::display("secondary-k")),
        ]
    }

    fn render_dialog(
        &mut self,
        palette: Palette,
        opacity: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        self.dialog_enter?;
        dialog(
            "confirm",
            "Delete this file?",
            420.0,
            opacity,
            self.dialog_exit.is_some(),
            palette,
            self,
            cx,
            div()
                .text_color(palette.text_secondary)
                .child("It goes to the Trash, and you can put it back later."),
            vec![
                DialogButton::new(
                    "cancel",
                    "Cancel",
                    ButtonVariant::Soft,
                    |this: &mut Gallery, _w, cx| {
                        this.dialog_exit = Some(Instant::now());
                        cx.notify();
                    },
                ),
                DialogButton::new(
                    "discard",
                    "Delete",
                    ButtonVariant::Danger,
                    |this: &mut Gallery, _w, cx| {
                        this.dialog_exit = Some(Instant::now());
                        this.note("Deleted report.pdf", cx);
                    },
                ),
            ],
            |this, _window, cx| {
                this.dialog_exit = Some(Instant::now());
                cx.notify();
            },
        )
        .map(IntoElement::into_any_element)
    }

    fn render_command_palette(
        &mut self,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.palette_open {
            return None;
        }
        let matches = self.palette_matches(cx);
        let highlighted = self.palette_index.min(matches.len().saturating_sub(1));

        let palette_element = command_palette(
            "commands",
            &self.palette_query,
            &matches,
            highlighted,
            palette,
            self,
            window,
            cx,
            |this, id, window, cx| this.run_command(id, window, cx),
            |this, window, cx| {
                this.show_palette(false, window, cx);
            },
        );
        Some(palette_element.into_any_element())
    }
}

/// Whether the OS is showing windows dark right now. GPUI folds the
/// vibrant variants in with their plain ones for this purpose.
fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// The system's interface face, under the name each platform knows it by.
/// An unknown name falls back to *something*, but not to the same something
/// everywhere, and not to the face the rest of the desktop is set in.
fn ui_font() -> &'static str {
    if cfg!(target_os = "macos") {
        ".SystemUIFont"
    } else if cfg!(target_os = "windows") {
        "Segoe UI"
    } else {
        "sans-serif"
    }
}

// ---- The menu bar -----------------------------------------------------------

/// The macOS menu bar.
///
/// This is not decoration. An app that never calls `set_menus` has no main
/// menu at all, and macOS leaves whatever the last app put there on screen
/// — usually Finder's — so the gallery looks like it is not the front app
/// even when it is. It is also the only route to ⌘Q, ⌘H and the Window
/// menu, none of which the system provides for free.
///
/// The name of the first menu comes from the bundle, not from here: see
/// `packaging/macos/` for how the gallery gets one.
fn app_menus() -> Vec<Menu> {
    vec![
        Menu::new("Vampir gallery").items(if cfg!(target_os = "macos") {
            vec![
                OsMenuItem::os_submenu("Services", SystemMenuType::Services),
                OsMenuItem::separator(),
                OsMenuItem::action("Hide Vampir gallery", Hide),
                OsMenuItem::action("Hide Others", HideOthers),
                OsMenuItem::action("Show All", ShowAll),
                OsMenuItem::separator(),
                OsMenuItem::action("Quit Vampir gallery", Quit),
            ]
        } else {
            // Services and hiding are macOS ideas; the other desktops have
            // neither.
            vec![OsMenuItem::action("Quit", Quit)]
        }),
        // The clipboard items carry an `OsAction` so macOS routes them
        // through the responder chain to whatever text is focused, which is
        // what makes Edit work for the `TextInput`s and for the system's own
        // fields alike.
        Menu::new("Edit").items([
            OsMenuItem::os_action("Undo", ti::Undo, OsAction::Undo),
            OsMenuItem::os_action("Redo", ti::Redo, OsAction::Redo),
            OsMenuItem::separator(),
            OsMenuItem::os_action("Cut", ti::Cut, OsAction::Cut),
            OsMenuItem::os_action("Copy", ti::Copy, OsAction::Copy),
            OsMenuItem::os_action("Paste", ti::Paste, OsAction::Paste),
            OsMenuItem::os_action("Select All", ti::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            OsMenuItem::action("Controls", ShowControls),
            OsMenuItem::action("Fields", ShowFields),
            OsMenuItem::action("Data", ShowData),
            OsMenuItem::action("Colour", ShowColour),
            OsMenuItem::separator(),
            OsMenuItem::action("System", UseSystemScheme),
            OsMenuItem::action("Light", UseLight),
            OsMenuItem::action("Dark", UseDark),
            OsMenuItem::separator(),
            OsMenuItem::action("Command Palette\u{2026}", TogglePalette),
        ]),
        // GPUI hands a menu named exactly "Window" to macOS as the windows
        // menu, which is what fills it with the window list.
        Menu::new("Window").items([
            OsMenuItem::action("Minimize", MinimizeWindow),
            OsMenuItem::action("Zoom", ZoomWindow),
            OsMenuItem::separator(),
            OsMenuItem::action("Close Window", CloseWindow),
        ]),
    ]
}

// ---- main -------------------------------------------------------------------

fn main() {
    application().run(|cx: &mut App| {
        // The text input ships the actions; binding them is the host's
        // business, exactly as it is in a real app. `secondary` is ⌘ on
        // macOS and Ctrl everywhere else, so one binding serves both. The
        // rest of the text keymap is the one place the platforms genuinely
        // disagree — ⌥ moves by word on a Mac, Ctrl does elsewhere — so it is
        // written out twice.
        let mut bindings = vec![
            KeyBinding::new("backspace", ti::Backspace, Some("TextInput")),
            KeyBinding::new("delete", ti::Delete, Some("TextInput")),
            KeyBinding::new("left", ti::Left, Some("TextInput")),
            KeyBinding::new("right", ti::Right, Some("TextInput")),
            KeyBinding::new("up", ti::Up, Some("TextInput")),
            KeyBinding::new("down", ti::Down, Some("TextInput")),
            KeyBinding::new("shift-left", ti::SelectLeft, Some("TextInput")),
            KeyBinding::new("shift-right", ti::SelectRight, Some("TextInput")),
            KeyBinding::new("shift-up", ti::SelectUp, Some("TextInput")),
            KeyBinding::new("shift-down", ti::SelectDown, Some("TextInput")),
            KeyBinding::new("home", ti::Home, Some("TextInput")),
            KeyBinding::new("end", ti::End, Some("TextInput")),
            KeyBinding::new("shift-home", ti::SelectToHome, Some("TextInput")),
            KeyBinding::new("shift-end", ti::SelectToEnd, Some("TextInput")),
            KeyBinding::new("enter", ti::Enter, Some("TextInput")),
            KeyBinding::new("secondary-a", ti::SelectAll, Some("TextInput")),
            KeyBinding::new("secondary-c", ti::Copy, Some("TextInput")),
            KeyBinding::new("secondary-v", ti::Paste, Some("TextInput")),
            KeyBinding::new("secondary-x", ti::Cut, Some("TextInput")),
            KeyBinding::new("secondary-z", ti::Undo, Some("TextInput")),
            KeyBinding::new("secondary-shift-z", ti::Redo, Some("TextInput")),
            KeyBinding::new("escape", Dismiss, None),
            // App and window shortcuts. macOS draws these next to the menu
            // items below, but it does not *implement* them: without the
            // binding the menu item is grey and the key does nothing.
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
            KeyBinding::new("secondary-k", TogglePalette, None),
            // Tab walks the window. The controls own the keys *inside*
            // themselves; this is the one that moves between them.
            KeyBinding::new("tab", FocusNext, None),
            KeyBinding::new("shift-tab", FocusPrevious, None),
            KeyBinding::new("secondary-1", ShowControls, None),
            KeyBinding::new("secondary-2", ShowFields, None),
            KeyBinding::new("secondary-3", ShowData, None),
            KeyBinding::new("secondary-4", ShowColour, None),
        ];
        if cfg!(target_os = "macos") {
            bindings.extend([
                KeyBinding::new("alt-backspace", ti::DeleteWordLeft, Some("TextInput")),
                KeyBinding::new("cmd-backspace", ti::DeleteToLineStart, Some("TextInput")),
                KeyBinding::new("alt-left", ti::WordLeft, Some("TextInput")),
                KeyBinding::new("alt-right", ti::WordRight, Some("TextInput")),
                KeyBinding::new("alt-shift-left", ti::SelectWordLeft, Some("TextInput")),
                KeyBinding::new("alt-shift-right", ti::SelectWordRight, Some("TextInput")),
                KeyBinding::new("cmd-left", ti::Home, Some("TextInput")),
                KeyBinding::new("cmd-right", ti::End, Some("TextInput")),
                KeyBinding::new("cmd-shift-left", ti::SelectToHome, Some("TextInput")),
                KeyBinding::new("cmd-shift-right", ti::SelectToEnd, Some("TextInput")),
                KeyBinding::new("cmd-up", ti::DocumentStart, Some("TextInput")),
                KeyBinding::new("cmd-down", ti::DocumentEnd, Some("TextInput")),
                // Hiding is a macOS idea; the other desktops have neither
                // the command nor a key for it.
                KeyBinding::new("cmd-h", Hide, None),
                KeyBinding::new("cmd-alt-h", HideOthers, None),
                KeyBinding::new("cmd-m", MinimizeWindow, None),
            ]);
        } else {
            bindings.extend([
                KeyBinding::new("ctrl-backspace", ti::DeleteWordLeft, Some("TextInput")),
                KeyBinding::new("ctrl-left", ti::WordLeft, Some("TextInput")),
                KeyBinding::new("ctrl-right", ti::WordRight, Some("TextInput")),
                KeyBinding::new("ctrl-shift-left", ti::SelectWordLeft, Some("TextInput")),
                KeyBinding::new("ctrl-shift-right", ti::SelectWordRight, Some("TextInput")),
                KeyBinding::new("ctrl-home", ti::DocumentStart, Some("TextInput")),
                KeyBinding::new("ctrl-end", ti::DocumentEnd, Some("TextInput")),
            ]);
        }
        cx.bind_keys(bindings);

        // Actions with no view behind them. A menu item is only enabled when
        // its action is reachable, and these have to stay reachable even
        // when no window has focus.
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
        cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());

        cx.set_menus(app_menus());
        // Vampir's own, because the gallery has no face of its own to draw.
        // An application with one swaps a line: `AppIcon::png(include_bytes!(..))`.
        ICON.install(cx);

        // One window, so closing it is quitting. Without this the process
        // stays alive with no window and no way back to it.
        cx.on_window_closed(|cx, _id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(980.0), px(720.0)), cx);
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "freebsd")),
            allow(unused_mut)
        )]
        let mut options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.0), px(520.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("Vampir gallery".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Set rather than passed in the literal: the field's type comes from
        // GPUI's own image crate, which only this platform's dev-dependencies
        // bring in, so elsewhere there is no name for it to be written under.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            options.icon = ICON.window_icon();
        }

        cx.open_window(options, |window, cx| cx.new(|cx| Gallery::new(window, cx)))
            .expect("failed to open the gallery window");

        cx.activate(true);
    });
}
