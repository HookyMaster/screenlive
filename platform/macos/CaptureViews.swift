import AppKit
import AVFoundation
import CoreImage
import ScreenCaptureKit

final class SelectionPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}
final class RegionCanvas: NSView {
    var snapshot: NSImage?
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    var finished: ((CGRect?) -> Void)?
    private var anchor = CGPoint.zero
    private var selection = CGRect.zero
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func mouseDown(with event: NSEvent) {
        anchor = convert(event.locationInWindow, from: nil)
        guard let window else { return }
        while let next = window.nextEvent(matching:[.leftMouseDragged,.leftMouseUp,.keyDown,.rightMouseDown]) {
            if next.type == .leftMouseUp { mouseUp(with:next); break }
            if next.type == .rightMouseDown || (next.type == .keyDown && next.keyCode == 53) { finished?(nil); break }
            if next.type == .leftMouseDragged { mouseDragged(with:next); displayIfNeeded() }
        }
    }
    override func mouseDragged(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        selection = CGRect(x: min(anchor.x,p.x), y: min(anchor.y,p.y), width: abs(anchor.x-p.x), height: abs(anchor.y-p.y)).intersection(bounds).integral
        needsDisplay = true
    }
    override func draw(_ dirtyRect: NSRect) {
        snapshot?.draw(in:bounds, from:.zero, operation:.copy, fraction:1, respectFlipped:true, hints:nil)
        NSColor.black.withAlphaComponent(0.28).setFill()
        bounds.fill()
        NSColor.white.setStroke()
        NSBezierPath(rect:selection).stroke()
        ("拖动框选 · Esc 取消" as NSString).draw(at: CGPoint(x:20,y:20), withAttributes:[.foregroundColor:NSColor.white,.font:NSFont.systemFont(ofSize:18)])
    }
    override func mouseUp(with event: NSEvent) {
        mouseDragged(with:event)
        finished?(selection.width >= 4 && selection.height >= 4 ? selection : nil)
    }
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { finished?(nil) }
    }
    override func rightMouseDown(with event: NSEvent) { finished?(nil) }
}

@MainActor func chooseRectangle(on screen: NSScreen, snapshot: CGImage? = nil, logicalSize: CGSize? = nil) async -> CGRect? {
    var frame = screen.frame
    if let logicalSize {
        let visible = screen.visibleFrame.insetBy(dx:30,dy:30)
        let scale = min(visible.width/logicalSize.width, visible.height/logicalSize.height, 1)
        let size = CGSize(width:logicalSize.width*scale,height:logicalSize.height*scale)
        frame = CGRect(x:visible.midX-size.width/2,y:visible.midY-size.height/2,width:size.width,height:size.height)
    }
    let window = SelectionPanel(contentRect:frame, styleMask:.borderless, backing:.buffered, defer:false)
    window.isMovable = false
    window.hidesOnDeactivate = false
    window.ignoresMouseEvents = false
    window.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
    window.level = .screenSaver
    window.backgroundColor = .clear
    window.isOpaque = false
    let canvas = RegionCanvas(frame:NSRect(origin:.zero,size:frame.size))
    if let snapshot { canvas.snapshot = NSImage(cgImage:snapshot,size:frame.size) }
    window.contentView = canvas
    NSApp.activate(ignoringOtherApps:true)
    window.makeKeyAndOrderFront(nil)
    window.makeFirstResponder(canvas)
    return await withCheckedContinuation { pending in
        canvas.finished = { rect in
            canvas.finished = nil
            window.orderOut(nil)
            pending.resume(returning:rect.map { r in
                guard let logicalSize else { return r }; return r.applying(CGAffineTransform(scaleX:logicalSize.width/frame.width,y:logicalSize.height/frame.height)).integral.intersection(CGRect(origin:.zero,size:logicalSize))
            })
        }
    }
}

final class PreviewFrames: NSObject, SCStreamOutput {
    let layer = AVSampleBufferDisplayLayer()
    // Both capture callbacks and consumers run on the main queue.
    private(set) var latest: CVPixelBuffer?
    var displaysFrames = true
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard let pixels = CMSampleBufferGetImageBuffer(sampleBuffer),
              let info = (CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary:false) as? [[SCStreamFrameInfo:Any]])?.first,
              (info[.status] as? NSNumber)?.intValue == SCFrameStatus.complete.rawValue else { return }
        latest = pixels
        guard displaysFrames else { return }
        let renderer = layer.sampleBufferRenderer
        if renderer.status == .failed { renderer.flush() }
        if renderer.isReadyForMoreMediaData { renderer.enqueue(sampleBuffer) }
    }
}
// SCStreamConfiguration does not retain its backgroundColor. Keep every preset
// alive through asynchronous capture startup, updates, and teardown.
private let capturePalette: [String: CGColor] = ["gradient":0x203651,"solid":0x222633,"mist":0xccd8e4,"mint":0xbbe6d0,"sunset":0xf09eb4,"lavender":0xcfc7ed,"apricot":0xfbbea1,"teal":0x93d7d2,"sand":0xedd7c0].mapValues { rgb in
    CGColor(srgbRed:CGFloat(rgb >> 16)/255,green:CGFloat((rgb >> 8) & 255)/255,blue:CGFloat(rgb & 255)/255,alpha:1)
}
func fillColor(_ name: String) -> CGColor {
    capturePalette[name] ?? capturePalette["gradient"]!
}
struct PreviewEdges: OptionSet {
    let rawValue: Int
    static let left = Self(rawValue:1), right = Self(rawValue:2)
    static let bottom = Self(rawValue:4), top = Self(rawValue:8)

    init(rawValue:Int) { self.rawValue = rawValue }
    init(at point: CGPoint, in bounds: CGRect) {
        self = []
        if point.x <= bounds.minX+9 { insert(.left) }
        if point.x >= bounds.maxX-9 { insert(.right) }
        if point.y <= bounds.minY+9 { insert(.bottom) }
        if point.y >= bounds.maxY-9 { insert(.top) }
    }
    func dragging(_ frame: CGRect, by delta: CGSize, aspect: CGFloat) -> CGRect {
        guard !isEmpty, aspect.isFinite, aspect > 0 else { return frame }
        let horizontal = contains(.left) || contains(.right)
        let vertical = contains(.top) || contains(.bottom)
        let dx = contains(.left) ? -delta.width : delta.width
        let dy = contains(.bottom) ? -delta.height : delta.height
        let change: CGFloat
        if horizontal && vertical {
            // Project the corner drag onto the preset's diagonal.
            change = (dx * aspect + dy) / (aspect * aspect + 1)
        } else {
            change = horizontal ? dx / aspect : dy
        }
        let height = max(96, 96 / aspect, frame.height + change)
        let width = height * aspect
        let x = contains(.left) ? frame.maxX - width : (horizontal ? frame.minX : frame.midX - width/2)
        let y = contains(.bottom) ? frame.maxY - height : (vertical ? frame.minY : frame.midY - height/2)
        return CGRect(x:x, y:y, width:width, height:height)
    }
}

final class PreviewSurface: NSView {
    var pose: CameraPose?
    let guide = CAShapeLayer()
    let tail = CAShapeLayer()
    let clippedContent = CALayer()
    var fullSize = CGSize.zero
    var region = CGRect.zero
    var video: CALayer?
    var sourceSize = CGSize.zero
    var canvasAspect: CGFloat = 1
    let canvas = CALayer()
    override func acceptsFirstMouse(for event:NSEvent?) -> Bool { true }
    override func resetCursorRects() {
        super.resetCursorRects()
        for x in [bounds.minX,bounds.maxX-9] {
            addCursorRect(CGRect(x:x,y:0,width:9,height:bounds.height),cursor:.resizeLeftRight)
        }
        for y in [bounds.minY,bounds.maxY-9] {
            addCursorRect(CGRect(x:0,y:y,width:bounds.width,height:9),cursor:.resizeUpDown)
        }
    }
    override func mouseDown(with event:NSEvent) {
        guard let window else { return }
        let edges = PreviewEdges(at:convert(event.locationInWindow,from:nil),in:bounds)
        if edges.isEmpty { window.performDrag(with:event); return }
        let anchor = window.convertPoint(toScreen:event.locationInWindow)
        let original = window.frame
        while let next = window.nextEvent(matching:[.leftMouseDragged,.leftMouseUp]) {
            let current = window.convertPoint(toScreen:next.locationInWindow)
            let delta = CGSize(width:current.x-anchor.x,height:current.y-anchor.y)
            window.setFrame(edges.dragging(original,by:delta,aspect:canvasAspect),display:true)
            if next.type == .leftMouseUp { break }
        }
    }
    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let backingScale = window?.backingScaleFactor ?? 1
        canvas.contentsScale = backingScale
        video?.contentsScale = backingScale
        let width = min(bounds.width,bounds.height*canvasAspect)
        let height = width/canvasAspect
        canvas.frame = CGRect(x:(bounds.width-width)/2,y:(bounds.height-height)/2,width:width,height:height)
        if let pose {
            let k = width / pose.view.width
            func place(_ rect:CGRect) -> CGRect {
                CGRect(x:(rect.minX-pose.view.minX)*k,y:(pose.view.maxY-rect.maxY)*k,width:rect.width*k,height:rect.height*k)
            }
            clippedContent.frame = place(CGRect(origin:.zero,size:sourceSize))
            guide.path = pose.guide.map { CGPath(rect:place($0).insetBy(dx:1,dy:1),transform:nil) }
            let path = CGMutablePath()
            for (i,p) in pose.trail.enumerated() {
                let r = 1.5+2*Double(i+1)/Double(pose.trail.count)
                path.addEllipse(in:CGRect(x:(p.x-pose.view.minX)*k-r,y:(pose.view.maxY-p.y)*k-r,width:2*r,height:2*r))
            }
            tail.path = path
        } else {
            let scale = min(width/max(sourceSize.width,1),height/max(sourceSize.height,1))
            let image = CGSize(width:sourceSize.width*scale,height:sourceSize.height*scale)
            clippedContent.frame = CGRect(x:(width-image.width)/2,y:(height-image.height)/2,width:image.width,height:image.height)
        }
        if region.width > 0, region.height > 0 {
            let sx = clippedContent.bounds.width / region.width
            let sy = clippedContent.bounds.height / region.height
            video?.frame = CGRect(x:-region.minX*sx,y:-(fullSize.height-region.maxY)*sy,
                                  width:fullSize.width*sx,height:fullSize.height*sy)
        }
        CATransaction.commit()
    }
}
@MainActor final class LivePreview: NSObject {
    private let cameraClock = CameraClock()
    private var generation = 0
    var stream: SCStream?
    var panel: NSPanel?
    private var frames: PreviewFrames?
    private var baseFilter: SCContentFilter?
    private let imageContext = CIContext()
    private func image(from sink: PreviewFrames) async throws -> CGImage {
        for _ in 0..<100 {
            if let pixels = sink.latest {
                let frame = CIImage(cvPixelBuffer:pixels)
                if let image = imageContext.createCGImage(frame, from:frame.extent) { return image }
            }
            try await Task.sleep(nanoseconds:40_000_000)
        }
        throw NSError(domain:"ScreenLive",code:18,userInfo:[NSLocalizedDescriptionKey:"等待来源画面超时。原选区已保留。"])
    }
    func regionImage(for source: CaptureSource, includePanel: Bool) async throws -> CGImage {
        if baseFilter === source.filter, let frames, stream != nil { return try await image(from:frames) }
        let size = source.filter.contentRect.size
        guard size.width > 0, size.height > 0 else { throw NSError(domain:"ScreenLive",code:19,userInfo:[NSLocalizedDescriptionKey:"来源尺寸无效"]) }
        let scale = min(1,1600/max(size.width,size.height))
        let config = SCStreamConfiguration()
        config.width = max(2,Int((size.width*scale).rounded()))
        config.height = max(2,Int((size.height*scale).rounded()))
        config.showsCursor = false
        let sink = PreviewFrames()
        sink.displaysFrames = false
        let capture = SCStream(filter:includePanel ? (source.panelFilter ?? source.filter) : source.filter,configuration:config,delegate:nil)
        try capture.addStreamOutput(sink,type:.screen,sampleHandlerQueue:.main)
        try await capture.startCapture()
        do {
            let result = try await image(from:sink)
            try await capture.stopCapture()
            return result
        } catch {
            try? await capture.stopCapture()
            throw error
        }
    }
    func stop() async {
        cameraClock.stop()
        generation += 1
        panel?.orderOut(nil)
        let previous = stream
        stream = nil
        if let previous { try? await previous.stopCapture() }
        if stream == nil { frames = nil; baseFilter = nil }
    }
    func show(source: CaptureSource, ratio:String, background:String,settings:CameraSettings,includePanel:Bool) async throws {
        let ticket = generation + 1
        await stop()
        guard generation == ticket else { return }
        let parts = ratio.split(separator:":").compactMap { Double($0) }
        let aspect = parts.count == 2 && parts[1] > 0 ? parts[0]/parts[1] : 1
        let size = NSSize(width: min(360,300*aspect), height:min(300,360/aspect))
        let window = panel ?? NSPanel(contentRect:NSRect(origin:CGPoint(x:410,y:320),size:size),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        window.isMovableByWindowBackground = false
        window.level = .floating
        window.backgroundColor = .black
        window.sharingType = .none
        // Fit the new preset inside the previous footprint when switching ratios.
        let footprint = panel?.frame.size ?? size
        let height = min(footprint.height, footprint.width/aspect)
        let targetHeight = max(96, 96/aspect, height)
        let fitted = CGSize(width:targetHeight*aspect, height:targetHeight)
        if window.frame.size != fitted {
            window.setFrame(CGRect(x:window.frame.minX, y:window.frame.maxY-fitted.height,
                                   width:fitted.width, height:fitted.height), display:true)
        }
        let sink = PreviewFrames()
        sink.layer.videoGravity = .resizeAspect
        let view = PreviewSurface(frame:NSRect(origin:.zero,size:window.frame.size))
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.black.cgColor
        view.canvas.backgroundColor = fillColor(background)
        let content = source.crop?.size ?? source.filter.contentRect.size
        guard content.width.isFinite, content.height.isFinite, content.width > 0, content.height > 0 else {
            throw NSError(domain:"ScreenLive",code:5,userInfo:[NSLocalizedDescriptionKey:"采集来源尺寸无效，请重新选择来源"])
        }
        view.sourceSize = content
        view.canvasAspect = aspect
        view.video = sink.layer
        view.layer?.addSublayer(view.canvas)
        view.fullSize = source.filter.contentRect.size
        view.region = source.crop ?? CGRect(origin:.zero,size:view.fullSize)
        view.clippedContent.masksToBounds = true
        view.canvas.addSublayer(view.clippedContent)
        view.clippedContent.addSublayer(sink.layer)
        view.canvas.masksToBounds = true
        view.guide.fillColor = nil
        view.guide.strokeColor = NSColor.white.withAlphaComponent(0.65).cgColor
        view.guide.lineDashPattern = [4,3]
        view.tail.fillColor = settings.color
        view.canvas.addSublayer(view.tail)
        view.canvas.addSublayer(view.guide)
        window.contentView = view
        view.needsLayout = true
        view.layoutSubtreeIfNeeded()
        let config = SCStreamConfiguration()
        let pixelScale = max(1, CGFloat(source.filter.pointPixelScale))
        config.captureResolution = .best
        config.width = max(2, Int((view.fullSize.width * pixelScale).rounded()))
        config.height = max(2, Int((view.fullSize.height * pixelScale).rounded()))
        config.minimumFrameInterval = CMTime(value:1,timescale:24)
        config.backgroundColor = fillColor(background)
        config.scalesToFit = true
        config.preservesAspectRatio = true
        let capture = SCStream(filter:includePanel ? (source.panelFilter ?? source.filter) : source.filter, configuration:config, delegate:nil)
        try capture.addStreamOutput(sink,type:.screen,sampleHandlerQueue:.main)
        try await capture.startCapture()
        guard generation == ticket else { try? await capture.stopCapture(); return }
        if settings.follow || settings.trail {
            cameraClock.start(source,aspect,settings) { [weak view] _,pose in
                view?.pose = pose
                view?.needsLayout = true
            }
        }
        baseFilter = source.filter
        frames = sink
        stream = capture
        panel = window
        window.orderFrontRegardless()
    }
}
