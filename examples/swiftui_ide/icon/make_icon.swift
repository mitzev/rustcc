// Renders the rustcc SwiftUI IDE app icon to a 1024×1024 PNG.
//   swift icon/make_icon.swift out.png
//
// Design: the macOS rounded-rect tile (824 in a 1024 grid, ~184 radius)
// with a Swift-orange → deep-red gradient and a cream `</>` code glyph —
// "an IDE" at a glance, the orange nodding to the SwiftUI front-end.
// Drawn with CoreGraphics/AppKit so there are no binary art assets in
// the tree; the build regenerates the .icns from this.

import AppKit

let outPath = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon_1024.png"
let px = 1024

guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
else { fatalError("bitmap rep") }

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext
ctx.clear(CGRect(x: 0, y: 0, width: px, height: px))   // transparent corners

// Rounded-rect tile on the macOS icon grid.
let inset: CGFloat = 100
let tile = NSRect(x: inset, y: inset, width: CGFloat(px) - 2 * inset, height: CGFloat(px) - 2 * inset)
let tilePath = NSBezierPath(roundedRect: tile, xRadius: 184, yRadius: 184)

NSGraphicsContext.saveGraphicsState()
tilePath.addClip()
let top = NSColor(srgbRed: 0.99, green: 0.46, blue: 0.18, alpha: 1)   // #FD7630
let bot = NSColor(srgbRed: 0.80, green: 0.20, blue: 0.11, alpha: 1)   // #CC331C
NSGradient(starting: top, ending: bot)!.draw(in: tile, angle: -65)
// subtle top sheen
NSColor(white: 1, alpha: 0.10).setFill()
NSBezierPath(roundedRect: NSRect(x: inset, y: 512, width: tile.width, height: 412),
             xRadius: 184, yRadius: 184).fill()
NSGraphicsContext.restoreGraphicsState()

// `</>` glyph — thick cream round strokes, centered on (512,512).
// (NSBezierPath is bottom-up.)
let cream = NSColor(srgbRed: 1.0, green: 0.97, blue: 0.90, alpha: 1)   // #FFF7E6
cream.setStroke()
func stroke(_ pts: [(CGFloat, CGFloat)], width: CGFloat) {
    let p = NSBezierPath()
    p.lineWidth = width
    p.lineCapStyle = .round
    p.lineJoinStyle = .round
    p.move(to: NSPoint(x: pts[0].0, y: pts[0].1))
    for q in pts.dropFirst() { p.line(to: NSPoint(x: q.0, y: q.1)) }
    p.stroke()
}
let w: CGFloat = 66
stroke([(452, 700), (300, 512), (452, 324)], width: w)   // <
stroke([(572, 700), (724, 512), (572, 324)], width: w)   // >
stroke([(486, 312), (538, 712)], width: w)               // /

NSGraphicsContext.restoreGraphicsState()

guard let png = rep.representation(using: .png, properties: [:]) else { fatalError("png") }
try! png.write(to: URL(fileURLWithPath: outPath))
print("wrote \(outPath)")
