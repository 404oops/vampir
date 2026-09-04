#!/usr/bin/env bash
#
# Installs the gallery, its desktop entry and its icon into a prefix.
#
#   packaging/linux/install.sh              # into ~/.local
#   packaging/linux/install.sh /usr/local   # system-wide, needs root
#
# X11 gets the window icon from the program itself, which sets it when the
# window opens. Wayland does not: it matches a window to a desktop entry by
# app id and takes the icon from there, so on Wayland this script is the only
# thing that gives the gallery a face.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
prefix="${1:-$HOME/.local}"

cargo build --release --example gallery

install -Dm755 "$root/target/release/examples/gallery" "$prefix/bin/vampir-gallery"
install -Dm644 "$root/packaging/linux/vampir-gallery.desktop" \
	"$prefix/share/applications/vampir-gallery.desktop"

# The name has to match `Icon=` in the desktop entry, at each size the theme
# looks in.
for size in 256 512; do
	install -Dm644 "$root/assets/icon-$size.png" \
		"$prefix/share/icons/hicolor/${size}x${size}/apps/vampir-gallery.png"
done

# GPUI is Apache-2.0, and a binary that ships it ships its notice.
install -Dm644 "$root/LICENSE" "$prefix/share/licenses/vampir-gallery/LICENSE"
install -Dm644 "$root/THIRD_PARTY.md" "$prefix/share/licenses/vampir-gallery/THIRD_PARTY.md"

echo "installed vampir-gallery into $prefix"
echo "if the icon does not appear, refresh the cache:"
echo "  gtk-update-icon-cache -f -t $prefix/share/icons/hicolor"
