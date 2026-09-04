# Shipping a macOS app

These details belong to GPUI and AppKit rather than Vampir, but most hosts will encounter them. The gallery demonstrates the complete setup: a menu bar, keyboard shortcuts and a proper application bundle.

There are three separate pieces to configure, each with its own failure mode.

## 1. Add a menu bar

An app that never calls `set_menus` has **no main menu at all**. macOS does not draw an empty menu bar in that case — it leaves whatever the previous front application put there. In practice that is Finder's menu bar, sitting above your window while your window has focus, which is why the app reads as not really being the front app.

It is also the only route to ⌘Q, ⌘H and the Window menu. None of those is provided by the system.

```rust
cx.set_menus(vec![
    Menu::new("Vampir gallery").items([
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide Vampir gallery", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit Vampir gallery", Quit),
    ]),
    // ...
]);
```

Two details worth knowing:

- A menu named exactly **`"Window"`** is handed to AppKit as the windows menu, which is what fills it with the window list and adds Enter Full Screen. Name it anything else and you get an inert menu.
- Clipboard items should carry an `OsAction`, so macOS routes them through the responder chain to whatever is focused:

  ```rust
  MenuItem::os_action("Copy", text_input::Copy, OsAction::Copy)
  ```

That is what makes one Edit menu work for Vampir's `TextInput` and for the system's own fields alike.

## 2. Bind the shortcuts

A menu item shows a key equivalent, but it does not implement one. Every shortcut needs a `KeyBinding` as well:

```rust
cx.bind_keys([
    KeyBinding::new("cmd-q", Quit, None),
    KeyBinding::new("cmd-k", TogglePalette, None),
    KeyBinding::new("cmd-w", CloseWindow, None),
]);
```

And a menu item is only **enabled** when its action is actually reachable from the current focus. Actions with a view behind them go on the root element with `.on_action(cx.listener(..))`. Actions with no view — quitting, hiding — need a global handler, or the item greys out whenever the window loses focus:

```rust
cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
```

One more thing a single-window app has to say out loud: closing the window is not quitting.

```rust
cx.on_window_closed(|cx, _id| {
    if cx.windows().is_empty() {
        cx.quit();
    }
})
.detach();
```

Without it the process survives its last window, with no window and no way back to one.

## 3. Build an application bundle

A bare Mach-O executable has no `Info.plist`, and Launch Services will tell you so:

```console
$ lsappinfo info -only name  <pid>
"LSDisplayName"="gallery"
$ lsappinfo info -only bundleid <pid>
"CFBundleIdentifier"=[ NULL ]
```

No identifier means there is nothing macOS can hang on the app: no Dock position it remembers, no entry in the privacy settings, no way to tell one build from another. The name everywhere is the file name.

That includes the application menu. GPUI sets its title from `Menu::new("Vampir gallery")`, and AppKit overwrites it with the process name — so an unbundled build shows a menu bar reading **gallery**, however the menu was named.

There are two levels of fix, and this repository does both.

### The cheap one: an embedded plist

Linking the plist into a `__TEXT,__info_plist` section gives a bare executable an info dictionary, which is enough to fix the *name*. That is what [`build.rs`](../build.rs) does, so `cargo run --example gallery` gets a menu bar reading "Vampir gallery" without any packaging step. It applies to example targets only, so it changes nothing for a crate that depends on Vampir.

It is not enough to make the thing an application. Launch Services still reports no bundle identifier, and there is still no icon.

### The real one: a bundle

[`packaging/macos/bundle.sh`](../packaging/macos/bundle.sh) builds a proper `.app` from the same plist:

```bash
packaging/macos/bundle.sh            # release build, into dist/
packaging/macos/bundle.sh --debug    # skip the slow optimised build
packaging/macos/bundle.sh --open     # build, then launch it
```

It lays out `Contents/{MacOS,Resources}`, copies in the executable, the plist and `assets/icon.icns`, and ad-hoc signs the result. The signature is not distributable, but it gives the bundle a stable identity, which is what lets macOS remember permissions granted to it instead of asking again after every rebuild.

The result is an actual application:

```console
$ lsappinfo info -only name <pid>
"LSDisplayName"="Vampir gallery"
$ lsappinfo info -only bundleid <pid>
"CFBundleIdentifier"="rs.vampir.gallery"
```

### Keys that matter in the plist

`CFBundleExecutable` must match the file name in `Contents/MacOS`. `CFBundleIdentifier` is the identity everything else hangs on. `CFBundleName` is the application menu's title, and macOS truncates it, so keep it under about sixteen characters. `NSPrincipalClass` is `NSApplication`, which is what GPUI drives. `NSHighResolutionCapable` is what stops the window being drawn at 1x and scaled up.

## The same problem on Windows and Linux

The icon is the part that does not travel: each platform wants it somewhere different, and none of those places is where a host would think to look. So the toolkit carries one — [`vampir::icon::AppIcon`](../src/icon.rs) — and an application swaps it for its own in a line:

```rust
const ICON: AppIcon = AppIcon::png(include_bytes!("../assets/my-icon.png"));

ICON.install(cx);                 // at startup
options.icon = ICON.window_icon(); // in the window options
```

Artwork is one square PNG drawn edge to edge; the platform differences are handled for you. It rides on the `app-icon` feature, which is on by default — `default-features = false` gets back to a crate with two dependencies and no icon handling.

**Windows** reads an icon *resource* out of the executable: GPUI asks the running module for icon resource 1. So it has to be linked in at build time rather than handed over at runtime, which needs a resource compiler. `build.rs` writes a one-line `.rc` naming `assets/icon.ico` and tries `rc.exe`, `llvm-rc` and `windres` in turn. If none of them is on the machine it prints a warning and the build carries on with the default icon, because a missing icon is not worth failing a build over.

**macOS** takes it from AppKit at startup, and `AppIcon::install` insets the artwork to Apple's grid on the way: a square macOS icon fills 824 pixels of a 1024 canvas, and artwork drawn edge to edge sits about a fifth larger than every icon beside it in the dock. That is what makes a new app look wrong before anyone can say why, and it is why the inset happens here rather than being left as a second file for the host to remember.

**X11** takes the icon as decoded pixels on the window itself, through `WindowOptions::icon`, which is what `AppIcon::window_icon` returns.

**Wayland** takes neither, in the GPUI release the toolkit pins. The protocol for a window to hand its compositor an icon is young, gpui-ce has it on its main branch and not yet in a release, and most compositors still do it the old way: match the window's app id to a `.desktop` file and read the icon named there. So on Wayland the icon is a packaging question, and `packaging/linux/install.sh` is the answer — it installs the binary, the desktop entry and the icon into the hicolor theme under the name the entry points at. Get that name wrong and the window has no icon however many pixels the program is holding.

## Checking your own app

The whole chain is inspectable without opening anything by hand:

```bash
# What does macOS think this process is?
lsappinfo info -only name -only bundleid "$(pgrep -n YourApp)"

# Whose menu bar is on screen, and what is in it?
osascript -e 'tell application "System Events" to tell (first application \
  process whose frontmost is true) to get name of every menu bar item of menu bar 1'

# Is a menu item actually enabled?
osascript -e 'tell application "System Events" to tell (first application \
  process whose frontmost is true) to tell menu bar item "File" of menu bar 1 \
  to get {name, enabled} of every menu item of menu 1'
```

If the second command lists Finder's menus while your window has focus, you have not called `set_menus`. If it lists yours but under the wrong name, you have no plist. If an item is there but disabled, its action is not reachable from the current focus.
