//! 타임라인 그리기와 조작: 휠 확대·축소, 스크롤, 재생 위치 이동,
//! 클립·트랙 선택, 클립을 끌어 이동·트림, 두 번 눌러 구간 지정, 트랙 이름 바꾸기.

use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke};
use trackcut_core::edit::Edge;
use trackcut_core::project::{Project, TrackKind};
use trackcut_core::time::{flicks_to_secs, secs_to_flicks, timecode, Flicks};

use crate::drag::Grab;
use crate::sound::Waves;

const HEADER_WIDTH: f32 = 130.0;
const RULER_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 36.0;
const MIN_PX_PER_SEC: f32 = 0.2;
/// 가장 크게 확대하면 60fps 기준 1프레임이 40px
const MAX_PX_PER_SEC: f32 = 2400.0;
/// 클립 양 끝에서 이 폭 안쪽을 잡으면 트림이 된다
const EDGE_WIDTH: f32 = 6.0;
/// 시작점·끝점 선에서 이 거리 안쪽을 잡으면 선을 끈다
const MARK_REACH: f32 = 5.0;
/// 두 번 눌러 구간을 만들 때, 누른 곳 양쪽으로 잡는 폭
const MARK_SPREAD: f32 = 60.0;

pub struct View {
    pub px_per_sec: f32,
    /// 화면 왼쪽 끝의 시각(초)
    pub scroll: f64,
    /// 다음에 그릴 때 전체 길이가 보이도록 맞춘다
    pub fit: bool,
    /// 클립을 끄는 중이면, 잡은 방식과 "잡은 지점 − 클립 기준점(시작 또는 끝)"
    grabbed: Option<(Grab, Flicks)>,
    /// 시작점(false)이나 끝점(true) 선을 끄는 중
    mark_grabbed: Option<bool>,
    /// 눈금이나 빈 곳을 끌어 재생 위치를 옮기는 중
    scrubbing: bool,
    /// 이름을 고치고 있는 트랙: (트랙, 입력 중인 글, 입력 칸에 포커스를 줬는지)
    rename: Option<(u64, String, bool)>,
}

impl Default for View {
    fn default() -> Self {
        View { px_per_sec: 50.0, scroll: 0.0, fit: true, grabbed: None, mark_grabbed: None, scrubbing: false, rename: None }
    }
}

pub struct Marks {
    pub playhead: Flicks,
    pub range: (Option<Flicks>, Option<Flicks>),
    /// 선택한 클립과 트랙
    pub clip: Option<u64>,
    pub track: Option<u64>,
}

/// 사용자가 타임라인에서 한 일. 시각은 프레임에 맞추기 전의 값이다.
pub enum Action {
    Seek(Flicks),
    SelectClip(u64),
    SelectTrack(u64),
    Deselect,
    DragStart(u64, Grab),
    /// 끄는 클립의 기준점(이동: 시작, 트림: 그 경계)이 가야 할 시각
    DragTo(Flicks),
    DragEnd,
    Rename(u64, String),
    /// 시작점과 끝점을 한꺼번에 지정
    SetMarks(Flicks, Flicks),
    /// 시작점(false)이나 끝점(true)을 옮긴다
    MoveMark(bool, Flicks),
}

/// 화면에 그려진 클립 하나의 위치.
struct Hit {
    clip: u64,
    body: Rect,
    start: Flicks,
    end: Flicks,
}

impl Hit {
    fn grab(&self, pos: Pos2) -> Grab {
        let edge = EDGE_WIDTH.min(self.body.width() / 4.0);
        if pos.x - self.body.left() < edge {
            Grab::Trim(Edge::Start)
        } else if self.body.right() - pos.x < edge {
            Grab::Trim(Edge::End)
        } else {
            Grab::Move
        }
    }
}

/// 타임라인을 그리고, 사용자가 한 일을 돌려준다.
pub fn show(ui: &mut egui::Ui, project: &Project, view: &mut View, marks: &Marks, waves: &Waves) -> Vec<Action> {
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

    let (scroll, px_per_sec) = (view.scroll, view.px_per_sec);
    let x_of = |t: Flicks| lane.left() + ((flicks_to_secs(t) - scroll) * px_per_sec as f64) as f32;
    let style = ui.visuals().clone();
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
        // 눈금 위의 손잡이: 여기나 선을 끌어 구간을 조절한다
        let handle = Rect::from_center_size(pos2(x, lane.top() + RULER_HEIGHT - 6.0), vec2(9.0, 12.0));
        lanes.rect_filled(handle, 2.0, Color32::from_rgb(255, 200, 60));
    }

    // 선택한 클립과 연결된 클립들은 옅은 테두리로 함께 표시한다
    let linked = marks.clip.and_then(|id| project.group(id).ok()).unwrap_or_default();
    let mut hits: Vec<Hit> = Vec::new();
    let mut headers: Vec<(u64, Rect)> = Vec::new();

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
            Stroke::new(1.0_f32, style.faint_bg_color),
        );
        for clip in project.timeline.clips.iter().filter(|c| c.track == track.id) {
            let (left, right) = (x_of(clip.start), x_of(clip.end()));
            if right < lane.left() || left > lane.right() {
                continue;
            }
            let body = Rect::from_min_max(pos2(left, top + 3.0), pos2(right.max(left + 1.0), top + ROW_HEIGHT - 3.0));
            lanes.rect_filled(body, 3.0, color);
            if let Some((peaks, stream_start)) = waves.get(&(clip.media, clip.stream)).filter(|_| track.kind == TrackKind::Audio) {
                // 화면 왼쪽 끝 시각에 해당하는 원본 위치(초). 속도를 바꾼 클립은 원본이 그만큼 빨리 지나간다
                let speed = clip.speed_pct as f64 / 100.0;
                let source_at_scroll = flicks_to_secs(clip.src_in - stream_start) + (view.scroll - flicks_to_secs(clip.start)) * speed;
                draw_wave(&lanes, body.intersect(lane), lane.left(), (view.px_per_sec as f64 / speed) as f32, source_at_scroll, peaks);
            }
            let outline = if marks.clip == Some(clip.id) {
                Stroke::new(2.0_f32, Color32::WHITE)
            } else if linked.contains(&clip.id) {
                Stroke::new(1.0_f32, Color32::from_white_alpha(170))
            } else {
                Stroke::new(1.0_f32, Color32::from_black_alpha(140))
            };
            lanes.rect_stroke(body, 3.0, outline);
            hits.push(Hit { clip: clip.id, body, start: clip.start, end: clip.end() });
        }
        // 트랙 이름
        let header = Rect::from_min_size(pos2(rect.left(), top), vec2(HEADER_WIDTH, ROW_HEIGHT));
        let selected = marks.track == Some(track.id);
        painter.rect_filled(header, 0.0, if selected { style.selection.bg_fill } else { style.panel_fill });
        let name_color = if selected {
            style.strong_text_color()
        } else if audible || track.kind == TrackKind::Video {
            style.text_color()
        } else {
            style.weak_text_color()
        };
        painter.with_clip_rect(header.shrink(4.0)).text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            &track.name,
            FontId::proportional(13.0),
            name_color,
        );
        headers.push((track.id, header));
    }

    // 재생 위치
    let x = x_of(marks.playhead);
    lanes.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(2.0_f32,Color32::from_rgb(235, 70, 70)));

    let mut actions = interact(ui, &response, project, view, marks, lane, &hits, &headers);
    rename_box(ui, view, &headers, &mut actions);
    actions
}

/// 클릭과 끌기를 해석한다. 클립을 잡으면 이동·트림, 눈금이나 빈 곳이면 재생 위치 이동.
fn interact(
    ui: &egui::Ui,
    response: &egui::Response,
    project: &Project,
    view: &mut View,
    marks: &Marks,
    lane: Rect,
    hits: &[Hit],
    headers: &[(u64, Rect)],
) -> Vec<Action> {
    let mut actions = Vec::new();
    let (scroll, px_per_sec) = (view.scroll, view.px_per_sec);
    let time_at = |x: f32| secs_to_flicks(scroll + ((x - lane.left()) / px_per_sec) as f64);
    let seek_at = |x: f32| time_at(x).clamp(0, project.duration());
    let clip_at = |p: Pos2| hits.iter().find(|h| lane.contains(p) && h.body.contains(p));

    // 가까이 있는 시작점·끝점 선(끝점이면 true)
    let mark_at = |p: Pos2| {
        let near = |mark: Option<Flicks>| {
            let x = lane.left() + ((flicks_to_secs(mark?) - scroll) * px_per_sec as f64) as f32;
            Some((p.x - x).abs()).filter(|d| lane.contains(p) && *d <= MARK_REACH)
        };
        match (near(marks.range.0), near(marks.range.1)) {
            (Some(a), Some(b)) => Some(b < a),
            (Some(_), None) => Some(false),
            (None, Some(_)) => Some(true),
            (None, None) => None,
        }
    };

    if response.drag_started() {
        if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
            if let Some(which) = mark_at(origin) {
                view.mark_grabbed = Some(which);
            } else if let Some(hit) = clip_at(origin) {
                let grab = hit.grab(origin);
                let anchor = if grab == Grab::Trim(Edge::End) { hit.end } else { hit.start };
                view.grabbed = Some((grab, time_at(origin.x) - anchor));
                actions.push(Action::SelectClip(hit.clip));
                actions.push(Action::DragStart(hit.clip, grab));
            } else if origin.x >= lane.left() {
                view.scrubbing = true;
            }
        }
    }
    if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged()) {
        if let Some(which) = view.mark_grabbed {
            actions.push(Action::MoveMark(which, seek_at(p.x)));
        } else if let Some((_, offset)) = view.grabbed {
            actions.push(Action::DragTo(time_at(p.x) - offset));
        } else if view.scrubbing {
            actions.push(Action::Seek(seek_at(p.x)));
        }
    }
    if response.drag_stopped() {
        if view.grabbed.take().is_some() {
            actions.push(Action::DragEnd);
        }
        view.scrubbing = false;
        view.mark_grabbed = None;
    }
    if let Some(p) = response.interact_pointer_pos().filter(|_| response.clicked()) {
        if let Some(&(track, _)) = headers.iter().find(|(_, r)| r.contains(p)) {
            actions.push(Action::SelectTrack(track));
            if response.double_clicked() {
                let name = project.track(track).map(|t| t.name.clone()).unwrap_or_default();
                view.rename = Some((track, name, false));
            }
        } else if let Some(hit) = clip_at(p) {
            actions.push(Action::SelectClip(hit.clip));
        } else if p.x >= lane.left() {
            if p.y > lane.top() + RULER_HEIGHT {
                actions.push(Action::Deselect);
            }
            actions.push(Action::Seek(seek_at(p.x)));
        }
    }

    // 타임라인을 두 번 누르면 그 자리 양쪽에 시작점과 끝점을 놓는다
    if let Some(p) = response.interact_pointer_pos().filter(|p| response.double_clicked() && p.x >= lane.left()) {
        actions.push(Action::SetMarks(seek_at(p.x - MARK_SPREAD), seek_at(p.x + MARK_SPREAD)));
    }

    // 커서 모양으로 잡을 수 있는 곳을 알려 준다
    let hover = ui.input(|i| i.pointer.hover_pos()).filter(|_| response.hovered());
    if view.mark_grabbed.is_some() || (view.grabbed.is_none() && hover.and_then(mark_at).is_some()) {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        return actions;
    }
    let grab = view.grabbed.map(|(grab, _)| grab).or_else(|| hover.and_then(|p| clip_at(p).map(|h| h.grab(p))));
    match grab {
        Some(Grab::Trim(_)) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
        Some(Grab::Move) if view.grabbed.is_some() => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
        Some(Grab::Move) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
        None => {}
    }
    actions
}

/// 트랙 이름 입력 칸. Enter나 다른 곳 클릭으로 확정하고 Esc로 취소한다.
fn rename_box(ui: &mut egui::Ui, view: &mut View, headers: &[(u64, Rect)], actions: &mut Vec<Action>) {
    let Some((track, mut text, focused)) = view.rename.take() else { return };
    let Some((_, header)) = headers.iter().find(|(id, _)| *id == track) else { return };
    let edit = ui.put(header.shrink(4.0), egui::TextEdit::singleline(&mut text));
    if !focused {
        edit.request_focus();
    }
    if edit.lost_focus() {
        if !ui.input(|i| i.key_pressed(Key::Escape)) {
            actions.push(Action::Rename(track, text));
        }
    } else {
        view.rename = Some((track, text, true));
    }
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
            painter.line_segment([pos2(x, lane.top() + RULER_HEIGHT), pos2(x, lane.bottom())], Stroke::new(1.0_f32, Color32::from_gray(45)));
            n += 1.0;
        }
    }
    let mut n = (view.scroll / step).floor();
    while x_of(n * step) <= lane.right() {
        let secs = n * step;
        let x = x_of(secs);
        painter.line_segment([pos2(x, lane.top()), pos2(x, lane.top() + RULER_HEIGHT)], Stroke::new(1.0_f32, Color32::from_gray(110)));
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

/// 클립 안에 파형을 그린다. 화면 1px마다 그 구간에서 가장 큰 소리 크기를 세로선으로 표시한다.
fn draw_wave(painter: &egui::Painter, area: Rect, lane_left: f32, px_per_sec: f32, source_at_scroll: f64, peaks: &[u8]) {
    const PEAKS_PER_SEC: f64 = 100.0;
    let color = Color32::from_rgba_unmultiplied(220, 255, 235, 150);
    let half = area.height() * 0.5 - 2.0;
    let mut x = area.left().floor();
    while x < area.right() {
        let source = source_at_scroll + ((x - lane_left) / px_per_sec) as f64;
        let first = (source * PEAKS_PER_SEC).max(0.0) as usize;
        let last = (((source + 1.0 / px_per_sec as f64) * PEAKS_PER_SEC).ceil() as usize).max(first + 1);
        let peak = peaks.get(first.min(peaks.len())..last.min(peaks.len())).and_then(|range| range.iter().max());
        if let Some(&peak) = peak {
            // 작은 소리도 보이도록 제곱근 눈금을 쓴다
            let height = (peak as f32 / 255.0).sqrt() * half;
            let center = area.center().y;
            painter.line_segment([pos2(x, center - height), pos2(x, center + height.max(0.5))], Stroke::new(1.0_f32, color));
        }
        x += 1.0;
    }
}
