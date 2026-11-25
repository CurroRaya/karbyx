// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx
//
// Dibuja el icono de Karbyx (un lingote al rojo con el prompt grabado) en PNG.
// Uso: swift scripts/icon.swift salida.png [tamaño]

import AppKit

let args = CommandLine.arguments
let out = args.count > 1 ? args[1] : "karbyx.png"
let size = CGFloat(args.count > 2 ? Double(args[2])! : 1024)
let s = size / 1024

func rgb(_ r: Int, _ g: Int, _ b: Int, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat(r) / 255, green: CGFloat(g) / 255, blue: CGFloat(b) / 255, alpha: a)
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
                    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!

// Rejilla de iconos de macOS: cuerpo de 824 px centrado, esquinas de 185 px.
let body = CGRect(x: 100 * s, y: 100 * s, width: 824 * s, height: 824 * s)
let tile = CGPath(roundedRect: body, cornerWidth: 185 * s, cornerHeight: 185 * s, transform: nil)

// Sombra bajo el icono.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12 * s), blur: 28 * s, color: rgb(0, 0, 0, 0.45))
ctx.addPath(tile)
ctx.setFillColor(rgb(0x15, 0x13, 0x11))
ctx.fillPath()
ctx.restoreGState()

// Fondo: carbón cálido, más claro arriba.
ctx.saveGState()
ctx.addPath(tile)
ctx.clip()
let bg = CGGradient(colorsSpace: space, colors: [rgb(0x2a, 0x22, 0x1c), rgb(0x12, 0x0f, 0x0d)] as CFArray,
                    locations: [0, 1])!
ctx.drawLinearGradient(bg, start: CGPoint(x: 0, y: body.maxY), end: CGPoint(x: 0, y: body.minY), options: [])
// Calor que sube desde abajo.
let heat = CGGradient(colorsSpace: space, colors: [rgb(0xf0, 0x8a, 0x4b, 0.38), rgb(0xf0, 0x8a, 0x4b, 0)] as CFArray,
                      locations: [0, 1])!
ctx.drawRadialGradient(heat, startCenter: CGPoint(x: 512 * s, y: 250 * s), startRadius: 0,
                       endCenter: CGPoint(x: 512 * s, y: 250 * s), endRadius: 560 * s, options: [])
ctx.restoreGState()

// El lingote: un trapecio al rojo, más ancho abajo.
func ingotPath(_ inset: CGFloat) -> CGPath {
    let p = CGMutablePath()
    let bottom: CGFloat = 300 * s, top: CGFloat = 560 * s
    let bl = 222 * s + inset, br = 802 * s - inset
    let tl = 312 * s + inset, tr = 712 * s - inset
    let r: CGFloat = 46 * s
    p.move(to: CGPoint(x: (bl + br) / 2, y: bottom + inset))
    p.addArc(tangent1End: CGPoint(x: br, y: bottom + inset), tangent2End: CGPoint(x: tr, y: top - inset), radius: r)
    p.addArc(tangent1End: CGPoint(x: tr, y: top - inset), tangent2End: CGPoint(x: tl, y: top - inset), radius: r)
    p.addArc(tangent1End: CGPoint(x: tl, y: top - inset), tangent2End: CGPoint(x: bl, y: bottom + inset), radius: r)
    p.addArc(tangent1End: CGPoint(x: bl, y: bottom + inset), tangent2End: CGPoint(x: br, y: bottom + inset), radius: r)
    p.closeSubpath()
    return p
}
let ingot = ingotPath(0)
ctx.saveGState()
ctx.setShadow(offset: .zero, blur: 140 * s, color: rgb(0xf0, 0x8a, 0x4b, 0.9))
ctx.addPath(ingot)
ctx.setFillColor(rgb(0xf0, 0x8a, 0x4b))
ctx.fillPath()
ctx.restoreGState()
ctx.saveGState()
ctx.addPath(ingot)
ctx.clip()
let metal = CGGradient(colorsSpace: space,
                       colors: [rgb(0xff, 0xe2, 0x8a), rgb(0xff, 0xa8, 0x50), rgb(0xc9, 0x52, 0x24)] as CFArray,
                       locations: [0, 0.5, 1])!
ctx.drawLinearGradient(metal, start: CGPoint(x: 0, y: 560 * s), end: CGPoint(x: 0, y: 300 * s), options: [])
let shine = CGGradient(colorsSpace: space, colors: [rgb(255, 255, 255, 0.45), rgb(255, 255, 255, 0)] as CFArray,
                       locations: [0, 1])!
ctx.drawLinearGradient(shine, start: CGPoint(x: 0, y: 560 * s), end: CGPoint(x: 0, y: 500 * s), options: [])
ctx.restoreGState()

// Grabado en el lingote: el prompt «❯_».
let ink = rgb(0x3a, 0x1a, 0x0a, 0.85)
ctx.setStrokeColor(ink)
ctx.setLineWidth(34 * s)
ctx.setLineCap(.round)
ctx.setLineJoin(.round)
ctx.move(to: CGPoint(x: 408 * s, y: 486 * s))
ctx.addLine(to: CGPoint(x: 478 * s, y: 430 * s))
ctx.addLine(to: CGPoint(x: 408 * s, y: 374 * s))
ctx.strokePath()
ctx.move(to: CGPoint(x: 530 * s, y: 374 * s))
ctx.addLine(to: CGPoint(x: 616 * s, y: 374 * s))
ctx.strokePath()

// Chispas que saltan del lingote.
let sparks: [(CGFloat, CGFloat, CGFloat)] = [(560, 690, 20), (642, 640, 13), (470, 742, 11), (700, 760, 9), (400, 660, 8)]
for (x, y, r) in sparks {
    ctx.saveGState()
    ctx.setShadow(offset: .zero, blur: r * 3 * s, color: rgb(0xff, 0xb0, 0x50, 1))
    ctx.setFillColor(rgb(0xff, 0xe2, 0x9a))
    ctx.fillEllipse(in: CGRect(x: (x - r) * s, y: (y - r) * s, width: 2 * r * s, height: 2 * r * s))
    ctx.restoreGState()
}

let img = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: img)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
