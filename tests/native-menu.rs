//! An AppKit menu adopted as the toolkit's rows, with its actions still
//! runnable. AppKit objects must be used on the main thread, so this runs
//! without the Rust test harness.

#[cfg(all(target_os = "macos", feature = "native"))]
mod native {
    use std::cell::Cell;
    use std::rc::Rc;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSUserInterfaceItemIdentification};
    use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
    use vampir::{MenuItem, NativeMenu};

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = Rc<Cell<usize>>]
        struct Target;
        unsafe impl NSObjectProtocol for Target {}
        impl Target {
            #[unsafe(method(selected:))]
            fn selected(&self, _sender: &AnyObject) {
                self.ivars().set(self.ivars().get() + 1);
            }
        }
    );

    fn row(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                None,
                &NSString::from_str(""),
            )
        }
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("AppKit regressions run on the main thread");
        NSApplication::sharedApplication(mtm);

        // Leading, trailing and repeated separators, hidden rows, Option
        // alternates and empty submenus all go; the rest keep their order.
        let menu = NSMenu::new(mtm);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&row(mtm, "Reload"));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let hidden = row(mtm, "Hidden service");
        hidden.setHidden(true);
        menu.addItem(&hidden);
        let alternate = row(mtm, "Reload Without Content Blockers");
        alternate.setAlternate(true);
        menu.addItem(&alternate);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let empty = row(mtm, "Empty submenu");
        empty.setSubmenu(Some(&NSMenu::new(mtm)));
        menu.addItem(&empty);
        let disabled = row(mtm, "Unavailable");
        disabled.setEnabled(false);
        menu.addItem(&disabled);
        let submenu = NSMenu::new(mtm);
        submenu.addItem(&NSMenuItem::separatorItem(mtm));
        let checked = row(mtm, "Checked option");
        checked.setState(1);
        checked.setIdentifier(Some(&NSString::from_str("OptionIdentifier")));
        submenu.addItem(&checked);
        submenu.addItem(&NSMenuItem::separatorItem(mtm));
        let parent = row(mtm, "Options");
        parent.setSubmenu(Some(&submenu));
        menu.addItem(&parent);
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let mut adopted = NativeMenu::adopt(&menu, |_| None);
        let items = adopted.items();
        assert_eq!(items.len(), 4, "{items:?}");
        let MenuItem::Action(reload) = &items[0] else {
            panic!("expected Reload, got {:?}", items[0]);
        };
        assert_eq!(
            (reload.label.as_ref(), reload.id, reload.enabled),
            ("Reload", 0, true)
        );
        assert!(matches!(items[1], MenuItem::Separator));
        let MenuItem::Action(unavailable) = &items[2] else {
            panic!("expected a disabled row, got {:?}", items[2]);
        };
        assert!(!unavailable.enabled);
        let MenuItem::Submenu {
            items: options,
            enabled,
            ..
        } = &items[3]
        else {
            panic!("expected a submenu, got {:?}", items[3]);
        };
        assert!(enabled);
        assert_eq!(options.len(), 1, "{options:?}");
        let MenuItem::Action(option) = &options[0] else {
            panic!("expected the checked option, got {:?}", options[0]);
        };
        assert!(option.checked);
        assert_eq!(
            adopted.actions().len(),
            3,
            "a branch has no action of its own"
        );
        assert_eq!(
            adopted.find(|action| action.identifier().as_deref() == Some("OptionIdentifier")),
            Some(option.id),
            "an action is found by its item's identifier"
        );
        let id = option.id;
        adopted.row_mut(id).expect("nested row").label = "Relabelled".into();
        let MenuItem::Submenu { items: options, .. } = &adopted.items()[3] else {
            unreachable!()
        };
        assert_eq!(options[0].label(), Some("Relabelled"));
        assert!(!adopted.run(9), "no such action");

        // Running an action goes through the item's target, which the
        // adopted menu keeps alive.
        let count = Rc::new(Cell::new(0));
        let target = Target::alloc(mtm).set_ivars(count.clone());
        let target: Retained<Target> = unsafe { msg_send![super(target), init] };
        let action_menu = NSMenu::new(mtm);
        action_menu.setAutoenablesItems(false);
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Copy Image"),
                Some(sel!(selected:)),
                &NSString::from_str(""),
            )
        };
        unsafe { item.setTarget(Some(&target)) };
        action_menu.addItem(&item);
        let adopted = NativeMenu::adopt(&action_menu, |_| None);
        drop(target);
        assert!(adopted.run(0));
        assert_eq!(count.get(), 1, "the native action ran once");
        assert!(NativeMenu::adopt(&NSMenu::new(mtm), |_| None).is_empty());
    }
}

fn main() {
    #[cfg(all(target_os = "macos", feature = "native"))]
    native::run();
    println!("Native menu regressions passed.");
}
