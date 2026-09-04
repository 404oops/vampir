// Where the ink is in a screenshot: the first and last column, within
// [x0, x1), of the rows [y0, y1) that differ from the region's background.
//
// For checking alignment by measurement rather than by eye — whether a
// header's right edge and its column's right edge land on the same pixel.
// The background is the colour at (x0, y0). Compiled on demand by
// tools/gallery.sh; not part of the crate.
//
//   ink file.png Y0 Y1 X0 X1     prints "first last", or "-1 -1" for none

import CoreGraphics
import Foundation
import ImageIO

let a = CommandLine.arguments
guard a.count == 6,
      let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: a[1]) as CFURL, nil),
      let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else {
    FileHandle.standardError.write("usage: ink file.png Y0 Y1 X0 X1\n".data(using: .utf8)!)
    exit(2)
}
let w = img.width, h = img.height
var buf = [UInt8](repeating: 0, count: w * h * 4)
buf.withUnsafeMutableBytes { raw in
    let ctx = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8,
                        bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
}
let y0 = Int(a[2])!, y1 = min(Int(a[3])!, h), x0 = Int(a[4])!, x1 = min(Int(a[5])!, w)
func px(_ x: Int, _ y: Int) -> (Int, Int, Int) {
    let i = (y * w + x) * 4
    return (Int(buf[i]), Int(buf[i + 1]), Int(buf[i + 2]))
}
let bg = px(x0, y0)
func differs(_ c: (Int, Int, Int)) -> Bool { abs(c.0 - bg.0) + abs(c.1 - bg.1) + abs(c.2 - bg.2) > 60 }
var first = -1, last = -1
for x in x0..<x1 {
    var inked = false
    for y in y0..<y1 where differs(px(x, y)) { inked = true; break }
    if inked { if first < 0 { first = x }; last = x }
}
print("\(first) \(last)")
