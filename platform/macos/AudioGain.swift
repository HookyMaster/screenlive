import AVFoundation

// Apply the same gain to every sample, including peaks above full scale.
func boostedSample(_ sample: Float) -> Float {
    4 * sample
}

private func writeBoostedAudio(asset: AVAsset, tracks: [AVAssetTrack], to url: URL, duration: Double, progress: (Double) -> Void) throws -> CMTime {
    let rate = 48000.0
    let reader = try AVAssetReader(asset:asset)
    let format = AVAudioFormat(standardFormatWithSampleRate:rate,channels:2)!
    let output: AVAssetReaderOutput = tracks.count == 1 ? AVAssetReaderTrackOutput(track:tracks[0],outputSettings:format.settings) : AVAssetReaderAudioMixOutput(audioTracks:tracks,audioSettings:format.settings)
    guard reader.canAdd(output) else { throw gainFailure() }
    reader.add(output)
    guard reader.startReading() else { throw reader.error ?? gainFailure() }
    defer { reader.cancelReading() }
    let file = try AVAudioFile(forWriting:url, settings:[AVFormatIDKey:kAudioFormatMPEG4AAC,
        AVSampleRateKey:rate, AVNumberOfChannelsKey:2, AVEncoderBitRateKey:192000])
    var start: CMTime?
    var written: Int64 = 0
    while let sample = output.copyNextSampleBuffer() {
        try autoreleasepool {
            let pts = sample.presentationTimeStamp
            if start == nil { start = pts }
            let frames = sample.numSamples
            guard frames > 0, let pcm = AVAudioPCMBuffer(pcmFormat:format,frameCapacity:AVAudioFrameCount(frames)),
                  let channels = pcm.floatChannelData else { throw gainFailure() }
            pcm.frameLength = pcm.frameCapacity
            let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(sample,at:0,frameCount:Int32(frames),into:pcm.mutableAudioBufferList)
            guard status == noErr else { throw NSError(domain:NSOSStatusErrorDomain,code:Int(status)) }
            // Preserve gaps between sample buffers, e.g. when a device starts late.
            let position = CMTimeConvertScale(pts - start!,timescale:48000,method:.roundHalfAwayFromZero).value
            if position > written {
                guard let silence = AVAudioPCMBuffer(pcmFormat:format,frameCapacity:4096) else { throw gainFailure() }
                for ch in 0..<2 { silence.floatChannelData![ch].initialize(repeating:0,count:4096) }
                while position > written {
                    silence.frameLength = AVAudioFrameCount(min(4096,position-written))
                    try file.write(from:silence)
                    written += Int64(silence.frameLength)
                }
            }
            for ch in 0..<2 {
                for i in 0..<frames { channels[ch][i] = boostedSample(channels[ch][i]) }
            }
            try file.write(from:pcm)
            written += Int64(frames)
            if duration.isFinite, duration > 0 { progress(min(1, Double(written) / (rate * duration))) }
        }
    }
    guard reader.status == .completed, let start else { throw reader.error ?? gainFailure() }
    return start
}

private func gainFailure() -> NSError {
    NSError(domain:"ScreenLive",code:24,userInfo:[NSLocalizedDescriptionKey:"无法处理录制音轨"])
}

func boostRecordingAudio(_ url: URL, to destination: URL, progress: @escaping @Sendable (Double) -> Void) async throws {
    let asset = AVURLAsset(url:url)
    let tracks = try await asset.loadTracks(withMediaType:.audio)
    guard !tracks.isEmpty else { throw gainFailure() }
    let audioURL = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".m4a")
    defer { try? FileManager.default.removeItem(at:audioURL) }
    let duration = try await asset.load(.duration).seconds
    let audioStart = try writeBoostedAudio(asset:asset,tracks:tracks,to:audioURL,duration:duration) { progress(0.8 * $0) }
    progress(0.8)
    let audioAsset = AVURLAsset(url:audioURL)
    let composition = AVMutableComposition()
    guard let audio = try await audioAsset.loadTracks(withMediaType:.audio).first,
          let video = try await asset.loadTracks(withMediaType:.video).first else { throw gainFailure() }
    for track in [video,audio] {
        guard let target = composition.addMutableTrack(withMediaType:track.mediaType,preferredTrackID:kCMPersistentTrackID_Invalid) else { throw gainFailure() }
        let range = try await track.load(.timeRange)
        try target.insertTimeRange(range,of:track,at:track.mediaType == .audio ? audioStart : range.start)
        if track.mediaType == .video { target.preferredTransform = try await track.load(.preferredTransform) }
    }
    guard let export = AVAssetExportSession(asset:composition,presetName:AVAssetExportPresetPassthrough) else { throw gainFailure() }
    try await exportMP4(export,to:destination) { progress(0.8 + 0.2 * $0) }
}

func exportMP4(_ export: AVAssetExportSession, to url: URL, progress: @escaping @Sendable (Double) -> Void) async throws {
    let monitor = Task {
        for await state in export.states(updateInterval:0.2) {
            if case .exporting(let value) = state { progress(value.fractionCompleted) }
        }
    }
    defer { monitor.cancel() }
    try await export.export(to:url,as:.mp4)
}
