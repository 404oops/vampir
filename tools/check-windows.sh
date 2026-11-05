#!/bin/sh
# cargo check for Windows from macOS: zig stands in for the C compiler and the
# resource compiler that GPUI's build scripts want. Checks; never links.
here=$(cd "$(dirname "$0")/cross" && pwd)
PATH="$here:$PATH" \
CC_x86_64_pc_windows_gnu="$here/zigcc-win" \
AR_x86_64_pc_windows_gnu="$here/zigar" \
CARGO_TARGET_DIR="$here/../../target/xwin" \
exec cargo check --target x86_64-pc-windows-gnu "$@"
