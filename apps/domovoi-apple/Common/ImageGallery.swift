import App
import SwiftUI

/// Images to look through, from the one tapped.
struct ImageGallery: Identifiable {
    let id = UUID()
    let images: [ImageView]
    let start: Int
}

private struct ShowImagesKey: EnvironmentKey {
    static let defaultValue: (@MainActor (ImageGallery) -> Void)? = nil
}

extension EnvironmentValues {
    /// Shows images full size, from a viewer the transcript holds: one on
    /// the row itself went when the row scrolled out of the lazy stack (or
    /// moved into a step group) while a reply streamed. Unset, the row shows
    /// its own.
    var showImages: (@MainActor (ImageGallery) -> Void)? {
        get { self[ShowImagesKey.self] }
        set { self[ShowImagesKey.self] = newValue }
    }
}

extension View {
    /// Shows a gallery full screen on iPhone and iPad, as a sheet on the Mac.
    func imageGallery(_ gallery: Binding<ImageGallery?>) -> some View {
        modifier(ImageGalleryPresenter(gallery: gallery))
    }
}

private struct ImageGalleryPresenter: ViewModifier {
    @Binding var gallery: ImageGallery?
    /// Handed on: the viewer shows what Copy and Save did itself.
    @Environment(\.notice) private var notice

    func body(content: Content) -> some View {
        #if os(iOS)
        content.fullScreenCover(item: $gallery) { ImageGalleryView(gallery: $0).environment(\.notice, notice) }
        #else
        content.sheet(item: $gallery) { ImageGalleryView(gallery: $0).frame(minWidth: 640, minHeight: 520).environment(\.notice, notice) }
        #endif
    }
}

extension ImageStore {
    /// The longer side of a thumbnail shown `side` points wide: sharp on any
    /// screen, and in the long-press preview.
    static func thumbnailPixels(_ side: CGFloat) -> CGFloat { max(side * 3, 512) }
}

/// One image as a square tile: a thumbnail made for its size, a tap that
/// shows it full size (or loads it again after a failure), and Share, Copy
/// and Save on a long press.
struct ImageTile: View {
    let image: ImageView
    let side: CGFloat
    /// More images than the row shows: "+N" over this one.
    var more = 0
    let open: () -> Void
    @State private var thumbnail: PlatformImage?
    @State private var file: ImageFile?
    @State private var failed = false
    @State private var attempt = 0

    init(image: ImageView, side: CGFloat, more: Int = 0, open: @escaping () -> Void) {
        self.image = image
        self.side = side
        self.more = more
        self.open = open
        // Drawn at once when it was loaded before (a row scrolled back in).
        if let url = URL(string: image.url) {
            _thumbnail = State(initialValue: ImageStore.shared.cachedThumbnail(url, pixels: ImageStore.thumbnailPixels(side)))
            _file = State(initialValue: ImageStore.shared.cachedFile(url))
        }
    }

    var body: some View {
        Button {
            if failed {
                failed = false
                attempt += 1
            } else {
                open()
            }
        } label: {
            ZStack {
                if let thumbnail {
                    Image(platformImage: thumbnail).resizable().scaledToFill()
                } else if failed {
                    Image(systemName: "arrow.clockwise").foregroundStyle(.secondary)
                } else if side >= 96 {
                    ProgressView()
                }
            }
            .frame(width: side, height: side)
            .background(.quaternary.opacity(0.4))
            .overlay {
                if more > 0 {
                    Color.black.opacity(0.45)
                    Text("+\(more)").font(.title3.weight(.semibold)).monospacedDigit().foregroundStyle(.white)
                }
            }
            // Concentric with the bubble they sit in.
            .clipShape(RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
            .contentShape(RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
        }
        .buttonStyle(.plain)
        .contextMenu {
            if let file {
                ImageActionItems(file: file, name: image.name, preview: thumbnail)
            }
        } preview: {
            if let thumbnail {
                Image(platformImage: thumbnail).resizable().scaledToFit().frame(maxWidth: 320, maxHeight: 320)
            }
        }
        .accessibilityLabel(failed ? "Couldn’t load image \(image.name)" : more > 0 ? "Image \(image.name), and \(more) more" : "Image \(image.name)")
        .accessibilityHint(failed ? "Loads it again" : "Shows it full size")
        .task(id: "\(image.url)\u{0}\(attempt)") {
            guard thumbnail == nil || file == nil else { return }
            guard let url = URL(string: image.url) else { failed = true; return }
            do {
                file = try await ImageStore.shared.file(url)
                thumbnail = try await ImageStore.shared.thumbnail(url, pixels: ImageStore.thumbnailPixels(side))
            } catch {
                if !Task.isCancelled { failed = true }
            }
        }
    }
}

/// A small thumbnail, for the gallery's strip.
private struct ThumbnailImage: View {
    let url: String
    let side: CGFloat
    @State private var thumbnail: PlatformImage?

    var body: some View {
        ZStack {
            if let thumbnail {
                Image(platformImage: thumbnail).resizable().scaledToFill()
            } else {
                Color.white.opacity(0.08)
            }
        }
        .frame(width: side, height: side)
        .clipShape(RoundedRectangle(cornerRadius: Radius.chip / 2, style: .continuous))
        .task(id: url) {
            guard let address = URL(string: url) else { return }
            thumbnail = try? await ImageStore.shared.thumbnail(address, pixels: ImageStore.thumbnailPixels(side))
        }
    }
}

private enum GalleryPage {
    case failed
    case ready(ImageFile, PlatformImage)
}

/// Images full size, one page each: swipe (or the arrow keys on the Mac)
/// between them, pinch or double-tap to zoom, and Share, Copy and Save the
/// one showing. On iPhone a tap hides the controls and a swipe down closes it.
struct ImageGalleryView: View {
    let gallery: ImageGallery
    @State private var index: Int
    /// The pages loaded around the one showing.
    @State private var pages: [Int: GalleryPage]
    @State private var retry = 0
    @Environment(\.notice) private var notice
    @State private var chrome = true
    /// How far a swipe down has pulled the picture.
    @State private var pull: CGFloat = 0
    @Environment(\.dismiss) private var dismiss
    #if os(macOS)
    @FocusState private var focused: Bool
    #endif

    init(gallery: ImageGallery) {
        self.gallery = gallery
        let start = min(max(0, gallery.start), max(0, gallery.images.count - 1))
        _index = State(initialValue: start)
        var pages: [Int: GalleryPage] = [:]
        if gallery.images.indices.contains(start), let url = URL(string: gallery.images[start].url),
           let file = ImageStore.shared.cachedFile(url), let picture = ImageStore.shared.cachedPicture(url) {
            pages[start] = .ready(file, picture)
        }
        _pages = State(initialValue: pages)
    }

    private var images: [ImageView] { gallery.images }
    private var current: ImageView? { images.indices.contains(index) ? images[index] : nil }
    private var counter: String { "\(index + 1) of \(images.count)" }

    /// The viewer covers the window's toasts: what Copy and Save did shows here.
    @ViewBuilder private var toasts: some View {
        if let core = notice.core {
            ToastCapsule(core: core).padding(.top, Space.s).animation(.snappy, value: core.view.toast?.id)
        }
    }

    var body: some View {
        content
            .task(id: "\(index)\u{0}\(retry)") { await load(around: index) }
    }

    // MARK: iPhone and iPad

    #if os(iOS)
    private var content: some View {
        NavigationStack {
            ZStack {
                Color.black.opacity(1 - min(0.7, pull / 400)).ignoresSafeArea()
                TabView(selection: $index) {
                    ForEach(images.indices, id: \.self) { i in
                        page(i).tag(i)
                    }
                }
                .tabViewStyle(.page(indexDisplayMode: .never))
                .ignoresSafeArea()
                .offset(y: pull)
            }
            .overlay(alignment: .top) { toasts }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if chrome, images.count > 1 { filmstrip.transition(.opacity) }
            }
            .navigationTitle(current?.name ?? "")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 0) {
                        Text(current?.name ?? "").font(.headline).lineLimit(1).truncationMode(.middle)
                        if images.count > 1 {
                            Text(counter).font(TypeRole.meta).monospacedDigit().foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityElement(children: .combine)
                }
                ToolbarItemGroup(placement: .bottomBar) {
                    if case .ready(let file, let picture)? = pages[index], let current {
                        ImageActionItems(file: file, name: current.name, preview: picture)
                    }
                }
            }
            .toolbarVisibility(chrome ? .visible : .hidden, for: .navigationBar, .bottomBar)
            .statusBarHidden(!chrome)
        }
        .preferredColorScheme(.dark)
    }

    @ViewBuilder
    private func page(_ i: Int) -> some View {
        let image = images[i]
        Group {
            switch pages[i] {
            case .ready(_, let picture)?:
                ZoomingImage(
                    image: picture,
                    current: i == index,
                    onTap: { withAnimation(.easeInOut(duration: 0.2)) { chrome.toggle() } },
                    onPull: { pull = $0 },
                    onRelease: { distance, speed in
                        if distance > 120 || speed > 900 {
                            dismiss()
                        } else {
                            withAnimation(.snappy) { pull = 0 }
                        }
                    }
                )
            case .failed?:
                failure(i)
            case nil:
                ProgressView().tint(.white)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Image \(image.name)")
        .accessibilityValue(images.count > 1 ? "\(i + 1) of \(images.count)" : "")
        .accessibilityAddTraits(.isImage)
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: step(1)
            case .decrement: step(-1)
            @unknown default: break
            }
        }
    }
    #endif

    // MARK: Mac

    #if os(macOS)
    private var content: some View {
        VStack(spacing: 0) {
            HStack(spacing: Space.m) {
                VStack(alignment: .leading, spacing: 0) {
                    Text(current?.name ?? "").font(.headline).lineLimit(1).truncationMode(.middle)
                    if images.count > 1 {
                        Text(counter).font(TypeRole.meta).monospacedDigit().foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
                if case .ready(let file, let picture)? = pages[index], let current {
                    ImageActionItems(file: file, name: current.name, preview: picture)
                        .labelStyle(.iconOnly)
                        .buttonStyle(.borderless)
                }
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            .padding(Space.m)
            ZStack {
                Color.black
                macPage(index).id(index)
                if images.count > 1 {
                    HStack {
                        pageButton("chevron.left", "Previous Image", -1).disabled(index == 0)
                        Spacer()
                        pageButton("chevron.right", "Next Image", 1).disabled(index == images.count - 1)
                    }
                    .padding(Space.m)
                }
            }
            .overlay(alignment: .top) { toasts }
            if images.count > 1 { filmstrip.background(Color.black) }
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onAppear { focused = true }
        .onKeyPress(.leftArrow) { step(-1); return .handled }
        .onKeyPress(.rightArrow) { step(1); return .handled }
    }

    @ViewBuilder
    private func macPage(_ i: Int) -> some View {
        if images.indices.contains(i) {
            switch pages[i] {
            case .ready(_, let picture)?:
                ZoomingImage(image: picture)
                    .accessibilityLabel("Image \(images[i].name)")
                    .accessibilityValue(images.count > 1 ? "\(i + 1) of \(images.count)" : "")
            case .failed?:
                failure(i)
            case nil:
                ProgressView()
            }
        }
    }

    private func pageButton(_ symbol: String, _ label: String, _ delta: Int) -> some View {
        Button { step(delta) } label: {
            Image(systemName: symbol)
                .font(.title2.weight(.semibold))
                .frame(width: Size.tap, height: Size.tap)
                .glassPanel(in: Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .help(label)
    }
    #endif

    // MARK: Both

    private func failure(_ i: Int) -> some View {
        VStack(spacing: Space.m) {
            Label("Couldn’t load the image.", systemImage: "photo").foregroundStyle(.secondary)
            Button("Try Again") {
                pages[i] = nil
                retry += 1
            }
        }
    }

    /// Every image small, the one showing marked; a tap shows another.
    private var filmstrip: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: Space.xs) {
                    ForEach(images.indices, id: \.self) { i in
                        Button {
                            withAnimation(.snappy) { index = i }
                        } label: {
                            ThumbnailImage(url: images[i].url, side: 40)
                                .opacity(i == index ? 1 : 0.55)
                                .overlay {
                                    if i == index {
                                        RoundedRectangle(cornerRadius: Radius.chip / 2, style: .continuous).strokeBorder(.white, lineWidth: 2)
                                    }
                                }
                        }
                        .buttonStyle(.plain)
                        .id(i)
                        .accessibilityLabel("Image \(images[i].name)")
                        .accessibilityAddTraits(i == index ? .isSelected : [])
                    }
                }
                .padding(.horizontal, Space.l)
                .padding(.vertical, Space.s)
            }
            .onAppear { proxy.scrollTo(index, anchor: .center) }
            .onChange(of: index) { withAnimation(.snappy) { proxy.scrollTo(index, anchor: .center) } }
        }
    }

    private func step(_ delta: Int) {
        let next = index + delta
        guard images.indices.contains(next) else { return }
        withAnimation(.snappy) { index = next }
    }

    /// The page showing first, then its neighbours, so a swipe lands on a
    /// picture; pages further away are let go.
    private func load(around i: Int) async {
        for j in [i, i + 1, i - 1] where images.indices.contains(j) {
            if pages[j] != nil { continue }
            guard let url = URL(string: images[j].url) else { pages[j] = .failed; continue }
            do {
                let file = try await ImageStore.shared.file(url)
                let picture = try await ImageStore.shared.picture(url)
                pages[j] = .ready(file, picture)
            } catch {
                if Task.isCancelled { return }
                pages[j] = .failed
            }
        }
        for j in pages.keys where abs(j - i) > 2 { pages[j] = nil }
    }
}

// MARK: - Zoom

#if os(iOS)
/// A picture that fits the screen, zoomed the way Photos does (a scroll
/// view's pinch, around the fingers, with bounds and bounce). A double-tap
/// zooms into where it landed; unzoomed, a swipe down pulls it away and
/// sideways swipes go to the pager.
struct ZoomingImage: UIViewRepresentable {
    let image: UIImage
    /// The page showing: one swiped away goes back to fitting.
    var current = true
    var onTap: () -> Void = {}
    /// A swipe down pulling it away; nil: no pulling.
    var onPull: ((CGFloat) -> Void)?
    var onRelease: (_ distance: CGFloat, _ speed: CGFloat) -> Void = { _, _ in }

    func makeUIView(context: Context) -> ZoomingScrollView { ZoomingScrollView() }

    func updateUIView(_ view: ZoomingScrollView, context: Context) {
        view.image = image
        view.onTap = onTap
        view.onPull = onPull
        view.onRelease = onRelease
        if !current { view.resetZoom() }
    }
}

final class ZoomingScrollView: UIScrollView, UIScrollViewDelegate {
    private let imageView = UIImageView()
    private let pullPan = UIPanGestureRecognizer()
    /// The bounds the picture was last fitted to.
    private var fitted: CGSize = .zero
    var onTap: () -> Void = {}
    var onPull: ((CGFloat) -> Void)? {
        didSet { pullPan.isEnabled = onPull != nil }
    }
    var onRelease: (CGFloat, CGFloat) -> Void = { _, _ in }

    var image: UIImage? {
        get { imageView.image }
        set {
            guard newValue !== imageView.image else { return }
            imageView.image = newValue
            fitted = .zero
            setNeedsLayout()
        }
    }

    private var zoomed: Bool { zoomScale > minimumZoomScale + 0.01 }

    override init(frame: CGRect) {
        super.init(frame: frame)
        delegate = self
        minimumZoomScale = 1
        maximumZoomScale = 6
        bouncesZoom = true
        showsVerticalScrollIndicator = false
        showsHorizontalScrollIndicator = false
        contentInsetAdjustmentBehavior = .never
        decelerationRate = .fast
        backgroundColor = .clear
        imageView.contentMode = .scaleAspectFit
        addSubview(imageView)

        let double = UITapGestureRecognizer(target: self, action: #selector(doubleTapped(_:)))
        double.numberOfTapsRequired = 2
        addGestureRecognizer(double)
        let single = UITapGestureRecognizer(target: self, action: #selector(tapped))
        single.require(toFail: double)
        addGestureRecognizer(single)
        pullPan.addTarget(self, action: #selector(pulled(_:)))
        addGestureRecognizer(pullPan)
    }

    required init?(coder: NSCoder) { nil }

    override func layoutSubviews() {
        super.layoutSubviews()
        if bounds.size != fitted, let size = imageView.image?.size, size.width > 0, size.height > 0, bounds.width > 0, bounds.height > 0 {
            fitted = bounds.size
            zoomScale = minimumZoomScale
            // Fits the screen; a small picture grows at most to twice its size.
            let scale = min(bounds.width / size.width, bounds.height / size.height, 2)
            imageView.frame = CGRect(origin: .zero, size: CGSize(width: size.width * scale, height: size.height * scale))
            contentSize = imageView.frame.size
        }
        centerImage()
    }

    /// Smaller than the screen, the picture sits in its middle.
    private func centerImage() {
        let x = max((bounds.width - contentSize.width) / 2, 0)
        let y = max((bounds.height - contentSize.height) / 2, 0)
        imageView.center = CGPoint(x: contentSize.width / 2 + x, y: contentSize.height / 2 + y)
    }

    func resetZoom() {
        if zoomed { setZoomScale(minimumZoomScale, animated: false) }
    }

    func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }

    func scrollViewDidZoom(_ scrollView: UIScrollView) { centerImage() }

    @objc private func tapped() { onTap() }

    @objc private func doubleTapped(_ tap: UITapGestureRecognizer) {
        if zoomed {
            setZoomScale(minimumZoomScale, animated: true)
            return
        }
        let point = tap.location(in: imageView)
        let scale = min(maximumZoomScale, 2.5)
        let size = CGSize(width: bounds.width / scale, height: bounds.height / scale)
        zoom(to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width, height: size.height), animated: true)
    }

    @objc private func pulled(_ pan: UIPanGestureRecognizer) {
        // In the window's space: the picture moves with the pull.
        let distance = max(0, pan.translation(in: nil).y)
        switch pan.state {
        case .changed: onPull?(distance)
        case .ended, .cancelled, .failed: onRelease(distance, pan.velocity(in: nil).y)
        default: break
        }
    }

    override func gestureRecognizerShouldBegin(_ recognizer: UIGestureRecognizer) -> Bool {
        guard recognizer === pullPan else { return super.gestureRecognizerShouldBegin(recognizer) }
        // Only unzoomed, and only mostly downwards (sideways is the pager's).
        guard !zoomed else { return false }
        let velocity = pullPan.velocity(in: self)
        return velocity.y > 0 && velocity.y > abs(velocity.x) * 1.5
    }
}
#else
/// A picture that fits the sheet, with pinch (and double-click, where it
/// lands) to zoom and a drag to look around while zoomed, never past its
/// edges.
struct ZoomingImage: View {
    let image: NSImage
    @State private var scale: CGFloat = 1
    @State private var settled: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var dragged: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            let box = geo.size
            let fitted = fit(image.size, in: box)
            Image(nsImage: image).resizable().scaledToFit()
                .frame(width: fitted.width, height: fitted.height)
                .scaleEffect(scale)
                .offset(offset)
                .frame(width: box.width, height: box.height)
                .contentShape(Rectangle())
                .gesture(
                    MagnifyGesture()
                        .onChanged { value in
                            scale = min(6, max(1, settled * value.magnification))
                            offset = bounded(offset, fitted, box)
                        }
                        .onEnded { _ in
                            settled = scale
                            if scale == 1 { withAnimation(.snappy) { offset = .zero } }
                            dragged = offset
                        }
                )
                .gesture(
                    DragGesture()
                        .onChanged { value in
                            offset = bounded(CGSize(width: dragged.width + value.translation.width, height: dragged.height + value.translation.height), fitted, box)
                        }
                        .onEnded { _ in dragged = offset },
                    including: scale > 1 ? .all : .subviews
                )
                .onTapGesture(count: 2, coordinateSpace: .local) { location in
                    withAnimation(.snappy) {
                        if scale > 1 {
                            scale = 1
                            offset = .zero
                        } else {
                            scale = 2.5
                            // The point clicked stays under the pointer.
                            let point = CGSize(width: location.x - box.width / 2, height: location.y - box.height / 2)
                            offset = bounded(CGSize(width: point.width * (1 - scale), height: point.height * (1 - scale)), fitted, box)
                        }
                        settled = scale
                        dragged = offset
                    }
                }
                .accessibilityAddTraits(.isImage)
        }
        .padding(Space.l)
        .clipped()
    }

    private func fit(_ size: CGSize, in box: CGSize) -> CGSize {
        guard size.width > 0, size.height > 0, box.width > 0, box.height > 0 else { return box }
        let scale = min(box.width / size.width, box.height / size.height, 2)
        return CGSize(width: size.width * scale, height: size.height * scale)
    }

    private func bounded(_ offset: CGSize, _ fitted: CGSize, _ box: CGSize) -> CGSize {
        let x = max(0, (fitted.width * scale - box.width) / 2)
        let y = max(0, (fitted.height * scale - box.height) / 2)
        return CGSize(width: min(x, max(-x, offset.width)), height: min(y, max(-y, offset.height)))
    }
}
#endif
