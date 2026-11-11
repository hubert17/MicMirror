import AppKit

// Let's create an automated script that can generate assets/AppIcon.icns in any environment (including GitHub Actions CI)
let size = NSSize(width: 1024, height: 1024)
let image = NSImage(size: size)
image.lockFocus()

// 1. Draw modern macOS squircle background with rich gradient
let rect = NSRect(x: 40, y: 40, width: 944, height: 944)
let path = NSBezierPath(roundedRect: rect, xRadius: 210, yRadius: 210)

// Gradient: royal purple to electric violet
let topColor = NSColor(calibratedRed: 0.58, green: 0.20, blue: 0.95, alpha: 1.0)
let bottomColor = NSColor(calibratedRed: 0.32, green: 0.08, blue: 0.65, alpha: 1.0)
let gradient = NSGradient(starting: topColor, ending: bottomColor)!
gradient.draw(in: path, angle: -45.0)

// Subtle inner border glow
NSColor(calibratedWhite: 1.0, alpha: 0.20).setStroke()
path.lineWidth = 10.0
path.stroke()

// 2. Draw crisp white Headphones SF symbol in center
if let symbol = NSImage(systemSymbolName: "headphones", accessibilityDescription: nil) {
    let symbolRect = NSRect(x: 232, y: 232, width: 560, height: 560)
    let tinted = NSImage(size: symbolRect.size, flipped: false) { r in
        symbol.draw(in: r, from: .zero, operation: .sourceOver, fraction: 1.0)
        NSColor.white.setFill()
        r.fill(using: .sourceIn)
        return true
    }
    tinted.draw(in: symbolRect, from: .zero, operation: .sourceOver, fraction: 1.0)
}

image.unlockFocus()

let iconsetDir = "/tmp/AppIcon.iconset"
try? FileManager.default.createDirectory(atPath: iconsetDir, withIntermediateDirectories: true)

let sizes: [(Int, Int, String)] = [
    (16, 1, "icon_16x16.png"),
    (16, 2, "icon_16x16@2x.png"),
    (32, 1, "icon_32x32.png"),
    (32, 2, "icon_32x32@2x.png"),
    (128, 1, "icon_128x128.png"),
    (128, 2, "icon_128x128@2x.png"),
    (256, 1, "icon_256x256.png"),
    (256, 2, "icon_256x256@2x.png"),
    (512, 1, "icon_512x512.png"),
    (512, 2, "icon_512x512@2x.png")
]

for (dim, scale, filename) in sizes {
    let px = dim * scale
    let resized = NSImage(size: NSSize(width: px, height: px))
    resized.lockFocus()
    image.draw(in: NSRect(x: 0, y: 0, width: px, height: px), from: .zero, operation: .copy, fraction: 1.0)
    resized.unlockFocus()
    
    if let tiff = resized.tiffRepresentation,
       let rep = NSBitmapImageRep(data: tiff),
       let png = rep.representation(using: .png, properties: [:]) {
        let dest = URL(fileURLWithPath: "\(iconsetDir)/\(filename)")
        try? png.write(to: dest)
    }
}
