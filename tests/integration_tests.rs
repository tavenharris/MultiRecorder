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

    let _ = fs::remove_dir_all(&temp_dir);
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
