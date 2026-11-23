import ImageIO
import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
typealias PlatformImage = NSImage
#else
import UIKit
typealias PlatformImage = UIImage
#endif

extension Image {
    init(platformImage: PlatformImage) {
        #if os(macOS)
        self.init(nsImage: platformImage)
        #else
        self.init(uiImage: platformImage)
        #endif
    }
}

/// An image as the server sent it: the bytes Share, Copy and Save hand on
/// untouched, and what kind of image they are.
final class ImageFile: @unchecked Sendable {
    let data: Data
    let type: UTType

    init(data: Data, type: UTType) {
        self.data = data
        self.type = type
    }

    /// `name` with the extension its bytes call for ("shot" → "shot.png").
    func fileName(_ name: String) -> String {
        let base = (name as NSString).deletingPathExtension
        let stem = base.isEmpty ? "Image" : base
        guard let ext = type.preferredFilenameExtension else { return name.isEmpty ? stem : name }
        return "\(stem).\(ext)"
    }
}

/// A decoded picture, handed between the decoding task and the main actor.
private final class Picture: @unchecked Sendable {
    let image: PlatformImage
    init(_ image: PlatformImage) { self.image = image }
}

/// The transcript's images, fetched once: their bytes (kept for Share, Copy
/// and Save), thumbnails made for the size they show at, and the few full
/// pictures the viewer has open. A thumbnail of a large screenshot costs a
/// fraction of the whole picture.
@MainActor
final class ImageStore {
    static let shared = ImageStore()

    private let files = NSCache<NSURL, ImageFile>()
    private let thumbnails = NSCache<NSString, Picture>()
    private let pictures = NSCache<NSURL, Picture>()
    /// One fetch per address, however many tiles ask at once.
    private var fetching: [URL: Task<ImageFile, Error>] = [:]

    private init() {
        files.totalCostLimit = 96 << 20
        thumbnails.totalCostLimit = 64 << 20
        pictures.countLimit = 6
    }

    func cachedFile(_ url: URL) -> ImageFile? { files.object(forKey: url as NSURL) }

    func cachedThumbnail(_ url: URL, pixels: CGFloat) -> PlatformImage? {
        thumbnails.object(forKey: Self.thumbnailKey(url, pixels))?.image
    }

    func cachedPicture(_ url: URL) -> PlatformImage? { pictures.object(forKey: url as NSURL)?.image }

    /// The image's bytes, fetched if they aren't here.
    func file(_ url: URL) async throws -> ImageFile {
        if let file = cachedFile(url) { return file }
        if let running = fetching[url] { return try await running.value }
        let task = Task.detached(priority: .userInitiated) { try await Self.fetch(url) }
        fetching[url] = task
        defer { fetching[url] = nil }
        let file = try await task.value
        files.setObject(file, forKey: url as NSURL, cost: file.data.count)
        return file
    }

    /// A picture whose longer side is at most `pixels`, turned upright.
    func thumbnail(_ url: URL, pixels: CGFloat) async throws -> PlatformImage {
        let key = Self.thumbnailKey(url, pixels)
        if let hit = thumbnails.object(forKey: key) { return hit.image }
        let file = try await file(url)
        let picture = try await Task.detached(priority: .userInitiated) { try Self.downsample(file.data, pixels: pixels) }.value
        thumbnails.setObject(picture, forKey: key, cost: Self.cost(picture.image))
        return picture.image
    }

    /// The whole picture, decoded off the main thread.
    func picture(_ url: URL) async throws -> PlatformImage {
        if let hit = pictures.object(forKey: url as NSURL) { return hit.image }
        let file = try await file(url)
        let picture = try await Task.detached(priority: .userInitiated) { try await Self.decode(file.data) }.value
        pictures.setObject(picture, forKey: url as NSURL)
        return picture.image
    }

    private static func thumbnailKey(_ url: URL, _ pixels: CGFloat) -> NSString {
        "\(url.absoluteString)\n\(Int(pixels))" as NSString
    }

    private static func cost(_ image: PlatformImage) -> Int {
        #if os(macOS)
        let size = image.representations.first.map { CGSize(width: $0.pixelsWide, height: $0.pixelsHigh) } ?? image.size
        return Int(size.width * size.height * 4)
        #else
        return Int(image.size.width * image.scale * image.size.height * image.scale * 4)
        #endif
    }

    nonisolated private static func fetch(_ url: URL) async throws -> ImageFile {
        let (data, response) = try await URLSession.shared.data(from: url)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else {
            throw URLError(.cannotDecodeContentData)
        }
        let type = (CGImageSourceGetType(source) as String?).flatMap { UTType($0) } ?? .png
        return ImageFile(data: data, type: type)
    }

    nonisolated private static func downsample(_ data: Data, pixels: CGFloat) throws -> Picture {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: max(1, Int(pixels)),
        ]
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            throw URLError(.cannotDecodeContentData)
        }
        #if os(macOS)
        return Picture(NSImage(cgImage: cg, size: CGSize(width: cg.width, height: cg.height)))
        #else
        return Picture(UIImage(cgImage: cg))
        #endif
    }

    nonisolated private static func decode(_ data: Data) async throws -> Picture {
        #if os(macOS)
        guard let image = NSImage(data: data) else { throw URLError(.cannotDecodeContentData) }
        return Picture(image)
        #else
        guard let image = UIImage(data: data) else { throw URLError(.cannotDecodeContentData) }
        // Decoded now, not on the first frame that draws it.
        return Picture(await image.byPreparingForDisplay() ?? image)
        #endif
    }
}
