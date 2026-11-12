//! 녹화 이름표(V0): 오디오 트랙 이름을 파일과 `*.tracks.json`에 남긴다.
//! 원본은 건드리지 않고, 다시 인코딩하지 않은 새 파일을 만든다.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::probe::run_tool;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackLabel {
    /// 오디오 트랙 번호(1부터)
    pub index: u32,
    pub name: String,
    /// 편집기에서 처음부터 음소거할 트랙(예: 다른 트랙과 겹치는 백업 트랙)
    #[serde(default)]
    pub default_muted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TracksMeta {
    pub tracks: Vec<TrackLabel>,
}

pub fn default_name(index: u32) -> String {
    format!("오디오 {index}")
}

impl TracksMeta {
    /// 이름 목록으로 이름표를 만든다. 이름이 모자란 트랙은 "오디오 N"이 된다.
    pub fn new(track_count: usize, names: &[String], muted: &[u32]) -> Self {
        let tracks = (1..=track_count as u32)
            .map(|index| TrackLabel {
                index,
                name: names
                    .get(index as usize - 1)
                    .map(|n| n.trim())
                    .filter(|n| !n.is_empty())
                    .map_or_else(|| default_name(index), str::to_string),
                default_muted: muted.contains(&index),
            })
            .collect();
        TracksMeta { tracks }
    }

    /// 미디어 파일 옆의 설명 파일 경로: `녹화.mp4` → `녹화.tracks.json`
    pub fn sidecar_path(media: &Path) -> PathBuf {
        media.with_extension("tracks.json")
    }

    /// 설명 파일이 있으면 읽는다. 없거나 깨졌으면 None.
    pub fn load_for(media: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(Self::sidecar_path(media)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_for(&self, media: &Path) -> Result<PathBuf> {
        let path = Self::sidecar_path(media);
        std::fs::write(&path, serde_json::to_vec_pretty(self)?)?;
        Ok(path)
    }

    pub fn label(&self, audio_index: u32) -> Option<&TrackLabel> {
        self.tracks.iter().find(|t| t.index == audio_index + 1)
    }
}

/// 이름표를 넣는 ffmpeg 인자. 스트림을 그대로 복사하고 기존 파일은 덮어쓰지 않는다(`-n`).
pub fn tag_args(input: &Path, output: &Path, meta: &TracksMeta) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-v".into(), "error".into(), "-n".into(), "-i".into(), input.into()];
    for a in ["-map", "0:v?", "-map", "0:a", "-c", "copy"] {
        args.push(a.into());
    }
    for label in &meta.tracks {
        let stream = label.index - 1;
        for key in ["title", "handler_name"] {
            args.push(format!("-metadata:s:a:{stream}").into());
            args.push(format!("{key}={}", label.name).into());
        }
    }
    args.push(output.into());
    args
}

/// 이름표를 넣은 새 파일을 만든다.
pub fn tag(input: &Path, output: &Path, meta: &TracksMeta) -> Result<()> {
    if output.exists() {
        return Err(Error::Edit(format!("출력 파일이 이미 있다: {}", output.display())));
    }
    run_tool("ffmpeg", tag_args(input, output, meta)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_missing_names_and_marks_muted() {
        let meta = TracksMeta::new(3, &["게임".to_string(), " ".to_string()], &[3]);
        assert_eq!(meta.tracks[0].name, "게임");
        assert_eq!(meta.tracks[1].name, "오디오 2");
        assert!(meta.tracks[2].default_muted);
        assert_eq!(meta.label(0).unwrap().name, "게임");
    }

    #[test]
    fn tag_args_copy_streams_without_overwrite() {
        let meta = TracksMeta::new(2, &["a".to_string(), "b".to_string()], &[]);
        let args: Vec<String> = tag_args(Path::new("in.mp4"), Path::new("out.mp4"), &meta)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-n".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "-c" && w[1] == "copy"));
        assert!(args.windows(2).any(|w| w[0] == "-metadata:s:a:1" && w[1] == "title=b"));
        assert_eq!(args.last().unwrap(), "out.mp4");
    }

    #[test]
    fn sidecar_sits_next_to_media() {
        assert_eq!(
            TracksMeta::sidecar_path(Path::new("rec/game 01.mp4")),
            Path::new("rec/game 01.tracks.json")
        );
    }
}
