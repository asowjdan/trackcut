//! 왼쪽 패널. 선택한 것에 따라 내용이 바뀐다: 영상 클립이면 영상 편집, 오디오면 오디오 편집.
//!
//! 숫자 칸은 끌거나 직접 입력해 고친다. 한 칸을 연달아 고친 것은 Undo 한 단계로 묶인다.

use eframe::egui::{self, Button, DragValue, RichText};
use trackcut_core::edit::Edge;
use trackcut_core::project::{Clip, Crop, Project, Track, TrackKind, SPEED_MAX_PCT, SPEED_MIN_PCT, VOLUME_MAX_DB, VOLUME_MIN_DB};
use trackcut_core::time::{floor_frame, frame_time, nearest_frame, timecode, Flicks};

use super::{TrackcutApp, PREVIEW_CHOICES};
use crate::timeline::{clip_labels, Action};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Properties,
    Order,
    Project,
    Keys,
}

const KEYS: [(&str, &str); 16] = [
    ("Space", "재생 / 정지"),
    ("← / →", "1프레임 이동"),
    ("Shift + ← / →", "1초 이동"),
    ("I / O", "선택한 구역의 시작 / 끝을 지금 위치로"),
    ("Delete", "선택한 구역 삭제"),
    ("M", "선택한 트랙 음소거"),
    ("U", "선택한 클립 연결 해제"),
    ("Ctrl+Z / Ctrl+Y", "되돌리기 / 다시 하기"),
    ("Ctrl+S", "프로젝트 저장"),
    ("Ctrl+휠 / 휠", "타임라인 확대·축소 / 좌우 이동"),
    ("타임라인 두 번 누르기", "구역 만들기"),
    ("구역 선 끌기", "구역 조절"),
    ("구역·클립 우클릭", "분할, 삭제, 순서 바꾸기"),
    ("클립 가운데 끌기", "이동"),
    ("클립 양 끝 끌기", "트림"),
    ("트랙 이름 두 번 누르기", "이름 바꾸기"),
];

/// 내보낼 화면 크기 선택지.
const SIZES: [(u32, u32); 4] = [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];

/// 이름과 값이 나란한 두 칸짜리 표.
fn grid(ui: &mut egui::Ui, id: &str, rows: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([12.0, 6.0]).show(ui, rows);
}

fn text_row(ui: &mut egui::Ui, name: &str, value: String) {
    ui.label(name);
    ui.label(value);
    ui.end_row();
}

impl TrackcutApp {
    pub(super) fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (tab, name) in [(Tab::Properties, "편집"), (Tab::Order, "순서"), (Tab::Project, "프로젝트"), (Tab::Keys, "단축키")] {
                ui.selectable_value(&mut self.tab, tab, name);
            }
        });
        ui.separator();
        if self.tab == Tab::Keys {
            grid(ui, "keys", |ui| {
                for (key, action) in KEYS {
                    ui.label(RichText::new(key).monospace());
                    ui.label(action);
                    ui.end_row();
                }
            });
            return;
        }
        let Some(project) = &self.project else {
            ui.weak("영상 파일을 열면 여기에 편집 기능이 나온다");
            return;
        };
        let clip = self.selected_clip.and_then(|id| project.clip(id).ok()).cloned();
        let track = self.selected_track.and_then(|id| project.track(id)).cloned();
        match self.tab {
            Tab::Project => self.project_section(ui),
            Tab::Order => self.order_section(ui),
            _ => {
                self.position_section(ui);
                ui.separator();
                self.region_section(ui);
                ui.separator();
                match (clip, track) {
                    (Some(clip), Some(track)) if track.kind == TrackKind::Video => self.video_section(ui, clip),
                    (clip, Some(track)) if track.kind == TrackKind::Audio => self.audio_section(ui, track, clip),
                    (_, Some(track)) => {
                        ui.strong("영상 트랙");
                        self.track_rows(ui, &track);
                    }
                    _ => {
                        ui.weak("영상 클립을 누르면 영상 편집, 오디오 클립이나 트랙 이름을 누르면 오디오 편집 기능이 나온다");
                    }
                }
            }
        }
    }

    fn position_section(&mut self, ui: &mut egui::Ui) {
        let Some(project) = &self.project else { return };
        let fps = project.settings.fps;
        let last = floor_frame(project.duration(), fps);
        let mut frame = floor_frame(self.playhead, fps);
        let mut go = None;
        ui.strong("재생 위치");
        grid(ui, "position", |ui| {
            ui.label("프레임");
            if ui.add(DragValue::new(&mut frame).range(0..=last)).changed() {
                go = Some(project.snap(frame_time(frame, fps)));
            }
            ui.end_row();
            text_row(ui, "시각", timecode(self.playhead, fps));
        });
        if let Some(t) = go {
            self.playing = false;
            self.seek(t);
        }
    }

    fn region_section(&mut self, ui: &mut egui::Ui) {
        let Some(project) = &self.project else { return };
        let fps = project.settings.fps;
        ui.strong("구역");
        if self.regions.is_empty() {
            ui.weak("타임라인을 두 번 누르면 구역이 생긴다. 선을 끌어 조절하고, 우클릭으로 분할·삭제한다");
            return;
        }
        let mut action = None;
        for (index, &(a, b)) in self.regions.iter().enumerate() {
            ui.horizontal(|ui| {
                let name = format!("구역 {}  {} ~ {}", index + 1, timecode(a, fps), timecode(b, fps));
                if ui.selectable_label(self.region == Some(index), name).clicked() {
                    action = Some(Action::SelectRegion(index));
                }
            });
            if self.region == Some(index) {
                ui.horizontal(|ui| {
                    if ui.small_button("분할").on_hover_text("구역의 양 끝에서 클립을 나눈다").clicked() {
                        action = Some(Action::SplitRegion(index));
                    }
                    if ui.small_button("삭제").on_hover_text("구역의 내용을 잘라내고 뒤를 당겨 붙인다").clicked() {
                        action = Some(Action::CutRegion(index));
                    }
                    if ui.small_button("표시 지우기").clicked() {
                        action = Some(Action::RemoveRegion(index));
                    }
                });
            }
        }
        if let Some(action) = action {
            self.on_timeline(action);
        }
    }

    /// 클립의 시작·길이·속도 칸(영상과 오디오 공통).
    fn clip_rows(&mut self, ui: &mut egui::Ui, clip: &Clip) {
        let Some(fps) = self.project.as_ref().map(|p| p.settings.fps) else { return };
        let id = clip.id;
        let frames = |t: Flicks| nearest_frame(t, fps);
        let (mut start, mut len, mut speed) = (frames(clip.start), frames(clip.len()), clip.speed_pct);
        let (mut moved, mut trimmed, mut sped) = (false, false, false);
        grid(ui, "clip", |ui| {
            ui.label("시작 (프레임)");
            moved = ui.add(DragValue::new(&mut start).range(0..=i64::MAX)).changed();
            ui.end_row();
            ui.label("길이 (프레임)");
            trimmed = ui.add(DragValue::new(&mut len).range(1..=i64::MAX)).changed();
            ui.end_row();
            ui.label("속도 (%)");
            ui.horizontal(|ui| {
                sped = ui.add(DragValue::new(&mut speed).range(SPEED_MIN_PCT..=SPEED_MAX_PCT)).changed();
                for preset in [50, 100, 200] {
                    if ui.small_button(format!("{preset}")).clicked() {
                        (speed, sped) = (preset, true);
                    }
                }
            });
            ui.end_row();
        });
        if moved {
            self.edit_keyed("클립 이동", &format!("move:{id}"), |p| p.move_clip(id, frame_time(start, fps)));
        }
        if trimmed {
            let end = clip.start + frame_time(len, fps);
            self.edit_keyed("트림", &format!("length:{id}"), |p| p.trim(id, Edge::End, end).map(|_| ()));
        }
        if sped {
            self.edit_keyed("재생 속도", &format!("speed:{id}"), |p| p.set_speed(id, speed));
        }
        if moved || trimmed || sped {
            self.seek(self.playhead);
        }
        ui.horizontal(|ui| {
            ui.label(if clip.link.is_some() { "다른 클립과 연결됨" } else { "연결 없음" });
            if clip.link.is_some() && ui.small_button("연결 해제").clicked() {
                self.unlink();
            }
        });
    }

    fn video_section(&mut self, ui: &mut egui::Ui, clip: Clip) {
        let Some(project) = &self.project else { return };
        let label = clip_labels(project).remove(&clip.id).unwrap_or_default();
        let source = project.media_info(clip.media).and_then(|m| m.video.as_ref()).map(|v| (v.width, v.height));
        let output = (project.settings.width, project.settings.height);
        ui.strong("영상 편집");
        ui.label(label);
        self.clip_rows(ui, &clip);

        ui.add_space(8.0);
        ui.strong("화면 자르기");
        let was_on = self.crop_mode;
        ui.toggle_value(&mut self.crop_mode, "미리보기에서 자르기");
        if self.crop_mode && !was_on && !(clip.start <= self.playhead && self.playhead < clip.end()) {
            // 자를 클립이 미리보기에 나오도록 재생 위치를 옮긴다
            self.playing = false;
            self.seek(clip.start);
        }
        if self.crop_mode {
            ui.weak("미리보기의 빨간 테두리에서 손잡이를 끌어 자르고, 가운데를 끌어 옮긴다");
        }
        if let Some((width, height)) = source {
            let crop = clip.crop;
            ui.label(format!("남는 화면  {} × {}", width.saturating_sub(crop.left + crop.right), height.saturating_sub(crop.top + crop.bottom)));
        }
        if !clip.crop.is_none() && ui.button("자르기 초기화").clicked() {
            let id = clip.id;
            self.edit("화면 자르기 초기화", |p| p.set_crop(id, Crop::default()));
        }

        ui.add_space(8.0);
        ui.strong("출력 해상도");
        let mut chosen = output;
        let name = |(w, h): (u32, u32)| format!("{w} × {h}");
        egui::ComboBox::from_id_salt("output-size").selected_text(name(output)).show_ui(ui, |ui| {
            if let Some(size) = source.filter(|s| !SIZES.contains(s)) {
                ui.selectable_value(&mut chosen, size, format!("{} (원본)", name(size)));
            }
            for size in SIZES {
                let text = if Some(size) == source { format!("{} (원본)", name(size)) } else { name(size) };
                ui.selectable_value(&mut chosen, size, text);
            }
        });
        ui.weak("내보내는 파일 전체의 화면 크기다. 키울 때는 Lanczos 방식으로 늘린다");
        if chosen != output {
            if let Some(project) = self.project.as_mut() {
                (project.settings.width, project.settings.height) = chosen;
                self.status = format!("출력 해상도 {}", name(chosen));
            }
        }
    }

    /// 트랙 이름과 소리 설정 칸.
    fn track_rows(&mut self, ui: &mut egui::Ui, track: &Track) {
        let id = track.id;
        let (mut name, mut volume, mut muted, mut solo) = (track.name.clone(), track.volume_db, track.muted, track.solo);
        let (mut renamed, mut volume_changed) = (false, false);
        grid(ui, "track", |ui| {
            ui.label("이름");
            renamed = ui.add(egui::TextEdit::singleline(&mut name).desired_width(130.0)).changed();
            ui.end_row();
            if track.kind == TrackKind::Audio {
                ui.label("볼륨 (dB)");
                let value = DragValue::new(&mut volume).range(VOLUME_MIN_DB..=VOLUME_MAX_DB).speed(0.1).fixed_decimals(1);
                volume_changed = ui.add(value).changed();
                ui.end_row();
                ui.label("소리");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut muted, "음소거");
                    ui.checkbox(&mut solo, "솔로");
                });
                ui.end_row();
            }
        });
        if renamed {
            self.edit_keyed("트랙 이름 바꾸기", &format!("name:{id}"), |p| p.rename_track(id, &name));
        }
        if volume_changed {
            self.edit_keyed("볼륨", &format!("volume:{id}"), |p| p.set_volume(id, volume).map(|_| ()));
        }
        if muted != track.muted {
            self.edit("음소거", |p| p.set_muted(id, muted));
        }
        if solo != track.solo {
            self.edit("솔로", |p| p.set_solo(id, solo));
        }
    }

    fn audio_section(&mut self, ui: &mut egui::Ui, track: Track, clip: Option<Clip>) {
        ui.strong("오디오 편집");
        self.track_rows(ui, &track);
        if let Some(clip) = &clip {
            ui.add_space(8.0);
            ui.strong("클립");
            self.clip_rows(ui, clip);
        }

        ui.add_space(8.0);
        ui.strong("소리 보정");
        let fx = &mut self.fx;
        ui.horizontal(|ui| {
            ui.checkbox(&mut fx.denoise, "노이즈 제거");
            ui.add_enabled(fx.denoise, DragValue::new(&mut fx.denoise_db).range(1.0..=40.0).speed(0.2).suffix(" dB"));
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut fx.highpass, "저음 잡음 차단");
            ui.add_enabled(fx.highpass, DragValue::new(&mut fx.highpass_hz).range(20..=1000).suffix(" Hz"));
        });
        ui.label("이퀄라이저");
        grid(ui, "equalizer", |ui| {
            for (name, value) in [("저음 (120Hz)", &mut fx.low_db), ("중음 (1.5kHz)", &mut fx.mid_db), ("고음 (6kHz)", &mut fx.high_db)] {
                ui.label(name);
                ui.add(DragValue::new(value).range(-12.0..=12.0).speed(0.1).fixed_decimals(1).suffix(" dB"));
                ui.end_row();
            }
        });
        let ready = self.fx.filter().is_some() && self.fx_job.is_none();
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, Button::new("보정한 새 트랙 만들기")).clicked() {
                self.start_fx(ui.ctx(), track.id);
            }
            if self.fx_job.is_some() {
                ui.spinner();
            }
        });
        ui.weak("원본 트랙은 그대로 두고 음소거만 한다. 보정한 소리는 바로 아래 새 트랙에 올라가서, 음소거·솔로로 번갈아 들으며 비교한다");
    }

    /// 클립 순서: 맨 위 트랙의 클립을 차례로 보여 주고, 앞뒤 순서를 바꾼다.
    fn order_section(&mut self, ui: &mut egui::Ui) {
        let Some(project) = &self.project else { return };
        let fps = project.settings.fps;
        let clips = &project.timeline.clips;
        let base = project.timeline.tracks.iter().find(|t| clips.iter().any(|c| c.track == t.id)).map(|t| t.id);
        let labels = clip_labels(project);
        let mut rows: Vec<(u64, Flicks, Flicks)> = clips.iter().filter(|c| Some(c.track) == base).map(|c| (c.id, c.start, c.len())).collect();
        rows.sort_by_key(|row| row.1);
        let linked = self.selected_clip.and_then(|id| project.group(id).ok()).unwrap_or_default();

        ui.strong("클립 순서");
        ui.weak("누르면 그 클립을 선택하고, 화살표로 앞뒤 순서를 바꾼다");
        let (mut select, mut swap) = (None, None);
        for (number, &(id, start, len)) in rows.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.add_enabled(number > 0, Button::new("▲").small()).clicked() {
                    swap = Some((id, false));
                }
                if ui.add_enabled(number + 1 < rows.len(), Button::new("▼").small()).clicked() {
                    swap = Some((id, true));
                }
                let label = labels.get(&id).cloned().unwrap_or_default();
                let text = format!("{}. {label}  {}  ({}프레임)", number + 1, timecode(start, fps), nearest_frame(len, fps));
                if ui.selectable_label(linked.contains(&id), text).clicked() {
                    select = Some((id, start));
                }
            });
        }
        if let Some((id, start)) = select {
            self.on_timeline(Action::SelectClip(id));
            self.playing = false;
            self.seek(start);
        }
        if let Some((id, forward)) = swap {
            self.on_timeline(Action::SelectClip(id));
            self.on_timeline(Action::SwapClip(id, forward));
        }
    }

    fn project_section(&mut self, ui: &mut egui::Ui) {
        let Some(project) = &self.project else { return };
        let (settings, fps) = (&project.settings, project.settings.fps);
        ui.strong("프로젝트");
        grid(ui, "project", |ui| {
            text_row(ui, "파일", file_names(project));
            text_row(ui, "화면", format!("{} × {}", settings.width, settings.height));
            text_row(ui, "프레임 속도", format!("{:.3} fps", fps.as_f64()));
            text_row(ui, "길이", timecode(project.duration(), fps));
            text_row(ui, "트랙 / 클립", format!("{} / {}", project.timeline.tracks.len(), project.timeline.clips.len()));
        });
        ui.separator();
        ui.strong("미리보기");
        let mut height = self.preview_height;
        grid(ui, "preview", |ui| {
            ui.label("화질");
            let current = PREVIEW_CHOICES.iter().find(|c| c.0 == height).map_or("원본", |c| c.1);
            egui::ComboBox::from_id_salt("preview-quality").selected_text(current).show_ui(ui, |ui| {
                for (value, name) in PREVIEW_CHOICES {
                    ui.selectable_value(&mut height, value, name);
                }
            });
            ui.end_row();
        });
        ui.weak("재생이 끊기면 화질을 낮춘다. 내보내는 파일의 화질과는 상관없다.");
        if height != self.preview_height {
            self.set_preview_height(height);
        }
    }
}

/// 프로젝트에 들어 있는 미디어 파일 이름(폴더 경로는 빼고).
fn file_names(project: &Project) -> String {
    let names = project.media.iter().map(|m| {
        std::path::Path::new(&m.info.path).file_name().map_or_else(|| m.info.path.clone(), |n| n.to_string_lossy().into_owned())
    });
    names.collect::<Vec<_>>().join(", ")
}
