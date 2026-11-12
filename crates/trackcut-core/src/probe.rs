//! ffprobe로 미디어 파일을 분석한다. 트랙 수나 fps를 가정하지 않는다.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::media::{AudioStream, MediaInfo, VideoStream};
use crate::time::{frame_time, nearest_frame, secs_to_flicks, ts_to_flicks, Flicks, FrameGrid, Rational};

/// 실행 파일 위치. 환경 변수(`TRACKCUT_FFMPEG`, `TRACKCUT_FFPROBE`)가 있으면 그것을 쓰고 없으면 PATH에서 찾는다.
pub fn tool_path(name: &str) -> OsString {
    let var = format!("TRACKCUT_{}", name.to_uppercase());
    std::env::var_os(var).unwrap_or_else(|| name.into())
}

/// 창이 있는 앱에서 ffmpeg를 실행할 때 검은 콘솔 창이 뜨지 않게 한다.
pub(crate) fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

pub fn run_tool<I, S>(name: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let tool_err = |message: String| Error::Tool { tool: name.to_string(), message };
    let mut command = Command::new(tool_path(name));
    command.args(args).stdin(Stdio::null());
    hide_console(&mut command);
    let out = command.output().map_err(|e| tool_err(e.to_string()))?;
    if !out.status.success() {
        return Err(tool_err(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let streams = run_tool(
        "ffprobe",
        [
            OsStr::new("-v"), OsStr::new("error"),
            OsStr::new("-print_format"), OsStr::new("json"),
            OsStr::new("-show_format"), OsStr::new("-show_streams"),
            path.as_os_str(),
        ],
    )?;
    let packets = run_tool(
        "ffprobe",
        [
            OsStr::new("-v"), OsStr::new("error"),
            OsStr::new("-select_streams"), OsStr::new("v:0"),
            OsStr::new("-show_entries"), OsStr::new("packet=pts,flags"),
            OsStr::new("-of"), OsStr::new("csv=p=0"),
            path.as_os_str(),
        ],
    )?;
    parse_media(&path.to_string_lossy(), &streams, &packets)
}

/// ffprobe 출력(스트림 JSON, 영상 패킷 CSV)을 해석한다. 프로세스 실행과 분리해 테스트하기 쉽게 했다.
pub fn parse_media(path: &str, streams_json: &str, packets_csv: &str) -> Result<MediaInfo> {
    let root: Value = serde_json::from_str(streams_json)?;
    let duration = root["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .map(secs_to_flicks)
        .unwrap_or(0);
    let streams = root["streams"]
        .as_array()
        .ok_or_else(|| Error::Probe("streams 항목이 없다".into()))?;

    let mut video = None;
    let mut audio = Vec::new();
    for s in streams {
        let index = int_field(s, "index").unwrap_or(0) as u32;
        let codec = s["codec_name"].as_str().unwrap_or("unknown").to_string();
        let tb = rational_field(s, "time_base").unwrap_or(Rational::new(1, 1000));
        let start = ts_to_flicks(int_field(s, "start_pts").unwrap_or(0), tb);
        match s["codec_type"].as_str() {
            Some("video") if video.is_none() && s["disposition"]["attached_pic"] != 1 => {
                let fps = rational_field(s, "avg_frame_rate")
                    .or_else(|| rational_field(s, "r_frame_rate"))
                    .unwrap_or(Rational::new(60, 1));
                let (grid, keyframes) = build_grid(packets_csv, tb, fps, start, duration);
                video = Some(VideoStream {
                    index,
                    codec,
                    width: int_field(s, "width").unwrap_or(0) as u32,
                    height: int_field(s, "height").unwrap_or(0) as u32,
                    fps,
                    grid,
                    keyframes,
                });
            }
            Some("audio") => {
                let stream_duration = int_field(s, "duration_ts")
                    .map(|d| ts_to_flicks(d, tb))
                    .unwrap_or((duration - start).max(0));
                audio.push(AudioStream {
                    index,
                    audio_index: audio.len() as u32,
                    codec,
                    sample_rate: int_field(s, "sample_rate").unwrap_or(48_000) as u32,
                    channels: int_field(s, "channels").unwrap_or(2) as u32,
                    title: stream_title(s),
                    start,
                    duration: stream_duration,
                });
            }
            _ => {}
        }
    }
    if video.is_none() && audio.is_empty() {
        return Err(Error::Probe("영상이나 오디오 스트림이 없다".into()));
    }
    Ok(MediaInfo { path: path.to_string(), duration, video, audio })
}

fn int_field(v: &Value, key: &str) -> Option<i64> {
    let f = v.get(key)?;
    f.as_i64().or_else(|| f.as_str()?.parse().ok())
}

fn rational_field(v: &Value, key: &str) -> Option<Rational> {
    Rational::parse(v.get(key)?.as_str()?)
}

/// 트랙 이름표. mp4는 `title`을 `handler_name`으로 저장하므로 둘 다 본다.
fn stream_title(s: &Value) -> Option<String> {
    let tags = s.get("tags")?;
    ["title", "handler_name"]
        .iter()
        .filter_map(|k| tags.get(*k)?.as_str())
        .map(str::trim)
        .find(|t| !t.is_empty() && *t != "SoundHandler")
        .map(str::to_string)
}

/// 패킷 목록에서 실제 프레임 시각과 키프레임 위치를 만든다.
fn build_grid(
    packets_csv: &str,
    tb: Rational,
    fps: Rational,
    stream_start: Flicks,
    duration: Flicks,
) -> (FrameGrid, Vec<u32>) {
    let mut packets: Vec<(i64, bool)> = packets_csv
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(',');
            let pts = fields.next()?.trim().parse().ok()?;
            Some((pts, fields.next().is_some_and(|f| f.contains('K'))))
        })
        .collect();
    // 패킷은 디코딩 순서라서(B 프레임) 표시 순서로 정렬한다
    packets.sort_by_key(|p| p.0);
    packets.dedup_by_key(|p| p.0);

    let keyframes = packets
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.1.then_some(i as u32))
        .collect();

    if packets.len() < 2 {
        let count = packets.len().max(nearest_frame(duration, fps).max(0) as usize) as u64;
        let start = packets.first().map_or(stream_start, |p| ts_to_flicks(p.0, tb));
        return (FrameGrid::Cfr { start, fps, count }, keyframes);
    }

    let start = ts_to_flicks(packets[0].0, tb);
    let count = packets.len() as u64;
    let first_delta = packets[1].0 - packets[0].0;
    if packets.windows(2).all(|w| w[1].0 - w[0].0 == first_delta) {
        let fps = Rational::new(tb.den, first_delta * tb.num).reduced();
        return (FrameGrid::Cfr { start, fps, count }, keyframes);
    }

    // time_base가 거칠어서(예: 1/1000) 간격이 16·17로 흔들리는 고정 fps 파일
    let tick = ts_to_flicks(1, tb);
    let pts: Vec<Flicks> = packets.iter().map(|p| ts_to_flicks(p.0, tb)).collect();
    let near_cfr = pts
        .iter()
        .enumerate()
        .all(|(i, &t)| (t - start - frame_time(i as i64, fps)).abs() <= tick);
    if near_cfr {
        return (FrameGrid::Cfr { start, fps, count }, keyframes);
    }

    let last = pts[pts.len() - 1];
    let end = last + (last - pts[pts.len() - 2]);
    (FrameGrid::Vfr { pts, end }, keyframes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAMS: &str = r#"{
      "streams": [
        {"index": 0, "codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
         "avg_frame_rate": "60/1", "time_base": "1/15360", "start_pts": 0},
        {"index": 1, "codec_type": "audio", "codec_name": "aac", "sample_rate": "48000", "channels": 2,
         "time_base": "1/48000", "start_pts": 0, "duration_ts": 96000, "tags": {"handler_name": "Game"}},
        {"index": 2, "codec_type": "audio", "codec_name": "aac", "sample_rate": "48000", "channels": 2,
         "time_base": "1/48000", "start_pts": 800, "duration_ts": 95200, "tags": {"handler_name": "SoundHandler"}}
      ],
      "format": {"duration": "2.000000"}
    }"#;

    #[test]
    fn parses_cfr_file_with_out_of_order_packets() {
        // 디코딩 순서: 0, 768, 256, 512
        let info = parse_media("a.mp4", STREAMS, "0,K__\n768,___\n256,___\n512,___\n1024,K__\n").unwrap();
        let v = info.video.unwrap();
        assert_eq!(v.grid, FrameGrid::Cfr { start: 0, fps: Rational::new(60, 1), count: 5 });
        assert_eq!(v.keyframes, vec![0, 4]);
        assert_eq!(info.audio.len(), 2);
        assert_eq!(info.audio[0].title.as_deref(), Some("Game"));
        assert_eq!(info.audio[1].title, None);
        assert_eq!(info.audio[1].start, frame_time(1, Rational::new(60, 1)));
    }

    #[test]
    fn detects_vfr() {
        let info = parse_media("a.mp4", STREAMS, "0,K__\n256,___\n1024,___\n1280,___\n").unwrap();
        let grid = info.video.unwrap().grid;
        assert!(!grid.is_cfr());
        assert_eq!(grid.count(), 4);
    }

    #[test]
    fn rejects_file_without_streams() {
        assert!(parse_media("a.txt", r#"{"streams": [], "format": {}}"#, "").is_err());
    }
}
