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
use crate::regions::{play_span, Regions};
use crate::sound::Sound;
use crate::timeline::{self, Action, Marks, View};
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

/// 시각 `t`에 화면에 보이는 영상 클립. `edit`이면 편집본 묶음에서, 아니면 원본 묶음에서 찾는다.
fn video_at(project: &Project, t: Flicks, edit: bool) -> Option<(&Clip, &VideoStream)> {
    let shown = |track: &trackcut_core::project::Track| track.kind == TrackKind::Video && track.is_edit() == edit;
    let clip = project.timeline.clips.iter().find(|c| c.start <= t && t < c.end() && project.track(c.track).is_some_and(shown))?;
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
    /// 재생을 멈출 위치(구역을 선택한 채 재생하면 그 구역의 끝)
    play_until: Option<Flicks>,
    last_tick: Instant,
    view: View,
    /// 타임라인에 지정한 구역들과 그 선택
    regions: Regions,
    /// 미리보기와 소리가 편집본 묶음을 따르는 중인지(아니면 원본 묶음)
    edit_view: bool,
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
    /// 실행 인자로 받은 확인용 동작을 이미 실행했는지
    startup_done: bool,
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
            play_until: None,
            last_tick: Instant::now(),
            view: View::default(),
            regions: Regions::default(),
            edit_view: false,
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
            startup_done: false,
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
                (self.wanted, self.shown) = (None, None);
                self.regions.clear();
                self.crop_mode = self.options.crop;
                for [a, b] in self.options.regions.clone() {
                    self.regions.add((secs_to_flicks(a), secs_to_flicks(b)));
                }
                self.regions.select(0, false);
                (self.playing, self.startup_done, self.edit_view) = (false, false, false);
                self.view = View::default();
                self.status = "열었다".into();
            }
            Err(e) => self.status = format!("열 수 없음: {e}"),
        }
    }

    /// 실행 인자로 받은 확인용 동작. 파일을 전부 올린 뒤에 한 번 실행한다.
    fn startup_actions(&mut self) {
        let idle = self.loading.is_none() && self.adding.is_none() && self.queue.is_empty();
        if !idle || self.project.is_none() || self.startup_done {
            return;
        }
        self.startup_done = true;
        if self.options.merge {
            for index in 0..self.regions.list().len() {
                self.regions.select(index, index > 0);
            }
            self.on_timeline(Action::MergeRegions);
        }
        if self.options.split {
            self.split_region(0);
        }
        if self.options.to_edit {
            for index in 0..self.regions.list().len() {
                self.copy_region(index);
            }
        }
        if self.options.edit_view {
            self.watch(true);
        }
        if self.options.play {
            self.start_play();
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
        let next = match video_at(project, probe_at, self.edit_view) {
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
        if self.playing || self.project.is_none() {
            self.playing = false;
        } else {
            self.start_play();
        }
    }

    /// 재생을 시작한다. 구역을 선택해 두었으면 그 구역의 끝에서 멈춘다.
    fn start_play(&mut self) {
        let region = self.regions.current().and_then(|i| self.regions.get(i));
        // 보고 있는 묶음(원본 또는 편집본)의 끝까지만 재생한다
        let end = self.project.as_ref().map_or(0, |p| p.group_duration(self.edit_view));
        let (from, until) = play_span(self.playhead, region, end);
        (self.playhead, self.play_until) = (from, Some(until.unwrap_or(end)));
        self.playing = true;
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

    /// 구역의 양 끝에서 클립을 나눠, 구역 안쪽이 따로 떨어진 하나의 구간이 되게 한다.
    /// 구역이 여러 영상에 걸쳐 있어도 그 안의 조각들은 한 묶음으로 선택되고 움직인다.
    fn split_region(&mut self, index: usize) {
        let Some((a, b)) = self.regions.get(index) else { return };
        let edit = self.edit_view;
        let done = self.edit("구역 분할", |p| {
            let tracks = p.group_tracks(edit);
            p.split_range(a, b, &tracks)
        });
        // 나뉜 구간을 바로 고칠 수 있게 선택한다
        if let (Some(pieces), Some(project)) = (done, &self.project) {
            let mut clips: Vec<&Clip> = pieces.iter().filter_map(|&id| project.clip(id).ok()).collect();
            clips.sort_by_key(|c| (project.track(c.track).is_none_or(|t| t.kind != TrackKind::Video), c.start));
            if let Some(clip) = clips.first() {
                (self.selected_clip, self.selected_track) = (Some(clip.id), Some(clip.track));
            }
        }
    }

    /// 구역의 내용을 잘라내고 뒤를 당겨 붙인다. 뒤쪽 구역들도 함께 당긴다.
    fn cut_region(&mut self, index: usize) {
        let Some((a, b)) = self.regions.get(index) else {
            self.status = "삭제할 구역을 먼저 고른다".into();
            return;
        };
        // 보고 있는 묶음(원본 또는 편집본)에서만 지운다
        let edit = self.edit_view;
        let cut = self.edit("구역 삭제", |p| {
            let tracks = p.group_tracks(edit);
            p.delete_range_in(a, b, &tracks, true)
        });
        if cut.is_none() {
            return;
        }
        self.regions.after_cut(index);
        self.seek(a);
    }

    /// 미리보기와 소리가 따를 묶음을 바꾼다(편집본이면 true).
    pub(crate) fn watch(&mut self, edit: bool) {
        if self.edit_view != edit {
            self.edit_view = edit;
            self.playing = false;
            self.status = if edit { "편집본을 본다".into() } else { "원본을 본다".into() };
        }
    }

    /// 원본의 범위 `[a, b)`를 편집본의 `at`에 복사한다. `at`이 없으면 편집본의 맨 끝에 붙인다.
    fn copy_to_edit(&mut self, a: Flicks, b: Flicks, at: Option<Flicks>) {
        let copied = self.edit("편집본에 복사", |p| {
            let at = at.map_or(p.group_duration(true), |t| p.snap(t));
            p.copy_to_edit(p.snap(a), p.snap(b), at)
        });
        if copied.is_some() {
            self.status = "편집본에 복사했다. 원본은 그대로다".into();
        }
    }

    /// 구역의 원본 내용을 편집본의 맨 끝에 복사한다.
    fn copy_region(&mut self, index: usize) {
        match self.regions.get(index) {
            Some((a, b)) => self.copy_to_edit(a, b, None),
            None => self.status = "복사할 구역을 먼저 고른다".into(),
        }
    }

    /// 선택한 구역의 시작(또는 끝)을 지금 위치로 옮긴다. 선택한 구역이 없으면 지금 위치에서 2초짜리를 만든다.
    fn mark_edge(&mut self, is_end: bool) {
        let (t, total, span) = (self.playhead, self.duration(), 2 * FLICKS_PER_SEC);
        match self.regions.current() {
            Some(index) => self.regions.set_edge(index, is_end, t),
            None => self.regions.add(if is_end { ((t - span).max(0), t) } else { (t, (t + span).min(total)) }),
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
            // 클립이나 트랙을 고르면 미리보기도 그 묶음(원본 또는 편집본)을 따라간다
            Action::SelectClip(id) => {
                self.selected_clip = Some(id);
                self.selected_track = project.clip(id).ok().map(|c| c.track);
                if let Some(edit) = project.clip(id).ok().map(|c| project.in_edit(c)) {
                    self.watch(edit);
                }
            }
            Action::SelectTrack(id) => {
                (self.selected_clip, self.selected_track) = (None, Some(id));
                if let Some(edit) = project.track(id).map(|t| t.is_edit()) {
                    self.watch(edit);
                }
            }
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
            Action::DragCancel => {
                if let Some(drag) = self.drag.take() {
                    drag.cancel(project);
                }
            }
            Action::CopyToEdit(a, b, at) => self.copy_to_edit(a, b, Some(at)),
            Action::CopyRegion(index) => self.copy_region(index),
            Action::AddRegion(a, b) => self.regions.add((project.snap(a), project.snap(b))),
            Action::SelectRegion(index, extend) => self.regions.select(index, extend),
            Action::DeselectRegions => self.regions.deselect(),
            Action::MergeRegions => match self.regions.merge() {
                Some(_) => self.status = "구역 병합".into(),
                None => self.status = "Shift를 누른 채 구역을 둘 이상 고른 뒤 병합한다".into(),
            },
            Action::MoveRegionEdge(index, is_end, t) => self.regions.set_edge(index, is_end, project.snap(t)),
            Action::SplitRegion(index) => self.split_region(index),
            Action::CutRegion(index) => self.cut_region(index),
            Action::RemoveRegion(index) => self.regions.remove(index),
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
            self.cut_region(self.regions.current().unwrap_or(usize::MAX));
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
        // 보고 있는 묶음의 맨 끝에서는 마지막 프레임을 보여 준다
        let end = project.group_duration(self.edit_view);
        let lookup = if self.playhead == end && end > 0 { self.playhead - 1 } else { self.playhead };
        let at = video_at(project, lookup, self.edit_view);
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
        // 구역 재생은 구역 끝에서 멈추므로, 멈춘 뒤의 화면을 저장한다
        let played = !self.options.play || (self.startup_done && !self.playing) || self.playhead >= 2 * FLICKS_PER_SEC;
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
        self.startup_actions();
        self.handle_keys(ctx);

        let now = Instant::now();
        if let Some(error) = self.project.as_ref().and_then(|project| self.sound.update(project, self.edit_view)) {
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
            let end = self.play_until.map_or(self.duration(), |t| t.min(self.duration()));
            if self.playhead >= end {
                self.playhead = end;
                self.playing = false;
            }
            ctx.request_repaint();
        } else {
            self.play_until = None;
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
                regions: self.regions.list(),
                picked: self.regions.picked(),
                edit_view: self.edit_view,
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
