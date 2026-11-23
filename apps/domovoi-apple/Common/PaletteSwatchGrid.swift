import SwiftUI

struct PaletteSwatchGrid: View {
    @Binding var selection: String
    var includesInk = false
    /// A swatch's dot, inside its full-size tap target.
    private let swatch: CGFloat = 26

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: Size.tap))]) {
            ForEach(includesInk ? ["ink"] + ProjectPalette.keys : ProjectPalette.keys, id: \.self) { key in
                Button { selection = key } label: {
                    Circle()
                        .fill(key == "ink" ? CompanionStyle.ink : ProjectPalette.foreground(key))
                        .frame(width: swatch, height: swatch)
                        .overlay {
                            if selection == key {
                                Image(systemName: "checkmark")
                                    .font(.caption.bold())
                                    .foregroundStyle(key == "ink" ? AnyShapeStyle(.white) : AnyShapeStyle(.background))
                            }
                        }
                        .frame(width: Size.tap, height: Size.tap)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(key.capitalized)
                .accessibilityAddTraits(selection == key ? .isSelected : [])
            }
        }
    }
}
