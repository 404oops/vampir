// Synthetic pointer input, for driving the gallery from a script.
//
// AppleScript can press keys but cannot right-click, double-click or scroll,
// so those go through CoreGraphics directly. Coordinates are screen points.
// Compiled on demand by tools/gallery.sh; not part of the crate.
//
//   input click X Y          left click
//   input right X Y          right click
//   input double X Y         double click — clickState, not timing, is what
//                            macOS counts, so two fast clicks are not one
//   input drag X1 Y1 X2 Y2 [steps]
//   input move X Y
//   input scroll X Y DX DY   a precise (trackpad-style) scroll at a point

import CoreGraphics
import Foundation

func post(_ type: CGEventType, _ p: CGPoint, _ button: CGMouseButton, clicks: Int64 = 1) {
    guard let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: p, mouseButton: button) else { return }
    e.setIntegerValueField(.mouseEventClickState, value: clicks)
    e.post(tap: .cghidEventTap)
}
func nap(_ ms: UInt32) { usleep(ms * 1000) }
func pt(_ x: String, _ y: String) -> CGPoint { CGPoint(x: Double(x)!, y: Double(y)!) }

let a = CommandLine.arguments
switch a.count > 1 ? a[1] : "" {
case "click", "right":
    let p = pt(a[2], a[3])
    let right = a[1] == "right"
    post(.mouseMoved, p, .left); nap(60)
    post(right ? .rightMouseDown : .leftMouseDown, p, right ? .right : .left); nap(60)
    post(right ? .rightMouseUp : .leftMouseUp, p, right ? .right : .left); nap(60)
case "double":
    let p = pt(a[2], a[3])
    post(.mouseMoved, p, .left); nap(60)
    for state in [1, 2] as [Int64] {
        post(.leftMouseDown, p, .left, clicks: state); nap(40)
        post(.leftMouseUp, p, .left, clicks: state); nap(60)
    }
case "drag":
    let from = pt(a[2], a[3]), to = pt(a[4], a[5])
    let steps = a.count > 6 ? Int(a[6])! : 25
    post(.mouseMoved, from, .left); nap(80)
    post(.leftMouseDown, from, .left); nap(80)
    for i in 1...steps {
        let t = Double(i) / Double(steps)
        post(.leftMouseDragged, CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t), .left)
        nap(16)
    }
    nap(80)
    post(.leftMouseUp, to, .left); nap(80)
case "move":
    post(.mouseMoved, pt(a[2], a[3]), .left)
case "scroll":
    let p = pt(a[2], a[3])
    let dx = Int32(a[4])!, dy = Int32(a[5])!
    post(.mouseMoved, p, .left); nap(80)
    for _ in 0..<10 {
        if let e = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0) {
            e.location = p
            e.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
            e.post(tap: .cghidEventTap)
        }
        nap(16)
    }
default:
    FileHandle.standardError.write("usage: input click|right|double X Y | drag X1 Y1 X2 Y2 [steps] | move X Y | scroll X Y DX DY\n".data(using: .utf8)!)
    exit(2)
}
