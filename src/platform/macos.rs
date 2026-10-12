#![allow(dead_code)]

use super::CapturePlatform;
use crate::config::AudioTrackConfig;
use crate::devices::{Device, DeviceType};
use anyhow::{Context, Result};
use regex::Regex;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::cell::Cell;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

std::thread_local! {
    /// Tracks on the current thread whether the most recent `build_recording_input_args` call
    /// placed Video-only on Input 0 (`offset = 1`) because Track 0 is a ScreenCaptureKit TCP stream,
    /// or combined Video + Track 0 AVFoundation audio on Input 0 (`offset = 0`).
    static LAST_AUDIO_INDEX_OFFSET: Cell<usize> = const { Cell::new(0) };
}

/// Embedded Swift helper source using Apple's native `ScreenCaptureKit` (`SCStream`)
/// to enumerate running macOS applications and capture per-app or full system stereo `f32le` PCM audio
/// without requiring BlackHole or third-party loopback drivers.
const SCK_HELPER_SWIFT_SRC: &str = r#"
import Foundation
import AppKit
import ScreenCaptureKit
import CoreMedia
import AudioToolbox

final class AudioOutputHandler: NSObject, SCStreamOutput, SCStreamDelegate {
    private let outHandle = FileHandle.standardOutput

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of outputType: SCStreamOutputType) {
        guard outputType == .audio, CMSampleBufferDataIsReady(sampleBuffer) else { return }

        do {
            try sampleBuffer.withAudioBufferList { buffers, _ in
                guard !buffers.isEmpty else { return }

                if buffers.count >= 2 {
                    guard let data0 = buffers[0].mData, let data1 = buffers[1].mData else { return }
                    let count0 = Int(buffers[0].mDataByteSize) / MemoryLayout<Float32>.size
                    let count1 = Int(buffers[1].mDataByteSize) / MemoryLayout<Float32>.size
                    let frames = min(count0, count1)
                    guard frames > 0 else { return }

                    let left = data0.bindMemory(to: Float32.self, capacity: frames)
                    let right = data1.bindMemory(to: Float32.self, capacity: frames)

                    var interleaved = [Float32](repeating: 0, count: frames * 2)
                    for i in 0..<frames {
                        interleaved[i * 2] = left[i]
                        interleaved[i * 2 + 1] = right[i]
                    }
                    interleaved.withUnsafeBufferPointer { ptr in
                        let data = Data(buffer: ptr)
                        do {
                            try outHandle.write(contentsOf: data)
                        } catch {
                            exit(0)
                        }
                    }
                } else {
                    guard let data0 = buffers[0].mData else { return }
                    let byteCount = Int(buffers[0].mDataByteSize)
                    guard byteCount > 0 else { return }
                    let channels = Int(buffers[0].mNumberChannels)
                    if channels == 2 {
                        let data = Data(bytes: data0, count: byteCount)
                        do {
                            try outHandle.write(contentsOf: data)
                        } catch {
                            exit(0)
                        }
                    } else if channels >= 1 {
                        let sampleCount = byteCount / MemoryLayout<Float32>.size
                        let frames = sampleCount / channels
                        guard frames > 0 else { return }
                        let src = data0.bindMemory(to: Float32.self, capacity: sampleCount)
                        var interleaved = [Float32](repeating: 0, count: frames * 2)
                        for i in 0..<frames {
                            let s = src[i * channels]
                            interleaved[i * 2] = s
                            interleaved[i * 2 + 1] = channels > 1 ? src[i * channels + 1] : s
                        }
                        interleaved.withUnsafeBufferPointer { ptr in
                            let data = Data(buffer: ptr)
                            do {
                                try outHandle.write(contentsOf: data)
                            } catch {
                                exit(0)
                            }
                        }
                    }
                }
            }
        } catch {
            return
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        exit(0)
    }
}

signal(SIGPIPE, SIG_IGN)
let _ = NSApplication.shared

let args = CommandLine.arguments
let mode = args.count > 1 ? args[1] : "--list-apps"

if mode == "--list-apps" {
    let apps = NSWorkspace.shared.runningApplications
    var seen = Set<String>()
    for app in apps {
        guard app.activationPolicy == .regular else { continue }
        guard let name = app.localizedName, !name.isEmpty else { continue }
        let bundleId = app.bundleIdentifier ?? name
        if name.lowercased().contains("multirecording") { continue }
        let key = "\(bundleId)|\(name)"
        if !seen.contains(key) {
            seen.insert(key)
            print("APP\t\(bundleId)\t\(name)")
        }
    }
    exit(0)
} else if mode == "--capture" {
    let target = args.count > 2 ? args[2] : "system"
    let sampleRate = args.count > 3 ? (Int(args[3]) ?? 48000) : 48000
    let handler = AudioOutputHandler()
    var activeStream: SCStream?

    Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
        if getppid() == 1 {
            exit(0)
        }
    }

    SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, error in
        guard let content = content, error == nil, let display = content.displays.first else {
            exit(1)
        }

        let filter: SCContentFilter
        if target == "system" || target == "desktop" {
            filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])
        } else {
            let matchedApps = content.applications.filter { app in
                app.bundleIdentifier.caseInsensitiveCompare(target) == .orderedSame ||
                app.applicationName.caseInsensitiveCompare(target) == .orderedSame ||
                app.applicationName.localizedCaseInsensitiveContains(target)
            }
            if !matchedApps.isEmpty {
                filter = SCContentFilter(display: display, including: matchedApps, exceptingWindows: [])
            } else {
                filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])
            }
        }

        let config = SCStreamConfiguration()
        config.capturesAudio = true
        config.sampleRate = sampleRate
        config.channelCount = 2
        config.excludesCurrentProcessAudio = true
        config.width = 64
        config.height = 64
        config.minimumFrameInterval = CMTime(value: 1, timescale: 5)

        let stream = SCStream(filter: filter, configuration: config, delegate: handler)
        activeStream = stream
        _ = activeStream
        let queue = DispatchQueue(label: "com.multirecording.sck.audio", qos: .userInteractive)
        do {
            try? stream.addStreamOutput(handler, type: .screen, sampleHandlerQueue: queue)
            try stream.addStreamOutput(handler, type: .audio, sampleHandlerQueue: queue)
            stream.startCapture { err in
                if err != nil {
                    exit(1)
                }
            }
        } catch {
            exit(1)
        }
    }

    RunLoop.main.run()
}
"#;

/// Compiles (if needed) and returns the path to the cached `ScreenCaptureKit` audio helper binary.
pub fn ensure_sck_helper_binary() -> Option<PathBuf> {
    static HELPER_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    HELPER_BIN
        .get_or_init(|| {
            let temp_dir = std::env::temp_dir();
            let src_path = temp_dir.join("multirecording_sck_audio_helper_v2.swift");
            let bin_path = temp_dir.join("multirecording_sck_audio_helper_v2");
            let module_cache = temp_dir.join("multirecording_swift_module_cache");

            let needs_compile = if bin_path.is_file() && src_path.is_file() {
                std::fs::read_to_string(&src_path)
                    .map(|existing| existing != SCK_HELPER_SWIFT_SRC)
                    .unwrap_or(true)
            } else {
                true
            };

            if needs_compile {
                let _ = std::fs::create_dir_all(&module_cache);
                if std::fs::write(&src_path, SCK_HELPER_SWIFT_SRC).is_err() {
                    return None;
                }
                let status = Command::new("swiftc")
                    .args([
                        "-O",
                        "-disable-sandbox",
                        "-module-cache-path",
                        &module_cache.to_string_lossy(),
                        &src_path.to_string_lossy(),
                        "-o",
                        &bin_path.to_string_lossy(),
                    ])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .ok()?;
                if !status.success() {
                    return None;
                }
            }

            if bin_path.is_file() {
                Some(bin_path)
            } else {
                None
            }
        })
        .clone()
}

/// Discovers running macOS GUI applications and system audio capture targets via `ScreenCaptureKit`.
pub fn list_running_app_audio_devices() -> Vec<Device> {
    let mut app_devices = Vec::new();

    // 1. Always provide a full Desktop / System Audio capture option via ScreenCaptureKit
    app_devices.push(Device {
        id: "out:app:system".to_string(),
        index: 0,
        name: "Desktop Audio (All Apps)".to_string(),
        device_type: DeviceType::AudioOutput,
        av_index: None,
        resolution: None,
        fps: None,
    });

    // 2. Query running macOS applications via our ScreenCaptureKit helper
    let Some(helper_bin) = ensure_sck_helper_binary() else {
        return app_devices;
    };

    if let Ok(output) = Command::new(&helper_bin)
        .arg("--list-apps")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.trim().split('\t').collect();
            if parts.len() >= 3 && parts[0] == "APP" {
                let bundle_id = parts[1].trim();
                let app_name = parts[2].trim();
                if !bundle_id.is_empty() && !app_name.is_empty() {
                    let idx = app_devices.len();
                    app_devices.push(Device {
                        id: format!("out:app:{}", bundle_id),
                        index: idx,
                        name: format!("App: {}", app_name),
                        device_type: DeviceType::AudioOutput,
                        av_index: None,
                        resolution: None,
                        fps: None,
                    });
                }
            }
        }
    }

    app_devices
}

/// Resolves an audio track `device_id` into the target string expected by `multirecording_sck_audio_helper_v1 --capture`.
fn resolve_sck_target(device_id: &str) -> Option<String> {
    let trimmed = device_id.trim();
    if let Some(app_target) = trimmed.strip_prefix("out:app:") {
        return Some(app_target.trim().to_string());
    }
    if let Some(app_target) = trimmed.strip_prefix("app:") {
        return Some(app_target.trim().to_string());
    }
    if let Some(out_target) = trimmed.strip_prefix("out:") {
        let clean = out_target.trim();
        if let Some(app_target) = clean.strip_prefix("App: ") {
            return Some(app_target.trim().to_string());
        }
        // Any hardware output endpoint on macOS (e.g. Speakers / AirPods) captures system audio via ScreenCaptureKit
        return Some("system".to_string());
    }
    None
}

fn is_sck_audio_track(track: &AudioTrackConfig) -> bool {
    resolve_sck_target(&track.device_id).is_some()
}

/// Starts an in-process TCP audio bridge backed by `ScreenCaptureKit` for a specific macOS application
/// (or full system audio). Includes wall-clock silence synthesis when the target app is paused/silent
/// so FFmpeg's stream prober and segment muxer never stall.
pub fn start_sck_audio_bridge(device_id: &str, sample_rate: u32) -> Option<(u16, u32)> {
    let target = resolve_sck_target(device_id)?;
    let helper_bin = ensure_sck_helper_binary()?;

    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();

    std::thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        let start_wait = Instant::now();
        let mut tcp_stream = None;
        while start_wait.elapsed() < Duration::from_secs(5) {
            match listener.accept() {
                Ok((s, _)) => {
                    let _ = s.set_nonblocking(false);
                    let _ = s.set_nodelay(true);
                    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                    tcp_stream = Some(s);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }

        let Some(mut stream) = tcp_stream else {
            return;
        };

        // 1. Immediately send a 50ms silent stereo primer so FFmpeg's `avformat_find_stream_info` completes in <5ms
        let primer_frames = (sample_rate as usize) / 20;
        let primer_bytes = vec![0u8; primer_frames * 8];
        if stream.write_all(&primer_bytes).is_err() || stream.flush().is_err() {
            return;
        }

        // 2. Spawn the ScreenCaptureKit audio helper subprocess for `target`
        let mut child = match Command::new(&helper_bin)
            .args(["--capture", &target, &sample_rate.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };

        let pcm_queue: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(16384)));
        let last_callback = Arc::new(Mutex::new(Instant::now()));

        if let Some(stdout) = child.stdout.take() {
            let q_clone = Arc::clone(&pcm_queue);
            let cb_clone = Arc::clone(&last_callback);
            std::thread::spawn(move || {
                let mut reader = stdout;
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut t) = cb_clone.lock() {
                        *t = Instant::now();
                    }
                    if let Ok(mut q) = q_clone.lock() {
                        if q.len() < (sample_rate as usize) * 8 {
                            q.extend_from_slice(&buf[..n]);
                        }
                    }
                }
            });
        }

        // 3. Forward real ScreenCaptureKit PCM audio or synthesize wall-clock silence when the app is quiet
        let bytes_per_sec = (sample_rate as f64) * 8.0;
        let mut silence_clock = Instant::now();
        let mut fractional_bytes: f64 = 0.0;

        loop {
            std::thread::sleep(Duration::from_millis(10));

            let chunk = if let Ok(mut q) = pcm_queue.lock() {
                if !q.is_empty() {
                    let aligned_len = (q.len() / 8) * 8;
                    if aligned_len > 0 {
                        Some(q.drain(..aligned_len).collect::<Vec<u8>>())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(real_bytes) = chunk {
                silence_clock = Instant::now();
                fractional_bytes = 0.0;
                if stream.write_all(&real_bytes).is_err() {
                    break;
                }
            } else {
                let since_cb = last_callback
                    .lock()
                    .map(|t| t.elapsed())
                    .unwrap_or(Duration::from_millis(100));
                if since_cb >= Duration::from_millis(80) {
                    let dt = silence_clock.elapsed().as_secs_f64();
                    silence_clock = Instant::now();
                    let raw_bytes = dt * bytes_per_sec + fractional_bytes;
                    let aligned_bytes = ((raw_bytes as usize) / 8) * 8;
                    fractional_bytes = raw_bytes - (aligned_bytes as f64);
                    if aligned_bytes > 0 {
                        let zeros = vec![0u8; aligned_bytes.min(sample_rate as usize * 2)];
                        if stream.write_all(&zeros).is_err() {
                            break;
                        }
                    }
                } else {
                    silence_clock = Instant::now();
                }
            }
        }

        // Cleanly terminate the ScreenCaptureKit helper when FFmpeg disconnects
        drop(child.stdin.take());
        let _ = child.kill();
        let _ = child.wait();
    });

    Some((port, sample_rate))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacOSPlatform;

impl MacOSPlatform {
    pub fn discover_devices(&self) -> Result<Vec<Device>> {
        let mut devices = Vec::new();

        // 1. Discover Video inputs & Audio inputs via AVFoundation
        let output = Command::new("ffmpeg")
            .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
            .output()
            .context("Failed to execute ffmpeg. Ensure ffmpeg is installed.")?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut in_devices = parse_avfoundation_devices(&stderr)?;
        let display_res = self.primary_display_resolution();
        for dev in &mut in_devices {
            if dev.device_type == DeviceType::Video && dev.name.to_lowercase().contains("screen") {
                if let Some(r) = &display_res {
                    dev.resolution = Some(r.clone());
                }
            }
        }
        devices.append(&mut in_devices);

        let loopback_dev = devices
            .iter()
            .find(|d| d.device_type == DeviceType::AudioInput && d.is_loopback())
            .cloned();
        let loopback_idx = loopback_dev.as_ref().and_then(|d| d.av_index);

        // 2. Discover Per-App Audio & System Audio targets via ScreenCaptureKit
        let mut app_devices = list_running_app_audio_devices();
        devices.append(&mut app_devices);

        // 3. Discover Hardware Audio Outputs via CoreAudio (system_profiler)
        if let Ok(mut out_devices) = list_output_devices(loopback_idx) {
            devices.append(&mut out_devices);
        }

        Ok(devices)
    }

    pub fn primary_display_resolution(&self) -> Option<String> {
        let output = Command::new("system_profiler")
            .args(["SPDisplaysDataType", "-json"])
            .output()
            .ok()?;
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        let displays = json.get("SPDisplaysDataType")?.as_array()?;
        for d in displays {
            if let Some(ndrvs) = d.get("spdisplays_ndrvs").and_then(|v| v.as_array()) {
                for screen in ndrvs {
                    if let Some(pixels) = screen.get("_spdisplays_pixels").and_then(|v| v.as_str())
                    {
                        let clean = pixels.replace(' ', "");
                        return Some(clean);
                    }
                }
            }
        }
        None
    }

    pub fn build_preview_input_args(&self, video_device: &str) -> Vec<String> {
        let clean = clean_avfoundation_video_device(video_device);
        let is_screen = clean.to_lowercase().contains("screen");
        let framerate = if is_screen { "60" } else { "30" };
        vec![
            "-f".to_string(),
            "avfoundation".to_string(),
            "-framerate".to_string(),
            framerate.to_string(),
            "-i".to_string(),
            format!("{}:none", clean),
        ]
    }

    pub fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[AudioTrackConfig],
    ) -> Vec<String> {
        let clean = clean_avfoundation_video_device(video_device);
        // AVFoundation devices strictly require 30 or 60 fps on input negotiation;
        // non-standard values (e.g. 15 fps migrated from Windows config.toml) cause exit code 251 (EIO).
        let av_fps = if in_fps == 60 { 60 } else { 30 };
        let mut args = Vec::new();

        let append_track_input = |args: &mut Vec<String>, track: &AudioTrackConfig| {
            if is_sck_audio_track(track) {
                if let Some((port, sr)) = start_sck_audio_bridge(&track.device_id, 48000) {
                    args.extend([
                        "-thread_queue_size".to_string(),
                        "1024".to_string(),
                        "-f".to_string(),
                        "f32le".to_string(),
                        "-sample_rate".to_string(),
                        sr.to_string(),
                        "-ch_layout".to_string(),
                        "stereo".to_string(),
                        "-i".to_string(),
                        format!("tcp://127.0.0.1:{}", port),
                    ]);
                } else {
                    args.extend([
                        "-f".to_string(),
                        "lavfi".to_string(),
                        "-i".to_string(),
                        "anullsrc=channel_layout=stereo:sample_rate=48000".to_string(),
                    ]);
                }
            } else {
                args.extend([
                    "-f".to_string(),
                    "avfoundation".to_string(),
                    "-i".to_string(),
                    format!("none:{}", track.device_index),
                ]);
            }
        };

        if let Some(first_track) = enabled_tracks.first() {
            if is_sck_audio_track(first_track) {
                // Track 0 is a ScreenCaptureKit App/System audio stream:
                // Input 0 is video-only, and audio tracks start at Input 1 (offset = 1).
                LAST_AUDIO_INDEX_OFFSET.with(|c| c.set(1));
                args.extend([
                    "-f".to_string(),
                    "avfoundation".to_string(),
                    "-framerate".to_string(),
                    av_fps.to_string(),
                    "-i".to_string(),
                    format!("{}:none", clean),
                ]);
                for track in enabled_tracks {
                    append_track_input(&mut args, track);
                }
            } else {
                // Track 0 is an AVFoundation hardware audio input:
                // Input 0 combines video + Track 0 audio (offset = 0).
                LAST_AUDIO_INDEX_OFFSET.with(|c| c.set(0));
                args.extend([
                    "-f".to_string(),
                    "avfoundation".to_string(),
                    "-framerate".to_string(),
                    av_fps.to_string(),
                    "-i".to_string(),
                    format!("{}:{}", clean, first_track.device_index),
                ]);
                for track in enabled_tracks.iter().skip(1) {
                    append_track_input(&mut args, track);
                }
            }
        } else {
            LAST_AUDIO_INDEX_OFFSET.with(|c| c.set(0));
            args.extend([
                "-f".to_string(),
                "avfoundation".to_string(),
                "-framerate".to_string(),
                av_fps.to_string(),
                "-i".to_string(),
                format!("{}:none", clean),
            ]);
        }

        args
    }

    pub fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String> {
        // Check if this identifier targets a macOS Application or System/Speaker Output via ScreenCaptureKit
        if resolve_sck_target(audio_device_identifier).is_some() {
            if let Some((port, sr)) = start_sck_audio_bridge(audio_device_identifier, 48000) {
                return vec![
                    "-probesize".to_string(),
                    "32".to_string(),
                    "-analyzeduration".to_string(),
                    "0".to_string(),
                    "-fflags".to_string(),
                    "nobuffer".to_string(),
                    "-f".to_string(),
                    "f32le".to_string(),
                    "-ar".to_string(),
                    sr.to_string(),
                    "-ac".to_string(),
                    "2".to_string(),
                    "-i".to_string(),
                    format!("tcp://127.0.0.1:{}", port),
                    "-vn".to_string(),
                    "-ac".to_string(),
                    "2".to_string(),
                    "-ar".to_string(),
                    "8000".to_string(),
                    "-f".to_string(),
                    "f32le".to_string(),
                    "-flush_packets".to_string(),
                    "1".to_string(),
                    "pipe:1".to_string(),
                ];
            }
        }

        let clean = audio_device_identifier
            .strip_prefix("in:")
            .unwrap_or(audio_device_identifier);
        let input = if clean.starts_with("none:") || (clean.contains(':') && !clean.starts_with("out:"))
        {
            clean.to_string()
        } else {
            let bare = clean.strip_prefix("out:").unwrap_or(clean);
            format!("none:{}", bare)
        };
        vec![
            "-f".to_string(),
            "avfoundation".to_string(),
            "-i".to_string(),
            input,
            "-vn".to_string(),
            "-ac".to_string(),
            "2".to_string(),
            "-ar".to_string(),
            "8000".to_string(),
            "-f".to_string(),
            "f32le".to_string(),
            "pipe:1".to_string(),
        ]
    }

    pub fn default_video_encoder(&self) -> &'static str {
        "h264_videotoolbox"
    }

    pub fn has_loopback_support(&self, devices: &[Device]) -> bool {
        devices.iter().any(|d| {
            (d.device_type == DeviceType::AudioInput && d.is_loopback())
                || (d.device_type == DeviceType::AudioOutput && d.id.starts_with("out:app:"))
        })
    }

    pub fn audio_input_index(&self, track_idx: usize) -> usize {
        track_idx + LAST_AUDIO_INDEX_OFFSET.with(|c| c.get())
    }

    pub fn available_encoders(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("h264_videotoolbox", "Apple H.264 (Hardware)"),
            ("hevc_videotoolbox", "Apple HEVC / H.265"),
            ("libx264", "libx264 (CPU)"),
        ]
    }

    pub fn validate_audio_tracks(
        &self,
        tracks: &[AudioTrackConfig],
        devices: &[Device],
    ) -> Result<(), String> {
        let sck_available = ensure_sck_helper_binary().is_some();
        for track in tracks {
            if track.enabled {
                if let Some(dev) = devices.iter().find(|d| d.id == track.device_id) {
                    if dev.device_type == DeviceType::AudioOutput
                        && dev.av_index.is_none()
                        && !sck_available
                    {
                        return Err(format!(
                            "⚠️ Track '{}' is set to '{}' (Audio Output). ScreenCaptureKit helper could not be initialized and BlackHole 2ch is not installed.",
                            track.name, dev.name
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl CapturePlatform for MacOSPlatform {
    fn discover_devices(&self) -> Result<Vec<Device>> {
        self.discover_devices()
    }

    fn primary_display_resolution(&self) -> Option<String> {
        self.primary_display_resolution()
    }

    fn build_preview_input_args(&self, video_device: &str) -> Vec<String> {
        self.build_preview_input_args(video_device)
    }

    fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[AudioTrackConfig],
    ) -> Vec<String> {
        self.build_recording_input_args(video_device, in_fps, enabled_tracks)
    }

    fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String> {
        self.build_audio_monitor_args(audio_device_identifier)
    }

    fn default_video_encoder(&self) -> &'static str {
        self.default_video_encoder()
    }

    fn has_loopback_support(&self, devices: &[Device]) -> bool {
        self.has_loopback_support(devices)
    }

    fn audio_input_index(&self, track_idx: usize) -> usize {
        self.audio_input_index(track_idx)
    }

    fn available_encoders(&self) -> &'static [(&'static str, &'static str)] {
        self.available_encoders()
    }

    fn validate_audio_tracks(
        &self,
        tracks: &[AudioTrackConfig],
        devices: &[Device],
    ) -> Result<(), String> {
        self.validate_audio_tracks(tracks, devices)
    }
}

/// Normalizes AVFoundation video device identifiers (e.g. "video:0", "video:1", "0", "FaceTime HD Camera")
/// into clean tokens suitable for FFmpeg AVFoundation inputs without redundant colons.
pub fn clean_avfoundation_video_device(dev: &str) -> &str {
    let mut s = dev.trim();
    if let Some(stripped) = s.strip_prefix("video:") {
        s = stripped.trim();
    }
    if let Some(stripped) = s.strip_suffix(":none") {
        s = stripped.trim();
    }
    s
}

pub fn parse_avfoundation_devices(output: &str) -> Result<Vec<Device>> {
    let mut devices = Vec::new();
    let mut current_type: Option<DeviceType> = None;

    static DEVICE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let device_re = DEVICE_RE.get_or_init(|| {
        Regex::new(r"\[(\d+)\]\s+(.+)$").expect("valid AVFoundation device regex")
    });

    for line in output.lines() {
        let line = line.trim();
        if line.contains("AVFoundation video devices:") {
            current_type = Some(DeviceType::Video);
            continue;
        } else if line.contains("AVFoundation audio devices:") {
            current_type = Some(DeviceType::AudioInput);
            continue;
        }

        if let Some(ref dev_type) = current_type {
            if let Some(caps) = device_re.captures(line) {
                if let (Some(idx_match), Some(name_match)) = (caps.get(1), caps.get(2)) {
                    if let Ok(idx) = idx_match.as_str().parse::<usize>() {
                        let name = name_match.as_str().trim().to_string();
                        let (id, av_idx) = match dev_type {
                            DeviceType::Video => (idx.to_string(), None),
                            DeviceType::AudioInput => (format!("in:{}", idx), Some(idx)),
                            DeviceType::AudioOutput => (format!("out:{}", name), None),
                        };
                        let (res, fps) = match dev_type {
                            DeviceType::Video => {
                                let lower = name.to_lowercase();
                                if lower.contains("screen") {
                                    (Some("3024x1964".to_string()), Some(60))
                                } else if lower.contains("cam") || lower.contains("face") {
                                    (Some("1920x1080".to_string()), Some(30))
                                } else if lower.contains("4k") {
                                    (Some("3840x2160".to_string()), Some(60))
                                } else {
                                    (Some("1920x1080".to_string()), Some(60))
                                }
                            }
                            _ => (None, None),
                        };
                        devices.push(Device {
                            id,
                            index: idx,
                            name,
                            device_type: dev_type.clone(),
                            av_index: av_idx,
                            resolution: res,
                            fps,
                        });
                    }
                }
            }
        }
    }

    Ok(devices)
}

pub fn list_output_devices(loopback_index: Option<usize>) -> Result<Vec<Device>> {
    let output = Command::new("system_profiler")
        .args(["SPAudioDataType", "-json"])
        .output()
        .context("Failed to query macOS audio outputs")?;

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    let mut outputs = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    // If ScreenCaptureKit is available, set `av_index = None` on AudioOutput endpoints so they use
    // native ScreenCaptureKit audio capture instead of requiring BlackHole loopback routing.
    let effective_loopback_idx = if ensure_sck_helper_binary().is_some() {
        None
    } else {
        loopback_index
    };

    if let Some(data_arr) = parsed.get("SPAudioDataType").and_then(|v| v.as_array()) {
        for wrapper in data_arr {
            if let Some(items) = wrapper.get("_items").and_then(|v| v.as_array()) {
                for item in items {
                    let name = item
                        .get("_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown Output");
                    let out_channels = item
                        .get("coreaudio_device_output")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if out_channels > 0 && seen_names.insert(name) {
                        let idx = outputs.len();
                        outputs.push(Device {
                            id: format!("out:{}", name),
                            index: idx,
                            name: name.to_string(),
                            device_type: DeviceType::AudioOutput,
                            av_index: effective_loopback_idx,
                            resolution: None,
                            fps: None,
                        });
                    }
                }
            }
        }
    }

    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_preview_args() {
        let mac = MacOSPlatform;
        let args = mac.build_preview_input_args("0");
        assert_eq!(
            args,
            vec!["-f", "avfoundation", "-framerate", "30", "-i", "0:none"]
        );

        let screen_args = mac.build_preview_input_args("Capture screen 0");
        assert_eq!(
            screen_args,
            vec![
                "-f",
                "avfoundation",
                "-framerate",
                "60",
                "-i",
                "Capture screen 0:none"
            ]
        );

        // Prefixed "video:0" and "video:1" (e.g. from device ID)
        let prefixed_args = mac.build_preview_input_args("video:0");
        assert_eq!(
            prefixed_args,
            vec!["-f", "avfoundation", "-framerate", "30", "-i", "0:none"]
        );

        let cam_args = mac.build_preview_input_args("FaceTime HD Camera");
        assert_eq!(
            cam_args,
            vec![
                "-f",
                "avfoundation",
                "-framerate",
                "30",
                "-i",
                "FaceTime HD Camera:none"
            ]
        );

        let prefixed_cam = mac.build_preview_input_args("video:FaceTime HD Camera");
        assert_eq!(
            prefixed_cam,
            vec![
                "-f",
                "avfoundation",
                "-framerate",
                "30",
                "-i",
                "FaceTime HD Camera:none"
            ]
        );
    }

    #[test]
    fn test_clean_avfoundation_video_device() {
        assert_eq!(clean_avfoundation_video_device("0"), "0");
        assert_eq!(clean_avfoundation_video_device("video:0"), "0");
        assert_eq!(clean_avfoundation_video_device("video:1"), "1");
        assert_eq!(clean_avfoundation_video_device("0:none"), "0");
        assert_eq!(clean_avfoundation_video_device("video:0:none"), "0");
        assert_eq!(
            clean_avfoundation_video_device("FaceTime HD Camera"),
            "FaceTime HD Camera"
        );
        assert_eq!(
            clean_avfoundation_video_device("video:FaceTime HD Camera"),
            "FaceTime HD Camera"
        );
    }

    #[test]
    fn test_macos_recording_args() {
        let mac = MacOSPlatform;
        let tracks = vec![
            AudioTrackConfig {
                enabled: true,
                name: "Mic".to_string(),
                device_index: 0,
                device_id: "in:0".to_string(),
                volume: 1.0,
            },
            AudioTrackConfig {
                enabled: true,
                name: "Desktop".to_string(),
                device_index: 1,
                device_id: "in:1".to_string(),
                volume: 1.0,
            },
        ];
        let args = mac.build_recording_input_args("0", 60, &tracks);
        assert_eq!(
            args,
            vec![
                "-f",
                "avfoundation",
                "-framerate",
                "60",
                "-i",
                "0:0",
                "-f",
                "avfoundation",
                "-i",
                "none:1"
            ]
        );
        assert_eq!(mac.audio_input_index(0), 0);
        assert_eq!(mac.audio_input_index(1), 1);

        // Prefixed video device: "video:1"
        let prefixed_args = mac.build_recording_input_args("video:1", 30, &tracks);
        assert_eq!(
            prefixed_args,
            vec![
                "-f",
                "avfoundation",
                "-framerate",
                "30",
                "-i",
                "1:0",
                "-f",
                "avfoundation",
                "-i",
                "none:1"
            ]
        );

        // Non-standard framerate (e.g. 15fps migrated from Windows config.toml) is normalized to 30
        let migrated_fps_args = mac.build_recording_input_args("0", 15, &tracks);
        assert_eq!(
            migrated_fps_args,
            vec![
                "-f",
                "avfoundation",
                "-framerate",
                "30",
                "-i",
                "0:0",
                "-f",
                "avfoundation",
                "-i",
                "none:1"
            ]
        );
    }

    #[test]
    fn test_macos_app_audio_recording_and_monitor_args() {
        let mac = MacOSPlatform;
        let app_tracks = vec![
            AudioTrackConfig {
                enabled: true,
                name: "App: Safari".to_string(),
                device_index: 0,
                device_id: "out:app:com.apple.Safari".to_string(),
                volume: 1.0,
            },
            AudioTrackConfig {
                enabled: true,
                name: "Mic".to_string(),
                device_index: 0,
                device_id: "in:0".to_string(),
                volume: 1.0,
            },
        ];

        let rec_args = mac.build_recording_input_args("0", 60, &app_tracks);
        // Video is placed on Input 0 ("0:none") and Track 0 (App: Safari) is placed on Input 1
        assert!(rec_args.contains(&"0:none".to_string()));
        assert_eq!(mac.audio_input_index(0), 1);
        assert_eq!(mac.audio_input_index(1), 2);
        assert!(rec_args.contains(&"none:0".to_string()));

        // Reset offset back to 0 when Track 0 is an AVFoundation input
        let av_tracks = vec![AudioTrackConfig {
            enabled: true,
            name: "Mic".to_string(),
            device_index: 0,
            device_id: "in:0".to_string(),
            volume: 1.0,
        }];
        let _ = mac.build_recording_input_args("0", 60, &av_tracks);
        assert_eq!(mac.audio_input_index(0), 0);
    }

    #[test]
    fn test_macos_audio_monitor_args() {
        let mac = MacOSPlatform;
        let args = mac.build_audio_monitor_args("0");
        assert_eq!(
            args,
            vec![
                "-f",
                "avfoundation",
                "-i",
                "none:0",
                "-vn",
                "-ac",
                "2",
                "-ar",
                "8000",
                "-f",
                "f32le",
                "pipe:1"
            ]
        );
    }
}
