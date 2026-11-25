import SwiftUI

/// Domovoi's brand badge: the domovoi in his hood, with a hearth-lit face,
/// cream beard, rosy nose and friendly eyes, drawn from the geometry in
/// `tools/icons.py` for connect and onboarding hero surfaces.
struct DomovoiMark: View {
    var size: CGFloat = 44

    var body: some View {
        let corner = size * 0.225
        ZStack {
            // Warm apricot backdrop.
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .fill(LinearGradient(colors: [Color(red: 0.988, green: 0.922, blue: 0.816), Color(red: 0.949, green: 0.690, blue: 0.486)], startPoint: .top, endPoint: .bottom))

            // The hood.
            CurvePath(start: (512, 172), curves: Self.hood)
                .fill(LinearGradient(colors: [Color(red: 0.867, green: 0.369, blue: 0.255), Color(red: 0.769, green: 0.271, blue: 0.180), Color(red: 0.576, green: 0.188, blue: 0.122)], startPoint: UnitPoint(x: 0.5, y: 168 / 1024.0), endPoint: UnitPoint(x: 0.5, y: 824 / 1024.0)))
                .shadow(color: .black.opacity(0.18), radius: size * 0.02, y: size * 0.01)

            // The hearth-lit face and rosy cheeks.
            Grid1024Ellipse(cx: 512, cy: 548, rx: 178, ry: 166)
                .fill(RadialGradient(colors: [Color(red: 1.000, green: 0.945, blue: 0.839), Color(red: 1.000, green: 0.851, blue: 0.639), Color(red: 0.953, green: 0.659, blue: 0.396)], center: UnitPoint(x: 512/1024.0, y: 518/1024.0), startRadius: 0, endRadius: size * 200 / 1024))
            Grid1024Ellipse(cx: 400, cy: 586, rx: 38, ry: 24).fill(Color(red: 0.933, green: 0.498, blue: 0.424).opacity(0.45))
            Grid1024Ellipse(cx: 624, cy: 586, rx: 38, ry: 24).fill(Color(red: 0.933, green: 0.498, blue: 0.424).opacity(0.45))

            // The beard and nose.
            CurvePath(start: (330, 640), curves: Self.beard)
                .fill(LinearGradient(colors: [Color(red: 1.000, green: 0.992, blue: 0.973), Color(red: 0.957, green: 0.918, blue: 0.859), Color(red: 0.886, green: 0.824, blue: 0.729)], startPoint: UnitPoint(x: 0.5, y: 586 / 1024.0), endPoint: UnitPoint(x: 0.5, y: 846 / 1024.0)))
            Grid1024Ellipse(cx: 512, cy: 606, rx: 36, ry: 32)
                .fill(RadialGradient(colors: [Color(red: 0.965, green: 0.643, blue: 0.529), Color(red: 0.867, green: 0.455, blue: 0.345)], center: UnitPoint(x: 502/1024.0, y: 594/1024.0), startRadius: 0, endRadius: size * 44 / 1024))

            // Friendly eyes with a catch light.
            Grid1024Ellipse(cx: 446, cy: 528, rx: 25, ry: 25).fill(Color(red: 0.290, green: 0.165, blue: 0.110))
            Grid1024Ellipse(cx: 454, cy: 519, rx: 7.5, ry: 7.5).fill(.white.opacity(0.85))
            Grid1024Ellipse(cx: 578, cy: 528, rx: 25, ry: 25).fill(Color(red: 0.290, green: 0.165, blue: 0.110))
            Grid1024Ellipse(cx: 586, cy: 519, rx: 7.5, ry: 7.5).fill(.white.opacity(0.85))
        }
        .frame(width: size, height: size)
        .overlay(
            RoundedRectangle(cornerRadius: corner, style: .continuous)
                .strokeBorder(.white.opacity(0.35), lineWidth: 1)
        )
        .accessibilityHidden(true)
    }

    private static let hood: [Curve] = [
        ((592, 172), (652, 244), (690, 336)),
        ((732, 438), (764, 562), (806, 684)),
        ((828, 748), (804, 806), (742, 812)),
        ((666, 820), (590, 824), (512, 824)),
        ((434, 824), (358, 820), (282, 812)),
        ((220, 806), (196, 748), (218, 684)),
        ((260, 562), (292, 438), (334, 336)),
        ((372, 244), (432, 172), (512, 172)),
    ]

    private static let beard: [Curve] = [
        ((322, 606), (346, 584), (378, 590)),
        ((410, 596), (440, 612), (462, 628)),
        ((482, 642), (498, 642), (512, 632)),
        ((526, 642), (542, 642), (562, 628)),
        ((584, 612), (614, 596), (646, 590)),
        ((678, 584), (702, 606), (694, 640)),
        ((724, 664), (722, 704), (690, 724)),
        ((698, 762), (664, 790), (628, 784)),
        ((614, 826), (566, 850), (512, 850)),
        ((458, 850), (410, 826), (396, 784)),
        ((360, 790), (326, 762), (334, 724)),
        ((302, 704), (300, 664), (330, 640)),
    ]
}

/// A cubic Bézier segment on the 1024-point icon grid: two control points and an end point.
private typealias Curve = ((CGFloat, CGFloat), (CGFloat, CGFloat), (CGFloat, CGFloat))

/// A closed path of cubic segments on the 1024-point icon grid, scaled to the frame.
private struct CurvePath: Shape {
    let start: (CGFloat, CGFloat)
    let curves: [Curve]

    func path(in rect: CGRect) -> Path {
        func p(_ point: (CGFloat, CGFloat)) -> CGPoint {
            CGPoint(x: rect.minX + point.0 * rect.width / 1024, y: rect.minY + point.1 * rect.height / 1024)
        }
        var path = Path()
        path.move(to: p(start))
        for (control1, control2, end) in curves {
            path.addCurve(to: p(end), control1: p(control1), control2: p(control2))
        }
        path.closeSubpath()
        return path
    }
}

/// An ellipse on the 1024-point icon grid, scaled to the frame.
private struct Grid1024Ellipse: Shape {
    let cx: CGFloat, cy: CGFloat, rx: CGFloat, ry: CGFloat

    func path(in rect: CGRect) -> Path {
        let sx = rect.width / 1024, sy = rect.height / 1024
        return Path(ellipseIn: CGRect(x: rect.minX + (cx - rx) * sx, y: rect.minY + (cy - ry) * sy, width: 2 * rx * sx, height: 2 * ry * sy))
    }
}
