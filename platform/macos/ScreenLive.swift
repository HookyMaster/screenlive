import SwiftUI
import ScreenCaptureKit
import AVFoundation
import AppKit
import CoreGraphics
import CoreMedia
import AudioToolbox

@MainActor private final class CaptureLog {
    static let shared = CaptureLog()
    private let clock = ISO8601DateFormatter()
    private var file: FileHandle?

    private init() {
        clock.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let manager = FileManager.default
        let directory = manager.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/ScreenLive")
        let url = directory.appendingPathComponent("capture.log")
        do {
            try manager.createDirectory(at:directory,withIntermediateDirectories:true)
            if !manager.fileExists(atPath:url.path) { try Data().write(to:url) }
            let handle = try FileHandle(forWritingTo:url)
            if try handle.seekToEnd() > 2_000_000 {
                try Data(contentsOf:url).write(to:directory.appendingPathComponent("capture.previous.log"),options:.atomic)
                try handle.truncate(atOffset:0)
            }
            file = handle
        } catch { NSLog("ScreenLive capture log: %@",error.localizedDescription) }
    }

    func write(_ event: String) {
        let line = "\(clock.string(from:Date())) [\(ProcessInfo.processInfo.processIdentifier)] \(event)\n"
        do { try file?.write(contentsOf:Data(line.utf8)) }
        catch { NSLog("ScreenLive capture log: %@",error.localizedDescription) }
    }
}

struct CaptureSource: Identifiable {
    let id: String
    let title: String
    let filter: SCContentFilter
    let size: CGSize
    var crop: CGRect? = nil
    var worldOrigin: CGPoint? = nil
    var windowID: CGWindowID? = nil
    var panelFilter: SCContentFilter? = nil
}

@MainActor
final class Recorder: NSObject, ObservableObject, SCRecordingOutputDelegate, SCStreamDelegate {
    @Published var sources: [CaptureSource] = []
    @Published var selected = ""
    var background = "gradient"
    var cameraSettings = CameraSettings()
    var includePanel = false
    var panelID: CGWindowID = 0
    let cameraClock = CameraClock()
    private var samples: [CameraSample] = []
    private var outputSize = CGSize.zero
    private var effectSize = CGSize.zero
    private var effectsActive = false
    @Published var ratio = "16:9"
    @Published var microphone = false
    var microphoneID: String?
    var boostAudio = true
    @Published var systemAudio = true
    @Published var busy = false
    @Published var recording = false
    @Published var status = "请选择录制来源"
    @Published var savedURL: URL?
    @Published var folder = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Movies/ScreenLive", isDirectory: true)
    private var recordingConfiguration: SCStreamConfiguration?
    func applyBackground(_ value: String) async throws {
        guard value != background else { return }
        if recording, let stream, let config = recordingConfiguration {
            config.backgroundColor = fillColor(value)
            try await stream.updateConfiguration(config)
        }
        background = value
    }
    private var stream: SCStream?
    private var output: SCRecordingOutput?
    private var pendingURL: URL?
    private var starting = false

    func refresh(explicit: Bool = false) async throws {
        func failure(_ text:String) -> NSError {
            NSError(domain:"ScreenLive",code:12,userInfo:[NSLocalizedDescriptionKey:text])
        }
        guard !busy, !recording else { throw failure("当前忙，请稍后刷新") }
        guard explicit || CGPreflightScreenCaptureAccess() else {
            throw failure("当前版本未通过屏幕访问预检。可继续使用系统选择器。")
        }
        busy = true
        defer { busy = false }
        do {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            let own = content.applications.filter { $0.processID == ProcessInfo.processInfo.processIdentifier }
            var result = content.displays.map { display in
                CaptureSource(id: "display-\(display.displayID)", title: "显示器 \(display.displayID)",
                    filter: SCContentFilter(display: display, excludingApplications: own, exceptingWindows: []),
                    size: CGSize(width: display.width, height: display.height),worldOrigin:display.frame.origin,
                    panelFilter:SCContentFilter(display:display,excludingApplications:own,exceptingWindows:content.windows.filter{$0.windowID == panelID}))
            }
            result += content.windows.filter { window in
                window.owningApplication?.processID != ProcessInfo.processInfo.processIdentifier &&
                window.frame.width > 0 && window.frame.height > 0 &&
                !(window.title ?? "").isEmpty
            }.map { window in
                CaptureSource(id: "window-\(window.windowID)", title: "\(window.owningApplication?.applicationName ?? "") · \(window.title ?? "")",
                    filter: SCContentFilter(desktopIndependentWindow: window), size: window.frame.size,worldOrigin:window.frame.origin,windowID:window.windowID)
            }
            guard !result.isEmpty else { throw failure("系统返回了空的来源列表") }
            let retained = sources.filter { $0.id == "system-picker" || $0.crop != nil }
            sources = result + retained
            if !sources.contains(where: { $0.id == selected }) { selected = "" }
            status = "准备就绪"
        } catch {
            let reason = error as NSError
            throw failure("读取来源列表失败（" + reason.domain + " " + String(reason.code) + "）：" + reason.localizedDescription)
        }
    }

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        if panel.runModal() == .OK, let url = panel.url { folder = url }
    }

    private(set) var saveProgress = Progress(totalUnitCount:100)

    func start() async {
        guard !busy, !recording, let source = sources.first(where: { $0.id == selected }) else { return }
        busy = true
        saveProgress = Progress(totalUnitCount:100)
        starting = true
        savedURL = nil
        do {
            if microphone {
                let authorized = await AVCaptureDevice.requestAccess(for: .audio)
                guard authorized else { throw NSError(domain: "ScreenLive", code: 1, userInfo: [NSLocalizedDescriptionKey: "请允许麦克风访问"]) }
            }
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let url = folder.appendingPathComponent("ScreenLive-\(UUID().uuidString).mp4")
            let config = SCStreamConfiguration()
            let dimensions: [String: (Int, Int)] = ["16:9": (3840,2160), "9:16": (2160,3840), "1:1": (2160,2160), "4:5": (2160,2700), "4:3": (2880,2160), "3:4": (2160,2880)]
            let (width, height) = dimensions[ratio] ?? (3840,2160)
            if let crop = source.crop { config.sourceRect = crop }
            outputSize = CGSize(width:width,height:height)
            effectsActive = cameraSettings.follow || cameraSettings.trail
            effectSize = source.crop?.size ?? source.filter.contentRect.size
            config.captureResolution = .best
            if effectsActive {
                let scale = min(max(1,CGFloat(source.filter.pointPixelScale)),4096/max(1,max(effectSize.width,effectSize.height)))
                config.width = max(2,Int((effectSize.width*scale/2).rounded())*2)
                config.height = max(2,Int((effectSize.height*scale/2).rounded())*2)
            } else {
                config.width = width
                config.height = height
            }
            config.scalesToFit = true
            config.backgroundColor = fillColor(background)
            config.preservesAspectRatio = true
            config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
            config.showsCursor = true
            config.capturesAudio = systemAudio
            config.excludesCurrentProcessAudio = true
            config.captureMicrophone = microphone
            if let microphoneID, microphoneID != "default" { config.microphoneCaptureDeviceID = microphoneID }
            let recordingConfig = SCRecordingOutputConfiguration()
            recordingConfig.outputURL = url
            recordingConfig.outputFileType = .mp4
            recordingConfig.videoCodecType = .h264
            let stream = SCStream(filter: includePanel ? (source.panelFilter ?? source.filter) : source.filter, configuration: config, delegate: self)
            let output = SCRecordingOutput(configuration: recordingConfig, delegate: self)
            self.recordingConfiguration = config
            self.stream = stream
            self.output = output
            pendingURL = url
            samples.removeAll(keepingCapacity:true)
            try stream.addRecordingOutput(output)
            try await stream.startCapture()
            starting = false
            if self.stream === stream {
                if effectsActive {
                    cameraClock.start(source,Double(width)/Double(height),cameraSettings) { [weak self] time,pose in
                        guard let self else { return }
                        samples.append(CameraSample(time:time,pose:pose,background:fillColor(background)))
                    }
                }
                recording = true
                busy = false
                status = "正在录制"
            }
        } catch {
            starting = false
            await fail(error)
        }
    }

    func stop() async {
        guard !busy, recording, let stream else { return }
        busy = true
        status = "正在保存…"
        do { try await stream.stopCapture() }
        catch { await fail(error) }
    }

    private func fail(_ error: Error) async {
        cameraClock.stop()
        let active = stream
        stream = nil
        output = nil
        pendingURL = nil
        if let active { try? await active.stopCapture() }
        recording = false
        busy = false
        status = "录制失败：\(error.localizedDescription)"
    }

    nonisolated func recordingOutputDidStartRecording(_ recordingOutput: SCRecordingOutput) {}
    nonisolated func recordingOutputDidFinishRecording(_ recordingOutput: SCRecordingOutput) {
        Task { @MainActor in
            guard self.output === recordingOutput else { return }
            cameraClock.stop()
            if let raw = pendingURL, microphone || systemAudio {
                do {
                    guard try await !AVURLAsset(url:raw).loadTracks(withMediaType:.audio).isEmpty else {
                        throw NSError(domain:"ScreenLive", code:22, userInfo:[NSLocalizedDescriptionKey:"原始录制没有音轨"])
                    }
                } catch {
                    pendingURL = nil; stream = nil; output = nil; recording = false; busy = false
                    status = "声音检查失败：" + error.localizedDescription + "。视频已保留在 " + raw.path
                    samples.removeAll()
                    return
                }
            }
            let progress = saveProgress
            let hasCamera = effectsActive && !samples.isEmpty
            let hasGain = boostAudio && (microphone || systemAudio)
            let videoWeight: Int64 = hasCamera ? (hasGain ? 75 : 99) : 0
            if hasCamera, let raw = pendingURL {
                busy = true
                let final = raw.deletingPathExtension().appendingPathExtension("framed.mp4")
                do {
                    try await exportCamera(raw:raw,to:final,size:effectSize,output:outputSize,samples:samples,color:cameraSettings.color) { value in
                        progress.completedUnitCount = Int64(Double(videoWeight) * value)
                    }
                    progress.completedUnitCount = videoWeight
                    if microphone || systemAudio {
                        guard try await !AVURLAsset(url:final).loadTracks(withMediaType:.audio).isEmpty else {
                            throw NSError(domain:"ScreenLive", code:23, userInfo:[NSLocalizedDescriptionKey:"构图导出时音轨丢失"])
                        }
                    }
                    savedURL = final
                    try? FileManager.default.removeItem(at:raw)
                } catch {
                    savedURL = nil; pendingURL = nil; stream = nil; output = nil; recording = false; busy = false
                    status = "构图导出失败，原始文件保留在 " + raw.path + "：" + error.localizedDescription
                    samples.removeAll()
                    return
                }
            } else { savedURL = pendingURL }
            if hasGain, let input = savedURL {
                busy = true
                savedURL = nil
                let boosted = input.deletingPathExtension().appendingPathExtension("audio.mp4")
                do {
                    try await boostRecordingAudio(input,to:boosted) { value in
                        progress.completedUnitCount = videoWeight + Int64(Double(99-videoWeight) * value)
                    }
                    savedURL = boosted
                    try? FileManager.default.removeItem(at:input)
                } catch {
                    try? FileManager.default.removeItem(at:boosted)
                    pendingURL = nil; stream = nil; output = nil; recording = false; busy = false
                    samples.removeAll()
                    status = "音量增强失败，原录制已保留在 " + input.path + "：" + error.localizedDescription
                    return
                }
            }
            samples.removeAll()
            pendingURL = nil
            stream = nil
            output = nil
            recording = false
            busy = false
            progress.completedUnitCount = 100
            status = "录制已保存"
        }
    }
    nonisolated func recordingOutput(_ recordingOutput: SCRecordingOutput, didFailWithError error: Error) {
        Task { @MainActor in
            guard self.output === recordingOutput else { return }
            await fail(error)
        }
    }
    nonisolated func stream(_ stream: SCStream, didStopWithError error: Error) {
        Task { @MainActor in
            guard self.stream === stream else { return }
            await fail(error)
        }
    }
}

import WebKit
final class AudioMeterSink: NSObject, SCStreamOutput {
    private let lock = NSLock()
    private var levels: [SCStreamOutputType: (Double, Date)] = [:]
    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio || type == .microphone,
              let format = CMSampleBufferGetFormatDescription(buffer),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(format) else { return }
        let channels = max(1, Int(asbd.pointee.mChannelsPerFrame))
        let size = MemoryLayout<AudioBufferList>.size + (channels - 1) * MemoryLayout<AudioBuffer>.stride
        let raw = UnsafeMutableRawPointer.allocate(byteCount: size, alignment: 16)
        defer { raw.deallocate() }
        let list = raw.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        let result = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(buffer, bufferListSizeNeededOut: nil, bufferListOut: list, bufferListSize: size, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, blockBufferOut: &block)
        guard result == noErr else { return }
        var sum = 0.0, count = 0
        for b in UnsafeMutableAudioBufferListPointer(list) {
            guard let data = b.mData else { continue }
            if asbd.pointee.mFormatFlags & kAudioFormatFlagIsFloat != 0 && asbd.pointee.mBitsPerChannel == 32 {
                let n = Int(b.mDataByteSize) / 4
                let samples = data.assumingMemoryBound(to: Float.self)
                for i in 0..<n { let v = Double(samples[i]); if v.isFinite { sum += v*v; count += 1 } }
            } else if asbd.pointee.mBitsPerChannel == 16 {
                let n = Int(b.mDataByteSize) / 2
                let samples = data.assumingMemoryBound(to: Int16.self)
                for i in 0..<n { let v = Double(samples[i])/32768; sum += v*v; count += 1 }
            }
        }
        let db = count > 0 ? max(-60, min(0, 10 * log10(max(sum/Double(count), 0.000001)))) : -60
        lock.lock(); levels[type] = (db, Date()); lock.unlock()
    }
    func level(_ type: SCStreamOutputType) -> Double {
        lock.lock(); defer { lock.unlock() }
        guard let item = levels[type], Date().timeIntervalSince(item.1) < 1 else { return -60 }
        return item.0
    }
}
final class DialPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}
@MainActor final class PanelController: NSObject, NSApplicationDelegate, WKScriptMessageHandlerWithReply, SCContentSharingPickerObserver {
    let recorder = Recorder()
    let preview = LivePreview()
    var panel: NSPanel!
    var web: WKWebView!
    var phase = "idle"
    var meterStream: SCStream?
    var meterSink: AudioMeterSink?
    var meterKey = ""
    var meterFilter: SCContentFilter?
    var meterGeneration = 0
    var permissionRequested = false
    func presentDisplayPicker() async throws {
        guard !recorder.recording, !recorder.busy else {
            throw NSError(domain:"ScreenLive",code:4,userInfo:[NSLocalizedDescriptionKey:"请在录制结束后重新选择来源"])
        }
        let picker = SCContentSharingPicker.shared
        var config = SCContentSharingPickerConfiguration()
        config.allowedPickerModes = [.singleDisplay]
        CaptureLog.shared.write("display.request previewRunning=\(preview.isCapturing)")
        await preview.clearSelection()
        config.excludedWindowIDs = preview.panel.map { [$0.windowNumber] } ?? []
        config.allowsChangingSelectedContent = true
        picker.defaultConfiguration = config
        picker.isActive = true
        NSApp.activate(ignoringOtherApps:true)
        CaptureLog.shared.write("display.present previewReleased=true")
        picker.present(using:.display)
    }

    private func notifyPicker(_ result: [String:String]) {
        web.callAsyncJavaScript("window.screenLivePickerChanged?.(result)",arguments:["result":result],
                                in:nil,in:.page,completionHandler:nil)
    }

    nonisolated func contentSharingPicker(_ picker: SCContentSharingPicker, didCancelFor stream: SCStream?) {
        Task { @MainActor in
            CaptureLog.shared.write("display.cancelled")
            self.notifyPicker(["status":"cancelled"])
        }
    }
    nonisolated func contentSharingPicker(_ picker: SCContentSharingPicker, didUpdateWith filter: SCContentFilter, for stream: SCStream?) {
        Task { @MainActor in
            guard picker.isActive, !self.recorder.recording, !self.recorder.busy else { return }
            var title = "已选显示器"
            if #available(macOS 15.2, *), let display = filter.includedDisplays.first {
                title = "显示器 " + String(display.displayID)
            }
            CaptureLog.shared.write("display.selected size=\(filter.contentRect.size)")
            let source = CaptureSource(id:"system-picker",title:title,filter:filter,size:filter.contentRect.size)
            self.recorder.sources.removeAll { $0.id == source.id || $0.crop != nil }
            self.recorder.sources.insert(source,at:0)
            self.recorder.selected = source.id
            self.notifyPicker(["status":"selected","id":source.id,"title":source.title])
        }
    }
    nonisolated func contentSharingPickerStartDidFailWithError(_ error: Error) {
        Task { @MainActor in
            CaptureLog.shared.write("display.failed domain=\((error as NSError).domain) code=\((error as NSError).code)")
            self.notifyPicker(["status":"error","message":error.localizedDescription])
        }
    }
    func stopMeter() async {
        meterGeneration += 1
        let old = meterStream
        meterStream = nil; meterSink = nil; meterKey = ""; meterFilter = nil
        if let old { try? await old.stopCapture() }
    }
    func meterLevels(_ input: String?, _ output: String?, sourceID: String?) async throws -> [[String: Any]] {
        let key = (input ?? "") + "|" + (output ?? "")
        if input == nil && output == nil { await stopMeter(); return [["db":-60],["db":-60]] }
        if recorder.recording || recorder.busy { return [["db":-60],["db":-60]] }
        guard let id = sourceID, let filter = recorder.sources.first(where: { $0.id == id })?.filter else {
            if meterStream != nil { await stopMeter() }
            throw NSError(domain:"ScreenLive", code:3, userInfo:[NSLocalizedDescriptionKey:"请先选择采集来源，再查看音量"])
        }
        if key != meterKey || meterStream == nil || meterFilter !== filter {
            await stopMeter()
            let generation = meterGeneration
            if input != nil, !(await AVCaptureDevice.requestAccess(for: .audio)) { throw NSError(domain:"ScreenLive", code:1, userInfo:[NSLocalizedDescriptionKey:"请允许麦克风权限"]) }
            let config = SCStreamConfiguration()
            config.width = 2; config.height = 2
            config.minimumFrameInterval = CMTime(value:1,timescale:1)
            config.capturesAudio = output != nil
            config.captureMicrophone = input != nil
            config.excludesCurrentProcessAudio = true
            if let input, input != "default" { config.microphoneCaptureDeviceID = input }
            let sink = AudioMeterSink()
            let stream = SCStream(filter: filter, configuration: config, delegate:nil)
            let queue = DispatchQueue(label:"screenlive.meters")
            if output != nil { try stream.addStreamOutput(sink, type:.audio, sampleHandlerQueue:queue) }
            if input != nil { try stream.addStreamOutput(sink, type:.microphone, sampleHandlerQueue:queue) }
            try await stream.startCapture()
            guard generation == meterGeneration, !recorder.recording, !recorder.busy else { try? await stream.stopCapture(); return [["db":-60],["db":-60]] }
            meterStream = stream; meterSink = sink; meterKey = key; meterFilter = filter
        }
        return [["db":meterSink?.level(.microphone) ?? -60],["db":meterSink?.level(.audio) ?? -60]]
    }
    var savedConfig: [String: Any] = [:]
    func applicationDidFinishLaunching(_ notification: Notification) {

        CaptureLog.shared.write("app.started version=\(Bundle.main.object(forInfoDictionaryKey:"CFBundleShortVersionString") ?? "?")")
        SCContentSharingPicker.shared.add(self)
        let config = WKWebViewConfiguration()
        config.userContentController.addScriptMessageHandler(self, contentWorld: .page, name: "screenlive")
        config.userContentController.addUserScript(WKUserScript(source: "window.__TAURI__ = {core:{invoke:(command,args={})=>window.webkit.messageHandlers.screenlive.postMessage({command,args})}};", injectionTime: .atDocumentStart, forMainFrameOnly: true))
        web = WKWebView(frame: .zero, configuration: config)
        web.setValue(false, forKey: "drawsBackground")
        panel = DialPanel(contentRect: NSRect(x: 100, y: 300, width: 280, height: 340), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        recorder.panelID = CGWindowID(panel.windowNumber)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        configureOverlay(panel)
        panel.isMovableByWindowBackground = true
        panel.contentView = web
        panel.makeKeyAndOrderFront(nil)
        if let root = Bundle.main.resourceURL?.appendingPathComponent("desktop") {
            web.loadFileURL(root.appendingPathComponent("prototype.html"), allowingReadAccessTo: root)
        }
    }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        panel.orderFrontRegardless()
        return true
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage, replyHandler: @escaping (Any?, String?) -> Void) {
        guard message.frameInfo.isMainFrame, let body = message.body as? [String: Any], let command = body["command"] as? String else { replyHandler(nil, "Invalid request"); return }
        let a = body["args"] as? [String: Any] ?? [:]
        Task { @MainActor in
            do { replyHandler(try await run(command, a), nil) }
            catch {
                CaptureLog.shared.write("\(command).failed domain=\((error as NSError).domain) code=\((error as NSError).code)")
                replyHandler(nil, error.localizedDescription)
            }
        }
    }
    func run(_ command: String, _ a: [String: Any]) async throws -> Any {
        func error(_ msg: String) -> NSError { NSError(domain: "ScreenLive", code: 1, userInfo: [NSLocalizedDescriptionKey: msg]) }
        switch command {
        case "panel_ready": return true
        case "clear_sources":
            guard !recorder.recording, !recorder.busy else { throw error("请在录制结束后取消来源") }
            CaptureLog.shared.write("selection.clear")
            SCContentSharingPicker.shared.isActive = false
            recorder.sources.removeAll()
            recorder.selected = ""
            await stopMeter()
            await preview.clearSelection()
            return true
        case "pick_display": try await presentDisplayPicker(); return true
        case "screen_permission": return CGPreflightScreenCaptureAccess()
        case "request_screen_permission":
            if CGPreflightScreenCaptureAccess() { return true }
            guard !permissionRequested else { return false }
            permissionRequested = true
            return CGRequestScreenCaptureAccess()
        case "open_screen_settings":
            if let url = URL(string:"x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") { NSWorkspace.shared.open(url) }
            return true
        case "size_panel":
            let collapsed = a["collapsed"] as? Bool ?? false
            let menu = a["menu"] as? Bool ?? false
            let size = collapsed ? NSSize(width: menu ? 190 : 74, height: menu ? 100 : 74) : NSSize(width: 280, height: 340)
            var f = panel.frame; f.origin.y += f.height - size.height; f.size = size
            panel.setFrame(f, display: true); return true
        case "drag_panel":
            if let event = NSApp.currentEvent, event.type == .leftMouseDown || event.type == .leftMouseDragged { panel.performDrag(with: event) }
            return true
        case "exit_app":
            guard !recorder.recording && !recorder.busy else { throw error("请先停止录制并等待保存") }
            NSApp.terminate(nil); return true
        case "list_sources":
            try await recorder.refresh(explicit: a["explicit"] as? Bool == true)
            if recorder.sources.isEmpty { throw error(recorder.status) }
            return recorder.sources.filter { $0.crop == nil }.map { ["id":$0.id,"title":$0.title] }
        case "load_config":
            if let data = UserDefaults.standard.data(forKey: "dialConfigData"), let config = try? JSONSerialization.jsonObject(with: data) { return config }; return ["mode":"fit","preview":"hide","systemAudio":true]
        case "save_config":
            if let config = a["config"] as? [String: Any], JSONSerialization.isValidJSONObject(config), let data = try? JSONSerialization.data(withJSONObject: config) {
                UserDefaults.standard.set(data, forKey: "dialConfigData")
            }
            return true
        case "pick_folder": recorder.chooseFolder(); return recorder.folder.path
        case "background_choices": return []
        case "audio_devices":
            let devices = AVCaptureDevice.DiscoverySession(deviceTypes: [.microphone, .external], mediaType: .audio, position: .unspecified).devices
            return ["input":[["default","系统默认麦克风"]] + devices.map { [$0.uniqueID, $0.localizedName] }, "output":[["default","系统音频"]]]
        case "read_levels":
            do { return try await meterLevels(a["input"] as? String, a["output"] as? String, sourceID:a["source"] as? String) }
            catch { return [["db": -60, "error": error.localizedDescription],["db": -60, "error": error.localizedDescription]] }
        case "set_preview":
            let options = a["options"] as? [String:Any] ?? [:]
            panel.sharingType = options["includePanel"] as? Bool == true ? .readOnly : .none
            if let color = options["background"] as? String { try await recorder.applyBackground(color) }
            if options["preview"] as? Bool == true, let source = recorder.sources.first(where: { $0.id == options["source"] as? String }) {
                do {
                    CaptureLog.shared.write("preview.start region=\(source.crop != nil)")
                    try await preview.show(source:source,ratio:options["ratio"] as? String ?? "9:16",background:options["background"] as? String ?? "gradient",settings:CameraSettings(options),includePanel:options["includePanel"] as? Bool == true)
                    CaptureLog.shared.write("preview.running")
                } catch let error as NSError where error.domain == SCStreamErrorDomain && error.code == -3801 {
                    await preview.stop()
                    CaptureLog.shared.write("preview.denied domain=\(error.domain) code=\(error.code)")
                    return ["previewDenied":true]
                }
            } else {
                if preview.isCapturing { CaptureLog.shared.write("preview.stop") }
                await preview.stop()
            }
            return true
        case "pick_region":
            CaptureLog.shared.write("region.request previewRunning=\(preview.isCapturing)")
            let choice = recorder.sources.first { $0.id == a["source"] as? String }
            guard let base = choice else { throw error("请先选择一个来源") }
            let selectedRect: CGRect?
            if base.id == "system-picker" || base.filter.style != .display {
                let logical = base.filter.contentRect.size
                guard logical.width > 0, logical.height > 0, let screen = panel.screen ?? NSScreen.main else { throw error("无法读取来源尺寸，请重新选择来源") }
                let snapshot = try await preview.regionImage(for:base,includePanel:a["includePanel"] as? Bool == true)
                CaptureLog.shared.write("region.snapshotReady")
                selectedRect = await chooseRectangle(on:screen,snapshot:snapshot,logicalSize:logical)
            } else {
                let id = UInt32(base.id.replacingOccurrences(of:"display-",with:""))
                guard let screen = NSScreen.screens.first(where:{ ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id }) else { throw error("显示器已断开，请重新选择") }
                selectedRect = await chooseRectangle(on:screen)
            }
            guard let rect = selectedRect else { CaptureLog.shared.write("region.cancelled"); return NSNull() }
            CaptureLog.shared.write("region.selected size=\(rect.size)")
            let id = ["region",String(Int(rect.minX)),String(Int(rect.minY)),String(Int(rect.maxX)),String(Int(rect.maxY))].joined(separator:":")
            recorder.sources.removeAll { $0.crop != nil }
            recorder.sources.append(CaptureSource(id:id,title:"框选区域",filter:base.filter,size:rect.size,crop:rect,worldOrigin:base.worldOrigin,windowID:base.windowID,panelFilter:base.panelFilter))
            return id
        case "begin":
            await stopMeter()
            let o = a["options"] as? [String: Any] ?? [:]
            let source = o["source"] as? String ?? ""
            guard !source.contains(","), recorder.sources.contains(where: {$0.id == source}) else { throw error("请先选择要录制的显示器") }
            recorder.selected = source
            recorder.cameraSettings = CameraSettings(o)
            recorder.includePanel = o["includePanel"] as? Bool == true
            panel.sharingType = recorder.includePanel ? .readOnly : .none
            recorder.background = o["background"] as? String ?? "gradient"
            recorder.ratio = o["ratio"] as? String ?? "9:16"
            recorder.boostAudio = o["boostAudio"] as? Bool ?? true
            recorder.microphone = o["inputDevice"] is String
            recorder.microphoneID = o["inputDevice"] as? String
            recorder.systemAudio = o["outputDevice"] is String
            if let path = o["outputFolder"] as? String, !path.isEmpty { recorder.folder = URL(fileURLWithPath:path) }
            else { recorder.folder = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Movies/ScreenLive") }
            await recorder.start()
            guard recorder.recording else { throw error(recorder.status) }
            phase = "running"; return true
        case "control":
            phase = "saving"; await recorder.stop(); return true
        case "recording_status":
            if recorder.recording { return ["state":phase,"progress":Int(recorder.saveProgress.completedUnitCount)] }
            if let url = recorder.savedURL { return ["state":"saved","path":url.path] }
            return ["state":"failed","error":recorder.status]
        case "show_recording":
            guard let url = recorder.savedURL else { throw error("未找到录制文件") }
            if a["folder"] as? Bool == true { NSWorkspace.shared.activateFileViewerSelecting([url]) }
            else { NSWorkspace.shared.open(url) }
            return true
        default: throw error("暂不支持：\(command)")
        }
    }
}
@main struct ScreenLiveApp {
    @MainActor static func main() {
        let app = NSApplication.shared
        let delegate = PanelController()
        app.delegate = delegate
        app.setActivationPolicy(.regular)
        app.run()
    }
}
