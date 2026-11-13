//! 편집 화면의 상태와 화면 구성.

mod inspector;
mod panels;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use eframe::egui::{self, Color32, Key};
use trackcut_core::decode::Decoder;
use trackcut_core::fx::{self, AudioFx};
use trackcut_core::history::History;
use trackcut_core::labels::TracksMeta;
use trackcut_core::media::{MediaInfo, VideoStream};
use trackcut_core::probe::probe;
use trackcut_core::project::{Clip, Crop, Project, TrackKind, VOLUME_MAX_DB, VOLUME_MIN_DB};
use trackcut_core::time::{floor_frame, frame_time, parse_time, secs_to_flicks, Flicks, FLICKS_PER_SEC};

use crate::drag::Drag;
use crate::export_ui::ExportUi;
use crate::sound::Sound;
use crate::timeline::{self, Action, Marks, Region, View};
use crate::Options;

/// 미리보기 화질 선택지: (디코딩할 최대 높이, 이름). 원본은 줄이지 않는다.
const PREVIEW_CHOICES: [(u32, &str); 3] = [(u32::MAX, "원본"), (720, "720p"), (540, "540p (빠름)")];

type Loaded = Result<(Project, PathBuf), String>;
type Added = Result<(MediaInfo, Option<TracksMeta>), String>;
/// 소리 보정 결과: (원본 트랙, [(원본 미디어, 스트림, 보정한 파일 정보)])
type Fixed = Result<(u64, Vec<(u64, u32, MediaInfo)>), String>;

/// 미디어나 프로젝트 파일을 연다. 미디어 옆에 프로젝트 파일이 이미 있으면 그것을 이어서 연다.
fn load(path: PathBuf) -> Loaded {
    let is_project = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    let project_path = if is_project { path.clone() } else { path.with_extension("trackcut.json") };
    let project = if project_path.exists() {
        Project::load(&project_path)
    } else {
        probe(&path).map(|info| Project::from_media(info, TracksMeta::load_for(&path).as_ref()))
    };
    project.map(|p| (p, project_path)).map_err(|e| e.to_string())
}

/// 시각 `t`에 화면에 보이는 영상 클립.
fn video_at(project: &Project, t: Flicks) -> Option<(&Clip, &VideoStream)> {
    let clip = project.timeline.clips.iter().find(|c| {
        c.start <= t && t < c.end() && project.track(c.track).is_some_and(|track| track.kind == TrackKind::Video)
    })?;
    Some((clip, project.media_info(clip.media)?.video.as_ref()?))
}

enum MixerAction {
    Mute(u64, bool),
    Solo(u64, bool),
    Volume(u64, f32),
}

pub struct TrackcutApp {
    options: Options,
    project: Option<Project>,
    project_path: PathBuf,
    history: History,
    loading: Option<Receiver<Loaded>>,
    /// 타임라인 끝에 이어 붙이려고 분석 중인 파일
    adding: Option<Receiver<Added>>,
    /// 열거나 이어 붙일 차례를 기다리는 파일
    queue: VecDeque<PathBuf>,
    playhead: Flicks,
    playing: bool,
    last_tick: Instant,
    view: View,
    /// 타임라인에 지정한 구역들과 선택한 구역
    regions: Vec<Region>,
    region: Option<usize>,
    /// 미리보기에서 화면 자르기를 고치는 중
    crop_mode: bool,
    /// 소리 보정 설정과 진행 중인 보정 작업
    fx: AudioFx,
    fx_job: Option<Receiver<Fixed>>,
    selected_clip: Option<u64>,
    selected_track: Option<u64>,
    /// 타임라인에서 클립을 끄는 중인 편집
    drag: Option<Drag>,
    decoders: HashMap<u64, Decoder>,
    texture: Option<egui::TextureHandle>,
    /// 지금 보여야 하는 (미디어, 프레임)과 실제로 화면에 올라온 것
    wanted: Option<(u64, u64)>,
    shown: Option<(u64, u64)>,
    /// 미리보기를 디코딩할 최대 높이
    preview_height: u32,
    /// 지금 보이는 클립과 그 화면 자르기, 원본 해상도
    view_crop: Option<(u64, Crop, u32, u32)>,
    tab: inspector::Tab,
    status: String,
    sound: Sound,
    /// 소리 엔진에 재생을 지시해 둔 상태인지
    sound_playing: bool,
    export: ExportUi,
    frames_drawn: u32,
    shot_sent: bool,
}

impl TrackcutApp {
    pub fn new(ctx: egui::Context, options: Options) -> Self {
        let mut app = TrackcutApp {
            project: None,
            project_path: PathBuf::new(),
            history: History::default(),
            loading: None,
            adding: None,
            queue: VecDeque::new(),
            playhead: 0,
            playing: false,
            last_tick: Instant::now(),
            view: View::default(),
            regions: Vec::new(),
            region: None,
            crop_mode: false,
            fx: AudioFx::default(),
            fx_job: None,
            selected_clip: None,
            selected_track: None,
            drag: None,
            decoders: HashMap::new(),
            texture: None,
            wanted: None,
            shown: None,
            preview_height: PREVIEW_CHOICES[0].0,
            view_crop: None,
            tab: inspector::Tab::default(),
            status: String::new(),
            sound: Sound::new(),
            sound_playing: false,
            export: ExportUi::default(),
            frames_drawn: 0,
            shot_sent: false,
            options,
        };
        app.queue.extend(std::mem::take(&mut app.options.inputs));
        app.next_file(&ctx);
        app
    }

    /// 분석이 오래 걸릴 수 있어 별도 스레드에서 연다.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load(path));
            ctx.request_repaint();
        });
        self.loading = Some(rx);
        self.status = "파일 분석 중…".into();
    }

    /// 기다리는 파일을 하나 꺼낸다. 열린 프로젝트가 없으면 그 파일로 시작하고, 있으면 끝에 이어 붙인다.
    fn next_file(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() || self.adding.is_some() {
            return;
        }
        let Some(path) = self.queue.pop_front() else { return };
        let is_project = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if self.project.is_none() || is_project {
            self.open(ctx, path);
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(probe(&path).map(|info| (info, TracksMeta::load_for(&path))).map_err(|e| e.to_string()));
            ctx.request_repaint();
        });
        self.adding = Some(rx);
        self.status = "추가할 파일 분석 중…".into();
    }

    fn finish_adding(&mut self, ctx: &egui::Context) {
        let Some(result) = self.adding.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.adding = None;
        match result {
            Ok((info, labels)) => {
                let added = self.edit("파일 추가", |p| {
                    p.append_media(info, labels.as_ref());
                    Ok(())
                });
                if let (Some(()), Some(project)) = (added, &self.project) {
                    self.sound.load(project, ctx);
                    self.view.fit = true;
                }
            }
            Err(e) => self.status = format!("추가할 수 없음: {e}"),
        }
    }

    fn finish_loading(&mut self, ctx: &egui::Context) {
        let Some(result) = self.loading.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.loading = None;
        match result {
            Ok((project, path)) => {
                self.playhead = match &self.options.seek {
                    Some(text) => parse_time(text, project.settings.fps).map_or(0, |t| project.snap(t)),
                    None => 0,
                };
                self.options.seek = None;
                self.sound.load(&project, ctx);
                let selected = self.options.select.and_then(|n| project.timeline.clips.get(n));
                self.selected_clip = selected.map(|c| c.id);
                self.selected_track = selected.map(|c| c.track);
                self.drag = None;
                self.project = Some(project);
                self.project_path = path;
                self.history = History::default();
                self.decoders.clear();
                (self.wanted, self.shown, self.region) = (None, None, None);
                self.regions.clear();
                self.crop_mode = self.options.crop;
                if let Some([a, b]) = self.options.region {
                    self.regions.push((secs_to_flicks(a), secs_to_flicks(b)));
                    self.region = Some(0);
                }
                self.playing = self.options.play;
                self.view = View::default();
                self.status = "열었다".into();
            }
            Err(e) => self.status = format!("열 수 없음: {e}"),
        }
    }

    fn duration(&self) -> Flicks {
        self.project.as_ref().map_or(0, Project::duration)
    }

    fn seek(&mut self, t: Flicks) {
        self.playhead = t.clamp(0, self.duration());
    }

    /// 원본의 실제 프레임을 기준으로 n프레임 이동한다.
    fn step_frames(&mut self, n: i64) {
        let Some(project) = &self.project else { return };
        let t = self.playhead;
        // 뒤로 갈 때는 바로 앞 프레임이 속한 클립을 기준으로 한다(클립 경계에서 멈추지 않게)
        let probe_at = if n < 0 { (t - 1).max(0) } else { t };
        let next = match video_at(project, probe_at) {
            Some((clip, video)) if clip.speed_pct == 100 => {
                let base = video.grid.frame_at(clip.src_in + (probe_at - clip.start)) as i64;
                let frame = if n < 0 { base + n + 1 } else { base + n };
                let src = video.grid.time_of(frame.max(0) as u64).clamp(clip.src_in, clip.src_out);
                clip.start + (src - clip.src_in)
            }
            // 영상이 없거나 속도를 바꾼 클립은 프로젝트 프레임 단위로 움직인다
            _ => {
                let fps = project.settings.fps;
                frame_time(floor_frame(t, fps) + n, fps)
            }
        };
        self.playing = false;
        self.seek(next);
    }

    fn step_seconds(&mut self, secs: i64) {
        let Some(project) = &self.project else { return };
        let next = project.snap((self.playhead + secs * FLICKS_PER_SEC).max(0));
        self.playing = false;
        self.seek(next);
    }

    fn toggle_play(&mut self) {
        if self.playhead >= self.duration() {
            self.playhead = 0;
        }
        self.playing = !self.playing && self.project.is_some();
    }

    /// 편집 하나를 실행하고 Undo 단계로 남긴다.
    fn edit<T>(&mut self, label: &str, f: impl FnOnce(&mut Project) -> trackcut_core::Result<T>) -> Option<T> {
        let project = self.project.as_mut()?;
        match self.history.apply(project, label, f) {
            Ok(value) => {
                self.status = label.to_string();
                Some(value)
            }
            Err(e) => {
                self.status = e.to_string();
                None
            }
        }
    }

    /// `edit`과 같지만 같은 `key`로 연달아 고친 값은 Undo 한 단계로 묶는다(숫자 칸, 슬라이더).
    fn edit_keyed(&mut self, label: &str, key: &str, f: impl FnOnce(&mut Project) -> trackcut_core::Result<()>) {
        let Some(project) = self.project.as_mut() else { return };
        self.status = match self.history.apply_keyed(project, label, key, f) {
            Ok(()) => label.to_string(),
            Err(e) => e.to_string(),
        };
    }

    fn set_preview_height(&mut self, height: u32) {
        self.preview_height = height;
        self.decoders.clear();
        (self.wanted, self.shown) = (None, None);
    }

    /// 구역의 양 끝에서 클립을 나눠, 구역이 따로 떨어진 조각이 되게 한다.
    fn split_region(&mut self, index: usize) {
        let Some(&(a, b)) = self.regions.get(index) else { return };
        let done = self.edit("구역 분할", |p| {
            p.split_at(a, None)?;
            p.split_at(b, None).map(|_| ())
        });
        // 나뉜 조각을 바로 고칠 수 있게 선택한다
        if let (Some(()), Some(project)) = (done, &self.project) {
            let is_video = |c: &&Clip| project.track(c.track).is_some_and(|t| t.kind == TrackKind::Video);
            let starts_here = || project.timeline.clips.iter().filter(|c| c.start == a);
            if let Some(clip) = starts_here().find(is_video).or_else(|| starts_here().next()) {
                (self.selected_clip, self.selected_track) = (Some(clip.id), Some(clip.track));
            }
        }
    }

    /// 구역의 내용을 잘라내고 뒤를 당겨 붙인다. 뒤쪽 구역들도 함께 당긴다.
    fn cut_region(&mut self, index: usize) {
        let Some(&(a, b)) = self.regions.get(index) else {
            self.status = "삭제할 구역을 먼저 고른다".into();
            return;
        };
        if self.edit("구역 삭제", |p| p.delete_range(a, b, None, true)).is_none() {
            return;
        }
        self.regions.remove(index);
        for region in &mut self.regions {
            if region.0 >= b {
                *region = (region.0 - (b - a), region.1 - (b - a));
            } else if region.1 > a {
                region.1 = region.1.min(a).max(region.0);
            }
        }
        self.regions.retain(|r| r.1 > r.0);
        self.region = None;
        self.seek(a);
    }

    /// 선택한 구역의 시작(또는 끝)을 지금 위치로 옮긴다. 선택한 구역이 없으면 지금 위치에서 2초짜리를 만든다.
    fn mark_edge(&mut self, is_end: bool) {
        let (t, total, span) = (self.playhead, self.duration(), 2 * FLICKS_PER_SEC);
        match self.region.and_then(|i| self.regions.get_mut(i)) {
            Some(region) if is_end => region.1 = t.max(region.0),
            Some(region) => region.0 = t.min(region.1),
            None => {
                let region = if is_end { ((t - span).max(0), t) } else { (t, (t + span).min(total)) };
                if region.1 > region.0 {
                    self.regions.push(region);
                    self.region = Some(self.regions.len() - 1);
                }
            }
        }
    }

    /// 트랙의 소리를 보정한 새 파일을 백그라운드에서 만든다. 끝나면 새 트랙으로 올라간다.
    fn start_fx(&mut self, ctx: &egui::Context, track: u64) {
        let Some(project) = &self.project else { return };
        let mut sources: Vec<(u64, u32, MediaInfo)> = Vec::new();
        for clip in project.timeline.clips.iter().filter(|c| c.track == track) {
            if !sources.iter().any(|s| (s.0, s.1) == (clip.media, clip.stream)) {
                if let Some(info) = project.media_info(clip.media) {
                    sources.push((clip.media, clip.stream, info.clone()));
                }
            }
        }
        if sources.is_empty() {
            self.status = "보정할 클립이 없다".into();
            return;
        }
        let (tx, rx) = channel();
        let (settings, ctx) = (self.fx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let fixed: Result<Vec<_>, String> = sources
                .into_iter()
                .map(|(media, stream, info)| fx::render(&info, stream, &settings).map(|done| (media, stream, done)).map_err(|e| e.to_string()))
                .collect();
            let _ = tx.send(fixed.map(|files| (track, files)));
            ctx.request_repaint();
        });
        self.fx_job = Some(rx);
        self.status = "소리 보정 중…".into();
    }

    fn finish_fx(&mut self, ctx: &egui::Context) {
        let Some(result) = self.fx_job.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.fx_job = None;
        match result {
            Ok((track, files)) => {
                let added = self.edit("소리 보정", |p| p.add_processed_track(track, files));
                if let (Some(new_track), Some(project)) = (added, &self.project) {
                    self.sound.load(project, ctx);
                    (self.selected_clip, self.selected_track) = (None, Some(new_track));
                }
            }
            Err(e) => self.status = format!("소리 보정 실패: {e}"),
        }
    }

    /// 선택한 트랙(없으면 선택한 클립의 트랙)의 음소거를 켜고 끈다.
    fn toggle_mute(&mut self) {
        let track = self.project.as_ref().zip(self.selected_track).and_then(|(p, id)| p.track(id));
        let Some((id, muted)) = track.map(|t| (t.id, t.muted)) else {
            self.status = "트랙을 먼저 선택한다".into();
            return;
        };
        self.edit(if muted { "음소거 해제" } else { "음소거" }, |p| p.set_muted(id, !muted));
    }

    /// 선택한 클립이 다른 클립과 연결돼 있는지.
    fn selection_linked(&self) -> bool {
        let clip = self.project.as_ref().zip(self.selected_clip).and_then(|(p, id)| p.clip(id).ok());
        clip.is_some_and(|c| c.link.is_some())
    }

    fn unlink(&mut self) {
        if !self.selection_linked() {
            self.status = "연결된 클립을 먼저 선택한다".into();
            return;
        }
        if let Some(id) = self.selected_clip {
            self.edit("연결 해제", |p| p.unlink(id));
        }
    }

    fn on_timeline(&mut self, action: Action) {
        let Some(project) = self.project.as_mut() else { return };
        match action {
            Action::Seek(t) => {
                self.playhead = project.snap(t).clamp(0, project.duration());
                self.playing = false;
            }
            Action::SelectClip(id) => {
                self.selected_clip = Some(id);
                self.selected_track = project.clip(id).ok().map(|c| c.track);
            }
            Action::SelectTrack(id) => (self.selected_clip, self.selected_track) = (None, Some(id)),
            Action::Deselect => (self.selected_clip, self.selected_track) = (None, None),
            Action::DragStart(clip, grab) => self.drag = Some(Drag::begin(project, clip, grab)),
            Action::DragTo(t) => {
                if let Some(drag) = &mut self.drag {
                    drag.update(project, t);
                }
            }
            Action::DragEnd => {
                if let Some(drag) = self.drag.take() {
                    let label = drag.grab.label();
                    match drag.finish(project, &mut self.history) {
                        Ok(true) => self.status = label.into(),
                        Ok(false) => {}
                        Err(e) => self.status = e.to_string(),
                    }
                    self.seek(self.playhead);
                }
            }
            Action::AddRegion(a, b) => {
                let (a, b) = (project.snap(a), project.snap(b));
                if b > a {
                    self.regions.push((a, b));
                    self.region = Some(self.regions.len() - 1);
                }
            }
            Action::SelectRegion(index) => self.region = Some(index),
            Action::MoveRegionEdge(index, is_end, t) => {
                // 시작과 끝이 서로를 넘어가지 않게 한다
                let t = project.snap(t);
                match self.regions.get_mut(index) {
                    Some(region) if is_end => region.1 = t.max(region.0),
                    Some(region) => region.0 = t.min(region.1),
                    None => {}
                }
            }
            Action::SplitRegion(index) => self.split_region(index),
            Action::CutRegion(index) => self.cut_region(index),
            Action::RemoveRegion(index) => {
                if index < self.regions.len() {
                    self.regions.remove(index);
                    self.region = None;
                }
            }
            Action::SwapClip(id, forward) => {
                self.edit("순서 변경", |p| p.swap_with_neighbor(id, forward));
            }
            Action::Unlink(id) => {
                self.selected_clip = Some(id);
                self.unlink();
            }
            Action::Rename(track, name) => {
                self.edit("트랙 이름 바꾸기", |p| p.rename_track(track, &name));
            }
        }
    }

    fn undo(&mut self) {
        if let Some(project) = self.project.as_mut() {
            self.status = self.history.undo(project).map_or("되돌릴 것이 없다".into(), |l| format!("되돌림: {l}"));
            self.seek(self.playhead);
        }
    }

    fn redo(&mut self) {
        if let Some(project) = self.project.as_mut() {
            self.status = self.history.redo(project).map_or("다시 할 것이 없다".into(), |l| format!("다시 함: {l}"));
        }
    }

    fn save(&mut self) {
        if let Some(project) = &self.project {
            self.status = match project.save(&self.project_path) {
                Ok(()) => "프로젝트를 저장했다".into(),
                Err(e) => format!("저장 실패: {e}"),
            };
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let pressed = |key: Key| ctx.input(|i| i.key_pressed(key));
        let (shift, command) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
        if pressed(Key::Space) {
            self.toggle_play();
        }
        for (key, direction) in [(Key::ArrowLeft, -1), (Key::ArrowRight, 1)] {
            if pressed(key) {
                if shift {
                    self.step_seconds(direction);
                } else {
                    self.step_frames(direction);
                }
            }
        }
        if pressed(Key::I) {
            self.mark_edge(false);
        }
        if pressed(Key::O) {
            self.mark_edge(true);
        }
        if pressed(Key::Delete) {
            self.cut_region(self.region.unwrap_or(usize::MAX));
        }
        if pressed(Key::M) {
            self.toggle_mute();
        }
        if pressed(Key::U) {
            self.unlink();
        }
        if command && pressed(Key::Z) {
            self.undo();
        } else if command && pressed(Key::Y) {
            self.redo();
        } else if command && pressed(Key::S) {
            self.save();
        }
    }

    /// 재생 위치에 맞는 프레임을 디코더에 요청하고, 도착한 프레임을 화면 텍스처에 올린다.
    fn update_preview(&mut self, ctx: &egui::Context) {
        let Some(project) = &self.project else { return };
        let lookup = if self.playhead >= project.duration() { self.playhead - 1 } else { self.playhead };
        let at = video_at(project, lookup);
        self.view_crop = at.map(|(clip, video)| (clip.id, clip.crop, video.width, video.height));
        let target = at.map(|(clip, video)| (clip.media, video.grid.frame_at(clip.src_at(lookup))));
        if target != self.wanted {
            self.wanted = target;
            if let Some((media, frame)) = target {
                if !self.decoders.contains_key(&media) {
                    let repaint = ctx.clone();
                    let decoder = project
                        .media_info(media)
                        .and_then(|info| Decoder::new(info, self.preview_height, move || repaint.request_repaint()));
                    if let Some(decoder) = decoder {
                        self.decoders.insert(media, decoder);
                    }
                }
                if let Some(decoder) = self.decoders.get(&media) {
                    decoder.request(frame);
                }
            }
        }
        let Some((media, _)) = self.wanted else { return };
        let Some(frame) = self.decoders.get(&media).and_then(Decoder::poll) else { return };
        let image = egui::ColorImage::from_rgb([frame.width as usize, frame.height as usize], &frame.rgb);
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR)),
        }
        self.shown = Some((media, frame.frame));
    }

    fn mixer(&mut self, ui: &mut egui::Ui) {
        ui.heading("믹서");
        let Some(project) = &self.project else { return };
        let mut actions = Vec::new();
        for track in project.timeline.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
            ui.separator();
            ui.label(&track.name);
            ui.horizontal(|ui| {
                let (mut muted, mut solo) = (track.muted, track.solo);
                if ui.toggle_value(&mut muted, "음소거").changed() {
                    actions.push(MixerAction::Mute(track.id, muted));
                }
                if ui.toggle_value(&mut solo, "솔로").changed() {
                    actions.push(MixerAction::Solo(track.id, solo));
                }
            });
            let mut volume = track.volume_db;
            let slider = egui::Slider::new(&mut volume, VOLUME_MIN_DB..=VOLUME_MAX_DB).suffix(" dB").fixed_decimals(1);
            if ui.add(slider).changed() {
                actions.push(MixerAction::Volume(track.id, volume));
            }
        }
        for action in actions {
            match action {
                MixerAction::Mute(id, on) => self.edit("음소거", |p| p.set_muted(id, on)),
                MixerAction::Solo(id, on) => self.edit("솔로", |p| p.set_solo(id, on)),
                // 슬라이더를 끄는 동안의 값들은 Undo 한 단계로 묶는다
                MixerAction::Volume(id, db) => {
                    self.edit_keyed("볼륨", &format!("volume:{id}"), |p| p.set_volume(id, db).map(|_| ()));
                    None
                }
            };
        }
    }

    /// `--shot`: 화면이 준비되면 PNG로 저장하고 끝낸다.
    fn handle_shot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.options.shot.clone() else { return };
        ctx.request_repaint();
        self.frames_drawn += 1;
        let played = !self.options.play || self.playhead >= 2 * FLICKS_PER_SEC;
        let ready = self.loading.is_none() && self.adding.is_none() && self.queue.is_empty() && self.sound.pending == 0 && played && (self.wanted.is_none() || self.shown == self.wanted || self.playing);
        if !self.shot_sent && ((ready && self.frames_drawn > 10) || self.frames_drawn > 1800) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            self.shot_sent = true;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let (width, height) = (image.size[0] as u32, image.size[1] as u32);
            if let Err(e) = image::save_buffer(&path, image.as_raw(), width, height, image::ExtendedColorType::Rgba8) {
                eprintln!("화면 저장 실패: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for TrackcutApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.finish_loading(ctx);
        self.finish_adding(ctx);
        self.finish_fx(ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        self.queue.extend(dropped);
        self.next_file(ctx);
        self.handle_keys(ctx);

        let now = Instant::now();
        if let Some(error) = self.project.as_ref().and_then(|project| self.sound.update(project)) {
            self.status = error;
        }
        if self.playing != self.sound_playing {
            if self.playing {
                self.sound.play(self.playhead);
            } else {
                self.sound.pause();
            }
            self.sound_playing = self.playing;
        }
        if self.playing {
            // 화면은 소리가 실제로 나간 위치를 따라간다. 출력 장치가 없으면 시계로 진행한다.
            match self.sound.position() {
                Some(position) => self.playhead = position,
                None => self.playhead += secs_to_flicks(now.duration_since(self.last_tick).as_secs_f64()),
            }
            if self.playhead >= self.duration() {
                self.playhead = self.duration();
                self.playing = false;
            }
            ctx.request_repaint();
        }
        self.last_tick = now;
        self.update_preview(ctx);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));
        egui::SidePanel::left("inspector").default_width(250.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui));
        });
        egui::TopBottomPanel::bottom("timeline").resizable(true).default_height(300.0).show(ctx, |ui| {
            let Some(project) = &self.project else { return };
            let labels = timeline::clip_labels(project);
            let marks = Marks {
                playhead: self.playhead,
                regions: &self.regions,
                region: self.region,
                clip: self.selected_clip,
                track: self.selected_track,
                labels: &labels,
            };
            for action in timeline::show(ui, project, &mut self.view, &marks, &self.sound.waves) {
                self.on_timeline(action);
            }
        });
        egui::SidePanel::right("mixer").default_width(230.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.mixer(ui));
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| self.preview(ui));

        let project_path = self.project_path.clone();
        self.export.show(ctx, self.project.as_ref(), &project_path);
        self.handle_shot(ctx);
    }
}
