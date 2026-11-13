use trackcut_core::edit::Edge;
use trackcut_core::history::History;
use trackcut_core::labels::TracksMeta;
use trackcut_core::media::{AudioStream, MediaInfo, VideoStream};
use trackcut_core::project::{Crop, Project, TrackKind};
use trackcut_core::time::{frame_time, to_samples, Flicks, FrameGrid, Rational};

const FPS: Rational = Rational::new(60, 1);

fn f(n: i64) -> Flicks {
    frame_time(n, FPS)
}

fn audio(i: u32, start: Flicks, duration: Flicks) -> AudioStream {
    AudioStream { index: i + 1, audio_index: i, codec: "aac".into(), sample_rate: 48_000, channels: 2, title: None, start, duration }
}

/// 60fps 영상 `frames`프레임과 같은 길이의 오디오 `tracks`개.
fn media(frames: u64, tracks: u32) -> MediaInfo {
    MediaInfo {
        path: "recording.mp4".into(),
        duration: f(frames as i64),
        video: Some(VideoStream {
            index: 0,
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            fps: FPS,
            grid: FrameGrid::Cfr { start: 0, fps: FPS, count: frames },
            keyframes: vec![0],
        }),
        audio: (0..tracks).map(|i| audio(i, 0, f(frames as i64))).collect(),
    }
}

fn clips_on(p: &Project, track_number: usize) -> Vec<(u64, Flicks, Flicks)> {
    let track = p.timeline.tracks[track_number].id;
    let mut v: Vec<_> = p.timeline.clips.iter().filter(|c| c.track == track).map(|c| (c.id, c.start, c.end())).collect();
    v.sort_by_key(|c| c.1);
    v
}

#[test]
fn import_makes_one_linked_clip_per_stream() {
    let labels = TracksMeta::new(3, &["게임".into(), "마이크".into()], &[3]);
    let p = Project::from_media(media(600, 3), Some(&labels));
    assert_eq!(p.timeline.tracks.len(), 4);
    assert_eq!(p.timeline.tracks[0].kind, TrackKind::Video);
    assert_eq!(p.timeline.tracks[1].name, "게임");
    assert_eq!(p.timeline.tracks[3].name, "오디오 3");
    assert!(p.timeline.tracks[3].muted);
    assert_eq!(p.timeline.clips.len(), 4);
    let link = p.timeline.clips[0].link;
    assert!(link.is_some() && p.timeline.clips.iter().all(|c| c.link == link));
    assert_eq!(p.duration(), f(600));
}

#[test]
fn import_compensates_stream_start_offsets() {
    let mut info = media(600, 2);
    info.audio[0].start = f(1); // 영상보다 1프레임 늦게 시작
    info.audio[1].start = -f(2); // 영상보다 2프레임 먼저 시작
    let p = Project::from_media(info, None);
    assert_eq!(clips_on(&p, 1)[0].1, f(1));
    let early = p.timeline.clips.iter().find(|c| c.stream == 1 && c.track == p.timeline.tracks[2].id).unwrap();
    assert_eq!((early.start, early.src_in), (0, 0));
}

#[test]
fn split_cuts_every_linked_track_on_the_same_frame() {
    let mut p = Project::from_media(media(600, 2), None);
    let created = p.split_at(f(100), None).unwrap();
    assert_eq!(created.len(), 3);
    for track in 0..3 {
        let clips = clips_on(&p, track);
        assert_eq!(clips.iter().map(|c| (c.1, c.2)).collect::<Vec<_>>(), vec![(0, f(100)), (f(100), f(600))]);
    }
    // 오른쪽 조각끼리 새 묶음으로 연결된다
    let right_link = p.clip(created[0]).unwrap().link;
    assert_ne!(right_link, p.timeline.clips[0].link);
    assert!(created.iter().all(|&id| p.clip(id).unwrap().link == right_link));
    // 오디오 편집점은 프레임당 800샘플 경계
    assert_eq!(to_samples(p.clip(created[1]).unwrap().src_in, 48_000), 100 * 800);
}

#[test]
fn split_outside_clip_is_rejected() {
    let mut p = Project::from_media(media(600, 1), None);
    let id = p.timeline.clips[0].id;
    assert!(p.split_at(0, Some(id)).is_err());
    assert!(p.split_at(f(600), None).unwrap().is_empty());
}

#[test]
fn ripple_delete_closes_the_gap_on_all_tracks() {
    let mut p = Project::from_media(media(600, 2), None);
    p.delete_range(f(100), f(160), None, true).unwrap();
    assert_eq!(p.duration(), f(540));
    for track in 0..3 {
        let clips = clips_on(&p, track);
        assert_eq!(clips.len(), 2);
        assert_eq!((clips[1].1, clips[1].2), (f(100), f(540)));
        assert_eq!(p.clip(clips[1].0).unwrap().src_in, f(160));
    }
}

#[test]
fn delete_without_ripple_leaves_a_gap() {
    let mut p = Project::from_media(media(600, 1), None);
    p.delete_range(f(100), f(160), None, false).unwrap();
    assert_eq!(p.duration(), f(600));
    assert_eq!(clips_on(&p, 0)[1].1, f(160));
}

#[test]
fn delete_covers_whole_and_partial_clips() {
    let mut p = Project::from_media(media(600, 0), None);
    p.split_at(f(100), None).unwrap();
    p.split_at(f(200), None).unwrap();
    // 가운데 클립 전체 + 양옆 클립 일부
    p.delete_range(f(50), f(250), None, true).unwrap();
    let clips = clips_on(&p, 0);
    assert_eq!(clips.iter().map(|c| (c.1, c.2)).collect::<Vec<_>>(), vec![(0, f(50)), (f(50), f(400))]);
    assert!(p.delete_range(f(10), f(10), None, true).is_err());
}

#[test]
fn delete_on_one_track_keeps_video_and_unlinks() {
    let mut p = Project::from_media(media(600, 2), None);
    let mic = p.timeline.tracks[2].id;
    p.delete_range(f(100), f(160), Some(&[mic]), false).unwrap();
    assert_eq!(clips_on(&p, 0).len(), 1);
    assert_eq!(clips_on(&p, 1).len(), 1);
    let mic_clips = clips_on(&p, 2);
    assert_eq!(mic_clips.len(), 2);
    assert!(mic_clips.iter().all(|c| p.clip(c.0).unwrap().link.is_none()));
}

#[test]
fn trim_moves_linked_edges_and_respects_source_length() {
    let mut p = Project::from_media(media(600, 1), None);
    let video = p.timeline.clips[0].id;
    assert_eq!(p.trim(video, Edge::End, f(500)).unwrap(), f(500));
    assert_eq!(p.duration(), f(500));
    // 원본보다 길게는 늘릴 수 없다
    assert_eq!(p.trim(video, Edge::End, f(900)).unwrap(), f(600));
    assert_eq!(p.trim(video, Edge::Start, f(30)).unwrap(), f(30));
    assert!(p.timeline.clips.iter().all(|c| c.start == f(30) && c.src_in == f(30)));
    assert_eq!(p.trim(video, Edge::Start, -f(10)).unwrap(), 0);
}

#[test]
fn trim_stops_at_neighbouring_clip() {
    let mut p = Project::from_media(media(600, 0), None);
    let right = p.split_at(f(300), None).unwrap()[0];
    p.trim(right, Edge::Start, f(400)).unwrap();
    assert_eq!(p.trim(right, Edge::Start, f(100)).unwrap(), f(300));
}

#[test]
fn unlinked_audio_moves_alone_but_cannot_overlap() {
    let mut p = Project::from_media(media(600, 1), None);
    let created = p.split_at(f(300), None).unwrap();
    let audio_right = created[1];
    p.unlink(audio_right).unwrap();
    assert!(p.move_clip(audio_right, f(100)).is_err());
    p.move_clip(audio_right, f(700)).unwrap();
    assert_eq!(p.clip(audio_right).unwrap().start, f(700));
    assert_eq!(p.clip(created[0]).unwrap().start, f(300));
    // 연결된 클립은 함께 움직인다
    p.link(&[created[0], audio_right]).unwrap();
    p.move_clip(created[0], f(310)).unwrap();
    assert_eq!(p.clip(audio_right).unwrap().start, f(710));
}

#[test]
fn volume_mute_solo() {
    let mut p = Project::from_media(media(60, 2), None);
    let (a, b) = (p.timeline.tracks[1].id, p.timeline.tracks[2].id);
    assert_eq!(p.set_volume(a, 40.0).unwrap(), 12.0);
    assert_eq!(p.set_volume(a, -100.0).unwrap(), -60.0);
    assert!(p.set_volume(a, f32::NAN).is_err());
    p.set_muted(a, true).unwrap();
    assert!(!p.is_audible(a) && p.is_audible(b));
    p.set_solo(a, true).unwrap();
    assert!(p.is_audible(a) && !p.is_audible(b));
    assert!(p.rename_track(a, "  ").is_err());
}

#[test]
fn undo_and_redo_restore_the_timeline() {
    let mut p = Project::from_media(media(600, 2), None);
    let mut h = History::default();
    let original = p.timeline.clone();
    h.apply(&mut p, "분할", |p| p.split_at(f(100), None)).unwrap();
    h.apply(&mut p, "삭제", |p| p.delete_range(f(200), f(260), None, true)).unwrap();
    let edited = p.timeline.clone();
    // 실패한 편집은 기록되지 않고 상태도 그대로다
    assert!(h.apply(&mut p, "잘못된 삭제", |p| p.delete_range(f(5), f(5), None, true)).is_err());
    assert_eq!(p.timeline, edited);

    assert_eq!(h.undo(&mut p).as_deref(), Some("삭제"));
    assert_eq!(h.undo(&mut p).as_deref(), Some("분할"));
    assert_eq!(p.timeline, original);
    assert!(h.undo(&mut p).is_none());
    h.redo(&mut p);
    h.redo(&mut p);
    assert_eq!(p.timeline, edited);
    assert!(!h.can_redo());
}

#[test]
fn snap_follows_source_frames_even_after_edits() {
    let mut info = media(4, 0);
    info.video.as_mut().unwrap().grid = FrameGrid::Vfr { pts: vec![0, 100, 350, 400], end: 500 };
    let mut p = Project::from_media(info, None);
    assert_eq!(p.snap(240), 350);
    // 앞 100을 지우면 원본 350 프레임은 타임라인 250에 온다
    p.delete_range(0, 100, None, true).unwrap();
    assert_eq!(p.snap(240), 250);
    // 영상이 없는 위치는 프로젝트 fps 격자
    assert_eq!(p.snap(f(1000) + 7), f(1000));
}

#[test]
fn save_roundtrips_and_never_overwrites_the_source() {
    let dir = std::env::temp_dir().join(format!("trackcut-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("recording.mp4");
    std::fs::write(&source, b"not really a video").unwrap();
    let mut info = media(600, 2);
    info.path = source.to_string_lossy().into_owned();
    let mut p = Project::from_media(info, None);
    p.split_at(f(100), None).unwrap();

    assert!(p.save(&source).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), b"not really a video");

    let file = dir.join("edit.trackcut.json");
    p.save(&file).unwrap();
    assert_eq!(Project::load(&file).unwrap(), p);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn spans(p: &Project, track_number: usize) -> Vec<(Flicks, Flicks)> {
    clips_on(p, track_number).iter().map(|c| (c.1, c.2)).collect()
}

#[test]
fn speed_changes_length_and_ripples_later_clips() {
    let mut p = Project::from_media(media(600, 1), None);
    p.split_at(f(200), None).unwrap();
    let first = clips_on(&p, 0)[0].0;
    p.set_speed(first, 200).unwrap();
    assert_eq!(spans(&p, 0), [(0, f(100)), (f(100), f(500))]);
    assert_eq!(spans(&p, 1), [(0, f(100)), (f(100), f(500))], "연결된 오디오도 같은 속도가 된다");
    // 빨라진 클립을 나누면 원본 위치는 두 배로 간다
    let right = p.split_at(f(50), Some(first)).unwrap();
    assert_eq!(p.clip(right[0]).unwrap().src_in, f(100));
    assert_eq!(spans(&p, 0), [(0, f(50)), (f(50), f(100)), (f(100), f(500))]);
    assert!(p.set_speed(first, 10).is_err());
}

#[test]
fn slowed_clip_can_be_trimmed_back_to_its_full_source() {
    let mut p = Project::from_media(media(100, 1), None);
    let clip = clips_on(&p, 0)[0].0;
    p.set_speed(clip, 50).unwrap();
    assert_eq!(spans(&p, 0), [(0, f(200))]);
    assert_eq!(p.trim(clip, Edge::End, f(120)).unwrap(), f(120));
    assert_eq!(p.clip(clip).unwrap().src_out, f(60));
    assert_eq!(p.trim(clip, Edge::End, f(999)).unwrap(), f(200), "원본 길이를 넘지 않는다");
}

#[test]
fn crop_only_applies_to_video_clips_and_keeps_a_minimum_size() {
    let mut p = Project::from_media(media(60, 1), None);
    let (video, audio) = (clips_on(&p, 0)[0].0, clips_on(&p, 1)[0].0);
    let crop = Crop { left: 100, top: 50, right: 100, bottom: 50 };
    p.set_crop(video, crop).unwrap();
    assert_eq!(p.clip(video).unwrap().crop, crop);
    assert!(p.set_crop(audio, crop).is_err());
    assert!(p.set_crop(video, Crop { left: 1000, right: 915, ..Crop::default() }).is_err());
    // 나눈 조각도 같은 자르기를 이어받는다
    let right = p.split_at(f(30), Some(video)).unwrap();
    assert!(right.iter().any(|&id| p.clip(id).unwrap().crop == crop));
}

#[test]
fn appended_file_goes_to_the_end_on_the_same_tracks() {
    let mut p = Project::from_media(media(600, 2), None);
    p.append_media(media(300, 1), None);
    assert_eq!(p.timeline.tracks.len(), 3, "새 트랙을 만들지 않는다");
    assert_eq!(spans(&p, 0), [(0, f(600)), (f(600), f(900))]);
    assert_eq!(spans(&p, 1), [(0, f(600)), (f(600), f(900))]);
    assert_eq!(spans(&p, 2), [(0, f(600))]);
    assert_eq!(p.media.len(), 2);
    // 오디오 트랙이 더 많은 파일을 붙이면 모자라는 트랙만 생긴다
    p.append_media(media(60, 3), None);
    assert_eq!(p.timeline.tracks.len(), 4);
    assert_eq!(spans(&p, 3), [(f(900), f(960))]);
}

#[test]
fn keyed_edits_merge_into_one_undo_step() {
    let mut p = Project::from_media(media(600, 1), None);
    let mut history = History::default();
    let track = p.timeline.tracks[1].id;
    for db in [-1.0, -2.0, -3.0] {
        history.apply_keyed(&mut p, "볼륨", "volume", |p| p.set_volume(track, db).map(|_| ())).unwrap();
    }
    assert_eq!(p.timeline.tracks[1].volume_db, -3.0);
    history.undo(&mut p);
    assert_eq!(p.timeline.tracks[1].volume_db, 0.0);
    assert!(!history.can_undo());
}

#[test]
fn swapping_neighbors_reorders_linked_pieces_together() {
    let mut p = Project::from_media(media(600, 1), None);
    p.split_at(f(100), None).unwrap();
    let first = clips_on(&p, 0)[0].0;
    p.swap_with_neighbor(first, true).unwrap();
    assert_eq!(spans(&p, 0), [(0, f(500)), (f(500), f(600))]);
    assert_eq!(spans(&p, 1), [(0, f(500)), (f(500), f(600))]);
    assert_eq!(p.clip(first).unwrap().start, f(500));
    assert!(p.swap_with_neighbor(first, true).is_err(), "맨 뒤에서는 더 뒤로 갈 수 없다");
    p.swap_with_neighbor(first, false).unwrap();
    assert_eq!(p.clip(first).unwrap().start, 0);
}

#[test]
fn processed_audio_goes_on_a_new_track_and_the_original_stays() {
    let mut p = Project::from_media(media(600, 1), None);
    p.split_at(f(100), None).unwrap();
    let mic = p.timeline.tracks[1].id;
    let mut fixed = media(600, 1);
    fixed.video = None;
    fixed.path = "recording.fixed.flac".into();
    let track = p.add_processed_track(mic, vec![(1, 0, fixed)]).unwrap();
    assert_eq!(p.timeline.tracks.len(), 3);
    assert_eq!(p.timeline.tracks[2].id, track);
    assert_eq!(p.timeline.tracks[2].name, "오디오 1 (보정)");
    assert!(p.timeline.tracks[1].muted && !p.timeline.tracks[2].muted);
    assert_eq!(spans(&p, 2), spans(&p, 1), "원본과 같은 자리에 놓인다");
    assert!(p.timeline.clips.iter().filter(|c| c.track == track).all(|c| c.media == 2 && c.stream == 0));
    assert_eq!(spans(&p, 1), [(0, f(100)), (f(100), f(600))], "원본 클립은 그대로다");
}

/// 두 파일을 이어 붙인 프로젝트(각 600프레임, 오디오 1개)와 원본 트랙들.
fn two_files() -> (Project, Vec<u64>) {
    let mut p = Project::from_media(media(600, 1), None);
    p.append_media(media(600, 1), None);
    let tracks = p.group_tracks(false);
    (p, tracks)
}

#[test]
fn region_split_across_two_files_makes_one_linked_section() {
    let (mut p, tracks) = two_files();
    let section = p.split_range(f(500), f(700), &tracks).unwrap();
    assert_eq!(spans(&p, 0), [(0, f(500)), (f(500), f(600)), (f(600), f(700)), (f(700), f(1200))]);
    assert_eq!(section.len(), 4, "영상 조각 2개와 오디오 조각 2개가 한 구간이 된다");
    let inside = clips_on(&p, 0)[1].0;
    let mut group = p.group(inside).unwrap();
    group.sort();
    let mut expected = section.clone();
    expected.sort();
    assert_eq!(group, expected);
    assert_eq!(p.group_span(inside).unwrap(), (f(500), f(700)));
    // 바깥 조각은 구간에 들어가지 않는다
    assert!(!group.contains(&clips_on(&p, 0)[0].0) && !group.contains(&clips_on(&p, 0)[3].0));
}

#[test]
fn linked_section_swaps_places_as_one_block() {
    let (mut p, tracks) = two_files();
    p.split_range(f(500), f(700), &tracks).unwrap();
    let inside = clips_on(&p, 0)[1].0;
    p.swap_with_neighbor(inside, true).unwrap();
    assert_eq!(p.group_span(inside).unwrap(), (f(1000), f(1200)));
    assert_eq!(spans(&p, 0), [(0, f(500)), (f(500), f(1000)), (f(1000), f(1100)), (f(1100), f(1200))]);
    assert_eq!(spans(&p, 1), spans(&p, 0));
}

#[test]
fn region_split_rejects_an_empty_region() {
    let (mut p, tracks) = two_files();
    assert!(p.split_range(f(700), f(500), &tracks).is_err());
    assert!(p.split_range(f(1300), f(1400), &tracks).is_err(), "클립이 없는 구간");
}

#[test]
fn copy_to_edit_puts_a_linked_copy_on_edit_tracks_and_keeps_the_source() {
    let (mut p, tracks) = two_files();
    let before = p.timeline.clips.clone();
    let copies = p.copy_to_edit(f(500), f(700), f(0)).unwrap();
    assert_eq!(copies.len(), 4);
    assert_eq!(&p.timeline.clips[..before.len()], &before[..], "원본 클립은 그대로다");
    assert_eq!(p.group_tracks(false), tracks);
    let edit = p.group_tracks(true);
    assert_eq!(edit.len(), 2, "원본 트랙마다 편집본 트랙이 하나씩 생긴다");
    assert_eq!(p.timeline.tracks[2].kind, TrackKind::Video);
    assert_eq!(p.timeline.tracks[2].edit_of, Some(tracks[0]));
    assert_eq!(p.timeline.tracks[3].edit_of, Some(tracks[1]));
    assert_eq!(spans(&p, 2), [(0, f(100)), (f(100), f(200))]);
    assert_eq!(spans(&p, 3), spans(&p, 2));
    let first = p.clip(clips_on(&p, 2)[0].0).unwrap();
    assert_eq!((first.src_in, first.src_out), (f(500), f(600)));
    assert_eq!(p.group_span(copies[0]).unwrap(), (0, f(200)), "복사한 조각은 한 묶음이다");
    assert!(p.has_edit());
    assert_eq!((p.group_duration(false), p.group_duration(true)), (f(1200), f(200)));
}

#[test]
fn copy_to_edit_goes_to_the_end_when_the_spot_is_taken() {
    let (mut p, _) = two_files();
    p.copy_to_edit(f(0), f(100), f(50)).unwrap();
    p.copy_to_edit(f(300), f(400), f(100)).unwrap();
    assert_eq!(spans(&p, 2), [(f(50), f(150)), (f(150), f(250))]);
    p.copy_to_edit(f(300), f(400), f(400)).unwrap();
    assert_eq!(spans(&p, 2), [(f(50), f(150)), (f(150), f(250)), (f(400), f(500))]);
    assert_eq!(p.group_tracks(true).len(), 2, "편집본 트랙을 다시 만들지 않는다");
    assert!(p.copy_to_edit(f(2000), f(2100), 0).is_err(), "원본에 내용이 없는 구간");
}

#[test]
fn edits_on_one_side_leave_the_other_side_alone() {
    let (mut p, tracks) = two_files();
    p.copy_to_edit(f(0), f(300), f(0)).unwrap();
    // 원본에서 구역을 지워도 편집본은 그대로다
    p.delete_range_in(f(100), f(200), &tracks, true).unwrap();
    assert_eq!(p.group_duration(false), f(1100));
    assert_eq!(spans(&p, 2), [(0, f(300))]);
    // 편집본 클립의 속도를 바꿔도 원본은 밀리지 않는다
    let copy = clips_on(&p, 2)[0].0;
    p.set_speed(copy, 200).unwrap();
    assert_eq!(spans(&p, 2), [(0, f(150))]);
    assert_eq!(p.group_duration(false), f(1100));
    // 새 파일은 원본 묶음의 트랙에 이어 붙는다
    p.append_media(media(60, 1), None);
    assert_eq!(p.group_tracks(false), tracks);
    assert_eq!(p.group_duration(false), f(1160));
}

#[test]
fn solo_only_silences_tracks_on_the_same_side() {
    let (mut p, tracks) = two_files();
    p.copy_to_edit(f(0), f(100), 0).unwrap();
    let edit_audio = p.group_tracks(true)[1];
    p.set_solo(tracks[1], true).unwrap();
    assert!(p.is_audible(edit_audio));
}

#[test]
fn project_file_without_edit_tracks_still_loads() {
    let p = Project::from_media(media(60, 1), None);
    let text = serde_json::to_string(&p).unwrap();
    assert!(!text.contains("edit_of"), "원본 트랙에는 편집본 표시를 쓰지 않는다");
    assert_eq!(serde_json::from_str::<Project>(&text).unwrap(), p);
}

#[test]
fn muting_a_clip_leaves_the_rest_of_the_track_on() {
    let mut p = Project::from_media(media(600, 1), None);
    p.split_at(f(100), None).unwrap();
    let pieces = clips_on(&p, 1);
    p.set_clip_muted(pieces[0].0, true).unwrap();
    assert!(p.clip(pieces[0].0).unwrap().muted);
    assert!(!p.clip(pieces[1].0).unwrap().muted);
    assert!(!p.timeline.tracks[1].muted, "트랙 전체 음소거는 건드리지 않는다");
    // 나눈 조각은 꺼진 상태를 이어받는다
    let right = p.split_at(f(50), Some(pieces[0].0)).unwrap();
    assert!(right.iter().all(|&id| p.clip(id).unwrap().muted || p.clip(id).unwrap().track != p.timeline.tracks[1].id));
}
