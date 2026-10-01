//! Every control in the toolkit, in one window.
//!
//! ```bash
//! cargo run --example gallery
//! ```
//!
//! It is also the smallest complete host: one struct, one `ControlHost`
//! impl, one `Render`. Everything that is not a widget being shown — the
//! theme, the keyboard, the drags, what the overlays remember — is the
//! toolkit's. What is left here is the gallery's own data, its pages, and
//! the menu bar and shortcuts a real application owns.

use gpui::{
    App, Bounds, Context, Entity, KeyBinding, Menu, MenuItem as OsMenuItem, Point, ScrollHandle,
    SharedString, SystemMenuType, TitlebarOptions, Window, WindowBounds, WindowOptions, actions,
    div, prelude::*, px, size,
};
use gpui_ce_platform::application;

#[cfg(feature = "app-icon")]
use vampir::AppIcon;
use vampir::{
    BadgeTone, ButtonVariant, CONTROL_HEIGHT, ChipSelection, Choice, Chord, Column, ComboId,
    Command, ControlHost, ControlState, DialogButton, Hint, InputStyle, MAX_CHROMA, MenuItem,
    Oklch, Palette, Scheme, ScrollAxis, SliderTrack, SortDirection, Span, TEXT_SIZE,
    TITLE_TEXT_SIZE, Tab, TextInput, Theme, TreeRow, WidgetContext, arriving, badge, bind_keys,
    button, caption, card, checkbox, chip_group, collapsible, color_pad, column, combo,
    command_palette, context_menu, dialog, edit_menu, fading_text, flatten_tree, glyph, ground,
    hue_picker, hue_slider, hue_wheel, icon_button, labelled, lit, menu_button, menu_target,
    progress_bar, radio_group, reorder, row, saturation_picker, scheme_picker, scroll_area,
    search_list, segmented, separator, shortcut_recorder, slider, spinbox, spinner, split_area,
    split_handle, swatch_grid, switch, tab_bar, table_header, table_row, text_area, text_field,
    tree_row, ui_font,
};

// The gallery is also the smallest complete *macOS* host, so it carries the
// actions a real app has to own: the menu bar routes through these, and so
// do the shortcuts the menu bar advertises.
/// The icon this application shows in the dock, the task bar and its window.
#[cfg(feature = "app-icon")]
const ICON: AppIcon = AppIcon::default_icon();

actions!(
    gallery,
    [
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

/// A node of the Data page's file tree. The host owns a real tree and
/// `flatten_tree` walks it into what is on screen each frame.
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

    /// The row the toolkit draws for this node.
    fn row(&self, depth: usize) -> TreeRow {
        let row = if self.children.is_empty() {
            TreeRow::leaf(self.id, self.label, depth)
        } else {
            TreeRow::branch(self.id, self.label, depth, self.expanded)
        };
        row.detail(self.detail)
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

// ---- The host ---------------------------------------------------------------

struct Gallery {
    controls: ControlState,
    scroll: ScrollHandle,

    // Tabs, in the order they are currently arranged. Dragging one reorders
    // this, which is why the page is an enum rather than an index.
    pages: Vec<Page>,
    page: usize,

    // Controls page.
    switched: bool,
    checks: [bool; 2],
    routine: usize,
    sync: usize,
    tags: Vec<bool>,
    volume: f32,
    steps: f32,
    takes: i32,
    sort_by: usize,

    // Fields page.
    name: Entity<TextInput>,
    notes: Entity<TextInput>,
    query: Entity<TextInput>,
    /// What the search field searches.
    search_items: Vec<Command>,
    takes_edit: Entity<TextInput>,
    chord: Option<Chord>,

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

    // The command palette's query and what it searches.
    palette_query: Entity<TextInput>,
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
    /// says which one moved. The theme's hue and saturation pickers are
    /// read by the toolkit itself.
    fn track_dragged(&mut self, id: ComboId, at: Point<f32>, cx: &mut Context<Self>) {
        match id {
            "volume" => self.volume = at.x,
            "steps" => self.steps = at.x,
            "split" => self.split = at.x.clamp(0.2, 0.8),
            "swatch-hue" => self.colour.hue = Theme::hue_from_track(at.x),
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
        match bar {
            "pages" => reorder(&mut self.pages, &mut self.page, from, to),
            "files" => reorder(&mut self.files, &mut self.file, from, to),
            _ => return,
        }
        self.note(format!("Moved a tab from {from} to {to}"), cx);
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
        let mut controls = ControlState::new();
        // Violet at standard saturation gives the gallery a consistent
        // starting point. Each application chooses its own look here.
        controls.theme.hue = 268.0;
        controls.theme.saturation = 1.0;
        // The scheme starts as the desktop's and follows it until someone
        // picks one by hand. `VAMPIR_SLOW_MOTION=8 tools/gallery.sh launch`
        // runs every fade and slide eight times slower, which is how a
        // screenshot can catch one half way; see CONTRIBUTING.md.
        controls.observe_appearance(window, cx);
        controls.slow_motion_from_env();
        let palette = controls.palette();
        let style = |size| InputStyle::from_palette(palette, size);

        let name = cx.new(|cx| TextInput::new(cx, "Untitled project", false, style(TEXT_SIZE)));
        let notes = cx.new(|cx| {
            let mut input =
                TextInput::new(cx, "Anything worth remembering...", true, style(TEXT_SIZE));
            input.content = "Ship the empty state first. Check #contrast in light mode, and ask \
                             Sam where the #copy for the welcome screen lives."
                .into();
            // Rich text is a closure from content to spans, re-run on layout.
            // `accent()` reads the colour when the text paints, so the tags
            // follow the theme without the highlighter being set again.
            input.set_highlighter(
                |text| {
                    hashtags(text)
                        .into_iter()
                        .map(|range| Span::new(range).accent().bold())
                        .collect()
                },
                cx,
            );
            input
        });
        let query = cx.new(|cx| TextInput::new(cx, "Search", false, style(TEXT_SIZE)));
        let takes_edit = cx.new(|cx| {
            let mut input = TextInput::new(cx, "", false, style(TEXT_SIZE));
            input.content = "24".into();
            input
        });
        let palette_query =
            cx.new(|cx| TextInput::new(cx, "Type a command", false, style(TITLE_TEXT_SIZE)));

        // The keyboard starts on the root, one Tab from the first control.
        controls.focus_root(window, cx);

        Self {
            controls,
            scroll: ScrollHandle::new(),
            pages: vec![Page::Controls, Page::Fields, Page::Data, Page::Colour],
            page: 0,
            switched: true,
            checks: [true, false],
            routine: 0,
            sync: 1,
            tags: vec![true, false, true, false, false, false, false],
            volume: 0.72,
            steps: 0.5,
            takes: 24,
            sort_by: 1,
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
            takes_edit,
            chord: Some(Chord {
                keystroke: "secondary-shift-s".into(),
                display: vampir::display("secondary-shift-s"),
            }),
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
            palette_query,
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
        }
    }

    // ---- Menu and shortcut actions ------------------------------------------
    //
    // Each of these is reachable two ways — a menu item and a key binding —
    // and macOS expects both to land in the same place, so they are methods
    // rather than closures buried in the menu definition.

    fn toggle_palette(&mut self, _: &TogglePalette, window: &mut Window, cx: &mut Context<Self>) {
        self.controls
            .toggle_palette("commands", &self.palette_query, window, cx);
        cx.notify();
    }

    fn use_system_scheme(&mut self, _: &UseSystemScheme, _: &mut Window, cx: &mut Context<Self>) {
        self.set_scheme(Scheme::System, cx);
    }

    fn use_light(&mut self, _: &UseLight, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_scheme(Scheme::Light, cx);
    }

    fn use_dark(&mut self, _: &UseDark, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_scheme(Scheme::Dark, cx);
    }

    fn set_scheme(&mut self, scheme: Scheme, cx: &mut Context<Self>) {
        self.controls.theme.scheme = scheme;
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
            // to go, and focus on an element that has gone dispatches
            // nothing — not even the root's own shortcuts.
            self.controls.focus_root(window, cx);
        }
        self.page = index;
        cx.notify();
    }

    /// Runs a command. The palette has already closed and handed the
    /// keyboard back by the time this is called, from a click and from Enter
    /// alike.
    fn run_command(&mut self, id: SharedString, cx: &mut Context<Self>) {
        match id.as_ref() {
            "theme" => {
                let theme = &mut self.controls.theme;
                theme.scheme = if theme.is_dark() {
                    Scheme::Light
                } else {
                    Scheme::Dark
                };
            }
            "hue" => {
                let theme = &mut self.controls.theme;
                theme.hue = (theme.hue + 47.0) % 360.0;
            }
            "sync" => self.switched = !self.switched,
            _ => {}
        }
        self.note(format!("Ran {id}"), cx);
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

    fn minimize(&mut self, _: &MinimizeWindow, window: &mut Window, _cx: &mut Context<Self>) {
        window.minimize_window();
    }

    fn zoom(&mut self, _: &ZoomWindow, window: &mut Window, _cx: &mut Context<Self>) {
        window.zoom_window();
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
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

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

// ---- Render -----------------------------------------------------------------

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The palette as shown this frame: the theme's, crossing over if the
        // scheme or the hue has just changed.
        let palette = self.controls.palette();
        let page = self.pages[self.page];

        let body = match page {
            Page::Controls => self.page_controls(palette, cx),
            Page::Fields => self.page_fields(palette, window, cx),
            Page::Data => self.page_data(palette, cx),
            Page::Colour => self.page_colour(palette, cx),
        };
        // Keyed by the page, not the tab's position, so reordering the tabs
        // is not a page change.
        let body = arriving("page", page as u64, &self.controls, body);

        // Asked *after* the page is built, on purpose: a control notices its
        // own state change while it renders. Fades and slides stop asking
        // when they finish; the spinner requests its own frames while visible.
        if self.controls.animating() {
            window.request_animation_frame();
        }

        // The toolkit's handlers first — Tab, Shift-Tab, Escape, the drags,
        // the mouse taking over — and the gallery's own actions after them.
        vampir::root(div().id("root"), self, cx)
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui_font())
            .text_size(px(TEXT_SIZE))
            .bg(ground(palette.backdrop))
            .text_color(palette.text_primary)
            .on_action(cx.listener(Self::toggle_palette))
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
            .child(self.header(palette, cx))
            .child(
                scroll_area(
                    "page",
                    &self.scroll,
                    ScrollAxis::Vertical,
                    WidgetContext::new(palette, self, cx),
                    window,
                    div().p(px(20.0)).child(body),
                )
                .flex_1(),
            )
            .child(self.footer(palette))
            .children(self.render_dialog(palette, cx))
            .children(command_palette(
                "commands",
                &self.palette_query,
                &self.commands,
                WidgetContext::new(palette, self, cx),
                window,
                |this, id, _window, cx| this.run_command(id, cx),
            ))
            .children(context_menu(
                "row",
                &self.row_menu(),
                palette,
                self,
                cx,
                |this, action, _window, cx| {
                    let target = this.controls.menu_target().unwrap_or("").to_string();
                    let description = match action {
                        "move-documents" => "Move to Documents",
                        "move-images" => "Move to Images",
                        "duplicate" => "Duplicate",
                        _ => action,
                    };
                    this.note(format!("{description} → {target}"), cx);
                },
            ))
            .children(context_menu(
                "view-menu",
                &self.view_menu(),
                palette,
                self,
                cx,
                |this, action, window, cx| {
                    match action {
                        "system" => {
                            this.set_scheme(Scheme::System, cx);
                            this.note("Following system appearance", cx);
                        }
                        "light" => {
                            this.set_scheme(Scheme::Light, cx);
                            this.note("Light appearance", cx);
                        }
                        "dark" => {
                            this.set_scheme(Scheme::Dark, cx);
                            this.note("Dark appearance", cx);
                        }
                        "violet" => {
                            this.controls.theme.hue = 268.0;
                            this.note("Violet accent", cx);
                        }
                        "teal" => {
                            this.controls.theme.hue = 186.0;
                            this.note("Teal accent", cx);
                        }
                        "palette" => {
                            this.controls
                                .open_palette("commands", &this.palette_query, window, cx)
                        }
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
            .px(px(20.0))
            .pt(px(20.0))
            .child(tab_bar(
                "pages",
                &tabs,
                self.page,
                WidgetContext::new(palette, self, cx),
                |this, index, _window, cx| {
                    this.page = index;
                    cx.notify();
                },
                |_this, _index, _window, _cx| {},
            ))
            .child(div().flex_1())
            .child(div().w(px(216.0)).child(scheme_picker(
                "scheme",
                WidgetContext::new(palette, self, cx),
            )))
            .child(menu_button(
                "view-menu",
                "View",
                true,
                WidgetContext::new(palette, self, cx),
            ))
    }

    fn footer(&self, palette: Palette) -> impl IntoElement {
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
                        fading_text("last-action", self.last_action.clone(), &self.controls)
                            .text_color(palette.text_secondary),
                    )
                    .child(div().flex_1())
                    // Says so when the gallery is running slowed down, so a
                    // screenshot taken that way cannot pass for the real thing.
                    .children(self.controls.time_scale().map(|scale| {
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
                            .child(icon_button(
                                "undo",
                                glyph("↺"),
                                "Undo",
                                CONTROL_HEIGHT,
                                false,
                                true,
                                palette,
                                cx,
                                |this, _window, cx| this.note("Undo", cx),
                            ))
                            .child(icon_button(
                                "redo",
                                glyph("↻"),
                                "Redo",
                                CONTROL_HEIGHT,
                                false,
                                true,
                                palette,
                                cx,
                                |this, _window, cx| this.note("Redo", cx),
                            ))
                            .child(icon_button(
                                "star",
                                glyph("★"),
                                // The label with its shortcut after it.
                                Hint::new("Favourite").shortcut(vampir::display("secondary-d")),
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
                        WidgetContext::new(palette, self, cx),
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
                        WidgetContext::new(palette, self, cx),
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
                        WidgetContext::new(palette, self, cx),
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
                        WidgetContext::new(palette, self, cx),
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
                            WidgetContext::new(palette, self, cx),
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
                                .child(labelled(
                                    palette,
                                    "Sync",
                                    segmented(
                                        "auto",
                                        &sync,
                                        self.sync,
                                        true,
                                        WidgetContext::new(palette, self, cx),
                                        |this, index, _window, cx| {
                                            this.sync = index;
                                            cx.notify();
                                        },
                                    ),
                                ))
                                .child(labelled(
                                    palette,
                                    "Sort by",
                                    combo(
                                        "sort-by",
                                        self.sort_by,
                                        &sorts,
                                        None,
                                        WidgetContext::new(palette, self, cx),
                                        |this, index, _window, cx| {
                                            this.sort_by = index;
                                            cx.notify();
                                        },
                                    ),
                                ))
                                .child(labelled(
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
                    WidgetContext::new(palette, self, cx),
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
                    .child(labelled(
                        palette,
                        "Opacity",
                        slider(
                            "volume",
                            self.volume,
                            SliderTrack::Continuous,
                            WidgetContext::new(palette, self, cx),
                        ),
                    ))
                    .child(labelled(
                        palette,
                        "Grid size",
                        slider(
                            "steps",
                            self.steps,
                            SliderTrack::Stepped { stops: 5 },
                            WidgetContext::new(palette, self, cx),
                        ),
                    ))
                    .child(labelled(
                        palette,
                        "Uploading",
                        progress_bar(self.volume, palette),
                    )),
            ))
            .into_any_element()
    }

    fn page_fields(
        &mut self,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        column()
            .child(card(
                palette,
                "Text",
                column()
                    .child(labelled(
                        palette,
                        "Project name",
                        text_field(&self.name, palette, window, cx),
                    ))
                    .child(labelled(
                        palette,
                        "Notes — #tags are highlighted by a Highlighter closure",
                        text_area(&self.notes, Some(96.0), true, palette, window, cx),
                    ))
                    .child(labelled(
                        palette,
                        "Search — the list filters as you type; ↑↓ and Enter pick",
                        search_list(
                            "search",
                            &self.query,
                            &self.search_items,
                            WidgetContext::new(palette, self, cx),
                            window,
                            |this, id, _window, cx| this.open_result(id, cx),
                        ),
                    )),
            ))
            .child(card(
                palette,
                "Shortcut",
                column()
                    .child(labelled(
                        palette,
                        "Quick open — click, then press a chord",
                        shortcut_recorder(
                            "chord",
                            self.chord.as_ref(),
                            WidgetContext::new(palette, self, cx),
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
                                    this.controls.open_dialog("confirm");
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
                                |this, window, cx| {
                                    this.controls.open_palette(
                                        "commands",
                                        &this.palette_query,
                                        window,
                                        cx,
                                    );
                                    cx.notify();
                                },
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

        // Flattened fresh each frame, skipping the children of anything
        // closed.
        let tree = flatten_tree(&self.tree, &|node, depth| node.row(depth), &|node| {
            &node.children
        });
        let mut tree_rows = Vec::with_capacity(tree.len());
        for (index, row) in tree.iter().enumerate() {
            let selected = self.picked.as_ref() == Some(&row.id);
            let row_element = tree_row(
                "tree",
                &tree,
                index,
                selected,
                WidgetContext::new(palette, self, cx),
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
            )
            .into_any_element();
            // Right-click opens the row menu on this row, selecting it on the
            // way so the highlight and the menu say the same thing.
            tree_rows.push(
                menu_target(
                    "row",
                    row.id.clone(),
                    cx,
                    |this, id, _cx| this.picked = Some(id),
                    row_element,
                )
                .into_any_element(),
            );
        }

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
                table_row(
                    "table",
                    file.name,
                    slot,
                    &columns,
                    [file.name, file.size, file.changed],
                    palette,
                    &self.controls,
                )
            }));

        column()
            .child(card(
                palette,
                "Tabs — drag one to reorder it",
                tab_bar(
                    "files",
                    &self.files,
                    self.file,
                    WidgetContext::new(palette, self, cx),
                    |this, index, _window, cx| {
                        this.file = index;
                        cx.notify();
                    },
                    |this, index, _window, cx| {
                        if this.files.len() > 1 {
                            this.files.remove(index);
                            if index < this.file {
                                this.file -= 1;
                            }
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
                    .child(split_handle(
                        "split",
                        self.split,
                        true,
                        WidgetContext::new(palette, self, cx),
                    ))
                    .child(table),
            ))
            .child(collapsible(
                "details",
                "Details",
                self.details_open,
                WidgetContext::new(palette, self, cx),
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
        let roles: [(&str, gpui::Rgba); 9] = [
            ("accent", palette.accent),
            ("backdrop", palette.backdrop),
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
                                .child(labelled(
                                    palette,
                                    "Hue",
                                    hue_slider(
                                        "swatch-hue",
                                        self.colour.hue,
                                        WidgetContext::new(palette, self, cx),
                                    ),
                                ))
                                .child(labelled(
                                    palette,
                                    "Chroma and lightness",
                                    color_pad(
                                        "colour-pad",
                                        self.colour,
                                        140.0,
                                        WidgetContext::new(palette, self, cx),
                                    ),
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
            .child(
                row()
                    .items_start()
                    .child(
                        div().flex_1().child(card(
                            palette,
                            "App theme",
                            column()
                                .child(labelled(
                                    palette,
                                    "Hue",
                                    hue_picker("theme-hue", WidgetContext::new(palette, self, cx)),
                                ))
                                .child(
                                    column()
                                        .gap(px(6.0))
                                        .child(
                                            row()
                                                .child(caption(palette, "Saturation"))
                                                .child(div().flex_1())
                                                .child(
                                                    fading_text(
                                                        "theme-saturation-label",
                                                        format!(
                                                            "{:.0}%",
                                                            self.controls.theme.saturation * 100.0
                                                        ),
                                                        &self.controls,
                                                    )
                                                    .text_color(palette.text_secondary),
                                                ),
                                        )
                                        .child(saturation_picker(
                                            "theme-saturation",
                                            WidgetContext::new(palette, self, cx),
                                        )),
                                )
                                .child(caption(palette, "0% grey · 100% standard · 200% vivid")),
                        )),
                    )
                    .child(
                        div().flex_1().child(card(
                            palette,
                            "App palette",
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
                        )),
                    ),
            )
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
            MenuItem::submenu(
                "Organize",
                vec![
                    MenuItem::submenu(
                        "Move to",
                        vec![
                            MenuItem::action("move-documents", "Documents"),
                            MenuItem::action("move-images", "Images"),
                        ],
                    ),
                    MenuItem::action("duplicate", "Duplicate"),
                ],
            ),
            MenuItem::separator(),
            MenuItem::action("delete", "Delete").shortcut("⌫").danger(),
        ]
    }

    fn view_menu(&self) -> Vec<MenuItem> {
        let scheme = self.controls.theme.scheme;
        vec![
            MenuItem::submenu(
                "Appearance",
                vec![
                    MenuItem::submenu(
                        "Colour scheme",
                        vec![
                            MenuItem::action("system", "System").checked(scheme == Scheme::System),
                            MenuItem::action("light", "Light").checked(scheme == Scheme::Light),
                            MenuItem::action("dark", "Dark").checked(scheme == Scheme::Dark),
                        ],
                    ),
                    MenuItem::separator(),
                    MenuItem::action("violet", "Violet accent"),
                    MenuItem::action("teal", "Teal accent"),
                ],
            ),
            MenuItem::separator(),
            MenuItem::action("palette", "Command palette…")
                .shortcut(vampir::display("secondary-k")),
        ]
    }

    fn render_dialog(
        &mut self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        dialog(
            "confirm",
            "Delete this file?",
            420.0,
            WidgetContext::new(palette, self, cx),
            div()
                .text_color(palette.text_secondary)
                .child("It goes to the Trash, and you can put it back later."),
            vec![
                DialogButton::new(
                    "cancel",
                    "Cancel",
                    ButtonVariant::Soft,
                    |_this: &mut Gallery, _window, _cx| {},
                ),
                DialogButton::new(
                    "discard",
                    "Delete",
                    ButtonVariant::Danger,
                    |this: &mut Gallery, _window, cx| this.note("Deleted report.pdf", cx),
                ),
            ],
            |_this, _window, _cx| {},
        )
        .map(IntoElement::into_any_element)
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
        // The toolkit's Edit menu: its items carry OS actions, so macOS
        // routes them to whatever text is focused — the gallery's inputs and
        // the system's own fields alike.
        edit_menu(),
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
        // The keys every application shares — Tab, Escape, text editing —
        // are the toolkit's to bind. The gallery's own come after: a later
        // binding wins, so an application that wanted Escape for something
        // else could still take it.
        bind_keys(cx);
        // macOS draws these next to the menu items below, but it does not
        // *implement* them: without the binding the menu item is grey and
        // the key does nothing. `secondary` is ⌘ on macOS and Ctrl
        // everywhere else, so one binding serves both.
        let mut bindings = vec![
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
            KeyBinding::new("secondary-k", TogglePalette, None),
            KeyBinding::new("secondary-1", ShowControls, None),
            KeyBinding::new("secondary-2", ShowFields, None),
            KeyBinding::new("secondary-3", ShowData, None),
            KeyBinding::new("secondary-4", ShowColour, None),
        ];
        if cfg!(target_os = "macos") {
            // Hiding is a macOS idea; the other desktops have neither the
            // command nor a key for it.
            bindings.extend([
                KeyBinding::new("cmd-h", Hide, None),
                KeyBinding::new("cmd-alt-h", HideOthers, None),
                KeyBinding::new("cmd-m", MinimizeWindow, None),
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
        #[cfg(feature = "app-icon")]
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
            not(all(feature = "app-icon", any(target_os = "linux", target_os = "freebsd"))),
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
        #[cfg(all(feature = "app-icon", any(target_os = "linux", target_os = "freebsd")))]
        {
            options.icon = ICON.window_icon();
        }

        cx.open_window(options, |window, cx| cx.new(|cx| Gallery::new(window, cx)))
            .expect("failed to open the gallery window");

        cx.activate(true);
    });
}
