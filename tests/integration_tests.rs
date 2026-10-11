use multirecording::config::{AudioTrackConfig, Config, SourceConfig};
use multirecording::devices::{DeviceManager, DeviceType};
use multirecording::remuxer::Remuxer;
use multirecording::ring_buffer::RingBufferManager;
use std::fs;
use std::time::Duration;

#[test]
fn test_config_custom_file_operations() {
    let temp_dir = std::env::temp_dir().join(format!("test_cfg_{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_dir);
    let cfg_path = temp_dir.join("test_config.toml");

    let mut cfg = Config::default();
    cfg.buffer_duration_secs = 600;
    cfg.segment_duration_secs = 3;
    cfg.output_format = "mov".to_string();
    cfg.audio_bitrate = "256k".to_string();

    cfg.sources = vec![
        SourceConfig {
            name: "main_game".to_string(),
            video_device: "1".to_string(),
            resolution: Some("2560x1440".to_string()),
            fps: Some(60),
            video_bitrate: "60M".to_string(),
            encoder: "hevc_videotoolbox".to_string(),
        },
        SourceConfig {
            name: "facecam".to_string(),
            video_device: "0".to_string(),
            resolution: Some("1920x1080".to_string()),
            fps: Some(30),
            video_bitrate: "12M".to_string(),
            encoder: "h264_videotoolbox".to_string(),
        },
    ];

    cfg.audio_tracks = vec![
        AudioTrackConfig {
            enabled: true,
            name: "Game Audio".to_string(),
            device_index: 0,
            device_id: "in:0".to_string(),
        },
        AudioTrackConfig {
            enabled: true,
            name: "Microphone".to_string(),
            device_index: 1,
            device_id: "in:1".to_string(),
        },
        AudioTrackConfig {
            enabled: true,
            name: "Discord".to_string(),
            device_index: 2,
            device_id: "in:2".to_string(),
        },
        AudioTrackConfig {
            enabled: false,
            name: "Music".to_string(),
            device_index: 0,
            device_id: "none".to_string(),
        },
        AudioTrackConfig {
            enabled: false,
            name: "Aux".to_string(),
            device_index: 0,
            device_id: "none".to_string(),
        },
    ];

    cfg.save_to_file(&cfg_path).expect("Failed to save config");
    assert!(cfg_path.exists());

    let loaded = Config::load_from_file(&cfg_path).expect("Failed to load config");
    assert_eq!(loaded.buffer_duration_secs, 600);
    assert_eq!(loaded.segment_duration_secs, 3);
    assert_eq!(loaded.output_format, "mov");
    assert_eq!(loaded.audio_bitrate, "256k");
    assert_eq!(loaded.sources.len(), 2);
    assert_eq!(loaded.sources[0].name, "main_game");
    assert_eq!(loaded.sources[0].resolution, Some("2560x1440".to_string()));
    assert_eq!(loaded.sources[0].encoder, "hevc_videotoolbox");
    assert_eq!(loaded.audio_tracks.len(), 5);
    assert_eq!(loaded.audio_tracks[2].name, "Discord");
    assert_eq!(loaded.clip_intervals, vec![30, 60, 300, 1200]);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_clip_intervals_and_formatting() {
    use multirecording::ui::MultiRecordingApp;

    assert_eq!(MultiRecordingApp::format_interval_label(15), "15s");
    assert_eq!(MultiRecordingApp::format_interval_label(30), "30s");
    assert_eq!(MultiRecordingApp::format_interval_label(45), "45s");
    assert_eq!(MultiRecordingApp::format_interval_label(60), "1m");
    assert_eq!(MultiRecordingApp::format_interval_label(120), "2m");
    assert_eq!(MultiRecordingApp::format_interval_label(300), "5m");
    assert_eq!(MultiRecordingApp::format_interval_label(600), "10m");
    assert_eq!(MultiRecordingApp::format_interval_label(900), "15m");
    assert_eq!(MultiRecordingApp::format_interval_label(1200), "20m");
    assert_eq!(MultiRecordingApp::format_interval_label(3600), "1h");
    assert_eq!(MultiRecordingApp::format_interval_label(90), "1m 30s");
    assert_eq!(MultiRecordingApp::format_interval_label(7200), "2h");
}

#[test]
fn test_ring_buffer_multi_stream_isolation() {
    let temp_dir = std::env::temp_dir().join(format!("test_isolation_{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_dir);

    let game_mgr = RingBufferManager::new("gameplay", &temp_dir, 2);
    let cam_mgr = RingBufferManager::new("facecam", &temp_dir, 2);

    // Create segments for both streams in the same directory
    for i in 0..4 {
        let g_file = temp_dir.join(format!("gameplay_20261010_12000{}.mp4", i * 2));
        let c_file = temp_dir.join(format!("facecam_20261010_12000{}.mp4", i * 2));
        fs::write(&g_file, b"gameplay_data").unwrap();
        fs::write(&c_file, b"facecam_data").unwrap();
    }

    // Unrelated file should be ignored
    fs::write(temp_dir.join("notes.txt"), b"some notes").unwrap();

    let g_segs = game_mgr.list_segments().unwrap();
    let c_segs = cam_mgr.list_segments().unwrap();

    assert_eq!(g_segs.len(), 4);
    assert_eq!(c_segs.len(), 4);

    for s in &g_segs {
        assert!(s.path.to_string_lossy().contains("gameplay"));
        assert!(!s.path.to_string_lossy().contains("facecam"));
    }

    for s in &c_segs {
        assert!(s.path.to_string_lossy().contains("facecam"));
        assert!(!s.path.to_string_lossy().contains("gameplay"));
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_flashback_segment_selection_bounds() {
    let temp_dir = std::env::temp_dir().join(format!("test_bounds_{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_dir);

    let mgr = RingBufferManager::new("stream", &temp_dir, 2);

    // 0 segments available
    let empty = mgr.get_segments_for_duration(Duration::from_secs(10)).unwrap();
    assert!(empty.is_empty());

    // Create 3 segments (total 6 seconds)
    for i in 0..3 {
        let f = temp_dir.join(format!("stream_20261010_12000{}.mp4", i * 2));
        fs::write(&f, b"data").unwrap();
    }

    // Requesting 1 second should return 1 segment (the newest one)
    let one = mgr.get_segments_for_duration(Duration::from_secs(1)).unwrap();
    assert_eq!(one.len(), 1);
    assert!(one[0].to_string_lossy().ends_with("stream_20261010_120004.mp4"));

    // Requesting 4 seconds should return 2 segments (the last two)
    let two = mgr.get_segments_for_duration(Duration::from_secs(4)).unwrap();
    assert_eq!(two.len(), 2);
    assert!(two[0].to_string_lossy().ends_with("stream_20261010_120002.mp4"));
    assert!(two[1].to_string_lossy().ends_with("stream_20261010_120004.mp4"));

    // Requesting 60 seconds (more than available) returns all 3 segments gracefully
    let all = mgr.get_segments_for_duration(Duration::from_secs(60)).unwrap();
    assert_eq!(all.len(), 3);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_device_classification() {
    let sample = r#"
[AVFoundation indev @ 0x123] AVFoundation video devices:
[AVFoundation indev @ 0x123] [0] FaceTime HD Camera
[AVFoundation indev @ 0x123] [1] Capture screen 0
[AVFoundation indev @ 0x123] AVFoundation audio devices:
[AVFoundation indev @ 0x123] [0] Built-in Microphone
[AVFoundation indev @ 0x123] [1] BlackHole 2ch
"#;
    let devs = DeviceManager::parse_avfoundation_devices(sample).unwrap();
    assert_eq!(devs.len(), 4);

    let camera = &devs[0];
    assert_eq!(camera.device_type, DeviceType::Video);
    assert_eq!(camera.fps, Some(30));

    let screen = &devs[1];
    assert_eq!(screen.device_type, DeviceType::Video);
    assert_eq!(screen.fps, Some(60));

    let mic = &devs[2];
    assert_eq!(mic.device_type, DeviceType::AudioInput);
    assert!(!mic.is_loopback());

    let loopback = &devs[3];
    assert_eq!(loopback.device_type, DeviceType::AudioInput);
    assert!(loopback.is_loopback());

    assert!(DeviceManager::has_loopback_device(&devs));
    assert_eq!(DeviceManager::find_loopback_device(&devs).unwrap().name, "BlackHole 2ch");
}

#[test]
fn test_session_timestamp_structure() {
    let ts = Remuxer::generate_session_timestamp();
    // YYYYMMDD_HHMMSS
    assert_eq!(ts.len(), 15);
    let parts: Vec<&str> = ts.split('_').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].len(), 8);
    assert_eq!(parts[1].len(), 6);
}

#[test]
fn test_nle_sync_timecode_and_audio_config() {
    let mut cfg = Config::default();
    assert!(cfg.sync_audio_to_secondary);
    assert!(cfg.embed_timecode);

    // Verify timecode extraction from segment filenames
    let segs = vec![
        std::path::PathBuf::from("/tmp/gameplay_20261010_184530.mp4"),
        std::path::PathBuf::from("/tmp/gameplay_20261010_184532.mp4"),
    ];
    let tc = Remuxer::extract_start_timecode_from_segments(&segs);
    assert_eq!(tc, "18:45:30:00");

    // Toggle configuration
    cfg.sync_audio_to_secondary = false;
    cfg.embed_timecode = false;
    assert!(!cfg.sync_audio_to_secondary);
    assert!(!cfg.embed_timecode);
}

#[test]
fn test_input_format_parsing_and_fps_constraints() {
    // Test resolution and fps extraction from hardware probes
    let raw_1 = "1552x1552  30fps";
    let mut res_1 = "1920x1080".to_string();
    let mut fps_1 = 60u32;
    for part in raw_1.split_whitespace() {
        if part.contains('x') {
            res_1 = part.to_string();
        } else if let Some(stripped) = part.strip_suffix("fps") {
            if let Ok(f) = stripped.parse::<u32>() {
                fps_1 = f;
            }
        }
    }
    assert_eq!(res_1, "1552x1552");
    assert_eq!(fps_1, 30);

    // If input fps is 30, output is clamped to <= 30
    let requested_fps = 60u32;
    let clamped_fps = requested_fps.min(fps_1);
    assert_eq!(clamped_fps, 30);

    let raw_2 = "3840x2160  60fps";
    let mut res_2 = "1920x1080".to_string();
    let mut fps_2 = 30u32;
    for part in raw_2.split_whitespace() {
        if part.contains('x') {
            res_2 = part.to_string();
        } else if let Some(stripped) = part.strip_suffix("fps") {
            if let Ok(f) = stripped.parse::<u32>() {
                fps_2 = f;
            }
        }
    }
    assert_eq!(res_2, "3840x2160");
    assert_eq!(fps_2, 60);
    assert_eq!(60u32.min(fps_2), 60);
}

#[test]
fn test_cross_platform_hal_command_generation() {
    use multirecording::platform::macos::MacOSPlatform;
    use multirecording::platform::windows::WindowsPlatform;
    use multirecording::platform::CapturePlatform;

    let mac: &dyn CapturePlatform = &MacOSPlatform;
    let win: &dyn CapturePlatform = &WindowsPlatform;

    // Preview stream commands
    let mac_prev = mac.build_preview_input_args("0");
    assert!(mac_prev.contains(&"avfoundation".to_string()));
    assert!(mac_prev.contains(&"0:none".to_string()));

    let win_desk_prev = win.build_preview_input_args("desktop");
    assert!(win_desk_prev.contains(&"gdigrab".to_string()));
    assert!(win_desk_prev.contains(&"desktop".to_string()));
    assert!(win_desk_prev.contains(&"-an".to_string()));

    let win_cam_prev = win.build_preview_input_args("Integrated Camera");
    assert!(win_cam_prev.contains(&"dshow".to_string()));
    assert!(win_cam_prev.contains(&"video=Integrated Camera".to_string()));
    assert!(win_cam_prev.contains(&"-an".to_string()));

    // Recording input commands with discrete audio routing
    let tracks = vec![
        AudioTrackConfig {
            enabled: true,
            name: "Microphone".to_string(),
            device_index: 0,
            device_id: "in:0".to_string(),
        },
        AudioTrackConfig {
            enabled: true,
            name: "Desktop Audio".to_string(),
            device_index: 1,
            device_id: "wasapi".to_string(),
        },
    ];

    let mac_rec = mac.build_recording_input_args("0", 60, &tracks);
    assert_eq!(mac.audio_input_index(0), 0);
    assert_eq!(mac.audio_input_index(1), 1);
    assert!(mac_rec.contains(&"0:0".to_string()));
    assert!(mac_rec.contains(&"none:1".to_string()));

    let win_rec = win.build_recording_input_args("desktop", 60, &tracks);
    assert_eq!(win.audio_input_index(0), 1);
    assert_eq!(win.audio_input_index(1), 2);
    assert!(win_rec.contains(&"desktop".to_string()));
    assert!(win_rec.contains(&"wasapi".to_string()));
    assert!(win_rec.contains(&"default".to_string()));

    // Audio VU meter monitor commands
    let mac_mon = mac.build_audio_monitor_args("0");
    assert!(mac_mon.contains(&"none:0".to_string()));
    assert!(mac_mon.contains(&"f32le".to_string()));

    let win_wasapi_mon = win.build_audio_monitor_args("wasapi");
    assert!(win_wasapi_mon.contains(&"wasapi".to_string()));
    assert!(win_wasapi_mon.contains(&"default".to_string()));

    let win_mic_mon = win.build_audio_monitor_args("Microphone Array");
    assert!(win_mic_mon.contains(&"dshow".to_string()));
    assert!(win_mic_mon.contains(&"audio=Microphone Array".to_string()));
}

#[test]
fn test_audio_track_monitor_lifecycle() {
    use multirecording::recorder::AudioTrackMonitor;

    let mut monitor = AudioTrackMonitor::new("wasapi");
    assert_eq!(monitor.device_identifier(), "wasapi");
    let peak = monitor.get_peak();
    assert!(peak >= 0.0 && peak <= 1.0);
    monitor.stop();

    #[cfg(target_os = "windows")]
    {
        let mut mic_mon = AudioTrackMonitor::new("Microphone (Razer BlackShark V2 X USB)");
        let _ = mic_mon.get_peak();
        mic_mon.stop();

        let mut out_mon = AudioTrackMonitor::new("ED270R M (NVIDIA High Definition Audio)");
        let _ = out_mon.get_peak();
        out_mon.stop();
    }
}

#[test]
fn test_recording_modes() {
    use multirecording::ui::RecordingMode;

    let mode_fb = RecordingMode::Flashback;
    let mode_cont = RecordingMode::Continuous;

    assert_ne!(mode_fb, mode_cont);
    assert_eq!(mode_fb, RecordingMode::Flashback);
    assert_eq!(mode_cont, RecordingMode::Continuous);
}

#[test]
fn test_cross_platform_config_audio_track_migration() {
    use multirecording::config::Config;

    let mut cfg = Config::default();
    cfg.audio_tracks[0].enabled = true;
    cfg.audio_tracks[0].name = "Taven’s AirPods Pro".to_string();
    cfg.audio_tracks[0].device_id = "in:0".to_string();

    // Verify Windows argument generation never chokes on in:0 with "audio=0"
    let win = multirecording::platform::windows::WindowsPlatform;
    let win_args = win.build_recording_input_args("desktop", 30, &cfg.audio_tracks);
    assert!(!win_args.contains(&"audio=0".to_string()));
}


