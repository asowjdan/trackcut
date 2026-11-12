//! 원본 미디어 파일 한 개의 분석 결과.

use serde::{Deserialize, Serialize};

use crate::time::{Flicks, FrameGrid, Rational};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoStream {
    /// 컨테이너 안의 스트림 번호
    pub index: u32,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    /// 실제 프레임 시각
    pub grid: FrameGrid,
    /// 키프레임인 프레임 번호(표시 순서). 스마트 렌더에서 다시 인코딩할 구간을 정할 때 쓴다.
    pub keyframes: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioStream {
    /// 컨테이너 안의 스트림 번호
    pub index: u32,
    /// 오디오 스트림 중 몇 번째인지(0부터). ffmpeg의 `a:N`
    pub audio_index: u32,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    /// 파일에 기록된 트랙 이름표
    pub title: Option<String>,
    /// 스트림마다 다를 수 있는 시작 시각
    pub start: Flicks,
    pub duration: Flicks,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: String,
    pub duration: Flicks,
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
}
