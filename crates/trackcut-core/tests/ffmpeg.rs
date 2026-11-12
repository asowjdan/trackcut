//! 실제 ffmpeg / ffprobe로 만든 다중 트랙 파일로 확인한다. ffmpeg가 없으면 건너뛴다.

use std::path::Path;

use trackcut_core::labels::{self, TracksMeta};
use trackcut_core::probe::{probe, run_tool};
use trackcut_core::project::Project;
use trackcut_core::time::{frame_time, FrameGrid, Rational};

fn make_sample(path: &Path) {
    let mut args: Vec<String> = ["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x180:r=60:d=2"]
        .map(String::from)
        .to_vec();
    for freq in [440, 660, 880] {
        args.extend(["-f".into(), "lavfi".into(), "-i".into(), format!("sine=f={freq}:r=48000:d=2")]);
    }
    for map in ["0:v", "1:a", "2:a", "3:a"] {
        args.extend(["-map".into(), map.into()]);
    }
    let encode = "-c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p -c:a aac";
    args.extend(encode.split(' ').map(String::from));
    args.push(path.to_string_lossy().into_owned());
    run_tool("ffmpeg", args).expect("샘플 파일 생성");
}

#[test]
fn probe_tag_and_edit_a_real_multitrack_file() {
    if run_tool("ffmpeg", ["-version"]).is_err() || run_tool("ffprobe", ["-version"]).is_err() {
        eprintln!("ffmpeg가 없어 건너뜀");
        return;
    }
    let dir = std::env::temp_dir().join(format!("trackcut-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("샘플 녹화.mp4");
    make_sample(&source);
    let source_bytes = std::fs::read(&source).unwrap();

    // 분석: 60fps 고정, 120프레임, 30프레임마다 키프레임, 오디오 3트랙
    let fps = Rational::new(60, 1);
    let info = probe(&source).unwrap();
    let video = info.video.as_ref().unwrap();
    assert_eq!(video.grid, FrameGrid::Cfr { start: 0, fps, count: 120 });
    assert_eq!(video.keyframes, vec![0, 30, 60, 90]);
    assert_eq!(info.audio.len(), 3);
    assert!(info.audio.iter().all(|a| a.sample_rate == 48_000 && a.title.is_none()));

    // 이름표: 다시 인코딩하지 않은 새 파일, 원본은 그대로
    let tagged = dir.join("샘플 녹화.tagged.mp4");
    let meta = TracksMeta::new(3, &["게임".into(), "마이크".into()], &[3]);
    labels::tag(&source, &tagged, &meta).unwrap();
    meta.save_for(&tagged).unwrap();
    assert!(labels::tag(&source, &tagged, &meta).is_err(), "기존 파일을 덮어쓰면 안 된다");
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);

    let tagged_info = probe(&tagged).unwrap();
    let titles: Vec<_> = tagged_info.audio.iter().map(|a| a.title.clone().unwrap()).collect();
    assert_eq!(titles, ["게임", "마이크", "오디오 3"]);
    assert_eq!(tagged_info.video.as_ref().unwrap().grid, video.grid);

    // 프로젝트: 이름표대로 트랙 배치, 프레임 경계 편집, 저장 후 다시 열기
    let labels = TracksMeta::load_for(&tagged);
    let mut project = Project::from_media(tagged_info, labels.as_ref());
    assert_eq!(project.timeline.tracks[1].name, "게임");
    assert!(project.timeline.tracks[3].muted);
    let cut = project.snap(frame_time(45, fps) + 12_345);
    assert_eq!(cut, frame_time(45, fps));
    project.delete_range(cut, frame_time(75, fps), None, true).unwrap();
    assert_eq!(project.snap(project.duration()), frame_time(90, fps));

    let file = dir.join("샘플.trackcut.json");
    project.save(&file).unwrap();
    assert_eq!(Project::load(&file).unwrap(), project);
    std::fs::remove_dir_all(&dir).unwrap();
}
