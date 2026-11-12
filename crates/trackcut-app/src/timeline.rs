//! 타임라인 그리기와 조작: 휠 확대·축소, 스크롤, 클릭·드래그로 재생 위치 이동.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use trackcut_core::project::{Project, TrackKind};
use trackcut_core::time::{flicks_to_secs, secs_to_flicks, timecode, Flicks};

const HEADER_WIDTH: f32 = 130.0;
const RULER_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 36.0;
const MIN_PX_PER_SEC: f32 = 0.2;
/// 가장 크게 확대하면 60fps 기준 1프레임이 40px
const MAX_PX_PER_SEC: f32 = 2400.0;

pub struct View {
    pub px_per_sec: f32,
    /// 화면 왼쪽 끝의 시각(초)
    pub scroll: f64,
    /// 다음에 그릴 때 전체 길이가 보이도록 맞춘다
    pub fit: bool,
}

impl Default for View {
    fn default() -> Self {
        View { px_per_sec: 50.0, scroll: 0.0, fit: true }
    }
}

pub struct Marks {
    pub playhead: Flicks,
    pub range: (Option<Flicks>, Option<Flicks>),
}

/// 타임라인을 그린다. 사용자가 클릭하거나 드래그한 시각이 있으면 돌려준다(프레임 맞춤 전).
pub fn show(ui: &mut egui::Ui, project: &Project, view: &mut View, marks: &Marks) -> Option<Flicks> {
    let rows = project.timeline.tracks.len().max(1) as f32;
    let size = vec2(ui.available_width(), (RULER_HEIGHT + rows * ROW_HEIGHT).max(ui.available_height()));
    let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    let rect = response.rect;
    let lane = Rect::from_min_max(pos2(rect.left() + HEADER_WIDTH, rect.top()), rect.max);
    let duration = flicks_to_secs(project.duration()).max(0.001);

    if view.fit {
        view.px_per_sec = ((lane.width() - 20.0) / duration as f32).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        view.scroll = 0.0;
        view.fit = false;
    }

    if response.hovered() {
        let (zoom, scroll, pointer) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
        if zoom != 1.0 {
            // Ctrl+휠: 커서 아래의 시각이 그대로 있도록 확대·축소
            let x = pointer.map_or(lane.center().x, |p| p.x) - lane.left();
            let anchor = view.scroll + (x / view.px_per_sec) as f64;
            view.px_per_sec = (view.px_per_sec * zoom).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
            view.scroll = anchor - (x / view.px_per_sec) as f64;
        } else if scroll != egui::Vec2::ZERO {
            view.scroll -= ((scroll.x + scroll.y) / view.px_per_sec) as f64;
        }
    }
    let max_scroll = (duration - (lane.width() * 0.5 / view.px_per_sec) as f64).max(0.0);
    view.scroll = view.scroll.clamp(0.0, max_scroll);

    let x_of = |t: Flicks| lane.left() + ((flicks_to_secs(t) - view.scroll) * view.px_per_sec as f64) as f32;
    let style = ui.visuals();
    painter.rect_filled(rect, 0.0, style.extreme_bg_color);

    let lanes = painter.with_clip_rect(lane);
    draw_ruler(&lanes, lane, view, project);

    // 선택 구간(시작점~끝점)
    if let (Some(a), Some(b)) = marks.range {
        let shade = Rect::from_min_max(pos2(x_of(a), lane.top()), pos2(x_of(b), lane.bottom()));
        lanes.rect_filled(shade, 0.0, Color32::from_rgba_unmultiplied(255, 200, 60, 40));
    }
    for mark in [marks.range.0, marks.range.1].into_iter().flatten() {
        let x = x_of(mark);
        lanes.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(1.5_f32,Color32::from_rgb(255, 200, 60)));
    }

    for (row, track) in project.timeline.tracks.iter().enumerate() {
        let top = rect.top() + RULER_HEIGHT + row as f32 * ROW_HEIGHT;
        let audible = project.is_audible(track.id);
        let color = match (track.kind, audible) {
            (TrackKind::Video, _) => Color32::from_rgb(70, 110, 190),
            (TrackKind::Audio, true) => Color32::from_rgb(60, 150, 110),
            (TrackKind::Audio, false) => Color32::from_gray(90),
        };
        lanes.line_segment(
            [pos2(lane.left(), top + ROW_HEIGHT), pos2(lane.right(), top + ROW_HEIGHT)],
            Stroke::new(1.0_f32,style.faint_bg_color),
        );
        for clip in project.timeline.clips.iter().filter(|c| c.track == track.id) {
            let (left, right) = (x_of(clip.start), x_of(clip.end()));
            if right < lane.left() || left > lane.right() {
                continue;
            }
            let body = Rect::from_min_max(pos2(left, top + 3.0), pos2(right.max(left + 1.0), top + ROW_HEIGHT - 3.0));
            lanes.rect_filled(body, 3.0, color);
            lanes.rect_stroke(body, 3.0, Stroke::new(1.0_f32,Color32::from_black_alpha(140)));
        }
        // 트랙 이름
        let header = Rect::from_min_size(pos2(rect.left(), top), vec2(HEADER_WIDTH, ROW_HEIGHT));
        painter.rect_filled(header, 0.0, style.panel_fill);
        let name_color = if audible || track.kind == TrackKind::Video { style.text_color() } else { style.weak_text_color() };
        painter.with_clip_rect(header.shrink(4.0)).text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            &track.name,
            FontId::proportional(13.0),
            name_color,
        );
    }

    // 재생 위치
    let x = x_of(marks.playhead);
    lanes.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(2.0_f32,Color32::from_rgb(235, 70, 70)));

    let pointer = response.interact_pointer_pos().filter(|p| p.x >= lane.left());
    if response.clicked() || response.dragged() {
        return pointer.map(|p| {
            let secs = view.scroll + ((p.x - lane.left()) / view.px_per_sec) as f64;
            secs_to_flicks(secs.clamp(0.0, duration))
        });
    }
    None
}

/// 눈금과 타임코드. 확대 수준에 맞는 간격을 고르고, 충분히 확대하면 프레임 선도 그린다.
fn draw_ruler(painter: &egui::Painter, lane: Rect, view: &View, project: &Project) {
    let fps = project.settings.fps;
    let frame = fps.den as f64 / fps.num as f64;
    let steps = [frame, frame * 5.0, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0];
    let step = steps.into_iter().find(|s| s * view.px_per_sec as f64 >= 90.0).unwrap_or(3600.0);
    let text_color = Color32::from_gray(170);
    let x_of = |secs: f64| lane.left() + ((secs - view.scroll) * view.px_per_sec as f64) as f32;

    if frame * view.px_per_sec as f64 >= 8.0 {
        let mut n = (view.scroll / frame).floor();
        while x_of(n * frame) <= lane.right() {
            let x = x_of(n * frame);
            painter.line_segment([pos2(x, lane.top() + RULER_HEIGHT), pos2(x, lane.bottom())], Stroke::new(1.0_f32,Color32::from_gray(45)));
            n += 1.0;
        }
    }
    let mut n = (view.scroll / step).floor();
    while x_of(n * step) <= lane.right() {
        let secs = n * step;
        let x = x_of(secs);
        painter.line_segment([pos2(x, lane.top()), pos2(x, lane.top() + RULER_HEIGHT)], Stroke::new(1.0_f32,Color32::from_gray(110)));
        painter.text(
            pos2(x + 4.0, lane.top() + RULER_HEIGHT * 0.5),
            Align2::LEFT_CENTER,
            timecode(secs_to_flicks(secs), fps),
            FontId::monospace(11.0),
            text_color,
        );
        n += 1.0;
    }
}
