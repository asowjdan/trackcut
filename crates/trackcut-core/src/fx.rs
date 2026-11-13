//! 소리 보정: 노이즈 제거, 저음 잡음 차단, 이퀄라이저.
//!
//! 원본 파일은 건드리지 않고, 보정한 소리를 새 파일(FLAC)로 만들어 새 트랙에 올린다.
//! 그래서 원본 트랙과 보정한 트랙을 번갈아 들으며 비교할 수 있다.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{edit_err, Error, Result};
use crate::media::MediaInfo;
use crate::probe::{hide_console, probe, tool_path};

#[derive(Clone, Debug, PartialEq)]
pub struct AudioFx {
    /// 노이즈 제거와 줄이는 양(dB)
    pub denoise: bool,
    pub denoise_db: f32,
    /// 이 주파수(Hz) 아래의 웅웅거리는 소리를 걸러낸다
    pub highpass: bool,
    pub highpass_hz: u32,
    /// 이퀄라이저: 저음(120Hz), 중음(1.5kHz), 고음(6kHz) 조절량(dB)
    pub low_db: f32,
    pub mid_db: f32,
    pub high_db: f32,
}

impl Default for AudioFx {
    fn default() -> Self {
        AudioFx { denoise: true, denoise_db: 12.0, highpass: true, highpass_hz: 80, low_db: 0.0, mid_db: 0.0, high_db: 0.0 }
    }
}

impl AudioFx {
    /// FFmpeg 오디오 필터. 아무것도 바꾸지 않는 설정이면 None.
    pub fn filter(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.highpass {
            parts.push(format!("highpass=f={}", self.highpass_hz.clamp(20, 1000)));
        }
        if self.denoise {
            parts.push(format!("afftdn=nr={:.1}:nf=-30", self.denoise_db.clamp(1.0, 40.0)));
        }
        for (name, frequency, gain) in [("bass", 120, self.low_db), ("treble", 6000, self.high_db)] {
            if gain.abs() >= 0.05 {
                parts.push(format!("{name}=g={:.1}:f={frequency}", gain.clamp(-20.0, 20.0)));
            }
        }
        if self.mid_db.abs() >= 0.05 {
            parts.push(format!("equalizer=f=1500:t=q:w=1:g={:.1}", self.mid_db.clamp(-20.0, 20.0)));
        }
        (!parts.is_empty()).then(|| parts.join(","))
    }
}

/// 보정한 소리를 둘 경로: `녹화.trackcut-audio/a<스트림>-<설정 해시>.flac`
fn output_path(media: &Path, audio_index: u32, filter: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    filter.hash(&mut hasher);
    media.with_extension("trackcut-audio").join(format!("a{}-{:016x}.flac", audio_index + 1, hasher.finish()))
}

/// 미디어의 오디오 스트림 하나를 처음부터 끝까지 보정해 새 파일로 만들고, 그 파일의 정보를 돌려준다.
/// 같은 설정으로 이미 만든 파일이 있으면 그대로 쓴다.
pub fn render(info: &MediaInfo, audio_index: u32, fx: &AudioFx) -> Result<MediaInfo> {
    let Some(filter) = fx.filter() else {
        return edit_err("적용할 보정이 없다");
    };
    let output = output_path(Path::new(&info.path), audio_index, &filter);
    if !output.exists() {
        if let Some(folder) = output.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let partial = output.with_extension("part.flac");
        let mut command = Command::new(tool_path("ffmpeg"));
        command.args(["-v", "error", "-y", "-i"]).arg(&info.path);
        command.args(["-map", &format!("0:a:{audio_index}"), "-vn", "-sn", "-af", &filter, "-ar", "48000", "-c:a", "flac"]);
        command.arg(&partial).stdin(Stdio::null());
        hide_console(&mut command);
        let tool_err = |message: String| Error::Tool { tool: "ffmpeg".into(), message };
        let result = command.output().map_err(|e| tool_err(e.to_string()))?;
        if !result.status.success() {
            let _ = std::fs::remove_file(&partial);
            return Err(tool_err(String::from_utf8_lossy(&result.stderr).trim().to_string()));
        }
        std::fs::rename(&partial, &output)?;
    }
    probe(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_lists_only_enabled_steps() {
        let fx = AudioFx { mid_db: 3.0, ..AudioFx::default() };
        assert_eq!(fx.filter().unwrap(), "highpass=f=80,afftdn=nr=12.0:nf=-30,equalizer=f=1500:t=q:w=1:g=3.0");
        let off = AudioFx { denoise: false, highpass: false, ..AudioFx::default() };
        assert_eq!(off.filter(), None);
    }

    #[test]
    fn render_makes_a_new_file_and_keeps_the_source() {
        let folder = std::env::temp_dir().join(format!("trackcut-fx-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let source = folder.join("voice.wav");
        let made = Command::new(tool_path("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-ar", "48000", "-ac", "2"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(made.success());
        let before = std::fs::read(&source).unwrap();
        let info = probe(&source).unwrap();
        let fixed = render(&info, 0, &AudioFx::default()).unwrap();
        assert_ne!(fixed.path, info.path);
        assert_eq!(fixed.audio.len(), 1);
        let (old, new) = (info.audio[0].duration, fixed.audio[0].duration);
        assert!((old - new).abs() < crate::time::FLICKS_PER_SEC / 20, "길이가 같아야 한다: {old} / {new}");
        assert_eq!(std::fs::read(&source).unwrap(), before, "원본은 바뀌지 않는다");
        std::fs::remove_dir_all(folder).ok();
    }
}
