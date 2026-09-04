#!/usr/bin/env bash
#
# Wraps the gallery example in a macOS .app bundle.
#
#   packaging/macos/bundle.sh            # release build, into dist/
#   packaging/macos/bundle.sh --debug    # skip the slow optimised build
#   packaging/macos/bundle.sh --open     # build, then launch it
#
# Running the bare binary out of target/ works, but macOS has no identity to
# attach to it: no bundle identifier, so nothing it can remember about the
# app, and the name everywhere is the file name, "gallery". A bundle is what
# turns it into an application with a name, an icon and a place in the Dock.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

profile=release
profile_dir=release
open_after=false

for arg in "$@"; do
	case "$arg" in
	--debug)
		profile=dev
		profile_dir=debug
		;;
	--open) open_after=true ;;
	-h | --help)
		sed -n '3,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "unknown option: $arg" >&2
		exit 2
		;;
	esac
done

app="dist/Vampir gallery.app"
contents="$app/Contents"

cargo build --profile "$profile" --example gallery

rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources"

# CFBundleExecutable in the plist names this file; keep the two in step.
cp "target/$profile_dir/examples/gallery" "$contents/MacOS/gallery"
cp packaging/macos/Info.plist "$contents/Info.plist"
cp assets/icon.icns "$contents/Resources/icon.icns"
# GPUI is Apache-2.0, and a binary that ships it ships its notice.
cp LICENSE "$contents/Resources/LICENSE"
cp THIRD_PARTY.md "$contents/Resources/THIRD_PARTY.md"

# An ad-hoc signature is enough to give the bundle a stable identity, which
# is what lets macOS remember permissions granted to it instead of asking
# again on every rebuild. It is not a distributable signature.
codesign --force --sign - "$app" >/dev/null 2>&1 ||
	echo "note: could not sign the bundle; it will still run" >&2

echo "built $app"

if [ "$open_after" = true ]; then
	open "$app"
fi
