#!/bin/sh
# cargo check for Windows from macOS, or cargo clippy with `clippy` as the first
# argument: zig stands in for the C compiler and the resource compiler that
# GPUI's build scripts want. Never links.
command=check
if [ "$1" = clippy ]; then command=clippy; shift; fi
here=$(cd "$(dirname "$0")/cross" && pwd)
PATH="$here:$PATH" \
CC_x86_64_pc_windows_gnu="$here/zigcc-win" \
AR_x86_64_pc_windows_gnu="$here/zigar" \
CARGO_TARGET_DIR="$here/../../target/xwin" \
exec cargo "$command" --target x86_64-pc-windows-gnu "$@"
