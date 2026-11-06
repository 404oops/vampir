#!/bin/sh
# cargo check for Linux from macOS, or cargo clippy with `clippy` as the first
# argument: zig stands in for the C compiler. Never links.
command=check
if [ "$1" = clippy ]; then command=clippy; shift; fi
here=$(cd "$(dirname "$0")/cross" && pwd)
PATH="$here:$PATH" \
CC_aarch64_unknown_linux_gnu="$here/zigcc-linux" \
AR_aarch64_unknown_linux_gnu="$here/zigar" \
PKG_CONFIG_ALLOW_CROSS=1 \
CARGO_TARGET_DIR="$here/../../target/xlinux" \
exec cargo "$command" --target aarch64-unknown-linux-gnu "$@"
