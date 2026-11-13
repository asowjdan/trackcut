//! 내보낸 파일이 편집한 그대로인지 확인한다. ffmpeg가 없으면 건너뛴다.
//! 프레임 번호를 밝기로 새긴 영상을 편집해 내보낸 뒤, 결과의 각 프레임이 원본의 몇 번째인지 되읽는다.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use trackcut_core::decode;
use trackcut_core::export::{export, AudioMode, ExportOptions, Quality, VideoEncoder};
use trackcut_core::labels::TracksMeta;
use trackcut_core::probe::{probe, run_tool};
use trackcut_core::project::Project;
use trackcut_core::time::{frame_time, to_samples, Rational};

const FPS: Rational = Rational::new(60, 1);

fn make_source(path: &Path) {
    let video = "nullsrc=s=64x64:r=60:d=2,geq=lum='20+2*N':cb=128:cr=128";
    let mut args: Vec<String> = ["-v", "error", "-y", "-f", "lavfi", "-i", video].map(String::from).to_vec();
    for freq in [440, 880] {
        args.extend(["-f".into(), "lavfi".into(), "-i".into(), format!("sine=f={freq}:r=48000:d=2")]);
    }
    args.extend("-map 0:v -map 1:a -map 2:a -c:v libx264 -g 40 -bf 2 -pix_fmt yuv420p -c:a aac".split(' ').map(String::from));
    args.push(path.to_string_lossy().into_owned());
    run_tool("ffmpeg", args).expect("샘플 파일 생성");
}

/// 내보낸 파일의 `frame`번 프레임이 원본의 몇 번 프레임인지(밝기에서 계산).
fn source_frame_of(decoder: &decode::Decoder, frame: u64) -> i64 {
    let image = decoder.get(frame, Duration::from_secs(20)).expect("프레임 디코딩");
    let gray = image.rgb[(32 * 64 + 32) * 3] as f64;
    ((gray * 219.0 / 255.0 + 16.0 - 20.0) / 2.0).round() as i64
}

fn options(output: &Path, audio: AudioMode) -> ExportOptions {
    ExportOptions { audio, quality: Quality::High, encoder: Some(VideoEncoder::X264), ..ExportOptions::new(output.to_path_buf()) }
}

#[test]
fn exported_file_matches_the_edit_frame_for_frame() {
    if run_tool("ffmpeg", ["-version"]).is_err() {
        eprintln!("ffmpeg가 없어 건너뜀");
        return;
    }
    let dir = std::env::temp_dir().join(format!("trackcut-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("원본.mp4");
    make_source(&source);
    let source_bytes = std::fs::read(&source).unwrap();
    let cancel = AtomicBool::new(false);

    let labels = TracksMeta::new(2, &["게임".into(), "마이크".into()], &[]);
    let mut project = Project::from_media(probe(&source).unwrap(), Some(&labels));
    // 30~60 프레임을 지워 당기고, 90~100 프레임은 자리를 비운다(검은 화면·무음)
    project.delete_range(frame_time(30, FPS), frame_time(60, FPS), None, true).unwrap();
    project.delete_range(frame_time(60, FPS), frame_time(70, FPS), None, false).unwrap();
    assert_eq!(project.duration(), frame_time(90, FPS));

    // 섞어서 내보내기
    let mixed = dir.join("섞음.mp4");
    let mut last_progress = 0.0;
    let outputs = export(&project, &options(&mixed, AudioMode::Mix), &cancel, |p| last_progress = p).unwrap();
    assert_eq!(outputs, vec![mixed.clone()]);
    assert!(last_progress > 0.5, "진행률이 보고되지 않음: {last_progress}");
    let (info, decoder) = decode::open(&mixed, 64).unwrap();
    assert_eq!(info.video.as_ref().unwrap().grid.count(), 90);
    // (밝기가 영상 범위 상한 235를 넘지 않는 107번 프레임까지만 번호를 되읽을 수 있다)
    for (output_frame, expected) in [(0, 0), (29, 29), (30, 60), (59, 89), (70, 100), (77, 107)] {
        assert_eq!(source_frame_of(&decoder, output_frame), expected, "출력 프레임 {output_frame}");
    }
    // 비워 둔 구간은 검은 화면
    assert!(decoder.get(65, Duration::from_secs(20)).unwrap().rgb[(32 * 64 + 32) * 3] < 8);
    assert_eq!(info.audio.len(), 1);
    let samples = to_samples(info.audio[0].duration, 48_000);
    assert!((samples - 90 * 800).abs() <= 2048, "오디오 길이 {samples}샘플");

    // 다중 트랙 내보내기: 트랙 수와 이름이 유지된다
    let tracks = dir.join("트랙.mp4");
    export(&project, &options(&tracks, AudioMode::Tracks), &cancel, |_| {}).unwrap();
    let info = probe(&tracks).unwrap();
    let titles: Vec<_> = info.audio.iter().map(|a| a.title.clone().unwrap_or_default()).collect();
    assert_eq!(titles, ["게임", "마이크"]);

    // 트랙별 WAV
    let wavs = export(&project, &options(&dir.join("소리.wav"), AudioMode::WavPerTrack), &cancel, |_| {}).unwrap();
    assert_eq!(wavs.len(), 2);
    assert!(wavs[1].file_name().unwrap().to_string_lossy().ends_with(".2.마이크.wav"));
    let wav = probe(&wavs[0]).unwrap();
    assert_eq!(to_samples(wav.audio[0].duration, 48_000), 90 * 800);

    // 기존 파일과 원본은 덮어쓰지 않는다
    assert!(export(&project, &options(&mixed, AudioMode::Mix), &cancel, |_| {}).is_err());
    assert!(export(&project, &options(&source, AudioMode::Mix), &cancel, |_| {}).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);

    // 취소하면 출력 파일을 남기지 않는다
    let cancelled = dir.join("취소.mp4");
    assert!(export(&project, &options(&cancelled, AudioMode::Mix), &AtomicBool::new(true), |_| {}).is_err());
    assert!(!cancelled.exists());

    drop(decoder);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn export_writes_the_edit_side_when_it_has_clips() {
    if run_tool("ffmpeg", ["-version"]).is_err() {
        eprintln!("ffmpeg가 없어 건너뜀");
        return;
    }
    let dir = std::env::temp_dir().join(format!("trackcut-export-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("원본.mp4");
    make_source(&source);
    let cancel = AtomicBool::new(false);

    let mut project = Project::from_media(probe(&source).unwrap(), None);
    // 원본의 40~60 프레임과 10~20 프레임을 차례로 편집본에 올린다
    project.copy_to_edit(frame_time(40, FPS), frame_time(60, FPS), 0).unwrap();
    project.copy_to_edit(frame_time(10, FPS), frame_time(20, FPS), frame_time(20, FPS)).unwrap();

    let output = dir.join("편집본.mp4");
    export(&project, &options(&output, AudioMode::Mix), &cancel, |_| {}).unwrap();
    let (info, decoder) = decode::open(&output, 64).unwrap();
    assert_eq!(info.video.as_ref().unwrap().grid.count(), 30, "원본 길이가 아니라 편집본 길이로 나온다");
    for (output_frame, expected) in [(0, 40), (19, 59), (20, 10), (29, 19)] {
        assert_eq!(source_frame_of(&decoder, output_frame), expected, "출력 프레임 {output_frame}");
    }
    assert_eq!(info.audio.len(), 1);
    drop(decoder);
    let _ = std::fs::remove_dir_all(&dir);
}
