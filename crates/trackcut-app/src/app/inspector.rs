//! 편집 패널. 선택한 것에 따라 내용이 바뀐다: 영상 클립이면 영상 편집, 오디오면 오디오 편집
//! (둘의 내용은 `edit_tab`에 있다). 그 밖에 재생 위치, 구역, 클립 순서, 프로젝트 탭이 있다.

use eframe::egui::{self, Button, DragValue, RichText};
use trackcut_core::project::{Project, TrackKind};
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

const KEYS: [(&str, &str); 20] = [
    ("Space", "재생 / 정지 (구역을 선택했으면 그 구역의 끝까지만)"),
    ("← / →", "1프레임 이동"),
    ("Shift + ← / →", "1초 이동"),
    ("I / O", "선택한 구역의 시작 / 끝을 지금 위치로"),
    ("Delete", "선택한 구역 삭제"),
    ("M", "선택한 트랙의 소리 끄기 / 켜기"),
    ("U", "선택한 클립 연결 해제"),
    ("Ctrl+Z / Ctrl+Y", "되돌리기 / 다시 하기"),
    ("Ctrl+S", "프로젝트 저장"),
    ("Ctrl+휠 / 휠", "타임라인 확대·축소 / 좌우 이동"),
    ("타임라인 두 번 누르기", "구역 만들기"),
    ("구역 선 끌기", "구역 조절"),
    ("Shift + 구역 누르기", "구역 여러 개 선택"),
    ("여러 구역 우클릭", "병합"),
    ("구역·클립 우클릭", "분할, 편집본에 복사, 삭제, 순서 바꾸기"),
    ("원본 구간을 편집본으로 끌기", "그 자리에 복사"),
    ("클립 가운데 끌기", "이동"),
    ("클립 양 끝 끌기", "트림"),
    ("트랙 이름 두 번 누르기", "이름 바꾸기"),
    ("트랙 이름 옆 \"켜짐 / 꺼짐\"", "그 트랙의 소리 켜기 / 끄기"),
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
        if self.regions.list().is_empty() {
            ui.weak("타임라인을 두 번 누르면 구역이 생긴다. 선을 끌어 조절하고, 우클릭으로 분할·삭제한다");
            return;
        }
        let mut action = None;
        let several = self.regions.picked().len() >= 2;
        for (index, &(a, b)) in self.regions.list().iter().enumerate() {
            ui.horizontal(|ui| {
                let name = format!("구역 {}  {} ~ {}", index + 1, timecode(a, fps), timecode(b, fps));
                if ui.selectable_label(self.regions.is_picked(index), name).clicked() {
                    action = Some(Action::SelectRegion(index, ui.input(|i| i.modifiers.shift)));
                }
            });
            if !several && self.regions.is_picked(index) {
                ui.horizontal(|ui| {
                    if ui.small_button("분할").on_hover_text("구역의 양 끝에서 클립을 나눈다").clicked() {
                        action = Some(Action::SplitRegion(index));
                    }
                    if ui.small_button("복사").on_hover_text("구역의 원본 내용을 편집본의 맨 끝에 복사한다").clicked() {
                        action = Some(Action::CopyRegion(index));
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
        if several {
            let count = self.regions.picked().len();
            if ui.button(format!("선택한 구역 {count}개 병합")).on_hover_text("가장 이른 시작부터 가장 늦은 끝까지를 하나의 구역으로 합친다").clicked() {
                action = Some(Action::MergeRegions);
            }
        } else if self.regions.picked().len() == 1 {
            ui.weak("재생하면 이 구역의 끝에서 멈춘다. Shift를 누른 채 다른 구역을 누르면 함께 선택된다");
        }
        if let Some(action) = action {
            self.on_timeline(action);
        }
    }

    /// 클립 순서: 맨 위 트랙의 클립을 차례로 보여 주고, 앞뒤 순서를 바꾼다.
    fn order_section(&mut self, ui: &mut egui::Ui) {
        let Some(project) = &self.project else { return };
        let fps = project.settings.fps;
        let clips = &project.timeline.clips;
        // 보고 있는 묶음(원본 또는 편집본)의 맨 위 트랙을 기준으로 한다
        let side = project.timeline.tracks.iter().filter(|t| t.is_edit() == self.edit_view);
        let base = side.clone().find(|t| clips.iter().any(|c| c.track == t.id)).map(|t| t.id);
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
