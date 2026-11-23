import App
import Foundation
import CoreGraphics

/// The same vector drawing serves SwiftUI previews and AppKit's toolbar layer.
struct CompanionPainter {
    let look: CompanionLook
    let dark: Bool

    private struct Paint {
        let r: CGFloat
        let g: CGFloat
        let b: CGFloat

        static let white = Paint(r: 1, g: 1, b: 1)
        static let black = Paint(r: 0, g: 0, b: 0)
        static let ink = Paint(r: 0.06, g: 0.08, b: 0.15)
        static let gray = Paint(r: 0.52, g: 0.53, b: 0.55)

        static func palette(_ key: String, dark: Bool) -> Self {
            let hex = ProjectPalette.rgb(key, dark: dark)
            return Paint(r: CGFloat((hex >> 16) & 255) / 255, g: CGFloat((hex >> 8) & 255) / 255, b: CGFloat(hex & 255) / 255)
        }

        func color(_ alpha: CGFloat = 1) -> CGColor { CGColor(srgbRed: r, green: g, blue: b, alpha: alpha) }

        func mix(with other: Self, by amount: CGFloat) -> Self {
            Paint(r: r + (other.r - r) * amount, g: g + (other.g - g) * amount, b: b + (other.b - b) * amount)
        }
    }

    private static let space = CGColorSpace(name: CGColorSpace.sRGB)!
    private static let width: CGFloat = 100

    func draw(in context: CGContext, size: CGSize, pose: CompanionPose) {
        let body = pose.offline ? Paint.gray : Paint.palette(look.body, dark: dark)
        context.saveGState()
        context.scaleBy(x: size.width / Self.width, y: size.height / Self.width)
        context.setLineCap(.round)
        context.setLineJoin(.round)
        context.saveGState()
        context.translateBy(x: 50 + pose.shake, y: 55 - pose.lift + (1 - pose.squash) * 30)
        context.scaleBy(x: 2 - pose.squash, y: pose.squash)
        if pose.offline { context.setAlpha(0.66) }
        drawAccessories(in: context, body: body, pose: pose)
        drawBody(in: context, body: body)
        for side in [-1.0, 1.0] {
            drawEye(in: context, x: side * 14, side: side, pose: pose)
        }
        context.restoreGState()

        if let dream = pose.dream {
            context.setAlpha(min(1, (1 - dream) * 2.4) * min(1, (dream + 0.1) * 5))
            drawDream(in: context, x: 79 + dream * 11, y: 27 - dream * 17, size: 7.5, color: body)
            drawDream(in: context, x: 74 + dream * 11, y: 36 - dream * 17, size: 5, color: body)
        }
        context.restoreGState()
    }

    private func drawBody(in context: CGContext, body: Paint) {
        let circle = box(x: 0, y: 0, width: 77, height: 77)
        context.saveGState()
        context.addEllipse(in: circle)
        context.clip()
        let stops = [body.mix(with: .white, by: dark ? 0.36 : 0.50).color(), body.color(), body.mix(with: .black, by: 0.27).color()]
        if let gradient = CGGradient(colorsSpace: Self.space, colors: stops as CFArray, locations: [0, 0.71, 1]) {
            let center = CGPoint(x: -16, y: -24)
            context.drawRadialGradient(gradient, startCenter: center, startRadius: 1, endCenter: center, endRadius: 68, options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
        }
        context.restoreGState()
        context.setStrokeColor(Paint.white.color(0.33))
        context.setLineWidth(1.5)
        context.strokeEllipse(in: circle)
        context.move(to: CGPoint(x: -27, y: -17))
        context.addQuadCurve(to: CGPoint(x: -7, y: -32), control: CGPoint(x: -19, y: -30))
        context.setLineWidth(2.5)
        context.strokePath()
    }

    private func drawAccessories(in context: CGContext, body: Paint, pose: CompanionPose) {
        let tint = body.mix(with: .black, by: 0.18)
        if look.ears != "none" {
            for side in [-1.0, 1.0] {
                if look.ears == "round" {
                    let x = side * 28
                    let y = -24 - pose.perk
                    fillEllipse(in: context, x: x, y: y, width: 23, height: 23, color: tint.color())
                    fillEllipse(in: context, x: x, y: y, width: 14, height: 14, color: body.mix(with: .white, by: 0.39).color())
                } else {
                    context.move(to: CGPoint(x: side * 14, y: -27))
                    context.addQuadCurve(to: CGPoint(x: side * 34, y: -40 - pose.perk), control: CGPoint(x: side * 29, y: -48 - pose.perk))
                    context.addQuadCurve(to: CGPoint(x: side * 36, y: -17), control: CGPoint(x: side * 39, y: -29))
                    context.closePath()
                    context.setFillColor(tint.color())
                    context.fillPath()
                }
            }
        }
        if look.antenna {
            let x = pose.perk * 0.65 - 3
            let y = -43 - pose.perk * 0.20
            context.move(to: CGPoint(x: 0, y: -32))
            context.addQuadCurve(to: CGPoint(x: x, y: y), control: CGPoint(x: 3, y: y + 5))
            context.setStrokeColor(tint.color())
            context.setLineWidth(3.8)
            context.strokePath()
            fillEllipse(in: context, x: x, y: y, width: 11, height: 11, color: tint.color())
            let light = body.mix(with: .white, by: 0.27 + pose.antennaGlow * 0.63)
            fillEllipse(in: context, x: x, y: y - 0.6, width: 8, height: 8, color: light.color())
        }
    }

    private func drawEye(in context: CGContext, x: Double, side: Double, pose: CompanionPose) {
        let style = CompanionStyle(look: look)
        let y = 1.0
        if pose.lids == .open && pose.openness > 0.11 {
            let width = style.eye.width * pose.eyeScale
            let height = style.eye.height * pose.eyeScale * pose.openness
            let radius = min(width, height) / 2
            let eye = CGPath(roundedRect: box(x: x, y: y, width: width, height: height), cornerWidth: radius, cornerHeight: radius, transform: nil)
            context.saveGState()
            context.addPath(eye)
            context.clip()
            context.setFillColor(Paint.white.color(0.98))
            context.fill(box(x: x, y: y, width: width, height: height))
            let diameter = min(width, style.eye.height * pose.eyeScale) * style.pupilScale
            let pupilX = x + pose.gaze.x * (width - diameter) * 0.46
            let pupilY = y + pose.gaze.y * max(0, height - diameter) * 0.46
            let pupil = look.pupil == "ink" ? Paint.ink : Paint.palette(look.pupil, dark: dark)
            fillEllipse(in: context, x: pupilX, y: pupilY, width: diameter, height: diameter, color: pupil.color())
            fillEllipse(in: context, x: pupilX - diameter * 0.17, y: pupilY - diameter * 0.19, width: diameter * 0.28, height: diameter * 0.28, color: Paint.white.color(0.95))
            context.restoreGState()
        } else {
            context.move(to: CGPoint(x: x - 7, y: y))
            switch pose.lids {
            case .smile: context.addQuadCurve(to: CGPoint(x: x + 7, y: y), control: CGPoint(x: x, y: y - 12))
            case .cross:
                context.move(to: CGPoint(x: x - 5, y: y - 5))
                context.addLine(to: CGPoint(x: x + 5, y: y + 5))
                context.move(to: CGPoint(x: x + 5, y: y - 5))
                context.addLine(to: CGPoint(x: x - 5, y: y + 5))
            case .flat: context.addLine(to: CGPoint(x: x + 7, y: y))
            default: context.addQuadCurve(to: CGPoint(x: x + 7, y: y), control: CGPoint(x: x, y: y + 8))
            }
            context.setStrokeColor(Paint.ink.color(pose.offline ? 0.78 : 1))
            context.setLineWidth(3.2)
            context.strokePath()
        }
        if look.eyebrows && !pose.offline {
            let level = y - style.eye.height / 2 - 5 - max(0, pose.perk * 0.55)
            context.move(to: CGPoint(x: x - 4, y: level + pose.brow * side * 0.5))
            context.addQuadCurve(to: CGPoint(x: x + 4, y: level - pose.brow * side * 0.5), control: CGPoint(x: x, y: level - 2.8))
            context.setStrokeColor(Paint.ink.color(0.72))
            context.setLineWidth(2)
            context.strokePath()
        }
    }

    private func drawDream(in context: CGContext, x: CGFloat, y: CGFloat, size: CGFloat, color: Paint) {
        let half = size / 2
        context.move(to: CGPoint(x: x - half, y: y - half))
        context.addLine(to: CGPoint(x: x + half, y: y - half))
        context.addLine(to: CGPoint(x: x - half, y: y + half))
        context.addLine(to: CGPoint(x: x + half, y: y + half))
        context.setStrokeColor(color.color())
        context.setLineWidth(size / 3)
        context.strokePath()
    }

    private func box(x: Double, y: Double, width: Double, height: Double) -> CGRect {
        CGRect(x: x - width / 2, y: y - height / 2, width: width, height: height)
    }

    private func fillEllipse(in context: CGContext, x: Double, y: Double, width: Double, height: Double, color: CGColor) {
        context.setFillColor(color)
        context.fillEllipse(in: box(x: x, y: y, width: width, height: height))
    }
}
