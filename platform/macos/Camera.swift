import AppKit
import AVFoundation
import CoreImage

struct CameraSettings {
    var follow = false
    var trail = false
    var fraction = 0.7
    var color = CGColor(srgbRed:0.2,green:0.8,blue:0.7,alpha:1)
    init(_ values: [String:Any] = [:]) {
        follow = values["mode"] as? String == "follow"
        trail = values["cursorTrail"] as? Bool == true
        fraction = max(0.25,min(1,(values["closeRange"] as? Double ?? 70)/100))
        if let hex = values["cursorColor"] as? String, let n = Int(hex.trimmingCharacters(in:CharacterSet(charactersIn:"# ")),radix:16) {
            color = CGColor(srgbRed:CGFloat(n>>16)/255,green:CGFloat((n>>8)&255)/255,blue:CGFloat(n&255)/255,alpha:1)
        }
    }
}
struct CameraPose {
    var view: CGRect
    var guide: CGRect?
    var trail: [CGPoint]
}
struct CameraMotion {
    var view = CGRect.zero
    private var anchor: CGPoint?
    private var previous: CGPoint?
    private var focused = false
    private var transitionStart = CGRect.zero
    private var transitionTime = 0.0
    private let idleDelay: Double = 1.0
    private var lastActivity = -Double.infinity
    var history: [(Double,CGPoint)] = []
    mutating func step(size: CGSize, aspect: Double, pointer: CGPoint?, settings: CameraSettings, now:Double, dt:Double, buttonAction:Bool = false) -> CameraPose {
        let center = CGPoint(x:size.width/2,y:size.height/2)
        let near = min(size.height,size.width/aspect)*settings.fraction
        func rect(_ p:CGPoint,_ h:Double)->CGRect { CGRect(x:p.x-h*aspect/2,y:p.y-h/2,width:h*aspect,height:h) }
        let overview = rect(center,max(size.height,size.width/aspect))
        if view.isEmpty { view = overview }
        let moved = pointer != nil && previous != nil && pointer != previous
        if let pointer {
            if previous == nil || now-lastActivity >= idleDelay { anchor = previous ?? pointer }
            if moved || buttonAction { lastActivity = now }
        }
        let threshold = anchor.map { rect($0,near) }
        let crossed = moved && pointer.map { p in
            guard let box = threshold else { return false }
            return p.x < box.minX || p.x > box.maxX || p.y < box.minY || p.y > box.maxY
        } == true
        var next = focused
        if pointer != nil && (buttonAction || crossed) { next = true }
        if now-lastActivity >= idleDelay { next = false; anchor = pointer }
        if !settings.follow { next = false; view = overview }
        if next != focused {
            focused = next
            transitionStart = view
            transitionTime = now
        }
        let target = focused ? rect(pointer ?? previous ?? CGPoint(x:view.midX,y:view.midY),near) : overview
        if settings.follow && !transitionStart.isEmpty {
            let progress = min(1,max(0,(now-transitionTime)/(focused ? 0.60 : 0.42)))
            let k = progress*progress*(3-2*progress)
            view = CGRect(x:transitionStart.minX+(target.minX-transitionStart.minX)*k,
                          y:transitionStart.minY+(target.minY-transitionStart.minY)*k,
                          width:transitionStart.width+(target.width-transitionStart.width)*k,
                          height:transitionStart.height+(target.height-transitionStart.height)*k)
        }
        previous = pointer ?? (focused ? previous : nil)
        history.removeAll { now-$0.0 > 0.28 }
        if settings.trail, let pointer, history.last?.1 != pointer { history.append((now,pointer)) }
        return CameraPose(view:view,guide:settings.follow && pointer != nil ? (focused ? pointer : anchor).map{rect($0,near)} : nil,trail:history.map{$0.1})
    }
}
@MainActor final class CameraClock {
    var timer: Timer?
    func stop() { timer?.invalidate(); timer=nil }
    func start(_ source: CaptureSource, _ aspect:Double, _ settings:CameraSettings, _ apply:@escaping (Double,CameraPose)->Void) {
        stop()
        let crop = source.crop ?? CGRect(origin:.zero,size:source.filter.contentRect.size)
        var origin = source.worldOrigin ?? source.filter.contentRect.origin
        var wid = source.windowID
        if #available(macOS 15.2,*), source.filter.style == .window, let win = source.filter.includedWindows.first { wid=win.windowID; origin=win.frame.origin }
        if #available(macOS 15.2,*), source.filter.style == .display, let display = source.filter.includedDisplays.first { origin=display.frame.origin }
        let start = CACurrentMediaTime()
        var last=start, checked = -1.0, motion=CameraMotion()
        let buttonEvents: [CGEventType] = [.leftMouseDown,.rightMouseDown,.otherMouseDown]
        func pressCount() -> UInt64 {
            buttonEvents.reduce(0) { $0 + UInt64(CGEventSource.counterForEventType(.combinedSessionState,eventType:$1)) }
        }
        var lastPressCount = pressCount()
        func tick() {
            let now=CACurrentMediaTime(), t=now-start
            if let wid, t-checked>0.20 {
                checked=t
                if let row=(CGWindowListCopyWindowInfo(.optionIncludingWindow,wid) as? [[String:Any]])?.first, let bounds=row[kCGWindowBounds as String] as? NSDictionary, let rect=CGRect(dictionaryRepresentation:bounds) { origin=rect.origin }
            }
            let global=CGEvent(source:nil)?.location
            let local=global.map{CGPoint(x:$0.x-origin.x-crop.minX,y:$0.y-origin.y-crop.minY)}
            let pointer=local.flatMap{CGRect(origin:.zero,size:crop.size).contains($0) ? $0 : nil}
            let count = pressCount()
            let pressed = count != lastPressCount || NSEvent.pressedMouseButtons != 0
            lastPressCount = count
            apply(t,motion.step(size:crop.size,aspect:aspect,pointer:pointer,settings:settings,now:t,dt:now-last,buttonAction:pressed)); last=now
        }
        tick()
        let timer=Timer(timeInterval:1/30,repeats:true){_ in MainActor.assumeIsolated { tick() }}
        self.timer=timer; RunLoop.main.add(timer,forMode:.common)
    }
}

struct CameraSample { let time: Double; let pose: CameraPose; let background:CGColor }

private final class CameraInstruction: NSObject, AVVideoCompositionInstructionProtocol {
    let timeRange: CMTimeRange
    let track: CMPersistentTrackID
    let size: CGSize
    let samples: [CameraSample]
    let color: CGColor
    let enablePostProcessing = false
    let containsTweening = true
    var requiredSourceTrackIDs: [NSValue]? { [NSNumber(value:track)] }
    let passthroughTrackID = kCMPersistentTrackID_Invalid
    init(duration:CMTime,track:CMPersistentTrackID,size:CGSize,samples:[CameraSample],color:CGColor) {
        timeRange = CMTimeRange(start:.zero,duration:duration)
        self.track=track; self.size=size; self.samples=samples; self.color=color
    }
}
final class CameraCompositor: NSObject, AVVideoCompositing {
    private let context = CIContext(options:[.cacheIntermediates:false])
    var sourcePixelBufferAttributes: [String:Any]? { [kCVPixelBufferPixelFormatTypeKey as String:kCVPixelFormatType_32BGRA] }
    var requiredPixelBufferAttributesForRenderContext: [String:Any] { [kCVPixelBufferPixelFormatTypeKey as String:kCVPixelFormatType_32BGRA] }
    func renderContextChanged(_ newRenderContext: AVVideoCompositionRenderContext) {}
    func startRequest(_ req: AVAsynchronousVideoCompositionRequest) {
        guard let plan = req.videoCompositionInstruction as? CameraInstruction,
              let source = req.sourceFrame(byTrackID:plan.track),
              let dest = req.renderContext.newPixelBuffer() else {
            req.finish(with:NSError(domain:"ScreenLive",code:21,userInfo:[NSLocalizedDescriptionKey:"无法取得构图视频帧"])); return
        }
        let samples = plan.samples, size = plan.size, color = plan.color
        let input = CIImage(cvPixelBuffer:source)
        let output = CGRect(origin:.zero,size:req.renderContext.size)
        var lo=0, hi=samples.count
        while lo<hi { let m=(lo+hi)/2; if samples[m].time<=req.compositionTime.seconds { lo=m+1 } else { hi=m } }
        let sample=samples[max(0,lo-1)], view=sample.pose.view, frame=input.extent
        let fit=min(frame.width/size.width,frame.height/size.height)
        let box=CGRect(x:frame.midX-size.width*fit/2,y:frame.midY-size.height*fit/2,width:size.width*fit,height:size.height*fit)
        let zoom=output.height/view.height
        let transform=CGAffineTransform(translationX:-box.minX,y:-box.minY).concatenating(CGAffineTransform(scaleX:zoom/fit,y:zoom/fit)).concatenating(CGAffineTransform(translationX:-view.minX*zoom,y:-(size.height-view.maxY)*zoom))
        var image=input.cropped(to:box).transformed(by:transform,highQualityDownsample:true).composited(over:CIImage(color:CIColor(cgColor:sample.background)))
        let points=sample.pose.trail
        for (i,p) in points.enumerated() {
            let x=(p.x-view.minX)*zoom,y=(view.maxY-p.y)*zoom, r=2+4*Double(i+1)/Double(points.count)
            let tint=CIColor(cgColor:color)
            if let dot=CIFilter(name:"CIRadialGradient",parameters:["inputCenter":CIVector(x:x,y:y),"inputRadius0":r*0.65,"inputRadius1":r,"inputColor0":CIColor(red:tint.red,green:tint.green,blue:tint.blue,alpha:0.8),"inputColor1":CIColor.clear])?.outputImage { image=dot.cropped(to:CGRect(x:x-r,y:y-r,width:2*r,height:2*r)).composited(over:image) }
        }
        context.render(image.cropped(to:output),to:dest,bounds:output,colorSpace:CGColorSpace(name:CGColorSpace.sRGB))
        req.finish(withComposedVideoFrame:dest)
    }
}

func exportCamera(raw:URL, to destination:URL, size:CGSize, output:CGSize, samples:[CameraSample], color:CGColor) async throws {
    let asset = AVURLAsset(url:raw)
    guard !samples.isEmpty, let track = try await asset.loadTracks(withMediaType:.video).first,
          let export = AVAssetExportSession(asset:asset,presetName:AVAssetExportPresetHighestQuality)
    else { throw NSError(domain:"ScreenLive",code:20) }
    let comp = AVMutableVideoComposition()
    comp.customVideoCompositorClass = CameraCompositor.self
    comp.renderSize = output
    comp.frameDuration = CMTime(value:1,timescale:30)
    comp.instructions = [CameraInstruction(duration:try await asset.load(.duration),track:track.trackID,size:size,samples:samples,color:color)]
    export.videoComposition = comp
    try await export.export(to:destination,as:.mp4)
}
