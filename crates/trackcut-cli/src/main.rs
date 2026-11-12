//! trackcut 명령줄 도구: 코어 기능을 실제 녹화 파일로 확인하는 용도다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use trackcut_core::labels::{self, TracksMeta};
use trackcut_core::probe::probe;
use trackcut_core::project::{Project, TrackKind};
use trackcut_core::time::{flicks_to_secs, parse_time, timecode, Flicks};

const USAGE: &str = "\
사용법: trackcut <명령> [인자]

  probe <미디어> [--json]              트랙과 프레임 정보를 본다
  tag <미디어> [-o 출력] [--names 이름,이름,...] [--mute 번호,...]
                                       오디오 트랙에 이름표를 넣은 새 파일을 만든다
  new <미디어> [-o 프로젝트]           프로젝트 파일을 만든다
  show <프로젝트>                      트랙과 클립을 본다
  split <프로젝트> <시각>              그 시각에서 모든 클립을 나눈다
  delete <프로젝트> <시작> <끝> [--tracks 번호,...] [--keep-gap]
                                       구간을 지운다 (기본: 뒤쪽을 당겨 붙임)
  export <프로젝트> [-o 출력] [--audio mix|tracks|wav] [--quality high|standard|small]
                                       편집 결과를 새 파일로 내보낸다

시각: 90f(프레임), 00:12:03:59(시:분:초:프레임), 12.5(초). 프레임 경계에 자동으로 붙는다.";

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Args {
    positional: Vec<String>,
    options: HashMap<String, String>,
}

impl Args {
    /// `--이름 값`, `-o 값`, 값 없는 스위치(`--json`, `--keep-gap`)를 읽는다.
    fn parse(raw: Vec<String>) -> Self {
        const SWITCHES: [&str; 2] = ["--json", "--keep-gap"];
        let (mut positional, mut options) = (Vec::new(), HashMap::new());
        let mut iter = raw.into_iter();
        while let Some(arg) = iter.next() {
            if SWITCHES.contains(&arg.as_str()) {
                options.insert(arg, String::new());
            } else if arg.starts_with('-') && arg.len() > 1 && !arg[1..2].chars().all(|c| c.is_ascii_digit()) {
                let value = iter.next().unwrap_or_default();
                options.insert(arg, value);
            } else {
                positional.push(arg);
            }
        }
        Args { positional, options }
    }

    fn pos(&self, i: usize, what: &str) -> CliResult<&str> {
        self.positional.get(i).map(String::as_str).ok_or_else(|| format!("{what}을(를) 지정해야 한다").into())
    }

    fn has(&self, key: &str) -> bool {
        self.options.contains_key(key)
    }

    fn list(&self, key: &str) -> Vec<String> {
        self.options.get(key).map(|v| v.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default()
    }

    fn numbers(&self, key: &str) -> CliResult<Vec<u32>> {
        self.list(key)
            .iter()
            .map(|s| s.parse().map_err(|_| format!("{key}: 번호가 아니다: {s}").into()))
            .collect()
    }
}

fn main() {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw[0] == "--help" || raw[0] == "-h" {
        println!("{USAGE}");
        return;
    }
    let command = raw.remove(0);
    if let Err(e) = run(&command, Args::parse(raw)) {
        eprintln!("오류: {e}");
        std::process::exit(1);
    }
}

fn run(command: &str, args: Args) -> CliResult<()> {
    match command {
        "probe" => cmd_probe(&args),
        "tag" => cmd_tag(&args),
        "new" => cmd_new(&args),
        "show" => cmd_show(&Project::load(Path::new(args.pos(0, "프로젝트 파일")?))?),
        "split" => cmd_split(&args),
        "delete" => cmd_delete(&args),
        "export" => cmd_export(&args),
        _ => Err(format!("모르는 명령: {command}\n\n{USAGE}").into()),
    }
}

fn cmd_probe(args: &Args) -> CliResult<()> {
    let info = probe(Path::new(args.pos(0, "미디어 파일")?))?;
    if args.has("--json") {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    println!("길이 {:.3}초", flicks_to_secs(info.duration));
    if let Some(v) = &info.video {
        println!(
            "영상  {} {}x{} {:.3}fps, 프레임 {}개, 키프레임 {}개, {}",
            v.codec,
            v.width,
            v.height,
            v.fps.as_f64(),
            v.grid.count(),
            v.keyframes.len(),
            if v.grid.is_cfr() { "프레임 간격 일정" } else { "프레임 간격 불규칙(VFR)" }
        );
    }
    for a in &info.audio {
        println!(
            "오디오 {}  {} {}Hz {}ch, 시작 {:+.4}초, 이름표: {}",
            a.audio_index + 1,
            a.codec,
            a.sample_rate,
            a.channels,
            flicks_to_secs(a.start),
            a.title.as_deref().unwrap_or("(없음)")
        );
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}{suffix}"))
}

fn cmd_tag(args: &Args) -> CliResult<()> {
    let input = Path::new(args.pos(0, "미디어 파일")?);
    let info = probe(input)?;
    let meta = TracksMeta::new(info.audio.len(), &args.list("--names"), &args.numbers("--mute")?);
    let ext = input.extension().map(|e| format!(".tagged.{}", e.to_string_lossy())).unwrap_or(".tagged".into());
    let output = args.options.get("-o").map_or_else(|| with_suffix(input, &ext), PathBuf::from);
    labels::tag(input, &output, &meta)?;
    let sidecar = meta.save_for(&output)?;
    println!("이름표를 넣은 파일: {}", output.display());
    println!("설명 파일: {}", sidecar.display());
    Ok(())
}

fn cmd_new(args: &Args) -> CliResult<()> {
    let input = Path::new(args.pos(0, "미디어 파일")?);
    let info = probe(input)?;
    let project = Project::from_media(info, TracksMeta::load_for(input).as_ref());
    let output = args.options.get("-o").map_or_else(|| with_suffix(input, ".trackcut.json"), PathBuf::from);
    project.save(&output)?;
    println!("프로젝트 파일: {}", output.display());
    cmd_show(&project)
}

fn cmd_show(project: &Project) -> CliResult<()> {
    let fps = project.settings.fps;
    println!(
        "{}x{} {:.3}fps, 길이 {}",
        project.settings.width,
        project.settings.height,
        fps.as_f64(),
        timecode(project.duration(), fps)
    );
    for (number, track) in project.timeline.tracks.iter().enumerate() {
        let kind = if track.kind == TrackKind::Video { "영상" } else { "소리" };
        let state = if track.muted { ", 음소거" } else { "" };
        println!("[{}] {kind} \"{}\" {:+.1}dB{state}", number + 1, track.name, track.volume_db);
        let mut clips: Vec<_> = project.timeline.clips.iter().filter(|c| c.track == track.id).collect();
        clips.sort_by_key(|c| c.start);
        for c in clips {
            let link = c.link.map_or("연결 없음".to_string(), |l| format!("연결 {l}"));
            println!("      클립 {:>3}  {} ~ {}  ({link})", c.id, timecode(c.start, fps), timecode(c.end(), fps));
        }
    }
    Ok(())
}

/// 사용자가 준 시각을 읽어 프레임 경계에 붙인다.
fn time_arg(project: &Project, text: &str) -> CliResult<Flicks> {
    let t = parse_time(text, project.settings.fps).ok_or_else(|| format!("시각을 읽을 수 없다: {text}"))?;
    Ok(project.snap(t))
}

fn cmd_split(args: &Args) -> CliResult<()> {
    let path = Path::new(args.pos(0, "프로젝트 파일")?);
    let mut project = Project::load(path)?;
    let t = time_arg(&project, args.pos(1, "시각")?)?;
    let created = project.split_at(t, None)?;
    project.save(path)?;
    println!("{}에서 클립 {}개를 나눴다", timecode(t, project.settings.fps), created.len());
    Ok(())
}

fn cmd_export(args: &Args) -> CliResult<()> {
    use std::io::Write;
    use trackcut_core::export::{export, AudioMode, ExportOptions, Quality};

    let path = Path::new(args.pos(0, "프로젝트 파일")?);
    let project = Project::load(path)?;
    let audio = match args.options.get("--audio").map(String::as_str) {
        None | Some("mix") => AudioMode::Mix,
        Some("tracks") => AudioMode::Tracks,
        Some("wav") => AudioMode::WavPerTrack,
        Some(other) => return Err(format!("--audio: mix, tracks, wav 중 하나여야 한다: {other}").into()),
    };
    let quality = match args.options.get("--quality").map(String::as_str) {
        None | Some("high") => Quality::High,
        Some("standard") => Quality::Standard,
        Some("small") => Quality::Small,
        Some(other) => return Err(format!("--quality: high, standard, small 중 하나여야 한다: {other}").into()),
    };
    let extension = if audio == AudioMode::WavPerTrack { ".wav" } else { ".export.mp4" };
    let default_output = with_suffix(&with_suffix(path, ""), extension);
    let output = args.options.get("-o").map_or(default_output, PathBuf::from);
    let options = ExportOptions { audio, quality, ..ExportOptions::new(output) };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let outputs = export(&project, &options, &cancel, |p| {
        print!("\r내보내는 중 {:3.0}%", p * 100.0);
        let _ = std::io::stdout().flush();
    })?;
    println!("\r내보내기 완료        ");
    for file in outputs {
        println!("{}", file.display());
    }
    Ok(())
}

fn cmd_delete(args: &Args) -> CliResult<()> {
    let path = Path::new(args.pos(0, "프로젝트 파일")?);
    let mut project = Project::load(path)?;
    let a = time_arg(&project, args.pos(1, "시작 시각")?)?;
    let b = time_arg(&project, args.pos(2, "끝 시각")?)?;
    let tracks = if args.has("--tracks") {
        let ids = args
            .numbers("--tracks")?
            .into_iter()
            .map(|n| {
                let track = (n as usize).checked_sub(1).and_then(|i| project.timeline.tracks.get(i));
                track.map(|t| t.id).ok_or_else(|| format!("트랙 {n}이(가) 없다"))
            })
            .collect::<Result<Vec<u64>, String>>()?;
        Some(ids)
    } else {
        None
    };
    project.delete_range(a, b, tracks.as_deref(), !args.has("--keep-gap"))?;
    project.save(path)?;
    let fps = project.settings.fps;
    println!("{} ~ {} 구간을 지웠다. 새 길이 {}", timecode(a, fps), timecode(b, fps), timecode(project.duration(), fps));
    Ok(())
}
