//! 왼쪽 패널 "편집" 탭의 영상 편집과 오디오 편집.
//!
//! 편집을 처음 하는 사람도 쓸 수 있게, 값마다 슬라이더(옆의 숫자 칸에 직접 입력할 수도 있다)와
//! 미리 정한 값 버튼을 함께 둔다. 한 값을 연달아 고친 것은 Undo 한 단계로 묶인다.

use eframe::egui::{self, Button, DragValue, Slider};
use trackcut_core::edit::Edge;
use trackcut_core::fx::AudioFx;
use trackcut_core::project::{Clip, Crop, Track, TrackKind, SPEED_MAX_PCT, SPEED_MIN_PCT, VOLUME_MAX_DB, VOLUME_MIN_DB};
use trackcut_core::time::{flicks_to_secs, frame_time, nearest_frame, timecode, Flicks};

use super::TrackcutApp;
use crate::timeline::clip_labels;

/// 내보낼 화면 크기 선택지.
const SIZES: [(&str, (u32, u32)); 4] = [("720p", (1280, 720)), ("1080p", (1920, 1080)), ("1440p", (2560, 1440)), ("4K", (3840, 2160))];
/// 자르고 남는 화면의 최소 크기(픽셀). 코어의 검사와 같은 값이다.
const MIN_CROPPED: u32 = 16;

/// 미리 정한 값 버튼 한 줄. 지금 값과 같은 버튼은 눌린 모양이 되고, 누른 값을 돌려준다.
fn presets<T: Copy + PartialEq>(ui: &mut egui::Ui, current: T, choices: &[(&str, T)]) -> Option<T> {
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        for &(name, value) in choices {
            if ui.selectable_label(current == value, name).clicked() {
                chosen = Some(value);
            }
        }
    });
    chosen
}

/// 원본 화면의 가운데를 `ratio`(가로:세로) 비율로 남기는 자르기.
fn center_crop(width: u32, height: u32, ratio: (u32, u32)) -> Crop {
    let (rw, rh) = (ratio.0.max(1) as u64, ratio.1.max(1) as u64);
    let (w, h) = (width as u64, height as u64);
    // 짝수 크기로 맞춘다(영상 인코더는 홀수 크기를 받지 않는다)
    let even = |n: u64| (n - n % 2) as u32;
    if w * rh > h * rw {
        let cut = width - even(h * rw / rh).min(width);
        Crop { left: cut / 2, right: cut - cut / 2, ..Crop::default() }
    } else {
        let cut = height - even(w * rh / rw).min(height);
        Crop { top: cut / 2, bottom: cut - cut / 2, ..Crop::default() }
    }
}

/// 소리 보정의 미리 정한 설정.
fn fx_presets() -> [(&'static str, AudioFx); 4] {
    let off = AudioFx { denoise: false, denoise_db: 12.0, highpass: false, highpass_hz: 80, low_db: 0.0, mid_db: 0.0, high_db: 0.0 };
    [
        ("기본", AudioFx::default()),
        ("목소리 또렷하게", AudioFx { denoise: true, highpass: true, highpass_hz: 100, low_db: -2.0, mid_db: 2.0, high_db: 3.0, ..off.clone() }),
        ("잡음 많이 줄이기", AudioFx { denoise: true, denoise_db: 24.0, highpass: true, highpass_hz: 120, ..off.clone() }),
        ("보정 안 함", off),
    ]
}

impl TrackcutApp {
    /// 클립의 시작·길이·속도(영상과 오디오 공통).
    fn clip_rows(&mut self, ui: &mut egui::Ui, clip: &Clip) {
        let Some(fps) = self.project.as_ref().map(|p| p.settings.fps) else { return };
        let id = clip.id;
        let frames = |t: Flicks| nearest_frame(t, fps);
        let (mut start, mut len, mut speed) = (frames(clip.start), frames(clip.len()), clip.speed_pct);
        let (mut moved, mut trimmed) = (false, false);
        // 숫자 칸 옆의 화살표: 1프레임씩 줄이고 늘린다
        let nudge = |ui: &mut egui::Ui, value: &mut i64, least: i64, changed: &mut bool| {
            for (text, step, hint) in [("◀", -1, "1프레임 줄이기"), ("▶", 1, "1프레임 늘리기")] {
                if ui.small_button(text).on_hover_text(hint).clicked() {
                    (*value, *changed) = ((*value + step).max(least), true);
                }
            }
        };

        ui.label("시작 위치");
        ui.horizontal(|ui| {
            moved = ui.add(DragValue::new(&mut start).range(0..=i64::MAX).suffix(" 프레임")).changed();
            nudge(ui, &mut start, 0, &mut moved);
            ui.weak(timecode(clip.start, fps));
        });
        ui.label("길이");
        ui.horizontal(|ui| {
            trimmed = ui.add(DragValue::new(&mut len).range(1..=i64::MAX).suffix(" 프레임")).changed();
            nudge(ui, &mut len, 1, &mut trimmed);
            ui.weak(format!("{:.2}초", flicks_to_secs(clip.len())));
        });
        ui.label("재생 속도");
        let mut sped = ui.add(Slider::new(&mut speed, SPEED_MIN_PCT..=SPEED_MAX_PCT).suffix(" %")).changed();
        if let Some(preset) = presets(ui, speed, &[("느리게 0.5배", 50), ("보통", 100), ("1.5배", 150), ("2배", 200), ("4배", 400)]) {
            (speed, sped) = (preset, true);
        }
        ui.weak("100%가 원래 속도다. 클수록 빨라지고 소리도 높아진다");

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
            let hint = "연결을 풀면 이 클립만 따로 옮기거나 자를 수 있다";
            if clip.link.is_some() && ui.small_button("연결 해제").on_hover_text(hint).clicked() {
                self.unlink();
            }
        });
    }

    pub(super) fn video_section(&mut self, ui: &mut egui::Ui, clip: Clip) {
        let Some(project) = &self.project else { return };
        let label = clip_labels(project).remove(&clip.id).unwrap_or_default();
        let source = project.media_info(clip.media).and_then(|m| m.video.as_ref()).map(|v| (v.width, v.height));
        let output = (project.settings.width, project.settings.height);
        let id = clip.id;
        ui.strong("영상 편집");
        ui.label(label);
        self.clip_rows(ui, &clip);

        ui.add_space(8.0);
        ui.strong("화면 자르기");
        let was_on = self.crop_mode;
        ui.toggle_value(&mut self.crop_mode, "미리보기에서 끌어서 자르기");
        if self.crop_mode && !was_on && !(clip.start <= self.playhead && self.playhead < clip.end()) {
            // 자를 클립이 미리보기에 나오도록 재생 위치를 옮긴다
            self.playing = false;
            self.seek(clip.start);
        }
        if self.crop_mode {
            ui.weak("미리보기의 빨간 테두리에서 손잡이를 끌어 자르고, 가운데를 끌어 옮긴다");
        }
        if let Some((width, height)) = source {
            let mut crop = clip.crop;
            let mut shapes = vec![("자르지 않기", Crop::default())];
            for (name, ratio) in [("16:9", (16, 9)), ("정사각형", (1, 1)), ("세로 9:16", (9, 16))] {
                // 원본이 이미 그 비율이면 자를 것이 없으니 버튼을 내지 않는다
                let shape = center_crop(width, height, ratio);
                if !shape.is_none() {
                    shapes.push((name, shape));
                }
            }
            let mut changed = false;
            if let Some(preset) = presets(ui, crop, &shapes) {
                (crop, changed) = (preset, true);
            }
            // 한 변을 늘릴 수 있는 한계는 맞은편에서 자른 양에 따라 달라진다
            let room = |total: u32, other: u32| total.saturating_sub(other + MIN_CROPPED);
            let now = crop;
            let sides = [
                ("왼쪽", room(width, now.right), &mut crop.left),
                ("오른쪽", room(width, now.left), &mut crop.right),
                ("위", room(height, now.bottom), &mut crop.top),
                ("아래", room(height, now.top), &mut crop.bottom),
            ];
            egui::Grid::new("crop-sides").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (name, most, value) in sides {
                    ui.label(name);
                    changed |= ui.add(Slider::new(value, 0..=most).suffix(" px")).changed();
                    ui.end_row();
                }
            });
            ui.label(format!("남는 화면  {} × {}", width.saturating_sub(crop.left + crop.right), height.saturating_sub(crop.top + crop.bottom)));
            if changed {
                self.edit_keyed("화면 자르기", &format!("crop:{id}"), |p| p.set_crop(id, crop));
            }
        }

        ui.add_space(8.0);
        ui.strong("내보낼 화면 크기");
        let mut choices: Vec<(&str, (u32, u32))> = SIZES.to_vec();
        if let Some(size) = source.filter(|size| SIZES.iter().all(|s| s.1 != *size)) {
            choices.insert(0, ("원본 크기", size));
        }
        let chosen = presets(ui, output, &choices);
        ui.weak(format!("지금 {} × {}. 내보내는 파일 전체의 화면 크기다. 원본보다 키우면 Lanczos 방식으로 늘린다", output.0, output.1));
        if let (Some(size), Some(project)) = (chosen.filter(|size| *size != output), self.project.as_mut()) {
            (project.settings.width, project.settings.height) = size;
            self.status = format!("내보낼 화면 크기 {} × {}", size.0, size.1);
        }
    }

    /// 트랙 이름과 소리 설정.
    pub(super) fn track_rows(&mut self, ui: &mut egui::Ui, track: &Track) {
        let id = track.id;
        let (mut name, mut volume, mut muted, mut solo) = (track.name.clone(), track.volume_db, track.muted, track.solo);
        ui.horizontal(|ui| {
            ui.label("이름");
            if ui.add(egui::TextEdit::singleline(&mut name).desired_width(150.0)).changed() {
                self.edit_keyed("트랙 이름 바꾸기", &format!("name:{id}"), |p| p.rename_track(id, &name));
            }
        });
        if track.kind != TrackKind::Audio {
            return;
        }
        ui.label("볼륨");
        let slider = Slider::new(&mut volume, VOLUME_MIN_DB..=VOLUME_MAX_DB).suffix(" dB").fixed_decimals(1);
        let mut changed = ui.add(slider).changed();
        if let Some(preset) = presets(ui, volume, &[("조용히", -12.0), ("조금 작게", -6.0), ("원래 크기", 0.0), ("크게", 6.0)]) {
            (volume, changed) = (preset, true);
        }
        ui.weak("0 dB가 녹음된 그대로의 크기다");
        if changed {
            self.edit_keyed("볼륨", &format!("volume:{id}"), |p| p.set_volume(id, volume).map(|_| ()));
        }
        ui.horizontal(|ui| {
            ui.toggle_value(&mut muted, "소리 끄기").on_hover_text("이 트랙의 소리를 끈다 (M)");
            ui.toggle_value(&mut solo, "이 트랙만 듣기").on_hover_text("다른 트랙을 모두 끄고 이 트랙만 듣는다");
        });
        if muted != track.muted {
            self.edit(if muted { "음소거" } else { "음소거 해제" }, |p| p.set_muted(id, muted));
        }
        if solo != track.solo {
            self.edit("이 트랙만 듣기", |p| p.set_solo(id, solo));
        }
    }

    pub(super) fn audio_section(&mut self, ui: &mut egui::Ui, track: Track, clip: Option<Clip>) {
        ui.strong("오디오 편집");
        self.track_rows(ui, &track);
        if let Some(clip) = &clip {
            ui.add_space(8.0);
            ui.strong("클립");
            self.clip_rows(ui, clip);
        }

        ui.add_space(8.0);
        ui.strong("소리 보정");
        ui.weak("잘 모르겠으면 아래에서 하나를 고르고 \"보정한 새 트랙 만들기\"를 누른다");
        ui.horizontal_wrapped(|ui| {
            for (name, preset) in fx_presets() {
                if ui.selectable_label(self.fx == preset, name).clicked() {
                    self.fx = preset;
                }
            }
        });
        let fx = &mut self.fx;
        ui.checkbox(&mut fx.denoise, "노이즈 제거").on_hover_text("\"쉬―\" 하는 바탕 잡음을 줄인다. 값이 클수록 많이 줄인다");
        ui.add_enabled(fx.denoise, Slider::new(&mut fx.denoise_db, 1.0..=40.0).suffix(" dB").fixed_decimals(0));
        ui.checkbox(&mut fx.highpass, "저음 잡음 차단").on_hover_text("이 주파수보다 낮은 웅웅거리는 소리를 걸러낸다");
        ui.add_enabled(fx.highpass, Slider::new(&mut fx.highpass_hz, 20..=1000).suffix(" Hz"));
        ui.label("음색 (이퀄라이저)");
        egui::Grid::new("equalizer").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for (name, value) in [("저음", &mut fx.low_db), ("중음", &mut fx.mid_db), ("고음", &mut fx.high_db)] {
                ui.label(name);
                ui.add(Slider::new(value, -12.0..=12.0).suffix(" dB").fixed_decimals(1));
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
        ui.weak("원본 트랙은 그대로 두고 소리만 끈다. 보정한 소리는 바로 아래 새 트랙에 올라가서, 번갈아 켜고 끄며 비교한다");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_crop_keeps_the_middle_at_the_given_ratio() {
        assert_eq!(center_crop(1920, 1080, (16, 9)), Crop::default(), "이미 16:9면 자르지 않는다");
        assert_eq!(center_crop(1920, 1080, (1, 1)), Crop { left: 420, right: 420, top: 0, bottom: 0 });
        // 세로 9:16은 폭 607.5가 되므로 짝수 606으로 맞춘다
        assert_eq!(center_crop(1920, 1080, (9, 16)), Crop { left: 657, right: 657, top: 0, bottom: 0 });
        assert_eq!(center_crop(1080, 1920, (16, 9)), Crop { left: 0, right: 0, top: 657, bottom: 657 });
        assert_eq!(center_crop(2560, 1080, (16, 9)), Crop { left: 320, right: 320, top: 0, bottom: 0 });
    }

    #[test]
    fn every_fx_preset_except_off_changes_the_sound() {
        let presets = fx_presets();
        assert!(presets[..3].iter().all(|(_, fx)| fx.filter().is_some()));
        assert!(presets[3].1.filter().is_none(), "\"보정 안 함\"은 아무것도 바꾸지 않는다");
    }
}
