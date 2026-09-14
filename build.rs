//! Gives the example binaries an identity the desktop can see.
//!
//! Two different problems, one per platform, both only about running
//! straight out of `target/`. A packaged application needs none of this:
//! `packaging/macos/bundle.sh` builds a real `.app`, and a Windows or Linux
//! package carries its own icon and metadata.
//!
//! Only example targets are affected, so a crate that depends on Vampir
//! links exactly as it did before.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=packaging/macos/Info.plist");
    println!("cargo::rerun-if-changed=assets/icon.ico");
    println!("cargo::rerun-if-changed=build.rs");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => embed_info_plist(&root),
        Ok("windows") => embed_icon_resource(&root),
        _ => {}
    }
}

/// macOS: an unbundled Mach-O has no `Info.plist`, so AppKit falls back to
/// the file name for everything it needs a name for. The application menu is
/// the visible one: GPUI sets its title from `Menu::new(..)`, AppKit
/// overwrites it with the process name, and `cargo run --example gallery`
/// ends up with a menu bar that says "gallery".
///
/// Linking the plist into a `__TEXT,__info_plist` section is the standard
/// way to hand a bare executable that identity, and it is enough to fix the
/// name. It is *not* enough to make the thing an application: Launch
/// Services still reports no bundle identifier, so there is no Dock icon —
/// the gallery hands AppKit one at startup instead — and nothing the system
/// can remember about it. That needs a real bundle.
fn embed_info_plist(root: &Path) {
    // Tolerate the file being absent so a vendored or repackaged copy of the
    // crate still builds; it only costs the examples their name.
    let Ok(template) = std::fs::read_to_string(root.join("packaging/macos/Info.plist")) else {
        return;
    };
    let plist =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("Info.plist");
    std::fs::write(
        &plist,
        template.replace("@VAMPIR_VERSION@", env!("CARGO_PKG_VERSION")),
    )
    .expect("write the gallery's Info.plist");
    println!(
        "cargo::rustc-link-arg-examples=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}

/// Windows: GPUI asks the executable's own module for icon resource 1, so
/// the icon has to be linked into the binary rather than handed over at
/// runtime. That means a compiled resource, which means a resource compiler.
///
/// Whichever of the three is on the machine will do, and if none is, the
/// examples build exactly as before and keep the default icon — a missing
/// icon is not worth failing a build over.
fn embed_icon_resource(root: &Path) {
    let icon = root.join("assets/icon.ico");
    if !icon.exists() {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let script = out.join("icon.rc");
    let compiled = out.join("icon.res");

    // Resource 1, because that is the id GPUI's `LoadImageW` asks for.
    let source = format!(
        "1 ICON \"{}\"\n",
        icon.display().to_string().replace('\\', "\\\\")
    );
    if std::fs::write(&script, source).is_err() {
        return;
    }

    let attempts: [(&str, Vec<String>); 3] = [
        // MSVC and the LLVM toolchain take the same shape of arguments.
        (
            "rc.exe",
            vec![
                "/fo".into(),
                compiled.display().to_string(),
                script.display().to_string(),
            ],
        ),
        (
            "llvm-rc",
            vec![
                "/fo".into(),
                compiled.display().to_string(),
                script.display().to_string(),
            ],
        ),
        // The GNU toolchain's, which wants the output format spelled out.
        (
            "windres",
            vec![
                script.display().to_string(),
                "-O".into(),
                "coff".into(),
                "-o".into(),
                compiled.display().to_string(),
            ],
        ),
    ];

    for (tool, args) in attempts {
        let ran = Command::new(tool).args(&args).status();
        if matches!(ran, Ok(status) if status.success()) && compiled.exists() {
            println!("cargo::rustc-link-arg-examples={}", compiled.display());
            return;
        }
    }
    println!(
        "cargo::warning=no resource compiler found; the gallery will use the default Windows icon"
    );
}
