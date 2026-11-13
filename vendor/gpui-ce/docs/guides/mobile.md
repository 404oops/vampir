# Build for iOS and Android

GPUI runs the same views on iOS and Android as on the desktop. `gpui_platform`
selects `gpui_ios` or `gpui_mobile` for the target you compile, and the app's
views, state and windows use the usual [`App`] and [`Window`] APIs. What differs
is the native host that starts the app, and the build that packages it.

The example in `examples/mobile` shares one view between both targets. Its
`open_main_window` opens that view; each target only calls it from its own entry
point.

## Build and run the Android app

Install Java 17, the Android SDK with platform and build tools 36, an Android
NDK, [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk) and the Rust targets
`aarch64-linux-android` and `x86_64-linux-android`. Then build the native
library and the APK, and install it:

```sh
ANDROID_NDK_HOME=/path/to/ndk examples/mobile/android/build.sh
adb install examples/mobile/android/app/build/outputs/apk/debug/app-debug.apk
```

The app is a `NativeActivity` that targets Android 8 (API 26) and newer.
Android calls the library's `android_main`, which hands the app to
`run_native_activity`. That creates the platform and runs GPUI on the
Activity's thread until Android destroys the Activity:

```rust,ignore
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {
    gpui_mobile::android::jni::run_native_activity(app, |cx| {
        // Open windows here, as in `gpui_platform::application().run(...)`.
    });
}
```

An app that owns its own Java Activity can drive GPUI through
`gpui_mobile::android::host` instead, forwarding surface, lifecycle, touch and
text-input callbacks in the order that module documents. Use one entry point or
the other, not both.

## Build and run the iOS app

On macOS, install Xcode and the Rust target `aarch64-apple-ios-sim` for the
simulator, or `aarch64-apple-ios` for a device. Then build the app bundle and
launch it in a booted simulator:

```sh
examples/mobile/ios/build-rust.sh Debug iphonesimulator
xcrun simctl install booted target/aarch64-apple-ios-sim/debug/GpuiMobile.app
xcrun simctl launch booted dev.gpui.mobile.example
```

The app targets iOS 15 and newer. Its `main` calls
`gpui_platform::application().run(...)`, as a desktop app does, and its
`Info.plist` registers GPUI's scene delegate. GPUI supports one connected
`UIWindowScene` at a time. The simulator build is ad-hoc signed; sign a device
build with your own identity before installing it.

## Keep content clear of system UI

[`Window::insets`] returns the edges covered by system UI, such as a notch or
navigation bar, in `safe_area`, and the edges covered by the keyboard in `ime`.
[`WindowInsets::effective`] takes the larger of the two on each side, which is
what content padding usually needs:

```rust
use gpui::{Pixels, Window, px};

fn bottom_padding(window: &Window) -> Pixels {
    window.insets().effective().bottom + px(12.)
}
```

The values are in logical pixels, like the rest of GPUI, so don't scale them by
the display density again. Text fields receive composition, selection and
autocorrection through the platform's text-input protocol, as on the desktop;
[accept text input](crate::_documentation::guides::text_input) explains the
element side.

## Show native Android views

`gpui_mobile` can place native Android views, such as a web view, in an overlay
above the GPUI surface. They always draw above GPUI content. Register the view
type on both sides under the same name, Rust first:

```rust,ignore
use gpui_mobile::{android::platform_view::AndroidPlatformViewFactory, platform_view::PlatformViewRegistry};

PlatformViewRegistry::global().register("web_view", Box::new(AndroidPlatformViewFactory::new("web_view")));
```

The Java bridge in the example provides `container` and `web_view`. Register
other types with `GpuiPlatformView.registerViewFactory`, whose factory runs on
Android's UI thread and must return quickly. An Activity that uses the bridge
calls `GpuiPlatformView.bindActivity(this)` after `super.onCreate` and
`GpuiPlatformView.unbindActivity(this)` before `super.onDestroy`.

## Fonts on Android

Android 13 and newer ship an emoji font with COLRv1 color glyphs, which GPUI
can't rasterize. The example bundles Noto Color Emoji with bitmap glyphs in its
assets and stores it uncompressed in the APK, so GPUI maps it in place instead
of copying it at startup. Add other fonts your app relies on with
[`TextSystem::add_fonts`].
