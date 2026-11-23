import SwiftUI

/// Domovoi's Hearth Hood brand badge: the sculpted peaked hood and luminous
/// ember eyes from `tools/icons.py`, rendered natively for connect and
/// onboarding hero surfaces.
struct DomovoiMark: View {
    var size: CGFloat = 44

    var body: some View {
        let corner = size * 0.225
        ZStack {
            // Night-indigo backdrop with soft hearth bloom behind the eyes.
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .fill(
                    LinearGradient(
                        colors: [Color(red: 0.153, green: 0.184, blue: 0.357), Color(red: 0.051, green: 0.063, blue: 0.145)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                )
            RadialGradient(
                colors: [Color(red: 1.0, green: 0.616, blue: 0.141).opacity(0.45), .clear],
                center: UnitPoint(x: 0.5, y: 0.61),
                startRadius: 0,
                endRadius: size * 0.34
            )
            .clipShape(RoundedRectangle(cornerRadius: corner, style: .continuous))

            // Sculpted peaked Hearth Hood.
            HearthHoodShape()
                .fill(
                    LinearGradient(
                        colors: [
                            Color(red: 1.0, green: 0.973, blue: 0.922),
                            Color(red: 0.976, green: 0.914, blue: 0.800),
                            Color(red: 0.898, green: 0.808, blue: 0.651),
                        ],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                )
                .shadow(color: .black.opacity(0.35), radius: size * 0.04, y: size * 0.02)

            // Two luminous ember eyes.
            HStack(spacing: size * 0.095) {
                emberEye.rotationEffect(.degrees(-2.5))
                emberEye.rotationEffect(.degrees(2.5))
            }
            .offset(y: size * 0.11)
        }
        .frame(width: size, height: size)
        .overlay(
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .strokeBorder(.white.opacity(0.14), lineWidth: 1)
        )
        .accessibilityHidden(true)
    }

    private var emberEye: some View {
        RoundedRectangle(cornerRadius: size * 0.048, style: .continuous)
            .fill(
                RadialGradient(
                    colors: [
                        Color(red: 1.0, green: 0.957, blue: 0.722),
                        Color(red: 1.0, green: 0.761, blue: 0.239),
                        Color(red: 0.949, green: 0.541, blue: 0.086),
                    ],
                    center: UnitPoint(x: 0.5, y: 0.38),
                    startRadius: 0,
                    endRadius: size * 0.10
                )
            )
            .frame(width: size * 0.118, height: size * 0.145)
            .shadow(color: Color(red: 0.95, green: 0.55, blue: 0.09).opacity(0.55), radius: size * 0.05)
    }
}

private struct HearthHoodShape: Shape {
    func path(in rect: CGRect) -> Path {
        let sx = rect.width / 1024
        let sy = rect.height / 1024
        func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: rect.minX + x * sx, y: rect.minY + y * sy)
        }
        var path = Path()
        path.move(to: p(512, 204))
        path.addCurve(to: p(718, 352), control1: p(554, 204), control2: p(636, 254))
        path.addCurve(to: p(858, 666), control1: p(792, 440), control2: p(844, 552))
        path.addCurve(to: p(792, 738), control1: p(864, 708), control2: p(832, 742))
        path.addCurve(to: p(730, 672), control1: p(758, 734), control2: p(738, 710))
        path.addCurve(to: p(634, 452), control1: p(714, 596), control2: p(682, 518))
        path.addCurve(to: p(512, 358), control1: p(590, 392), control2: p(544, 358))
        path.addCurve(to: p(390, 452), control1: p(480, 358), control2: p(434, 392))
        path.addCurve(to: p(294, 672), control1: p(342, 518), control2: p(310, 596))
        path.addCurve(to: p(232, 738), control1: p(286, 710), control2: p(266, 734))
        path.addCurve(to: p(166, 666), control1: p(192, 742), control2: p(160, 708))
        path.addCurve(to: p(306, 352), control1: p(180, 552), control2: p(232, 440))
        path.addCurve(to: p(512, 204), control1: p(388, 254), control2: p(470, 204))
        path.closeSubpath()
        return path
    }
}
