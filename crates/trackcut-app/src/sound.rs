//! 프로젝트의 소리 상태: 트랙별 오디오 캐시 준비, 파형 데이터, 재생 구성 갱신.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use eframe::egui;
use trackcut_core::audio::{prepare, TrackAudio, SAMPLE_RATE};
use trackcut_core::project::{Project, Timeline, TrackKind};
use trackcut_core::time::{to_samples, Flicks, FLICKS_PER_SEC};

use crate::engine::{Engine, PlanClip, PlanTrack};

/// (미디어 id, 오디오 스트림 번호)
pub type StreamKey = (u64, u32);

/// 파형 그리기에 필요한 것: 10ms 단위 피크와 그 스트림의 시작 시각
pub type Waves = HashMap<StreamKey, (Arc<Vec<u8>>, Flicks)>;

type Prepared = (StreamKey, Flicks, Result<TrackAudio, String>);

pub struct Sound {
    engine: Option<Engine>,
    pub engine_error: Option<String>,
    tracks: HashMap<StreamKey, TrackAudio>,
    pub waves: Waves,
    loading: Option<Receiver<Prepared>>,
    /// 아직 준비 중인 트랙 수
    pub pending: usize,
    /// 재생 구성을 만들 때의 타임라인과, 그때 들려주던 묶음(편집본이면 true)
    last_timeline: Option<(Timeline, bool)>,
}

impl Sound {
    pub fn new() -> Self {
        let (engine, engine_error) = match Engine::new() {
            Ok(engine) => (Some(engine), None),
            Err(e) => (None, Some(e)),
        };
        Sound { engine, engine_error, tracks: HashMap::new(), waves: HashMap::new(), loading: None, pending: 0, last_timeline: None }
    }

    /// 프로젝트의 모든 오디오 스트림을 백그라운드에서 준비한다.
    pub fn load(&mut self, project: &Project, ctx: &egui::Context) {
        self.tracks.clear();
        self.waves.clear();
        self.last_timeline = None;
        let jobs: Vec<_> = project
            .media
            .iter()
            .flat_map(|m| m.info.audio.iter().map(move |a| (m.id, a.audio_index, a.start, m.info.clone())))
            .collect();
        self.pending = jobs.len();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (media, stream, start, info) in jobs {
                let result = prepare(&info, stream).map_err(|e| e.to_string());
                if tx.send(((media, stream), start, result)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.loading = Some(rx);
    }

    /// 준비가 끝난 트랙을 받고, 타임라인이 바뀌었으면 재생 구성을 다시 만든다.
    /// 준비에 실패한 트랙이 있으면 오류 메시지를 돌려준다.
    /// `edit`이 true면 편집본 묶음의 트랙만, 아니면 원본 묶음의 트랙만 들려준다.
    pub fn update(&mut self, project: &Project, edit: bool) -> Option<String> {
        let mut error = None;
        let mut changed = false;
        let ready: Vec<Prepared> = self.loading.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for (key, start, result) in ready {
            self.pending = self.pending.saturating_sub(1);
            match result {
                Ok(audio) => {
                    self.waves.insert(key, (audio.peaks.clone(), start));
                    self.tracks.insert(key, audio);
                    changed = true;
                }
                Err(e) => error = Some(format!("소리 준비 실패: {e}")),
            }
        }
        if changed || self.last_timeline.as_ref().is_none_or(|(timeline, side)| *timeline != project.timeline || *side != edit) {
            self.last_timeline = Some((project.timeline.clone(), edit));
            if let Some(engine) = &self.engine {
                engine.set_plan(self.plan(project, edit));
            }
        }
        error
    }

    fn plan(&self, project: &Project, edit: bool) -> Vec<PlanTrack> {
        let audio_tracks = project.timeline.tracks.iter().filter(|t| t.kind == TrackKind::Audio && t.is_edit() == edit);
        audio_tracks
            .map(|track| {
                let gain = if project.is_audible(track.id) { 10f32.powf(track.volume_db / 20.0) } else { 0.0 };
                let clips = project.timeline.clips.iter().filter(|c| c.track == track.id && !c.muted).filter_map(|clip| {
                    let audio = self.tracks.get(&(clip.media, clip.stream))?;
                    let stream = project.media_info(clip.media)?.audio.iter().find(|a| a.audio_index == clip.stream)?;
                    Some(PlanClip {
                        pcm: audio.pcm.clone(),
                        start: to_samples(clip.start, SAMPLE_RATE),
                        offset: to_samples(clip.src_in - stream.start, SAMPLE_RATE),
                        len: to_samples(clip.len(), SAMPLE_RATE),
                        speed: clip.speed_pct as f64 / 100.0,
                    })
                });
                PlanTrack { gain, clips: clips.collect() }
            })
            .collect()
    }

    pub fn play(&self, from: Flicks) {
        if let Some(engine) = &self.engine {
            engine.play(to_samples(from, SAMPLE_RATE));
        }
    }

    pub fn pause(&self) {
        if let Some(engine) = &self.engine {
            engine.pause();
        }
    }

    /// 소리가 실제로 나가고 있는 타임라인 시각. 출력 장치가 없으면 None.
    pub fn position(&self) -> Option<Flicks> {
        self.engine.as_ref().map(|e| e.position() * (FLICKS_PER_SEC / SAMPLE_RATE as i64))
    }
}
