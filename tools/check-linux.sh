#!/bin/sh
# cargo check for Linux from macOS: zig stands in for the C compiler. Checks;
# never links, so no system libraries are needed.
here=$(cd "$(dirname "$0")/cross" && pwd)
PATH="$here:$PATH" \
CC_aarch64_unknown_linux_gnu="$here/zigcc-linux" \
AR_aarch64_unknown_linux_gnu="$here/zigar" \
PKG_CONFIG_ALLOW_CROSS=1 \
CARGO_TARGET_DIR="$here/../../target/xlinux" \
exec cargo check --target aarch64-unknown-linux-gnu "$@"
