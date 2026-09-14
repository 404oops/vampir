#!/usr/bin/env bash
#
# Drives the gallery from a script: build it, launch it, press keys, click,
# scroll, take screenshots, measure pixels. This is how the docs screenshots
# are made and how a change is checked on screen without a hand on the mouse.
#
# macOS only: the keyboard goes through AppleScript, the pointer through
# CoreGraphics and the pixels through screencapture. Pointer coordinates are
# relative to the gallery window, so a script does not care where the window
# opened.
#
#   tools/gallery.sh launch                  build the .app and open it
#   tools/gallery.sh key cmd-3               a keystroke (cmd-shift-s, escape, tab, enter...)
#   tools/gallery.sh type "sync"             text
#   tools/gallery.sh click X Y               left click; also: right, double
#   tools/gallery.sh drag X1 Y1 X2 Y2
#   tools/gallery.sh scroll X Y DX DY        precise scroll at a point
#   tools/gallery.sh park                    move the pointer off the window
#   tools/gallery.sh bounds                  the window: X Y W H on screen
#   tools/gallery.sh shot out.png [X Y W H]  the window, or a region of it
#   tools/gallery.sh shot --no-park out.png preserve keyboard focus rings
#   tools/gallery.sh ink out.png Y0 Y1 X0 X1 first and last inked column
#   tools/gallery.sh screenshots             regenerate docs/*.png
#   tools/gallery.sh quit
#
# Every input step brings the gallery to the front first and checks that it
# is there. Keystrokes go to whichever window is frontmost, and the one thing
# that reliably goes wrong when driving a window from a terminal is another
# window quietly taking that place.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app="$root/dist/Vampir gallery.app"
bin="$root/target/tools"
process="gallery" # the executable's name, which is how System Events knows it

die() { echo "gallery.sh: $*" >&2; exit 1; }

tools() {
	mkdir -p "$bin"
	for t in input ink; do
		if [ ! -x "$bin/$t" ] || [ "$root/tools/$t.swift" -nt "$bin/$t" ]; then
			swiftc -O -o "$bin/$t" "$root/tools/$t.swift" >/dev/null
		fi
	done
}

as() { osascript -e "$1" 2>/dev/null; }
frontmost() { as 'tell application "System Events" to get name of first application process whose frontmost is true'; }

# Brings the gallery to the front and refuses to go on if it is not there.
activate() {
	for _ in 1 2 3 4 5; do
		as 'tell application "Vampir gallery" to activate'
		sleep 0.3
		[ "$(frontmost)" = "$process" ] && return 0
	done
	die "could not bring the gallery to the front (frontmost is '$(frontmost)')"
}

launch() {
	"$root/packaging/macos/bundle.sh" --debug >/dev/null
	pkill -f "Vampir gallery.app" 2>/dev/null || true
	sleep 0.5
	# `open` does not pass the caller's environment on; the one variable
	# the gallery reads is handed over by name. VAMPIR_SLOW_MOTION=8 runs
	# every animation eight times slower, so a screenshot can catch one.
	if [ -n "${VAMPIR_SLOW_MOTION:-}" ]; then
		open --env "VAMPIR_SLOW_MOTION=$VAMPIR_SLOW_MOTION" "$app"
	else
		open "$app"
	fi
	sleep 5
	activate
}

quit() { pkill -f "Vampir gallery.app" 2>/dev/null || true; }

# "X Y W H" of the window on screen.
bounds() {
	as "tell application \"System Events\" to tell (first application process whose name is \"$process\") to get {position, size} of window 1" | tr -d ','
}

# Window-relative -> screen.
abs() {
	read -r wx wy _ _ <<<"$(bounds)"
	echo "$((wx + $1)) $((wy + $2))"
}

# Keystroke syntax matches GPUI's: "cmd-shift-s", "escape", "tab". `secondary`
# is accepted and means cmd here, as it does in the toolkit on macOS.
key() {
	activate
	# macOS ships bash 3.2: no negative array indices, and "${arr[@]}" on an
	# empty array trips `set -u`, hence the shape of this.
	local spec="$1" key
	local mods=()
	IFS='-' read -ra parts <<<"$spec"
	local last=$((${#parts[@]} - 1))
	key="${parts[$last]}"
	unset "parts[$last]"
	for m in ${parts[@]+"${parts[@]}"}; do
		case "$m" in
		cmd | secondary) mods+=("command down") ;;
		shift) mods+=("shift down") ;;
		alt | option) mods+=("option down") ;;
		ctrl | control) mods+=("control down") ;;
		*) die "unknown modifier '$m'" ;;
		esac
	done
	local using=""
	if [ "${#mods[@]}" -gt 0 ]; then
		using=" using {$(IFS=,; echo "${mods[*]}")}"
	fi
	local code=""
	case "$key" in
	escape) code=53 ;; tab) code=48 ;; enter | return) code=36 ;; space) code=49 ;;
	up) code=126 ;; down) code=125 ;; left) code=123 ;; right) code=124 ;;
	backspace | delete) code=51 ;; home) code=115 ;; end) code=119 ;;
	esac
	if [ -n "$code" ]; then
		as "tell application \"System Events\" to key code $code$using"
	else
		as "tell application \"System Events\" to keystroke \"$key\"$using"
	fi
}

type_text() {
	activate
	local text="${1//\\/\\\\}"
	text="${text//\"/\\\"}"
	as "tell application \"System Events\" to keystroke \"$text\""
}

pointer() {
	tools
	activate
	local verb="$1"
	shift
	case "$verb" in
	click | right | double)
		read -r x y <<<"$(abs "$1" "$2")"
		"$bin/input" "$verb" "$x" "$y"
		;;
	drag)
		read -r x1 y1 <<<"$(abs "$1" "$2")"
		read -r x2 y2 <<<"$(abs "$3" "$4")"
		"$bin/input" drag "$x1" "$y1" "$x2" "$y2" "${5:-25}"
		;;
	scroll)
		read -r x y <<<"$(abs "$1" "$2")"
		"$bin/input" scroll "$x" "$y" "$3" "$4"
		;;
	esac
}

# Parks the pointer below the window, so a screenshot carries no hover state.
park() {
	tools
	read -r wx wy _ wh <<<"$(bounds)"
	"$bin/input" move "$((wx + 20))" "$((wy + wh + 60))"
	sleep 0.4
}

# The window, or a window-relative region of it, at 1x.
shot() {
	activate
	if [ "${1:-}" = --no-park ]; then
		shift
	else
		park
	fi
	read -r wx wy ww wh <<<"$(bounds)"
	if [ "$#" -ge 5 ]; then
		screencapture -x -o -R "$((wx + $2)),$((wy + $3)),$4,$5" "$1"
	else
		screencapture -x -o -R "$wx,$wy,$ww,$wh" "$1"
	fi
	echo "$1"
}

ink() {
	tools
	"$bin/ink" "$@"
}

# The six screenshots the docs use, in the state each one shows. From a fresh
# launch, so nothing carries over from whatever was clicked before.
screenshots() {
	launch
	read -r _ _ ww _ <<<"$(bounds)"
	# The System | Light | Dark segments in the header, which is right-aligned:
	# 216 wide, ending 10 short of the 70-wide View button at the 20 margin.
	local light=$((ww - 208)) dark=$((ww - 136)) system=$((ww - 280))
	local header=67 # vertical centre of the header row
	# The gallery opens in whatever scheme the desktop is in. The docs show
	# dark first, so ask for it rather than trusting the machine.
	pointer click "$dark" "$header"; sleep 0.9
	shot "$root/docs/controls.png"
	key cmd-2; sleep 0.8
	shot "$root/docs/fields.png"
	key cmd-k; sleep 0.8
	shot "$root/docs/overlays.png"
	key escape; sleep 0.5
	key cmd-3; sleep 0.8
	shot "$root/docs/data.png"
	key cmd-4; sleep 0.8
	shot "$root/docs/colour.png"
	key cmd-1; sleep 0.8
	pointer click "$light" "$header"; sleep 0.9
	shot "$root/docs/light.png"
	# Back to following the desktop, so the next launch starts clean.
	pointer click "$system" "$header"; sleep 0.4
	quit
}

case "${1:-}" in
launch) launch ;;
quit) quit ;;
activate) activate ;;
bounds) bounds ;;
key) key "$2" ;;
type) type_text "$2" ;;
click | right | double | drag | scroll) pointer "$@" ;;
park) park ;;
shot) shift; shot "$@" ;;
ink) shift; ink "$@" ;;
screenshots) screenshots ;;
-h | --help | "") sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
*) die "unknown command '$1'" ;;
esac
