//! 편집 화면의 상태와 화면 구성.

mod inspector;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use eframe::egui::{self, pos2, Color32, Key, Rect, RichText};
use trackcut_core::decode::Decoder;
use trackcut_core::history::History;
use trackcut_core::labels::TracksMeta;
use trackcut_core::media::{MediaInfo, VideoStream};
use trackcut_core::probe::probe;
use trackcut_core::project::{Clip, Crop, Project, TrackKind, VOLUME_MAX_DB, VOLUME_MIN_DB};
use trackcut_core::time::{floor_frame, frame_time, parse_time, secs_to_flicks, timecode, Flicks, FLICKS_PER_SEC};

use crate::drag::Drag;
use crate::export_ui::ExportUi;
use crate::sound::Sound;
use crate::timeline::{self, Action, Marks, View};
use crate::Options;

/// 미리보기 화질 선택지: (디코딩할 최대 높이, 이름). 원본은 줄이지 않는다.
const PREVIEW_CHOICES: [(u32, &str); 3] = [(u32::MAX, "원본"), (720, "720p"), (540, "540p (빠름)")];

type Loaded = Result<(Project, PathBuf), String>;
type Added = Result<(MediaInfo, Option<TracksMeta>), String>;

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
    mark_in: Option<Flicks>,
    mark_out: Option<Flicks>,
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
    /// 지금 보이는 클립의 화면 자르기와 원본 해상도
    view_crop: Option<(Crop, u32, u32)>,
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
            mark_in: None,
            mark_out: None,
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
                (self.wanted, self.shown, self.mark_in, self.mark_out) = (None, None, None, None);
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

    fn split(&mut self) {
        let t = self.playhead;
        self.edit("분할", |p| p.split_at(t, None));
    }

    fn delete_marked(&mut self) {
        let (Some(a), Some(b)) = (self.mark_in, self.mark_out) else {
            self.status = "시작점(I)과 끝점(O)을 먼저 지정한다".into();
            return;
        };
        if self.edit("구간 삭제", |p| p.delete_range(a, b, None, true)).is_some() {
            (self.mark_in, self.mark_out) = (None, None);
            self.seek(a);
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
            Action::SetMarks(a, b) => (self.mark_in, self.mark_out) = (Some(project.snap(a)), Some(project.snap(b))),
            Action::MoveMark(is_out, t) => {
                // 두 선이 서로를 넘어가지 않게 한다
                let t = project.snap(t);
                if is_out {
                    self.mark_out = Some(t.max(self.mark_in.unwrap_or(0)));
                } else {
                    self.mark_in = Some(t.min(self.mark_out.unwrap_or(Flicks::MAX)));
                }
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
            self.mark_in = Some(self.playhead);
        }
        if pressed(Key::O) {
            self.mark_out = Some(self.playhead);
        }
        if pressed(Key::Delete) {
            self.delete_marked();
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
        } else if pressed(Key::S) {
            self.split();
        }
    }

    /// 재생 위치에 맞는 프레임을 디코더에 요청하고, 도착한 프레임을 화면 텍스처에 올린다.
    fn update_preview(&mut self, ctx: &egui::Context) {
        let Some(project) = &self.project else { return };
        let lookup = if self.playhead >= project.duration() { self.playhead - 1 } else { self.playhead };
        let at = video_at(project, lookup);
        self.view_crop = at.map(|(clip, video)| (clip.crop, video.width, video.height));
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

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "정지" } else { "재생" }).on_hover_text("Space").clicked() {
                self.toggle_play();
            }
            if ui.button("◀ 프레임").on_hover_text("←").clicked() {
                self.step_frames(-1);
            }
            if ui.button("프레임 ▶").on_hover_text("→").clicked() {
                self.step_frames(1);
            }
            ui.separator();
            if ui.button("시작점").on_hover_text("I").clicked() {
                self.mark_in = Some(self.playhead);
            }
            if ui.button("끝점").on_hover_text("O").clicked() {
                self.mark_out = Some(self.playhead);
            }
            if ui.button("분할").on_hover_text("S").clicked() {
                self.split();
            }
            if ui.button("구간 삭제").on_hover_text("Delete").clicked() {
                self.delete_marked();
            }
            if ui.add_enabled(self.selection_linked(), egui::Button::new("연결 해제")).on_hover_text("U").clicked() {
                self.unlink();
            }
            ui.separator();
            if ui.add_enabled(self.history.can_undo(), egui::Button::new("되돌리기")).on_hover_text("Ctrl+Z").clicked() {
                self.undo();
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("다시 하기")).on_hover_text("Ctrl+Y").clicked() {
                self.redo();
            }
            if ui.button("저장").on_hover_text("Ctrl+S").clicked() {
                self.save();
            }
            if ui.add_enabled(self.project.is_some(), egui::Button::new("내보내기")).clicked() {
                self.export.open = true;
            }
            ui.separator();
            if let Some(project) = &self.project {
                let fps = project.settings.fps;
                ui.label(RichText::new(timecode(self.playhead, fps)).monospace().size(16.0));
                ui.label(format!("프레임 {}", floor_frame(self.playhead, fps)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.sound.pending > 0 {
                    ui.weak(format!("소리 준비 중 ({}개 남음)", self.sound.pending));
                } else if let Some(error) = &self.sound.engine_error {
                    ui.weak(format!("소리 출력 불가: {error}"));
                } else {
                    ui.weak(&self.status);
                }
            });
        });
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

    fn preview(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        match (&self.project, &self.texture) {
            (Some(project), Some(texture)) if self.wanted.is_some() => {
                let size = texture.size_vec2();
                let (crop, width, height) = self.view_crop.unwrap_or((Crop::default(), size.x as u32, size.y as u32));
                let (width, height) = (width.max(1) as f32, height.max(1) as f32);
                // 자른 화면만 텍스처에서 골라, 내보낼 때처럼 비율을 지켜 프로젝트 화면 안에 맞춘다
                let uv = Rect::from_min_max(
                    pos2(crop.left as f32 / width, crop.top as f32 / height),
                    pos2(1.0 - crop.right as f32 / width, 1.0 - crop.bottom as f32 / height),
                );
                let fit = |inner: egui::Vec2, outer: Rect| {
                    Rect::from_center_size(outer.center(), inner * (outer.width() / inner.x).min(outer.height() / inner.y))
                };
                let frame = fit(egui::vec2(project.settings.width.max(1) as f32, project.settings.height.max(1) as f32), area);
                let rect = fit(egui::vec2(width * uv.width(), height * uv.height()), frame);
                ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
            }
            (Some(_), _) => {}
            (None, _) => {
                let text = if self.loading.is_some() { "파일 분석 중…" } else { "영상 파일을 이 창에 끌어다 놓으세요 (여러 개를 놓으면 차례로 이어 붙는다)" };
                ui.centered_and_justified(|ui| ui.label(RichText::new(text).size(18.0).color(Color32::GRAY)));
            }
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
            let marks = Marks {
                playhead: self.playhead,
                range: (self.mark_in, self.mark_out),
                clip: self.selected_clip,
                track: self.selected_track,
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
