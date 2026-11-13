//! 프로젝트 모델: 하나의 타임라인에 영상·오디오 트랙과 클립이 놓인다.
//! 편집 내용은 JSON 프로젝트 파일에만 저장하고 원본 미디어는 수정하지 않는다.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::labels::{default_name, TracksMeta};
use crate::media::MediaInfo;
use crate::time::{floor_frame, frame_time, nearest_frame, Flicks, FrameGrid, Rational};

pub const PROJECT_VERSION: u32 = 1;
pub const VOLUME_MIN_DB: f32 = -60.0;
pub const VOLUME_MAX_DB: f32 = 12.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { width: 1920, height: 1080, fps: Rational::new(60, 1) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub kind: TrackKind,
    pub name: String,
    pub volume_db: f32,
    pub muted: bool,
    pub solo: bool,
    /// 편집본 트랙이면 짝이 되는 원본 트랙. 원본 트랙은 None이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_of: Option<u64>,
}

impl Track {
    /// 편집본 묶음의 트랙인지(아니면 원본 묶음).
    pub fn is_edit(&self) -> bool {
        self.edit_of.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub track: u64,
    pub media: u64,
    /// 영상 클립은 0, 오디오 클립은 원본의 오디오 스트림 번호(`a:N`)
    pub stream: u32,
    /// 타임라인에서의 시작 시각
    pub start: Flicks,
    /// 원본 파일 안에서 쓰는 구간(원본 타임스탬프 기준)
    pub src_in: Flicks,
    pub src_out: Flicks,
    /// 같은 번호끼리 연결돼 함께 잘리고 움직인다
    pub link: Option<u64>,
    /// 재생 속도(%). 100이 원래 속도, 200이면 두 배 빠르다.
    #[serde(default = "full_speed")]
    pub speed_pct: u32,
    /// 화면 자르기(영상 클립만 쓴다)
    #[serde(default, skip_serializing_if = "Crop::is_none")]
    pub crop: Crop,
}

pub const SPEED_MIN_PCT: u32 = 25;
pub const SPEED_MAX_PCT: u32 = 400;

fn full_speed() -> u32 {
    100
}

/// 화면의 네 변에서 잘라낼 픽셀 수(원본 해상도 기준).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crop {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Crop {
    pub fn is_none(&self) -> bool {
        *self == Crop::default()
    }
}

/// 반올림하는 정수 나눗셈(음수도 가까운 쪽으로).
fn round_div(a: i64, b: i64) -> i64 {
    (2 * a + b).div_euclid(2 * b)
}

impl Clip {
    /// 타임라인에서 차지하는 길이(속도 반영).
    pub fn len(&self) -> Flicks {
        self.to_timeline(self.src_out - self.src_in)
    }

    /// 타임라인 길이를 원본 길이로.
    pub fn to_source(&self, d: Flicks) -> Flicks {
        round_div(d * self.speed_pct as i64, 100)
    }

    /// 원본 길이를 타임라인 길이로.
    pub fn to_timeline(&self, d: Flicks) -> Flicks {
        round_div(d * 100, self.speed_pct.max(1) as i64)
    }

    /// 타임라인 시각 `t`에 해당하는 원본 시각.
    pub fn src_at(&self, t: Flicks) -> Flicks {
        self.src_in + self.to_source(t - self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0
    }

    pub fn end(&self) -> Flicks {
        self.start + self.len()
    }
}

/// Undo 단위가 되는 편집 상태.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
    pub next_id: u64,
}

impl Timeline {
    pub fn alloc(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaEntry {
    pub id: u64,
    pub info: MediaInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub settings: Settings,
    pub media: Vec<MediaEntry>,
    pub timeline: Timeline,
}

impl Project {
    pub fn new(settings: Settings) -> Self {
        Project { version: PROJECT_VERSION, settings, media: Vec::new(), timeline: Timeline::default() }
    }

    /// 파일 하나로 프로젝트를 시작한다. 프로젝트 설정은 그 파일의 영상에 맞춘다.
    pub fn from_media(info: MediaInfo, labels: Option<&TracksMeta>) -> Self {
        let settings = info.video.as_ref().map_or_else(Settings::default, |v| Settings {
            width: v.width,
            height: v.height,
            fps: v.fps,
        });
        let mut project = Project::new(settings);
        project.add_media(info, labels, 0);
        project
    }

    /// 미디어를 추가하고, 스트림마다 새 트랙과 클립을 만들어 `at` 위치에 놓는다.
    /// 한 파일에서 나온 클립은 서로 연결된다. 스트림별 시작 시각 차이는 여기서 보정한다.
    pub fn add_media(&mut self, info: MediaInfo, labels: Option<&TracksMeta>, at: Flicks) -> u64 {
        self.place_media(info, labels, at, false)
    }

    /// 미디어를 타임라인 끝에 이어 붙인다. 새 트랙을 만들지 않고 기존 트랙에 순서대로 놓는다
    /// (영상은 첫 영상 트랙, N번째 오디오는 N번째 오디오 트랙). 모자라는 트랙만 새로 만든다.
    pub fn append_media(&mut self, info: MediaInfo, labels: Option<&TracksMeta>) -> u64 {
        let (end, fps) = (self.duration(), self.settings.fps);
        let frame = floor_frame(end, fps);
        let at = if frame_time(frame, fps) < end { frame_time(frame + 1, fps) } else { end };
        self.place_media(info, labels, at, true)
    }

    fn place_media(&mut self, info: MediaInfo, labels: Option<&TracksMeta>, at: Flicks, reuse: bool) -> u64 {
        let media_id = self.media.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let link = Some(self.timeline.alloc());
        // 타임라인 기준점: 영상의 첫 프레임(영상이 없으면 가장 먼저 시작하는 오디오)
        let origin = match &info.video {
            Some(v) => v.grid.start(),
            None => info.audio.iter().map(|a| a.start).min().unwrap_or(0),
        };

        if let Some(v) = &info.video {
            let name = match self.track_count(TrackKind::Video) {
                0 => "영상".to_string(),
                n => format!("영상 {}", n + 1),
            };
            let existing = self.timeline.tracks.iter().find(|t| reuse && !t.is_edit() && t.kind == TrackKind::Video).map(|t| t.id);
            let track = existing.unwrap_or_else(|| self.add_track(TrackKind::Video, name, false));
            let (src_in, src_out) = (v.grid.start(), v.grid.end());
            let id = self.timeline.alloc();
            self.timeline.clips.push(Clip { id, track, media: media_id, stream: 0, start: at, src_in, src_out, link, speed_pct: 100, crop: Crop::default() });
        }

        for a in &info.audio {
            let label = labels.and_then(|l| l.label(a.audio_index));
            let name = label
                .map(|l| l.name.clone())
                .or_else(|| a.title.clone())
                .unwrap_or_else(|| default_name(a.audio_index + 1));
            let audio_tracks = self.timeline.tracks.iter().filter(|t| reuse && !t.is_edit() && t.kind == TrackKind::Audio);
            let existing = audio_tracks.map(|t| t.id).nth(a.audio_index as usize);
            let track = existing.unwrap_or_else(|| self.add_track(TrackKind::Audio, name, label.is_some_and(|l| l.default_muted)));
            // 영상보다 먼저 시작하는 부분은 잘라내고, 늦게 시작하면 그만큼 뒤에 놓는다
            let src_in = a.start.max(origin);
            let src_out = a.start + a.duration;
            if src_out <= src_in {
                continue;
            }
            let id = self.timeline.alloc();
            self.timeline.clips.push(Clip {
                id,
                track,
                media: media_id,
                stream: a.audio_index,
                start: at + (src_in - origin),
                src_in,
                src_out,
                link,
                speed_pct: 100,
                crop: Crop::default(),
            });
        }

        self.media.push(MediaEntry { id: media_id, info });
        media_id
    }

    /// 원본 트랙을 하나 만든다. 편집본 트랙이 이미 있으면 그 앞에 넣어 원본 묶음이 위에 모이게 한다.
    fn add_track(&mut self, kind: TrackKind, name: String, muted: bool) -> u64 {
        let id = self.timeline.alloc();
        let at = self.timeline.tracks.iter().position(Track::is_edit).unwrap_or(self.timeline.tracks.len());
        self.timeline.tracks.insert(at, Track { id, kind, name, volume_db: 0.0, muted, solo: false, edit_of: None });
        id
    }

    fn track_count(&self, kind: TrackKind) -> usize {
        self.timeline.tracks.iter().filter(|t| t.kind == kind && !t.is_edit()).count()
    }

    /// 원본 묶음(`edit`이 false)이나 편집본 묶음의 트랙 id들.
    pub fn group_tracks(&self, edit: bool) -> Vec<u64> {
        self.timeline.tracks.iter().filter(|t| t.is_edit() == edit).map(|t| t.id).collect()
    }

    /// 클립이 편집본 묶음에 있는지.
    pub fn in_edit(&self, clip: &Clip) -> bool {
        self.track(clip.track).is_some_and(Track::is_edit)
    }

    /// 편집본 묶음에 클립이 하나라도 있는지. 있으면 내보내기는 편집본을 내보낸다.
    pub fn has_edit(&self) -> bool {
        self.timeline.clips.iter().any(|c| self.in_edit(c))
    }

    /// 한 묶음(원본 또는 편집본)의 길이.
    pub fn group_duration(&self, edit: bool) -> Flicks {
        self.timeline.clips.iter().filter(|c| self.in_edit(c) == edit).map(Clip::end).max().unwrap_or(0)
    }

    pub fn track(&self, id: u64) -> Option<&Track> {
        self.timeline.tracks.iter().find(|t| t.id == id)
    }

    pub fn media_info(&self, id: u64) -> Option<&MediaInfo> {
        self.media.iter().find(|m| m.id == id).map(|m| &m.info)
    }

    fn video_grid(&self, media: u64) -> Option<&FrameGrid> {
        self.media_info(media)?.video.as_ref().map(|v| &v.grid)
    }

    /// 타임라인 전체 길이(가장 늦게 끝나는 클립의 끝).
    pub fn duration(&self) -> Flicks {
        self.timeline.clips.iter().map(Clip::end).max().unwrap_or(0)
    }

    /// 클립이 원본에서 쓸 수 있는 최대 구간.
    pub fn source_bounds(&self, clip: &Clip) -> (Flicks, Flicks) {
        let Some(info) = self.media_info(clip.media) else {
            return (clip.src_in, clip.src_out);
        };
        let is_video = self.track(clip.track).is_some_and(|t| t.kind == TrackKind::Video);
        if is_video {
            if let Some(v) = &info.video {
                return (v.grid.start(), v.grid.end());
            }
        } else if let Some(a) = info.audio.iter().find(|a| a.audio_index == clip.stream) {
            return (a.start, a.start + a.duration);
        }
        (clip.src_in, clip.src_out)
    }

    /// 타임라인 시각을 프레임 경계에 붙인다. 그 위치에 영상 클립이 있으면 원본 파일의 실제
    /// 프레임 시각을 따르고, 없으면 프로젝트 fps 격자를 따른다.
    pub fn snap(&self, t: Flicks) -> Flicks {
        let t = t.max(0);
        for c in &self.timeline.clips {
            let is_video = self.track(c.track).is_some_and(|tr| tr.kind == TrackKind::Video);
            if !is_video || t < c.start || t > c.end() {
                continue;
            }
            if c.speed_pct != 100 {
                // 속도를 바꾼 클립은 원본 프레임과 화면 프레임이 1:1이 아니라서 프로젝트 격자를 따른다
                let fps = self.settings.fps;
                return (c.start + frame_time(nearest_frame(t - c.start, fps), fps)).min(c.end());
            }
            if let Some(grid) = self.video_grid(c.media) {
                let src = grid.snap(c.src_in + (t - c.start)).clamp(c.src_in, c.src_out);
                return c.start + (src - c.src_in);
            }
        }
        let fps = self.settings.fps;
        frame_time(nearest_frame(t, fps), fps)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let project: Project = serde_json::from_slice(&std::fs::read(path)?)?;
        if project.version > PROJECT_VERSION {
            return Err(Error::Edit(format!("더 새로운 버전({})의 프로젝트 파일이다", project.version)));
        }
        Ok(project)
    }

    /// 프로젝트를 저장한다. 원본 미디어 경로에는 절대 쓰지 않는다.
    pub fn save(&self, path: &Path) -> Result<()> {
        if self.media.iter().any(|m| same_file(path, Path::new(&m.info.path))) {
            return Err(Error::Edit("원본 미디어 파일 위에는 저장할 수 없다".into()));
        }
        // 임시 파일에 쓴 뒤 이름을 바꿔서, 저장 중 중단돼도 기존 파일이 깨지지 않게 한다
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}
