//! 내보내기: 타임라인을 FFmpeg 필터 그래프로 바꿔 새 파일을 만든다.
//!
//! 지금은 전체를 다시 인코딩한다(고화질 설정). 손대지 않은 구간을 그대로 복사하는
//! 스마트 렌더는 아직 없다. 원본과 기존 파일은 절대 덮어쓰지 않는다.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{edit_err, Error, Result};
use crate::probe::{hide_console, tool_path};
use crate::project::{Clip, Project, Track, TrackKind};
use crate::time::{flicks_to_secs, to_samples, Flicks, FLICKS_PER_SEC};

pub const SAMPLE_RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    /// 들리는 트랙을 모두 섞어 스테레오 하나로(공유용)
    Mix,
    /// 트랙을 섞지 않고 각각 담은 다중 트랙 파일
    Tracks,
    /// 영상 없이 트랙마다 WAV 파일
    WavPerTrack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    High,
    Standard,
    Small,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEncoder {
    Nvenc,
    X264,
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// 출력 파일. WAV 내보내기에서는 `이름.트랙이름.wav`의 기준 경로로 쓴다.
    pub output: PathBuf,
    pub audio: AudioMode,
    pub quality: Quality,
    /// None이면 NVENC를 먼저 쓰고, 안 되면 x264로 바꾼다.
    pub encoder: Option<VideoEncoder>,
    /// 잘린 경계에 넣는 페이드 길이(ms). 0이면 넣지 않는다.
    pub fade_ms: u32,
}

impl ExportOptions {
    pub fn new(output: PathBuf) -> Self {
        ExportOptions { output, audio: AudioMode::Mix, quality: Quality::High, encoder: None, fade_ms: 5 }
    }
}

fn secs(t: Flicks) -> String {
    format!("{:.9}", flicks_to_secs(t.max(0)))
}

struct Graph {
    /// 입력으로 쓸 미디어 id(ffmpeg 입력 번호 순서)
    inputs: Vec<u64>,
    filters: Vec<String>,
    video: Option<String>,
    /// (출력 라벨, 트랙)
    tracks: Vec<(String, Track)>,
}

fn sorted_clips(project: &Project, track: u64) -> Vec<&Clip> {
    let mut clips: Vec<&Clip> = project.timeline.clips.iter().filter(|c| c.track == track).collect();
    clips.sort_by_key(|c| c.start);
    clips
}

fn build_graph(project: &Project, options: &ExportOptions) -> Result<Graph> {
    // 편집본 묶음에 올린 것이 있으면 편집본을, 없으면 원본 묶음을 내보낸다
    let edit = project.has_edit();
    let total = project.group_duration(edit);
    if total <= 0 {
        return edit_err("내보낼 내용이 없다");
    }
    let mut graph = Graph { inputs: Vec::new(), filters: Vec::new(), video: None, tracks: Vec::new() };
    let input_of = |media: u64, inputs: &mut Vec<u64>| match inputs.iter().position(|&m| m == media) {
        Some(i) => i,
        None => {
            inputs.push(media);
            inputs.len() - 1
        }
    };
    let (width, height, fps) = (project.settings.width, project.settings.height, project.settings.fps);
    let rate = format!("{}/{}", fps.num, fps.den);

    let video_track = project
        .timeline
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video && t.is_edit() == edit && project.timeline.clips.iter().any(|c| c.track == t.id));
    if let (Some(track), true) = (video_track, options.audio != AudioMode::WavPerTrack) {
        let mut labels = Vec::new();
        let mut cursor = 0;
        // 클립 사이 빈 구간은 검은 화면으로 채운다
        let gap = |from: Flicks, to: Flicks, filters: &mut Vec<String>, labels: &mut Vec<String>| {
            let label = format!("[v{}]", labels.len());
            let length = secs(to - from);
            filters.push(format!("color=c=black:s={width}x{height}:r={rate}:d={length},setsar=1,format=yuv420p{label}"));
            labels.push(label);
        };
        for clip in sorted_clips(project, track.id) {
            if clip.start > cursor {
                gap(cursor, clip.start, &mut graph.filters, &mut labels);
            }
            let info = project.media_info(clip.media).ok_or_else(|| Error::Edit("미디어 정보가 없다".into()))?;
            let video = info.video.as_ref().ok_or_else(|| Error::Edit("영상 스트림이 없다".into()))?;
            let input = input_of(clip.media, &mut graph.inputs);
            // 프레임 시각보다 반 프레임 앞에서 자르면 반올림 오차와 무관하게 정확한 프레임이 선택된다
            let first = video.grid.frame_at(clip.src_in);
            let half = (video.grid.time_of(first + 1) - video.grid.time_of(first)) / 2;
            let offset = info.file_start() + half;
            let label = format!("[v{}]", labels.len());
            graph.filters.push(format!(
                "[{input}:v:0]trim=start={}:end={},setpts=(PTS-STARTPTS)*100/{}{},scale={width}:{height}:force_original_aspect_ratio=decrease:flags=lanczos,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,fps={rate},setsar=1,format=yuv420p,trim=duration={}{label}",
                secs(clip.src_in - offset),
                secs(clip.src_out - offset),
                clip.speed_pct,
                crop_filter(clip, video.width, video.height),
                secs(clip.len()),
            ));
            labels.push(label);
            cursor = clip.end();
        }
        if cursor < total {
            gap(cursor, total, &mut graph.filters, &mut labels);
        }
        graph.video = Some(if labels.len() == 1 {
            labels.remove(0)
        } else {
            graph.filters.push(format!("{}concat=n={}:v=1:a=0[vout]", labels.concat(), labels.len()));
            "[vout]".into()
        });
    }

    let fade = options.fade_ms as i64 * FLICKS_PER_SEC / 1000;
    for track in project.timeline.tracks.iter().filter(|t| t.kind == TrackKind::Audio && t.is_edit() == edit) {
        if options.audio == AudioMode::Mix && !project.is_audible(track.id) {
            continue;
        }
        let number = graph.tracks.len();
        let mut pieces = Vec::new();
        for clip in sorted_clips(project, track.id).into_iter().filter(|c| !c.muted) {
            let info = project.media_info(clip.media).ok_or_else(|| Error::Edit("미디어 정보가 없다".into()))?;
            let input = input_of(clip.media, &mut graph.inputs);
            let offset = info.file_start();
            let mut chain = format!(
                "[{input}:a:{}]atrim=start={}:end={},asetpts=PTS-STARTPTS,aresample={SAMPLE_RATE},aformat=sample_fmts=fltp:channel_layouts=stereo",
                clip.stream,
                secs(clip.src_in - offset),
                secs(clip.src_out - offset),
            );
            // 속도를 바꾼 소리는 음높이도 함께 바뀐다(미리듣기와 같다)
            if clip.speed_pct != 100 {
                let rate = SAMPLE_RATE as u64 * clip.speed_pct as u64 / 100;
                chain += &format!(",asetrate={rate},aresample={SAMPLE_RATE},atrim=duration={}", secs(clip.len()));
            }
            // 잘린 경계에서 "딱" 소리가 나지 않게 짧은 페이드를 넣는다
            let (source_start, source_end) = project.source_bounds(clip);
            if fade > 0 && clip.len() > fade * 2 {
                if clip.src_in > source_start {
                    chain += &format!(",afade=t=in:d={}", secs(fade));
                }
                if clip.src_out < source_end {
                    chain += &format!(",afade=t=out:st={}:d={}", secs(clip.len() - fade), secs(fade));
                }
            }
            let delay = to_samples(clip.start, SAMPLE_RATE);
            if delay > 0 {
                chain += &format!(",adelay={delay}S:all=1");
            }
            let label = format!("[a{number}_{}]", pieces.len());
            graph.filters.push(chain + &label);
            pieces.push(label);
        }
        if pieces.is_empty() {
            continue;
        }
        let combine = match pieces.len() {
            1 => "anull".to_string(),
            n => format!("amix=inputs={n}:normalize=0:dropout_transition=0"),
        };
        let label = format!("[t{number}]");
        // apad: 트랙 길이를 타임라인 끝까지 맞춘다(출력의 -t가 끝을 자른다)
        graph.filters.push(format!("{}{combine},volume={:.2}dB,apad{label}", pieces.concat(), track.volume_db));
        graph.tracks.push((label, track.clone()));
    }

    if options.audio == AudioMode::Mix && graph.tracks.len() > 1 {
        let labels: String = graph.tracks.iter().map(|t| t.0.as_str()).collect();
        graph.filters.push(format!("{labels}amix=inputs={}:normalize=0:dropout_transition=0[aout]", graph.tracks.len()));
        graph.tracks = vec![("[aout]".into(), graph.tracks[0].1.clone())];
    }
    if graph.video.is_none() && graph.tracks.is_empty() {
        return edit_err("내보낼 영상이나 소리가 없다");
    }
    Ok(graph)
}

/// 화면 자르기 필터(자르지 않으면 빈 문자열).
fn crop_filter(clip: &Clip, width: u32, height: u32) -> String {
    let c = clip.crop;
    if c.is_none() {
        return String::new();
    }
    let w = width.saturating_sub(c.left + c.right).max(2);
    let h = height.saturating_sub(c.top + c.bottom).max(2);
    format!(",crop={w}:{h}:{}:{}", c.left, c.top)
}

fn safe_file_name(name: &str) -> String {
    name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect()
}

/// 내보내기로 만들어질 파일 경로.
fn output_paths(options: &ExportOptions, graph: &Graph) -> Vec<PathBuf> {
    if options.audio != AudioMode::WavPerTrack {
        return vec![options.output.clone()];
    }
    let stem = options.output.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    graph
        .tracks
        .iter()
        .enumerate()
        .map(|(i, (_, track))| options.output.with_file_name(format!("{stem}.{}.{}.wav", i + 1, safe_file_name(&track.name))))
        .collect()
}

fn build_args(project: &Project, options: &ExportOptions, graph: &Graph, encoder: VideoEncoder) -> Result<Vec<OsString>> {
    let mut args: Vec<OsString> = ["-v", "error", "-n", "-nostats", "-progress", "pipe:1"].map(Into::into).to_vec();
    for media in &graph.inputs {
        let info = project.media_info(*media).ok_or_else(|| Error::Edit("미디어 정보가 없다".into()))?;
        args.push("-i".into());
        args.push(info.path.clone().into());
    }
    args.push("-filter_complex".into());
    args.push(graph.filters.join(";").into());
    let total = secs(project.group_duration(project.has_edit()));
    let mut push = |items: &[&str]| args.extend(items.iter().map(|s| OsString::from(*s)));

    if options.audio == AudioMode::WavPerTrack {
        for ((label, _), path) in graph.tracks.iter().zip(output_paths(options, graph)) {
            push(&["-map", label, "-t", &total, "-c:a", "pcm_s24le", "-ar", "48000"]);
            push(&[&path.to_string_lossy()]);
        }
        return Ok(args);
    }
    if let Some(video) = &graph.video {
        push(&["-map", video]);
        let level = match (encoder, options.quality) {
            (VideoEncoder::Nvenc, Quality::High) => "17",
            (VideoEncoder::Nvenc, Quality::Standard) => "22",
            (VideoEncoder::Nvenc, Quality::Small) => "28",
            (VideoEncoder::X264, Quality::High) => "15",
            (VideoEncoder::X264, Quality::Standard) => "20",
            (VideoEncoder::X264, Quality::Small) => "26",
        };
        match encoder {
            VideoEncoder::Nvenc => push(&["-c:v", "h264_nvenc", "-preset", "p6", "-rc", "vbr", "-cq", level, "-b:v", "0"]),
            VideoEncoder::X264 => push(&["-c:v", "libx264", "-preset", "medium", "-crf", level]),
        }
        push(&["-pix_fmt", "yuv420p"]);
    }
    for (i, (label, track)) in graph.tracks.iter().enumerate() {
        push(&["-map", label]);
        if options.audio == AudioMode::Tracks {
            for key in ["title", "handler_name"] {
                push(&[&format!("-metadata:s:a:{i}"), &format!("{key}={}", track.name)]);
            }
        }
    }
    if !graph.tracks.is_empty() {
        push(&["-c:a", "aac", "-b:a", "320k", "-ar", "48000"]);
    }
    push(&["-t", &total, "-movflags", "+faststart"]);
    args.push(options.output.clone().into());
    Ok(args)
}

/// ffmpeg를 실행하며 진행률(0~1)을 알린다. `cancel`이 켜지면 중단한다.
fn run(args: &[OsString], total: Flicks, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Result<()> {
    let tool_err = |message: String| Error::Tool { tool: "ffmpeg".into(), message };
    let mut command = Command::new(tool_path("ffmpeg"));
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console(&mut command);
    let mut child = command.spawn().map_err(|e| tool_err(e.to_string()))?;
    let mut stderr = child.stderr.take().expect("stderr");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let total_us = flicks_to_secs(total) * 1e6;
    for line in BufReader::new(child.stdout.take().expect("stdout")).lines().map_while(|l| l.ok()) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            progress((us / total_us).clamp(0.0, 1.0) as f32);
        }
    }
    let status = child.wait().map_err(|e| tool_err(e.to_string()))?;
    let message = errors.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return edit_err("내보내기를 취소했다");
    }
    if !status.success() {
        return Err(tool_err(message.trim().to_string()));
    }
    Ok(())
}

/// 프로젝트를 내보내고 만들어진 파일 경로를 돌려준다.
pub fn export(
    project: &Project,
    options: &ExportOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<Vec<PathBuf>> {
    let graph = build_graph(project, options)?;
    let outputs = output_paths(options, &graph);
    if let Some(existing) = outputs.iter().find(|p| p.exists()) {
        return edit_err(format!("이미 있는 파일은 덮어쓰지 않는다: {}", existing.display()));
    }
    let encoders = match (options.encoder, options.audio) {
        (Some(encoder), _) => vec![encoder],
        (None, AudioMode::WavPerTrack) => vec![VideoEncoder::X264],
        (None, _) => vec![VideoEncoder::Nvenc, VideoEncoder::X264],
    };
    let mut last_error = None;
    for encoder in encoders {
        let args = build_args(project, options, &graph, encoder)?;
        match run(&args, project.group_duration(project.has_edit()), cancel, &mut progress) {
            Ok(()) => return Ok(outputs),
            Err(e) => {
                // 실패하거나 취소된 출력은 남기지 않는다
                outputs.iter().for_each(|p| remove_partial(p));
                if cancel.load(Ordering::Relaxed) {
                    return Err(e);
                }
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| Error::Edit("내보내기 실패".into())))
}

fn remove_partial(path: &Path) {
    let _ = std::fs::remove_file(path);
}
