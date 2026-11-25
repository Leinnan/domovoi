import SwiftUI

/// Domovoi's brand badge ("Night Watch Brass"): a tapered brushed-brass
/// keeper's lantern cradling a glowing hearth flame over night-indigo, drawn
/// from the canonical 1024-point geometry in `tools/icons.py` for connect and
/// onboarding hero surfaces.
struct DomovoiMark: View {
    var size: CGFloat = 44

    var body: some View {
        let corner = size * 0.225
        ZStack {
            // Deep Hearth night-indigo backdrop.
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .fill(LinearGradient(
                    colors: [
                        Color(red: 0.149, green: 0.176, blue: 0.271),
                        Color(red: 0.067, green: 0.082, blue: 0.137),
                    ],
                    startPoint: .top,
                    endPoint: .bottom
                ))

            // Ambient hearth glow behind the lantern.
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .fill(RadialGradient(
                    stops: [
                        .init(color: Color(red: 1.000, green: 0.659, blue: 0.220).opacity(0.34), location: 0.00),
                        .init(color: Color(red: 0.910, green: 0.478, blue: 0.118).opacity(0.143), location: 0.40),
                        .init(color: Color(red: 0.620, green: 0.231, blue: 0.063).opacity(0.034), location: 0.76),
                        .init(color: .clear, location: 1.00),
                    ],
                    center: UnitPoint(x: 0.5, y: 580 / 1024.0),
                    startRadius: 0,
                    endRadius: size * 440 / 1024
                ))

            // Recessed warm amber glass chamber.
            Grid1024Path(subpaths: [Self.chamber])
                .fill(RadialGradient(
                    stops: [
                        .init(color: Color(red: 1.000, green: 0.878, blue: 0.541), location: 0.00),
                        .init(color: Color(red: 0.925, green: 0.549, blue: 0.133), location: 0.50),
                        .init(color: Color(red: 0.612, green: 0.247, blue: 0.063), location: 1.00),
                    ],
                    center: UnitPoint(x: 0.5, y: 600 / 1024.0),
                    startRadius: 0,
                    endRadius: size * 210 / 1024
                ))

            // Luminous hearth flame.
            Grid1024Path(subpaths: [Self.flame])
                .fill(LinearGradient(
                    stops: [
                        .init(color: Color(red: 1.000, green: 0.992, blue: 0.965), location: 0.00),
                        .init(color: Color(red: 1.000, green: 0.929, blue: 0.659), location: 0.56),
                        .init(color: Color(red: 1.000, green: 0.737, blue: 0.298), location: 1.00),
                    ],
                    startPoint: UnitPoint(x: 0.5, y: 470 / 1024.0),
                    endPoint: UnitPoint(x: 0.5, y: 710 / 1024.0)
                ))

            // Brushed antique champagne-brass bail ring and lantern frame (separate
            // even-odd fills so the ring/cap overlap never cancels out).
            ZStack {
                Grid1024Ring(cx: 512, cy: 186, outerR: 56, innerR: 22)
                    .fill(
                        LinearGradient(
                            colors: [
                                Color(red: 0.969, green: 0.906, blue: 0.769),
                                Color(red: 0.729, green: 0.569, blue: 0.322),
                            ],
                            startPoint: UnitPoint(x: 0.5, y: 130 / 1024.0),
                            endPoint: UnitPoint(x: 0.5, y: 242 / 1024.0)
                        ),
                        style: FillStyle(eoFill: true)
                    )

                Grid1024Path(subpaths: [Self.outerFrame, Self.windowCutout])
                    .fill(
                        LinearGradient(
                            stops: [
                                .init(color: Color(red: 0.953, green: 0.875, blue: 0.706), location: 0.00),
                                .init(color: Color(red: 0.839, green: 0.690, blue: 0.439), location: 0.45),
                                .init(color: Color(red: 0.604, green: 0.439, blue: 0.220), location: 1.00),
                            ],
                            startPoint: UnitPoint(x: 0.5, y: 180 / 1024.0),
                            endPoint: UnitPoint(x: 0.5, y: 838 / 1024.0)
                        ),
                        style: FillStyle(eoFill: true)
                    )
            }
            .shadow(color: .black.opacity(0.28), radius: size * 0.025, y: size * 0.012)
        }
        .frame(width: size, height: size)
        .overlay(
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .strokeBorder(.white.opacity(0.16), lineWidth: 1)
        )
        .accessibilityHidden(true)
    }

    // Canonical 1024-grid paths mirrored from `tools/icons.py`.
    private static let outerFrame = Subpath(start: (457.2, 248.4), ops: [
        .curve((487.5, 234.9), (536.5, 234.9), (566.8, 248.4)),
        .line((761.9, 335.1)),
        .curve((773.0, 340.0), (773.7, 349.4), (763.6, 356.0)),
        .line((752.1, 363.5)),
        .curve((746.5, 367.1), (741.5, 375.3), (740.9, 381.9)),
        .line((707.1, 744.1)),
        .curve((706.5, 750.7), (710.8, 758.5), (716.6, 761.6)),
        .line((732.1, 769.6)),
        .curve((740.9, 774.3), (748.0, 786.1), (748.0, 796.0)),
        .line((748.0, 810.0)),
        .curve((748.0, 825.5), (735.5, 838.0), (720.0, 838.0)),
        .line((304.0, 838.0)),
        .curve((288.5, 838.0), (276.0, 825.5), (276.0, 810.0)),
        .line((276.0, 796.0)),
        .curve((276.0, 786.1), (283.1, 774.3), (291.9, 769.6)),
        .line((307.4, 761.6)),
        .curve((313.2, 758.5), (317.5, 750.7), (316.9, 744.1)),
        .line((283.1, 381.9)),
        .curve((282.5, 375.3), (277.5, 367.1), (271.9, 363.5)),
        .line((260.4, 356.0)),
        .curve((250.3, 349.4), (251.0, 340.0), (262.1, 335.1)),
    ])

    private static let windowCutout = Subpath(start: (512.0, 396.0), ops: [
        .curve((593.7, 396.0), (674.0, 462.3), (674.0, 544.0)),
        .line((650.0, 720.0)),
        .curve((650.0, 733.3), (639.3, 744.0), (626.0, 744.0)),
        .line((398.0, 744.0)),
        .curve((384.7, 744.0), (374.0, 733.3), (374.0, 720.0)),
        .line((350.0, 544.0)),
        .curve((350.0, 462.3), (430.3, 396.0), (512.0, 396.0)),
    ])

    private static let chamber = Subpath(start: (512.0, 392.0), ops: [
        .curve((595.9, 392.0), (678.0, 460.1), (678.0, 544.0)),
        .line((654.0, 722.0)),
        .curve((654.0, 736.4), (642.4, 748.0), (628.0, 748.0)),
        .line((396.0, 748.0)),
        .curve((381.6, 748.0), (370.0, 736.4), (370.0, 722.0)),
        .line((346.0, 544.0)),
        .curve((346.0, 460.1), (428.1, 392.0), (512.0, 392.0)),
    ])

    private static let flame = Subpath(start: (512.0, 470.0), ops: [
        .curve((531.7, 518.0), (594.0, 566.0), (594.0, 621.2)),
        .curve((594.0, 698.0), (556.3, 710.0), (512.0, 710.0)),
        .curve((467.7, 710.0), (430.0, 698.0), (430.0, 621.2)),
        .curve((430.0, 566.0), (492.3, 518.0), (512.0, 470.0)),
    ])
}

private typealias Pt = (CGFloat, CGFloat)

private enum PathOp {
    case line(Pt)
    case curve(Pt, Pt, Pt)
}

private struct Subpath {
    let start: Pt
    let ops: [PathOp]
}

/// Closed 1024-grid vector path (supports even-odd multi-subpath fills).
private struct Grid1024Path: Shape {
    let subpaths: [Subpath]

    func path(in rect: CGRect) -> Path {
        func p(_ pt: Pt) -> CGPoint {
            CGPoint(x: rect.minX + pt.0 * rect.width / 1024, y: rect.minY + pt.1 * rect.height / 1024)
        }
        var path = Path()
        for sub in subpaths {
            path.move(to: p(sub.start))
            for op in sub.ops {
                switch op {
                case .line(let end):
                    path.addLine(to: p(end))
                case .curve(let c1, let c2, let end):
                    path.addCurve(to: p(end), control1: p(c1), control2: p(c2))
                }
            }
            path.closeSubpath()
        }
        return path
    }
}

/// Hollow ring on the 1024-point icon grid, filled with `FillStyle(eoFill: true)`.
private struct Grid1024Ring: Shape {
    let cx: CGFloat, cy: CGFloat, outerR: CGFloat, innerR: CGFloat

    func path(in rect: CGRect) -> Path {
        let sx = rect.width / 1024, sy = rect.height / 1024
        var path = Path()
        path.addEllipse(in: CGRect(x: rect.minX + (cx - outerR) * sx, y: rect.minY + (cy - outerR) * sy, width: 2 * outerR * sx, height: 2 * outerR * sy))
        path.addEllipse(in: CGRect(x: rect.minX + (cx - innerR) * sx, y: rect.minY + (cy - innerR) * sy, width: 2 * innerR * sx, height: 2 * innerR * sy))
        return path
    }
}
