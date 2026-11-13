//! 상단 메뉴바와 미리보기. 미리보기에서는 화면 자르기를 손잡이로 조절한다.

use eframe::egui::{self, pos2, vec2, Button, Color32, CursorIcon, Rect, RichText, Sense, Stroke};
use trackcut_core::project::Crop;
use trackcut_core::time::{floor_frame, timecode};

use super::TrackcutApp;

/// 자르고 남는 화면의 최소 크기(픽셀). 코어의 검사와 같은 값이다.
const MIN_CROPPED: i64 = 16;
const CROP_COLOR: Color32 = Color32::from_rgb(235, 60, 60);

impl TrackcutApp {
    /// 왼쪽은 파일, 가운데는 재생, 오른쪽은 지금 위치.
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let open = self.project.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(open, Button::new("저장")).on_hover_text("Ctrl+S").clicked() {
                self.save();
            }
            if ui.add_enabled(open, Button::new("내보내기")).clicked() {
                self.export.open = true;
            }
            ui.separator();
            if ui.add_enabled(self.history.can_undo(), Button::new("되돌리기")).on_hover_text("Ctrl+Z").clicked() {
                self.undo();
            }
            if ui.add_enabled(self.history.can_redo(), Button::new("다시 하기")).on_hover_text("Ctrl+Y").clicked() {
                self.redo();
            }

            let center = Rect::from_center_size(full.center(), vec2(236.0, full.height()));
            let layout = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(center).layout(layout), |ui| {
                if ui.button("◀ 프레임").on_hover_text("←").clicked() {
                    self.step_frames(-1);
                }
                let play = Button::new(if self.playing { "정지" } else { "재생" }).min_size(vec2(64.0, 0.0));
                if ui.add(play).on_hover_text("Space").clicked() {
                    self.toggle_play();
                }
                if ui.button("프레임 ▶").on_hover_text("→").clicked() {
                    self.step_frames(1);
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(project) = &self.project {
                    let fps = project.settings.fps;
                    ui.label(format!("프레임 {}", floor_frame(self.playhead, fps)));
                    ui.label(RichText::new(timecode(self.playhead, fps)).monospace().size(16.0));
                    ui.separator();
                }
                let status = if self.sound.pending > 0 {
                    format!("소리 준비 중 ({}개 남음)", self.sound.pending)
                } else if let Some(error) = &self.sound.engine_error {
                    format!("소리 출력 불가: {error}")
                } else {
                    self.status.clone()
                };
                ui.add(egui::Label::new(RichText::new(status).weak()).truncate());
            });
        });
    }

    pub(super) fn preview(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        let Some(project) = &self.project else {
            let text = if self.loading.is_some() { "파일 분석 중…" } else { "영상 파일을 이 창에 끌어다 놓으세요 (여러 개를 놓으면 차례로 이어 붙는다)" };
            ui.centered_and_justified(|ui| ui.label(RichText::new(text).size(18.0).color(Color32::GRAY)));
            return;
        };
        let Some(texture) = self.texture.as_ref().filter(|_| self.wanted.is_some()) else { return };
        let size = texture.size_vec2();
        let (clip, crop, width, height) = self.view_crop.unwrap_or((0, Crop::default(), size.x as u32, size.y as u32));
        let (width, height) = (width.max(1) as f32, height.max(1) as f32);
        let fit = |inner: egui::Vec2, outer: Rect| {
            Rect::from_center_size(outer.center(), inner * (outer.width() / inner.x).min(outer.height() / inner.y))
        };
        let frame = fit(vec2(project.settings.width.max(1) as f32, project.settings.height.max(1) as f32), area);
        let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

        if !(self.crop_mode && self.selected_clip == Some(clip)) {
            // 자른 화면만 텍스처에서 골라, 내보낼 때처럼 비율을 지켜 프로젝트 화면 안에 맞춘다
            let uv = Rect::from_min_max(
                pos2(crop.left as f32 / width, crop.top as f32 / height),
                pos2(1.0 - crop.right as f32 / width, 1.0 - crop.bottom as f32 / height),
            );
            let rect = fit(vec2(width * uv.width(), height * uv.height()), frame);
            ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
            return;
        }

        // 자르기 편집: 원본 화면 전체를 보여 주고, 남길 부분을 빨간 테두리로 표시한다
        let full = fit(vec2(width, height), area.shrink(14.0));
        let scale = full.width() / width;
        ui.painter().image(texture.id(), full, whole, Color32::WHITE);
        let kept = Rect::from_min_max(
            full.min + vec2(crop.left as f32, crop.top as f32) * scale,
            full.max - vec2(crop.right as f32, crop.bottom as f32) * scale,
        );
        let shade = Color32::from_black_alpha(170);
        for outside in [
            Rect::from_min_max(full.min, pos2(full.right(), kept.top())),
            Rect::from_min_max(pos2(full.left(), kept.bottom()), full.max),
            Rect::from_min_max(pos2(full.left(), kept.top()), pos2(kept.left(), kept.bottom())),
            Rect::from_min_max(pos2(kept.right(), kept.top()), pos2(full.right(), kept.bottom())),
        ] {
            ui.painter().rect_filled(outside, 0.0, shade);
        }
        ui.painter().rect_stroke(kept, 0.0, Stroke::new(2.0_f32, CROP_COLOR));

        let (source_width, source_height) = (width as i64, height as i64);
        let (left, top, right, bottom) = (crop.left as i64, crop.top as i64, crop.right as i64, crop.bottom as i64);
        let mut next = (left, top, right, bottom);
        let to_source = |screen: f32| (screen / scale).round() as i64;

        // 가운데를 끌면 남길 부분을 통째로 옮긴다
        let body = ui.interact(kept.shrink(10.0), ui.id().with("crop-body"), Sense::drag());
        if body.dragged() {
            let delta = body.drag_delta();
            let (dx, dy) = (to_source(delta.x).clamp(-left, right), to_source(delta.y).clamp(-top, bottom));
            next = (left + dx, top + dy, right - dx, bottom - dy);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if body.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        // 손잡이 8개: 모서리는 두 변을, 변의 가운데는 한 변을 움직인다
        for (index, (hx, hy)) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)].into_iter().enumerate() {
            let pick = |side: i32, low: f32, high: f32| match side {
                -1 => low,
                1 => high,
                _ => (low + high) * 0.5,
            };
            let center = pos2(pick(hx, kept.left(), kept.right()), pick(hy, kept.top(), kept.bottom()));
            let handle = ui.interact(Rect::from_center_size(center, vec2(18.0, 18.0)), ui.id().with(("crop-handle", index)), Sense::drag());
            ui.painter().rect_filled(Rect::from_center_size(center, vec2(10.0, 10.0)), 1.0, CROP_COLOR);
            ui.painter().rect_stroke(Rect::from_center_size(center, vec2(10.0, 10.0)), 1.0, Stroke::new(1.0_f32, Color32::WHITE));
            if handle.hovered() || handle.dragged() {
                ui.ctx().set_cursor_icon(match (hx, hy) {
                    (0, _) => CursorIcon::ResizeVertical,
                    (_, 0) => CursorIcon::ResizeHorizontal,
                    (-1, -1) | (1, 1) => CursorIcon::ResizeNwSe,
                    _ => CursorIcon::ResizeNeSw,
                });
            }
            if handle.dragged() {
                let delta = handle.drag_delta();
                let (dx, dy) = (to_source(delta.x), to_source(delta.y));
                match hx {
                    -1 => next.0 = (left + dx).clamp(0, source_width - right - MIN_CROPPED),
                    1 => next.2 = (right - dx).clamp(0, source_width - left - MIN_CROPPED),
                    _ => {}
                }
                match hy {
                    -1 => next.1 = (top + dy).clamp(0, source_height - bottom - MIN_CROPPED),
                    1 => next.3 = (bottom - dy).clamp(0, source_height - top - MIN_CROPPED),
                    _ => {}
                }
            }
        }
        let remaining = format!("{} × {}", source_width - next.0 - next.2, source_height - next.1 - next.3);
        ui.painter().text(kept.left_top() + vec2(6.0, 6.0), egui::Align2::LEFT_TOP, remaining, egui::FontId::proportional(12.0), Color32::WHITE);

        if next != (left, top, right, bottom) {
            let crop = Crop { left: next.0 as u32, top: next.1 as u32, right: next.2 as u32, bottom: next.3 as u32 };
            self.edit_keyed("화면 자르기", &format!("crop:{clip}"), |p| p.set_crop(clip, crop));
        }
    }
}
