//! 오디오 캐시와 파형 계산을 실제 ffmpeg로 확인한다. ffmpeg가 없으면 건너뛴다.

use trackcut_core::audio::{prepare, FRAME_BYTES};
use trackcut_core::probe::{probe, run_tool};

#[test]
fn extracts_pcm_and_peaks_per_track() {
    if run_tool("ffmpeg", ["-version"]).is_err() {
        eprintln!("ffmpeg가 없어 건너뜀");
        return;
    }
    let dir = std::env::temp_dir().join(format!("trackcut-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("TRACKCUT_CACHE", dir.join("캐시"));
    let source = dir.join("소리.mp4");
    // 트랙 1: 1초 내내 큰 소리, 트랙 2: 앞 0.5초는 무음이고 뒤 0.5초만 소리
    let args = [
        "-v", "error", "-y",
        "-f", "lavfi", "-i", "sine=f=440:r=48000:d=1,volume=4",
        "-f", "lavfi", "-i", "sine=f=440:r=48000:d=1,volume=4,volume=enable='lt(t,0.5)':volume=0",
        "-map", "0:a", "-map", "1:a", "-c:a", "aac", "-b:a", "192k",
    ];
    let mut args: Vec<String> = args.map(String::from).to_vec();
    args.push(source.to_string_lossy().into_owned());
    run_tool("ffmpeg", args).expect("샘플 파일 생성");

    let info = probe(&source).unwrap();
    let loud = prepare(&info, 0).unwrap();
    let half = prepare(&info, 1).unwrap();
    assert_ne!(loud.pcm, half.pcm);
    for track in [&loud, &half] {
        assert!((track.frames as i64 - 48_000).abs() < 2_100, "프레임 수 {}", track.frames);
        assert_eq!(std::fs::metadata(&track.pcm).unwrap().len(), track.frames * FRAME_BYTES as u64);
        assert_eq!(track.peaks.len() as u64, track.frames.div_ceil(480));
    }
    // sine 기본 크기 1/8에 4배 = 0.5, 모노를 스테레오로 펼치면 −3dB → 피크 약 90
    assert!(loud.peaks[10..90].iter().all(|&p| (80..=100).contains(&p)), "{:?}", &loud.peaks[10..20]);
    assert!(half.peaks[5..40].iter().all(|&p| p < 4), "{:?}", &half.peaks[5..15]);
    assert!(half.peaks[60..90].iter().all(|&p| p > 70));

    // 두 번째 호출은 캐시를 그대로 쓴다
    let modified = std::fs::metadata(&loud.pcm).unwrap().modified().unwrap();
    let again = prepare(&info, 0).unwrap();
    assert_eq!(std::fs::metadata(&again.pcm).unwrap().modified().unwrap(), modified);
    assert_eq!(again.peaks, loud.peaks);
    std::fs::remove_dir_all(&dir).ok();
}
