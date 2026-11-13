//! 왼쪽 패널: 재생 위치, 선택한 클립·트랙의 수치, 프로젝트 정보와 미리보기 설정, 단축키.
//!
//! 숫자 칸은 끌거나 직접 입력해 고친다. 한 칸을 연달아 고친 것은 Undo 한 단계로 묶인다.

use eframe::egui::{self, DragValue, RichText};
use trackcut_core::edit::Edge;
use trackcut_core::project::{Crop, Project, TrackKind, SPEED_MAX_PCT, SPEED_MIN_PCT, VOLUME_MAX_DB, VOLUME_MIN_DB};
use trackcut_core::time::{floor_frame, frame_time, nearest_frame, timecode, Flicks};

use super::{TrackcutApp, PREVIEW_CHOICES};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Properties,
    Project,
    Keys,
}

const KEYS: [(&str, &str); 14] = [
    ("Space", "재생 / 정지"),
    ("← / →", "1프레임 이동"),
    ("Shift + ← / →", "1초 이동"),
    ("I / O", "시작점 / 끝점"),
    ("S", "현재 위치에서 분할"),
    ("Delete", "시작점~끝점 구간 삭제"),
    ("M", "선택한 트랙 음소거"),
    ("U", "선택한 클립 연결 해제"),
    ("Ctrl+Z / Ctrl+Y", "되돌리기 / 다시 하기"),
    ("Ctrl+S", "프로젝트 저장"),
    ("Ctrl+휠 / 휠", "타임라인 확대·축소 / 좌우 이동"),
    ("클립 가운데 끌기", "이동"),
    ("클립 양 끝 끌기", "트림"),
    ("트랙 이름 두 번 누르기", "이름 바꾸기"),
];

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
            for (tab, name) in [(Tab::Properties, "속성"), (Tab::Project, "프로젝트"), (Tab::Keys, "단축키")] {
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
        if self.project.is_none() {
            ui.weak("영상 파일을 열면 여기에 수치가 나온다");
            return;
        }
        match self.tab {
            Tab::Project => self.project_section(ui),
            _ => {
                self.position_section(ui);
                ui.separator();
                self.clip_section(ui);
                ui.separator();
                self.track_section(ui);
            }
        }
    }

    fn position_section(&mut self, ui: &mut egui::Ui) {
        let Some(project) = &self.project else { return };
        let fps = project.settings.fps;
        let last = floor_frame(project.duration(), fps);
        let mut frame = floor_frame(self.playhead, fps);
        let mut go = None;
        let (mut set_in, mut set_out) = (None, None);
        ui.strong("재생 위치");
        grid(ui, "position", |ui| {
            ui.label("프레임");
            if ui.add(DragValue::new(&mut frame).range(0..=last)).changed() {
                go = Some(frame_time(frame, fps));
            }
            ui.end_row();
            text_row(ui, "시각", timecode(self.playhead, fps));
            for (name, mark, set) in [("시작점", self.mark_in, &mut set_in), ("끝점", self.mark_out, &mut set_out)] {
                ui.label(name);
                ui.horizontal(|ui| {
                    ui.label(mark.map_or("없음".to_string(), |t| timecode(t, fps)));
                    if ui.small_button("여기로").clicked() {
                        *set = Some(Some(self.playhead));
                    }
                    if mark.is_some() && ui.small_button("지우기").clicked() {
                        *set = Some(None);
                    }
                });
                ui.end_row();
            }
        });
        if let Some(t) = go {
            let t = project.snap(t);
            self.playing = false;
            self.seek(t);
        }
        if let Some(mark) = set_in {
            self.mark_in = mark;
        }
        if let Some(mark) = set_out {
            self.mark_out = mark;
        }
    }

    fn clip_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("클립");
        let Some(project) = &self.project else { return };
        let Some(clip) = self.selected_clip.and_then(|id| project.clip(id).ok()).cloned() else {
            ui.weak("타임라인에서 클립을 누르면 수치를 고칠 수 있다");
            return;
        };
        let fps = project.settings.fps;
        let id = clip.id;
        let frames = |t: Flicks| nearest_frame(t, fps);
        let video = project.track(clip.track).filter(|t| t.kind == TrackKind::Video).and_then(|_| project.media_info(clip.media)?.video.as_ref());
        let size = video.map(|v| (v.width, v.height));
        let track_name = project.track(clip.track).map(|t| t.name.clone()).unwrap_or_default();

        let (mut start, mut len, mut speed, mut crop) = (frames(clip.start), frames(clip.len()), clip.speed_pct, clip.crop);
        let (mut moved, mut trimmed, mut sped, mut cropped) = (false, false, false, false);
        grid(ui, "clip", |ui| {
            text_row(ui, "트랙", track_name);
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
        if let Some((width, height)) = size {
            ui.add_space(6.0);
            ui.label("화면 자르기 (픽셀)");
            grid(ui, "crop", |ui| {
                let sides = [("왼쪽", &mut crop.left, width), ("위", &mut crop.top, height), ("오른쪽", &mut crop.right, width), ("아래", &mut crop.bottom, height)];
                for (name, value, full) in sides {
                    ui.label(name);
                    cropped |= ui.add(DragValue::new(value).range(0..=full.saturating_sub(16))).changed();
                    ui.end_row();
                }
                text_row(ui, "남는 화면", format!("{} × {}", width.saturating_sub(crop.left + crop.right), height.saturating_sub(crop.top + crop.bottom)));
            });
            if !clip.crop.is_none() && ui.button("자르기 초기화").clicked() {
                (crop, cropped) = (Crop::default(), true);
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(if clip.link.is_some() { "다른 클립과 연결됨" } else { "연결 없음" });
            if clip.link.is_some() && ui.small_button("연결 해제").clicked() {
                self.unlink();
            }
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
        if cropped {
            self.edit_keyed("화면 자르기", &format!("crop:{id}"), |p| p.set_crop(id, crop));
        }
        if moved || trimmed || sped {
            self.seek(self.playhead);
        }
    }

    fn track_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("트랙");
        let Some(track) = self.project.as_ref().zip(self.selected_track).and_then(|(p, id)| p.track(id)).cloned() else {
            ui.weak("트랙 이름을 누르면 수치를 고칠 수 있다");
            return;
        };
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
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("저장").clicked() {
                self.save();
            }
            if ui.button("내보내기").clicked() {
                self.export.open = true;
            }
        });
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
