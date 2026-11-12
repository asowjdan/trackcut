//! 편집 화면의 상태와 화면 구성.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use eframe::egui::{self, pos2, Color32, Key, Rect, RichText};
use trackcut_core::decode::Decoder;
use trackcut_core::history::History;
use trackcut_core::labels::TracksMeta;
use trackcut_core::media::VideoStream;
use trackcut_core::probe::probe;
use trackcut_core::project::{Clip, Project, TrackKind, VOLUME_MAX_DB, VOLUME_MIN_DB};
use trackcut_core::time::{floor_frame, frame_time, parse_time, secs_to_flicks, timecode, Flicks, FLICKS_PER_SEC};

use crate::export_ui::ExportUi;
use crate::sound::Sound;
use crate::timeline::{self, Marks, View};
use crate::Options;

/// 미리보기는 이 높이 이하로 줄여서 디코딩한다.
const PREVIEW_HEIGHT: u32 = 540;

type Loaded = Result<(Project, PathBuf), String>;

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
    playhead: Flicks,
    playing: bool,
    last_tick: Instant,
    view: View,
    mark_in: Option<Flicks>,
    mark_out: Option<Flicks>,
    decoders: HashMap<u64, Decoder>,
    texture: Option<egui::TextureHandle>,
    /// 지금 보여야 하는 (미디어, 프레임)과 실제로 화면에 올라온 것
    wanted: Option<(u64, u64)>,
    shown: Option<(u64, u64)>,
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
            playhead: 0,
            playing: false,
            last_tick: Instant::now(),
            view: View::default(),
            mark_in: None,
            mark_out: None,
            decoders: HashMap::new(),
            texture: None,
            wanted: None,
            shown: None,
            status: String::new(),
            sound: Sound::new(),
            sound_playing: false,
            export: ExportUi::default(),
            frames_drawn: 0,
            shot_sent: false,
            options,
        };
        if let Some(path) = app.options.input.clone() {
            app.open(&ctx, path);
        }
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
            Some((clip, video)) => {
                let base = video.grid.frame_at(clip.src_in + (probe_at - clip.start)) as i64;
                let frame = if n < 0 { base + n + 1 } else { base + n };
                let src = video.grid.time_of(frame.max(0) as u64).clamp(clip.src_in, clip.src_out);
                clip.start + (src - clip.src_in)
            }
            None => {
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
        let target = video_at(project, lookup).map(|(clip, video)| {
            (clip.media, video.grid.frame_at(clip.src_in + (lookup - clip.start)))
        });
        if target != self.wanted {
            self.wanted = target;
            if let Some((media, frame)) = target {
                if !self.decoders.contains_key(&media) {
                    let repaint = ctx.clone();
                    let decoder = project
                        .media_info(media)
                        .and_then(|info| Decoder::new(info, PREVIEW_HEIGHT, move || repaint.request_repaint()));
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
                // 슬라이더를 끄는 동안 Undo 단계가 쌓이지 않게 볼륨은 바로 적용한다
                MixerAction::Volume(id, db) => self.project.as_mut().and_then(|p| p.set_volume(id, db).ok()).map(|_| ()),
            };
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        match (&self.project, &self.texture) {
            (Some(_), Some(texture)) if self.wanted.is_some() => {
                let size = texture.size_vec2();
                let scale = (area.width() / size.x).min(area.height() / size.y);
                let rect = Rect::from_center_size(area.center(), size * scale);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
            }
            (Some(_), _) => {}
            (None, _) => {
                let text = if self.loading.is_some() { "파일 분석 중…" } else { "영상 파일을 이 창에 끌어다 놓으세요" };
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
        let ready = self.loading.is_none() && self.sound.pending == 0 && played && (self.wanted.is_none() || self.shown == self.wanted || self.playing);
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
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(path) = dropped {
            self.open(ctx, path);
        }
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
        egui::TopBottomPanel::bottom("timeline").resizable(true).default_height(300.0).show(ctx, |ui| {
            let Some(project) = &self.project else { return };
            let marks = Marks { playhead: self.playhead, range: (self.mark_in, self.mark_out) };
            if let Some(t) = timeline::show(ui, project, &mut self.view, &marks, &self.sound.waves) {
                self.playhead = project.snap(t).clamp(0, project.duration());
                self.playing = false;
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
