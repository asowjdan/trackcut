//! 미리듣기와 파형을 위한 오디오 캐시.
//!
//! 오디오 트랙을 48kHz 스테레오 16비트 PCM 파일로 풀어 캐시 폴더에 두고(원본은 그대로),
//! 10ms마다의 최대 크기(피크)를 계산해 파형으로 쓴다. 한 번 만든 캐시는 다시 쓴다.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;
use crate::media::MediaInfo;
use crate::probe::run_tool;

pub const SAMPLE_RATE: u32 = 48_000;
/// 피크 하나가 대표하는 프레임 수(10ms)
pub const PEAK_FRAMES: usize = 480;
/// PCM 한 프레임의 바이트 수(스테레오 16비트)
pub const FRAME_BYTES: usize = 4;

#[derive(Clone, Debug)]
pub struct TrackAudio {
    /// 48kHz 스테레오 s16le. 0번 프레임이 원본 스트림의 시작 시각이다.
    pub pcm: PathBuf,
    /// 10ms마다의 최대 크기(0~255)
    pub peaks: Arc<Vec<u8>>,
    pub frames: u64,
}

/// 캐시 폴더. `TRACKCUT_CACHE` 환경 변수로 바꿀 수 있다.
pub fn cache_root() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("TRACKCUT_CACHE")
        .or_else(|| var("LOCALAPPDATA").map(|p| p.join("trackcut").join("cache")))
        .or_else(|| var("XDG_CACHE_HOME").map(|p| p.join("trackcut")))
        .or_else(|| var("HOME").map(|p| p.join(".cache").join("trackcut")))
        .unwrap_or_else(|| std::env::temp_dir().join("trackcut-cache"))
}

/// 파일 경로·크기·수정 시각으로 캐시 폴더 이름을 정한다. 파일이 바뀌면 새로 만든다.
fn cache_dir(media: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    media.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(media) {
        meta.len().hash(&mut hasher);
        meta.modified().ok().hash(&mut hasher);
    }
    cache_root().join(format!("{:016x}", hasher.finish()))
}

fn compute_peaks(pcm: &Path) -> Result<Vec<u8>> {
    let mut file = std::io::BufReader::new(std::fs::File::open(pcm)?);
    let mut chunk = vec![0u8; PEAK_FRAMES * FRAME_BYTES];
    let mut peaks = Vec::new();
    loop {
        let mut filled = 0;
        while filled < chunk.len() {
            match file.read(&mut chunk[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        if filled == 0 {
            break;
        }
        let peak = chunk[..filled]
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
            .max()
            .unwrap_or(0);
        peaks.push((peak >> 7).min(255) as u8);
        if filled < chunk.len() {
            break;
        }
    }
    Ok(peaks)
}

/// 오디오 트랙 하나의 PCM과 파형을 준비한다. 캐시가 있으면 그대로 쓴다.
pub fn prepare(info: &MediaInfo, audio_index: u32) -> Result<TrackAudio> {
    let media = Path::new(&info.path);
    let dir = cache_dir(media);
    std::fs::create_dir_all(&dir)?;
    let pcm = dir.join(format!("a{audio_index}.pcm"));
    let peaks_path = dir.join(format!("a{audio_index}.peaks"));
    if !pcm.exists() {
        // 임시 이름으로 만든 뒤 바꿔서, 중간에 끊긴 파일이 캐시로 남지 않게 한다
        let tmp = dir.join(format!("a{audio_index}.pcm.tmp"));
        let map = format!("0:a:{audio_index}");
        let mut args: Vec<std::ffi::OsString> = ["-v", "error", "-y", "-i"].map(Into::into).to_vec();
        args.push(media.into());
        args.extend(["-map", &map, "-vn", "-ac", "2", "-ar", "48000", "-f", "s16le"].map(Into::into));
        args.push(tmp.clone().into());
        run_tool("ffmpeg", args)?;
        std::fs::rename(&tmp, &pcm)?;
        let _ = std::fs::remove_file(&peaks_path);
    }
    let peaks = match std::fs::read(&peaks_path) {
        Ok(peaks) => peaks,
        Err(_) => {
            let peaks = compute_peaks(&pcm)?;
            std::fs::write(&peaks_path, &peaks)?;
            peaks
        }
    };
    let frames = std::fs::metadata(&pcm)?.len() / FRAME_BYTES as u64;
    Ok(TrackAudio { pcm, peaks: Arc::new(peaks), frames })
}
