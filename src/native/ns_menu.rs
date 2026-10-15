//! Adopting an AppKit menu: its rows as [`MenuItem`]s the toolkit's menus
//! can show, and its actions kept alive to run when a row is chosen.
//!
//! A native view that proposes an `NSMenu` — a web view's context menu, a
//! text view's — expects AppKit to pop it up. A host that would rather
//! draw it with the toolkit adopts it instead: hidden rows and Option-key
//! alternates are left out, separators are collapsed, an empty submenu is
//! dropped and a disabled one becomes a disabled row, and every row that
//! does something keeps the menu and index AppKit runs it by.
//!
//! ```ignore
//! let menu = NativeMenu::adopt(proposed, |item| host_icon_for(item));
//! floating_menu::open_with(cx, window, at, menu.items().to_vec(), palette,
//!     move |choice, _window, _cx| {
//!         if let Some(id) = choice {
//!             menu.run(id);
//!         }
//!     });
//! ```
//!
//! The menu must be run while whoever proposed it still holds it: WebKit,
//! for one, releases the proxy its actions go through once its completion
//! handler is called.

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSMenu, NSMenuItem, NSUserInterfaceItemIdentification};

use crate::menu::{MenuAction, MenuIcon, MenuItem};

/// One row of an adopted menu that does something, kept so it can be run.
pub struct NativeAction {
    menu: Retained<NSMenu>,
    index: isize,
    // NSMenuItem targets are weak. Keep them alive as long as the menu.
    _target: Option<Retained<AnyObject>>,
}

impl NativeAction {
    /// The AppKit item itself, for whatever the host wants to read off it.
    pub fn item(&self) -> Option<Retained<NSMenuItem>> {
        self.menu.itemAtIndex(self.index)
    }

    /// The item's `identifier`, the stable name a framework gives a row
    /// where its title is localised.
    pub fn identifier(&self) -> Option<String> {
        self.item()?.identifier().map(|id| id.to_string())
    }

    /// Runs the item's action as AppKit would have on a click.
    pub fn run(&self) {
        self.menu.performActionForItemAtIndex(self.index);
    }
}

/// An AppKit menu as rows the toolkit draws, with its actions kept.
pub struct NativeMenu {
    items: Vec<MenuItem<usize>>,
    actions: Vec<NativeAction>,
}

impl NativeMenu {
    /// Adopts `menu`. `icon` is asked for each row's icon, from the item.
    pub fn adopt(menu: &NSMenu, icon: impl Fn(&NSMenuItem) -> Option<MenuIcon>) -> Self {
        let mut actions = Vec::new();
        let items = convert(menu, &mut actions, &icon);
        NativeMenu { items, actions }
    }

    /// The rows, each action's id being its index among [`Self::actions`].
    pub fn items(&self) -> &[MenuItem<usize>] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn actions(&self) -> &[NativeAction] {
        &self.actions
    }

    pub fn action(&self, id: usize) -> Option<&NativeAction> {
        self.actions.get(id)
    }

    /// The id of the first action `matches`, for a host that treats one
    /// of a framework's rows as its own.
    pub fn find(&self, matches: impl Fn(&NativeAction) -> bool) -> Option<usize> {
        self.actions.iter().position(matches)
    }

    /// The row an action is drawn as, wherever it is in the tree, for a
    /// host that relabels one.
    pub fn row_mut(&mut self, id: usize) -> Option<&mut MenuAction<usize>> {
        MenuItem::action_mut(&mut self.items, |action| action.id == id)
    }

    /// Runs the action with `id`. False if there is none.
    pub fn run(&self, id: usize) -> bool {
        match self.actions.get(id) {
            Some(action) => {
                action.run();
                true
            }
            None => false,
        }
    }
}

impl std::fmt::Debug for NativeMenu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeMenu")
            .field("rows", &self.items.len())
            .field("actions", &self.actions.len())
            .finish()
    }
}

fn convert(
    menu: &NSMenu,
    actions: &mut Vec<NativeAction>,
    icon: &impl Fn(&NSMenuItem) -> Option<MenuIcon>,
) -> Vec<MenuItem<usize>> {
    let mut items = Vec::new();
    for (index, item) in menu.itemArray().iter().enumerate() {
        // Option-key alternates replace their primary row in AppKit menus;
        // drawn here they would duplicate it.
        if item.isHidden() || item.isAlternate() {
            continue;
        }
        if item.isSeparatorItem() {
            if items.is_empty() || matches!(items.last(), Some(MenuItem::Separator)) {
                continue;
            }
            items.push(MenuItem::separator());
            continue;
        }
        let with_icon = |row: MenuItem<usize>| match icon(&item) {
            Some(glyph) => row.icon(glyph),
            None => row,
        };
        let row = |actions: &mut Vec<NativeAction>| {
            let id = actions.len();
            actions.push(NativeAction {
                menu: menu.retain(),
                index: index as isize,
                _target: item.target(),
            });
            with_icon(MenuItem::action(id, item.title().to_string()))
        };
        if let Some(submenu) = item.submenu() {
            if !item.isEnabled() {
                items.push(row(actions).disabled());
                continue;
            }
            let children = convert(&submenu, actions, icon);
            if children.is_empty() {
                continue;
            }
            items.push(with_icon(MenuItem::submenu(
                item.title().to_string(),
                children,
            )));
        } else {
            items.push(
                row(actions)
                    .enabled(item.isEnabled())
                    .checked(item.state() != 0),
            );
        }
    }
    if matches!(items.last(), Some(MenuItem::Separator)) {
        items.pop();
    }
    items
}
