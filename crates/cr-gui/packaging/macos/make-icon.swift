// Draws the calibre-rust app icon as a 1024×1024 PNG.
//
//   swift make-icon.swift <output.png>
//
// The artwork follows the macOS icon grid: an 824 pt rounded square centred
// on a 1024 pt canvas, leaving room for the system drop shadow.

import AppKit
import CoreGraphics

let size: CGFloat = 1024
let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"

func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(
        srgbRed: CGFloat((hex >> 16) & 0xff) / 255,
        green: CGFloat((hex >> 8) & 0xff) / 255,
        blue: CGFloat(hex & 0xff) / 255,
        alpha: alpha)
}

func linearGradient(_ colors: [CGColor], _ locations: [CGFloat]) -> CGGradient {
    CGGradient(
        colorsSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
        colors: colors as CFArray,
        locations: locations)!
}

guard
    let ctx = CGContext(
        data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
else { fatalError("could not create a drawing context") }

// CoreGraphics has a bottom-left origin; flip so y grows downwards.
ctx.translateBy(x: 0, y: size)
ctx.scaleBy(x: 1, y: -1)

// MARK: Tile

let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
let tilePath = CGPath(roundedRect: tile, cornerWidth: 185, cornerHeight: 185, transform: nil)

ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 10), blur: 28, color: color(0x000000, 0.35))
ctx.addPath(tilePath)
ctx.setFillColor(color(0x1b2a4a))
ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(tilePath)
ctx.clip()
// Night-sky blue, lighter at the top.
ctx.drawLinearGradient(
    linearGradient([color(0x3a5a9b), color(0x1f3263), color(0x141f3d)], [0, 0.55, 1]),
    start: CGPoint(x: 0, y: tile.minY), end: CGPoint(x: 0, y: tile.maxY), options: [])
// Soft glow behind the books.
ctx.drawRadialGradient(
    linearGradient([color(0xffd68a, 0.32), color(0xffd68a, 0)], [0, 1]),
    startCenter: CGPoint(x: 512, y: 560), startRadius: 0,
    endCenter: CGPoint(x: 512, y: 560), endRadius: 420, options: [])

// MARK: Shelf

let shelfTop: CGFloat = 742
let shelf = CGRect(x: 196, y: shelfTop, width: 632, height: 34)
ctx.addPath(CGPath(roundedRect: shelf, cornerWidth: 10, cornerHeight: 10, transform: nil))
ctx.setFillColor(color(0x8a5a3c))
ctx.fillPath()
ctx.setFillColor(color(0xb47a52))
ctx.fill(CGRect(x: shelf.minX + 8, y: shelf.minY, width: shelf.width - 16, height: 8))
ctx.setFillColor(color(0x000000, 0.25))
ctx.fill(CGRect(x: shelf.minX + 20, y: shelf.maxY, width: shelf.width - 40, height: 10))

// MARK: Books

struct Book {
    var x: CGFloat
    var width: CGFloat
    var height: CGFloat
    var tilt: CGFloat  // degrees; positive leans right, negative leans left
    var body: UInt32
    var light: UInt32
    var band: UInt32
}

func drawBook(_ book: Book) {
    ctx.saveGState()
    // Rotate about the bottom corner that stays on the shelf: the right one
    // for books leaning right, the left one for books leaning left.
    let pivot = CGPoint(x: book.tilt > 0 ? book.x + book.width : book.x, y: shelfTop)
    ctx.translateBy(x: pivot.x, y: pivot.y)
    ctx.rotate(by: book.tilt * .pi / 180)
    ctx.translateBy(x: -pivot.x, y: -pivot.y)

    let rect = CGRect(x: book.x, y: shelfTop - book.height, width: book.width, height: book.height)
    let path = CGPath(roundedRect: rect, cornerWidth: 14, cornerHeight: 14, transform: nil)

    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 6, height: 8), blur: 14, color: color(0x000000, 0.35))
    ctx.addPath(path)
    ctx.setFillColor(color(book.body))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(path)
    ctx.clip()
    // Rounded spine: light on the left edge, darker on the right.
    ctx.drawLinearGradient(
        linearGradient([color(book.light), color(book.body), color(book.body, 1)], [0, 0.45, 1]),
        start: CGPoint(x: rect.minX, y: 0), end: CGPoint(x: rect.maxX, y: 0), options: [])
    ctx.setFillColor(color(0x000000, 0.18))
    ctx.fill(CGRect(x: rect.maxX - 12, y: rect.minY, width: 12, height: rect.height))
    // Decorative bands near the top and bottom of the spine.
    for y in [rect.minY + 52, rect.maxY - 78] {
        ctx.setFillColor(color(book.band))
        ctx.fill(CGRect(x: rect.minX, y: y, width: rect.width, height: 14))
        ctx.setFillColor(color(book.band, 0.6))
        ctx.fill(CGRect(x: rect.minX, y: y + 22, width: rect.width, height: 5))
    }
    // A label plate in the middle of the spine.
    let plate = CGRect(
        x: rect.minX + rect.width * 0.22, y: rect.minY + rect.height * 0.36,
        width: rect.width * 0.56, height: rect.height * 0.2)
    ctx.addPath(CGPath(roundedRect: plate, cornerWidth: 6, cornerHeight: 6, transform: nil))
    ctx.setFillColor(color(0xfff4dc, 0.9))
    ctx.fillPath()
    ctx.restoreGState()
    ctx.restoreGState()
}

let books = [
    Book(x: 236, width: 118, height: 430, tilt: 0, body: 0xc8553d, light: 0xe8826a, band: 0xf2c46d),
    Book(x: 362, width: 132, height: 492, tilt: 0, body: 0x2f8f83, light: 0x58b8a9, band: 0xf2c46d),
    Book(x: 502, width: 104, height: 404, tilt: 0, body: 0xe3a33b, light: 0xf6c76f, band: 0x7a3e1d),
    Book(x: 680, width: 124, height: 452, tilt: -10.4, body: 0x6a5acd, light: 0x9486e8, band: 0xf2c46d),
]
books.forEach(drawBook)

// MARK: Gloss

ctx.drawLinearGradient(
    linearGradient([color(0xffffff, 0.16), color(0xffffff, 0)], [0, 1]),
    start: CGPoint(x: 0, y: tile.minY), end: CGPoint(x: 0, y: tile.minY + 300), options: [])
ctx.restoreGState()

// Hairline edge for definition on light backgrounds.
ctx.addPath(
    CGPath(
        roundedRect: tile.insetBy(dx: 1, dy: 1), cornerWidth: 184, cornerHeight: 184,
        transform: nil))
ctx.setStrokeColor(color(0xffffff, 0.12))
ctx.setLineWidth(2)
ctx.strokePath()

// MARK: Write

guard let image = ctx.makeImage() else { fatalError("could not render the icon") }
let rep = NSBitmapImageRep(cgImage: image)
guard let png = rep.representation(using: .png, properties: [:]) else {
    fatalError("could not encode PNG")
}
do {
    try png.write(to: URL(fileURLWithPath: output))
} catch {
    fatalError("could not write \(output): \(error)")
}
print("wrote \(output)")
