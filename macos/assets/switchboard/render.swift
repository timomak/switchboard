// Convert the SVG's traced contours to vector PDF for AppKit template rendering.
import Cocoa
let root = URL(fileURLWithPath: CommandLine.arguments[1]).appendingPathComponent("macos/assets/switchboard")
let paths = try JSONDecoder().decode([[[Double]]].self, from: Data(contentsOf: root.appendingPathComponent("outlines.json")))
final class Mark: NSView {
    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.black.setFill()
        let shape = NSBezierPath(); shape.windingRule = .evenOdd
        for points in paths {
            shape.move(to: NSPoint(x: points[0][0], y: points[0][1]))
            for p in points.dropFirst() { shape.line(to: NSPoint(x: p[0], y: p[1])) }
            shape.close()
        }
        shape.fill()
    }
}
let view = Mark(frame: NSRect(x: 0, y: 0, width: 128, height: 128))
try view.dataWithPDF(inside: view.bounds).write(to: root.appendingPathComponent("Switchboard-menubar.pdf"))
let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
view.cacheDisplay(in: view.bounds, to: rep)
try rep.representation(using: .png, properties: [:])!.write(to: root.appendingPathComponent("Switchboard-menubar.png"))
