import AppKit
import ImageIO
import UniformTypeIdentifiers

/// An image made ready to attach: small PNG, JPEG, GIF and WebP files go as
/// they are; anything larger (or another format, such as HEIC or TIFF)
/// becomes a JPEG at most 2048 pixels on its long side, as on iPhone.
struct ImageAttachment: Sendable {
    let name: String
    let mimeType: String
    let data: Data
    /// A small picture for the composer's chip.
    let thumbnail: CGImage?

    static let maxPixels = 2048
    static let maxBytes = 4_000_000

    /// Nil when the data isn't an image.
    static func prepare(_ data: Data, name: String) -> ImageAttachment? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else { return nil }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
        let width = properties[kCGImagePropertyPixelWidth] as? Int ?? 0
        let height = properties[kCGImagePropertyPixelHeight] as? Int ?? 0
        guard width > 0, height > 0 else { return nil }
        let thumbnail = scaled(source, to: 96)
        let type = (CGImageSourceGetType(source) as String?).flatMap(UTType.init)
        let passThrough: [UTType] = [.png, .jpeg, .gif, .webP]
        if let type, passThrough.contains(type), max(width, height) <= maxPixels, data.count <= maxBytes {
            return ImageAttachment(name: name, mimeType: type.preferredMIMEType ?? "image/png", data: data, thumbnail: thumbnail)
        }
        guard let image = scaled(source, to: maxPixels) else { return nil }
        let rep = NSBitmapImageRep(cgImage: image)
        guard let jpeg = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85]) else { return nil }
        let stem = (name as NSString).deletingPathExtension
        return ImageAttachment(name: (stem.isEmpty ? "image" : stem) + ".jpg", mimeType: "image/jpeg", data: jpeg, thumbnail: thumbnail)
    }

    private static func scaled(_ source: CGImageSource, to pixels: Int) -> CGImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: pixels,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }
}
