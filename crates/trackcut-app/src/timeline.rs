//! 타임라인 그리기와 조작: 휠 확대·축소, 스크롤, 재생 위치 이동, 클립·트랙 선택,
//! 클립을 끌어 이동·트림, 구역(여러 개) 지정과 우클릭 메뉴, 트랙 이름 바꾸기.
//!
//! 위쪽은 원본 묶음, 아래쪽은 편집본 묶음이고 둘은 같은 시간축을 쓴다.
//! 원본의 구간(또는 구역)을 편집본 쪽으로 끌어다 놓으면 그 자리에 복사된다.

use std::collections::HashMap;

use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke};
use trackcut_core::edit::Edge;
use trackcut_core::project::{Clip, Project, Track, TrackKind};
use trackcut_core::time::{flicks_to_secs, secs_to_flicks, timecode, Flicks};

use crate::drag::Grab;
use crate::regions::Region;
use crate::sound::Waves;

const HEADER_WIDTH: f32 = 130.0;
const RULER_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 46.0;
/// 타임라인이 낮으면 트랙 줄을 이 높이까지 줄인다
const ROW_MIN_HEIGHT: f32 = 30.0;
/// "원본", "편집본" 묶음 이름 띠의 높이
const GROUP_HEIGHT: f32 = 18.0;
const MIN_PX_PER_SEC: f32 = 0.2;
/// 가장 크게 확대하면 60fps 기준 1프레임이 40px
const MAX_PX_PER_SEC: f32 = 2400.0;
/// 클립 양 끝에서 이 폭 안쪽을 잡으면 트림이 된다
const EDGE_WIDTH: f32 = 6.0;
/// 구역 경계선에서 이 거리 안쪽을 잡으면 선을 끈다
const MARK_REACH: f32 = 5.0;
/// 두 번 눌러 구역을 만들 때, 누른 곳 양쪽으로 잡는 폭
const MARK_SPREAD: f32 = 60.0;
const REGION_COLOR: Color32 = Color32::from_rgb(255, 200, 60);

/// 우클릭한 자리에 있던 것.
#[derive(Clone, Copy)]
struct Menu {
    region: Option<usize>,
    /// 여러 개 선택한 구역 가운데 하나를 눌렀는지(병합 메뉴를 낸다)
    merge: bool,
    clip: Option<u64>,
    time: Flicks,
}

/// 원본의 구간을 편집본 쪽으로 끌어 복사하는 중.
#[derive(Clone, Copy)]
struct Copying {
    /// 복사할 원본 범위
    range: Region,
    /// "잡은 지점 − 범위의 시작"
    offset: Flicks,
    /// 끌기 시작할 때 잡은 클립이 있던 자리(편집본 위에 있는 동안 클립을 여기에 둔다)
    home: Flicks,
    /// 포인터가 편집본 위에 있으면, 놓았을 때 복사본이 시작할 시각
    over: Option<Flicks>,
}

pub struct View {
    pub px_per_sec: f32,
    /// 화면 왼쪽 끝의 시각(초)
    pub scroll: f64,
    /// 다음에 그릴 때 전체 길이가 보이도록 맞춘다
    pub fit: bool,
    /// 클립을 끄는 중이면, 잡은 방식과 "잡은 지점 − 클립 기준점(시작 또는 끝)"
    grabbed: Option<(Grab, Flicks)>,
    copying: Option<Copying>,
    /// 구역의 시작(false)이나 끝(true) 선을 끄는 중: (구역 번호, 끝인지)
    edge_grabbed: Option<(usize, bool)>,
    /// 눈금이나 빈 곳을 끌어 재생 위치를 옮기는 중
    scrubbing: bool,
    /// 이름을 고치고 있는 트랙: (트랙, 입력 중인 글, 입력 칸에 포커스를 줬는지)
    rename: Option<(u64, String, bool)>,
    menu: Option<Menu>,
}

impl Default for View {
    fn default() -> Self {
        View { px_per_sec: 50.0, scroll: 0.0, fit: true, grabbed: None, copying: None, edge_grabbed: None, scrubbing: false, rename: None, menu: None }
    }
}

pub struct Marks<'a> {
    pub playhead: Flicks,
    pub regions: &'a [Region],
    /// 선택한 구역들(마지막이 가장 최근에 고른 것), 선택한 클립과 트랙
    pub picked: &'a [usize],
    pub clip: Option<u64>,
    pub track: Option<u64>,
    /// 미리보기가 편집본 묶음을 보여 주는 중인지(아니면 원본)
    pub edit_view: bool,
    /// 클립마다 붙이는 제목(영상 번호와 분할 번호)
    pub labels: &'a HashMap<u64, String>,
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
    /// 트랙의 소리를 켜거나 끈다(트랙 이름 옆의 버튼)
    ToggleMute(u64),
    /// 끌던 것을 없던 일로 한다
    DragCancel,
    /// 원본의 범위(시작, 끝)를 편집본의 이 시각에 복사한다
    CopyToEdit(Flicks, Flicks, Flicks),
    Rename(u64, String),
    AddRegion(Flicks, Flicks),
    /// 구역을 고른다. true면 이미 고른 것에 더한다(Shift + 클릭)
    SelectRegion(usize, bool),
    DeselectRegions,
    /// 선택한 구역들을 하나로 합친다
    MergeRegions,
    /// 구역의 시작(false)이나 끝(true)을 옮긴다
    MoveRegionEdge(usize, bool, Flicks),
    /// 구역의 양 끝에서 클립을 나눈다
    SplitRegion(usize),
    /// 구역의 원본 내용을 편집본의 맨 끝에 복사한다
    CopyRegion(usize),
    /// 구역의 내용을 잘라내고 뒤를 당겨 붙인다
    CutRegion(usize),
    /// 구역 표시만 지운다
    RemoveRegion(usize),
    /// 클립을 뒤(true)나 앞의 클립과 자리를 바꾼다
    SwapClip(u64, bool),
    Unlink(u64),
}

/// 여러 조각을 하나로 묶은 구간들: 연결 번호 → 구간 번호(1부터, 시작 시각 순).
/// 한 트랙에 조각이 둘 이상 들어 있는 연결 묶음이 구간이다(여러 영상에 걸친 구역을 분할한 결과).
fn sections(project: &Project) -> HashMap<u64, usize> {
    let clips = &project.timeline.clips;
    let mut found: Vec<(Flicks, u64)> = Vec::new();
    for clip in clips {
        let Some(link) = clip.link else { continue };
        let same = |c: &&Clip| c.link == Some(link);
        if found.iter().all(|f| f.1 != link) && clips.iter().filter(same).any(|c| c.id != clip.id && c.track == clip.track) {
            found.push((clips.iter().filter(same).map(|c| c.start).min().unwrap_or(0), link));
        }
    }
    found.sort();
    found.into_iter().enumerate().map(|(number, (_, link))| (link, number + 1)).collect()
}

/// 클립마다 붙일 제목. 같은 파일에서 나온 조각이 여럿이면 "영상 1 · 분할 2"처럼 번호를 붙이고,
/// 여러 조각을 묶은 구간에 든 클립은 앞에 "구간 N"을 붙인다.
pub fn clip_labels(project: &Project) -> HashMap<u64, String> {
    let mut labels = HashMap::new();
    let sections = sections(project);
    for track in &project.timeline.tracks {
        let mut clips: Vec<&Clip> = project.timeline.clips.iter().filter(|c| c.track == track.id).collect();
        clips.sort_by_key(|c| c.start);
        for (number, media) in project.media.iter().enumerate() {
            let kind = if media.info.video.is_some() { "영상" } else { "소리" };
            let pieces: Vec<&&Clip> = clips.iter().filter(|c| c.media == media.id).collect();
            for (piece, clip) in pieces.iter().enumerate() {
                let label = match pieces.len() {
                    1 => format!("{kind} {}", number + 1),
                    _ => format!("{kind} {} · 분할 {}", number + 1, piece + 1),
                };
                let label = match clip.link.and_then(|link| sections.get(&link)) {
                    Some(section) => format!("구간 {section} · {label}"),
                    None => label,
                };
                labels.insert(clip.id, label);
            }
        }
    }
    labels
}

/// 화면에 그려진 클립 하나의 위치.
struct Hit {
    clip: u64,
    /// 편집본 묶음의 클립인지
    edit: bool,
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
    // 세로 배치: 눈금, "원본" 띠, 원본 트랙들, "편집본" 띠, 편집본 트랙들(없으면 빈 줄 하나)
    let source: Vec<&Track> = project.timeline.tracks.iter().filter(|t| !t.is_edit()).collect();
    let edited: Vec<&Track> = project.timeline.tracks.iter().filter(|t| t.is_edit()).collect();
    let rows = (source.len() + edited.len().max(1)) as f32;
    let fixed = RULER_HEIGHT + 2.0 * GROUP_HEIGHT;
    let row_h = ((ui.available_height() - fixed) / rows).clamp(ROW_MIN_HEIGHT, ROW_HEIGHT);
    let size = vec2(ui.available_width(), (fixed + rows * row_h).max(ui.available_height()));
    let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    let rect = response.rect;
    let source_top = rect.top() + RULER_HEIGHT;
    let edit_top = source_top + GROUP_HEIGHT + source.len() as f32 * row_h;
    let edit_bottom = edit_top + GROUP_HEIGHT + edited.len().max(1) as f32 * row_h;
    let place = |tracks: &[&'_ Track], top: f32| -> Vec<(u64, f32)> {
        tracks.iter().enumerate().map(|(row, t)| (t.id, top + GROUP_HEIGHT + row as f32 * row_h)).collect()
    };
    let tops: HashMap<u64, f32> = place(&source, source_top).into_iter().chain(place(&edited, edit_top)).collect();
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

    // 선택한 클립과 연결된 클립들은 옅은 테두리로 함께 표시한다
    let linked = marks.clip.and_then(|id| project.group(id).ok()).unwrap_or_default();
    let mut hits: Vec<Hit> = Vec::new();
    let mut headers: Vec<(u64, Rect)> = Vec::new();
    // 오디오 트랙 이름 옆의 소리 켜기/끄기 버튼
    let mut mutes: Vec<(u64, Rect)> = Vec::new();

    for track in &project.timeline.tracks {
        let Some(&top) = tops.get(&track.id) else { continue };
        let audible = project.is_audible(track.id);
        // 편집본 묶음은 원본과 다른 색으로 칠해 구분한다
        let color = match (track.kind, audible, track.is_edit()) {
            (TrackKind::Video, _, false) => Color32::from_rgb(70, 110, 190),
            (TrackKind::Video, _, true) => Color32::from_rgb(130, 95, 190),
            (TrackKind::Audio, true, false) => Color32::from_rgb(60, 150, 110),
            (TrackKind::Audio, true, true) => Color32::from_rgb(45, 140, 155),
            (TrackKind::Audio, false, _) => Color32::from_gray(90),
        };
        lanes.line_segment(
            [pos2(lane.left(), top + row_h), pos2(lane.right(), top + row_h)],
            Stroke::new(1.0_f32, style.faint_bg_color),
        );
        for clip in project.timeline.clips.iter().filter(|c| c.track == track.id) {
            let (left, right) = (x_of(clip.start), x_of(clip.end()));
            if right < lane.left() || left > lane.right() {
                continue;
            }
            let body = Rect::from_min_max(pos2(left, top + 3.0), pos2(right.max(left + 1.0), top + row_h - 3.0));
            let selected = marks.clip == Some(clip.id);
            // 소리를 끈 클립은 회색으로 칠한다
            lanes.rect_filled(body, 3.0, if clip.muted { Color32::from_gray(75) } else { color });
            // 제목은 막대의 왼쪽 끝에 붙인다. 막대의 앞부분이 화면 밖이면 보이는 곳의 왼쪽 끝에 붙는다
            let mut label = marks.labels.get(&clip.id).cloned().unwrap_or_default();
            if clip.speed_pct != 100 {
                label += &format!("  {}%", clip.speed_pct);
            }
            let (title_fill, title_text) = if selected {
                (Color32::from_rgb(250, 250, 250), Color32::BLACK)
            } else if linked.contains(&clip.id) {
                (Color32::from_black_alpha(90), Color32::WHITE)
            } else {
                (Color32::from_black_alpha(130), Color32::from_gray(225))
            };
            let galley = lanes.layout_no_wrap(label, FontId::proportional(11.0), title_text);
            let title_left = body.left().max(lane.left());
            let title_right = (title_left + galley.size().x + 10.0).min(body.right());
            let title = Rect::from_min_max(pos2(title_left, body.top()), pos2(title_right, body.bottom()));
            let content = Rect::from_min_max(pos2(title.right(), body.top()), body.max);
            if let Some((peaks, stream_start)) = waves.get(&(clip.media, clip.stream)).filter(|_| track.kind == TrackKind::Audio) {
                // 화면 왼쪽 끝 시각에 해당하는 원본 위치(초). 속도를 바꾼 클립은 원본이 그만큼 빨리 지나간다
                let speed = clip.speed_pct as f64 / 100.0;
                let source_at_scroll = flicks_to_secs(clip.src_in - stream_start) + (view.scroll - flicks_to_secs(clip.start)) * speed;
                draw_wave(&lanes, content.intersect(lane), lane.left(), (view.px_per_sec as f64 / speed) as f32, source_at_scroll, peaks);
            }
            // 선택한 클립은 제목을 밝게 칠해서 한눈에 보이게 한다
            lanes.rect_filled(title, 3.0, title_fill);
            let text_at = pos2(title.left() + 5.0, title.center().y - galley.size().y * 0.5);
            lanes.with_clip_rect(title.intersect(lane)).galley(text_at, galley, title_text);
            let outline = if selected {
                Stroke::new(2.0_f32, Color32::WHITE)
            } else if linked.contains(&clip.id) {
                Stroke::new(1.0_f32, Color32::from_white_alpha(170))
            } else {
                Stroke::new(1.0_f32, Color32::from_black_alpha(140))
            };
            lanes.rect_stroke(body, 3.0, outline);
            hits.push(Hit { clip: clip.id, edit: track.is_edit(), body, start: clip.start, end: clip.end() });
        }
        // 트랙 이름
        let header = Rect::from_min_size(pos2(rect.left(), top), vec2(HEADER_WIDTH, row_h));
        let selected = marks.track == Some(track.id);
        painter.rect_filled(header, 0.0, if selected { style.selection.bg_fill } else { style.panel_fill });
        let name_color = if selected {
            style.strong_text_color()
        } else if audible || track.kind == TrackKind::Video {
            style.text_color()
        } else {
            style.weak_text_color()
        };
        let mut name_area = header.shrink(4.0);
        if track.kind == TrackKind::Audio {
            // 믹서 대신: 트랙마다 소리를 바로 켜고 끈다
            let button = Rect::from_center_size(pos2(header.right() - 22.0, header.center().y), vec2(36.0, 18.0));
            // 이 트랙에서 고른 클립이 있으면 그 클립의 상태를, 없으면 트랙 전체의 상태를 보여 준다
            let chosen = marks.clip.and_then(|id| project.clip(id).ok()).filter(|c| c.track == track.id);
            let off = chosen.map_or(track.muted, |c| c.muted);
            let (fill, text) = if off { (Color32::from_gray(70), "꺼짐") } else { (Color32::from_rgb(45, 120, 85), "켜짐") };
            painter.rect_filled(button, 4.0, fill);
            painter.text(button.center(), Align2::CENTER_CENTER, text, FontId::proportional(11.0), Color32::WHITE);
            mutes.push((track.id, button));
            name_area.set_right(button.left() - 4.0);
        }
        painter.with_clip_rect(name_area).text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            &track.name,
            FontId::proportional(13.0),
            name_color,
        );
        headers.push((track.id, header));
    }

    // 묶음 이름 띠. 미리보기가 보여 주는 묶음은 밝게 표시한다
    let band = |top: f32, name: &str, hint: &str, watched: bool, ready: bool| {
        let area = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), GROUP_HEIGHT));
        let fill = match (ready, watched) {
            (true, _) => Color32::from_rgb(110, 90, 30),
            (false, true) => Color32::from_rgb(54, 62, 80),
            (false, false) => Color32::from_gray(40),
        };
        painter.rect_filled(area, 0.0, fill);
        let title = if watched { format!("{name}  ● 보는 중") } else { name.to_string() };
        let color = if watched { Color32::WHITE } else { Color32::from_gray(175) };
        painter.text(area.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, title, FontId::proportional(12.0), color);
        painter.text(pos2(lane.left() + 6.0, area.center().y), Align2::LEFT_CENTER, hint, FontId::proportional(11.0), Color32::from_gray(150));
    };
    band(source_top, "원본", "", !marks.edit_view, false);
    band(edit_top, "편집본", "원본의 구간이나 구역을 이 아래로 끌어다 놓으면 복사된다", marks.edit_view, view.copying.is_some());
    if edited.is_empty() {
        let empty = Rect::from_min_max(pos2(lane.left(), edit_top + GROUP_HEIGHT), pos2(lane.right(), edit_bottom));
        lanes.text(empty.center(), Align2::CENTER_CENTER, "아직 비어 있다", FontId::proportional(12.0), Color32::from_gray(110));
    }

    // 구역: 선택한 구역은 진하게, 나머지는 옅게. 클립 위에 겹쳐 보이도록 클립 다음에 그린다
    for (index, &(a, b)) in marks.regions.iter().enumerate() {
        let active = marks.picked.contains(&index);
        let shade = Rect::from_min_max(pos2(x_of(a), lane.top()), pos2(x_of(b), lane.bottom()));
        lanes.rect_filled(shade, 0.0, Color32::from_rgba_unmultiplied(255, 200, 60, if active { 55 } else { 22 }));
        let line = if active { REGION_COLOR } else { Color32::from_rgb(170, 140, 70) };
        for x in [shade.left(), shade.right()] {
            lanes.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(1.5_f32, line));
            // 눈금 위의 손잡이: 여기나 선을 끌어 구역을 조절한다
            lanes.rect_filled(Rect::from_center_size(pos2(x, lane.top() + RULER_HEIGHT - 6.0), vec2(9.0, 12.0)), 2.0, line);
        }
        lanes.with_clip_rect(shade.intersect(lane)).text(
            pos2(shade.left() + 8.0, lane.top() + 1.0),
            Align2::LEFT_TOP,
            format!("구역 {}", index + 1),
            FontId::proportional(10.0),
            line,
        );
    }

    // 재생 위치
    let x = x_of(marks.playhead);
    lanes.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(2.0_f32,Color32::from_rgb(235, 70, 70)));

    let mut actions = interact(ui, &response, project, view, marks, lane, edit_top, &hits, &headers, &mutes);
    // 편집본 위로 끌고 온 구간이 놓일 자리를 미리 보여 준다
    if let Some((copy, at)) = view.copying.and_then(|copy| copy.over.map(|at| (copy, at))) {
        let ghost = Rect::from_min_max(pos2(x_of(at), edit_top + GROUP_HEIGHT + 2.0), pos2(x_of(at + copy.range.1 - copy.range.0), edit_bottom - 2.0));
        lanes.rect_filled(ghost, 3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 50));
        lanes.rect_stroke(ghost, 3.0, Stroke::new(2.0_f32, REGION_COLOR));
        lanes.with_clip_rect(ghost.intersect(lane)).text(ghost.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, "여기에 복사", FontId::proportional(12.0), Color32::WHITE);
    }
    context_menu(&response, view, &mut actions);
    rename_box(ui, view, &headers, &mut actions);
    actions
}

/// 클릭과 끌기를 해석한다. 구역 선을 잡으면 구역 조절, 클립을 잡으면 이동·트림,
/// 눈금이나 빈 곳이면 재생 위치 이동.
#[allow(clippy::too_many_arguments)]
fn interact(
    ui: &egui::Ui,
    response: &egui::Response,
    project: &Project,
    view: &mut View,
    marks: &Marks,
    lane: Rect,
    edit_top: f32,
    hits: &[Hit],
    headers: &[(u64, Rect)],
    mutes: &[(u64, Rect)],
) -> Vec<Action> {
    let mut actions = Vec::new();
    let (scroll, px_per_sec) = (view.scroll, view.px_per_sec);
    let time_at = |x: f32| secs_to_flicks(scroll + ((x - lane.left()) / px_per_sec) as f64);
    let seek_at = |x: f32| time_at(x).clamp(0, project.duration());
    let x_of = |t: Flicks| lane.left() + ((flicks_to_secs(t) - scroll) * px_per_sec as f64) as f32;
    let clip_at = |p: Pos2| hits.iter().find(|h| lane.contains(p) && h.body.contains(p));
    // 가까이 있는 구역 경계선: (구역 번호, 끝인지)
    let edge_at = |p: Pos2| {
        let edges = marks.regions.iter().enumerate().flat_map(|(i, &(a, b))| [(i, false, a), (i, true, b)]);
        let near = edges.map(|(i, is_end, t)| (i, is_end, (p.x - x_of(t)).abs())).filter(|e| lane.contains(p) && e.2 <= MARK_REACH);
        near.min_by(|a, b| a.2.total_cmp(&b.2)).map(|(i, is_end, _)| (i, is_end))
    };
    // 그 자리를 덮는 구역(선택한 구역을 먼저)
    let region_at = |p: Pos2| {
        let inside = |i: &usize| lane.contains(p) && x_of(marks.regions[*i].0) <= p.x && p.x <= x_of(marks.regions[*i].1);
        let picked = marks.picked.iter().rev().copied().filter(|i| *i < marks.regions.len());
        picked.clone().find(inside).or_else(|| (0..marks.regions.len()).find(inside))
    };

    if response.drag_started() {
        if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
            if let Some(edge) = edge_at(origin) {
                view.edge_grabbed = Some(edge);
                if !marks.picked.contains(&edge.0) {
                    actions.push(Action::SelectRegion(edge.0, false));
                }
            } else if let Some(hit) = clip_at(origin) {
                let grab = hit.grab(origin);
                let anchor = if grab == Grab::Trim(Edge::End) { hit.end } else { hit.start };
                view.grabbed = Some((grab, time_at(origin.x) - anchor));
                // 원본 클립의 가운데를 잡았으면 편집본으로 끌어 복사할 수 있다. 구역 안을 잡았으면
                // 그 구역을, 아니면 잡은 클립이 속한 구간(연결 묶음) 전체를 복사한다
                view.copying = (grab == Grab::Move && !hit.edit)
                    .then(|| region_at(origin).map(|i| marks.regions[i]).or_else(|| project.group_span(hit.clip).ok()))
                    .flatten()
                    .map(|range| Copying { range, offset: time_at(origin.x) - range.0, home: hit.start, over: None });
                actions.push(Action::SelectClip(hit.clip));
                actions.push(Action::DragStart(hit.clip, grab));
            } else if origin.x >= lane.left() {
                view.scrubbing = true;
            }
        }
    }
    if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged()) {
        if let Some((index, is_end)) = view.edge_grabbed {
            actions.push(Action::MoveRegionEdge(index, is_end, seek_at(p.x)));
        } else if let Some((_, offset)) = view.grabbed {
            match &mut view.copying {
                // 편집본 위에서는 원본 클립을 제자리에 두고 복사할 자리만 따라간다
                Some(copy) if p.y >= edit_top => {
                    copy.over = Some((time_at(p.x) - copy.offset).max(0));
                    actions.push(Action::DragTo(copy.home));
                }
                copy => {
                    if let Some(copy) = copy {
                        copy.over = None;
                    }
                    actions.push(Action::DragTo(time_at(p.x) - offset));
                }
            }
        } else if view.scrubbing {
            actions.push(Action::Seek(seek_at(p.x)));
        }
    }
    if response.drag_stopped() {
        let copy = view.copying.take();
        if view.grabbed.take().is_some() {
            match copy.and_then(|copy| copy.over.map(|at| (copy.range, at))) {
                Some(((a, b), at)) => actions.extend([Action::DragCancel, Action::CopyToEdit(a, b, at)]),
                None => actions.push(Action::DragEnd),
            }
        }
        view.scrubbing = false;
        view.edge_grabbed = None;
    }
    if let Some(p) = response.interact_pointer_pos().filter(|_| response.clicked()) {
        if let Some(&(track, _)) = mutes.iter().find(|(_, r)| r.contains(p)) {
            actions.push(Action::ToggleMute(track));
        } else if let Some(&(track, _)) = headers.iter().find(|(_, r)| r.contains(p)) {
            actions.push(Action::SelectTrack(track));
            if response.double_clicked() {
                let name = project.track(track).map(|t| t.name.clone()).unwrap_or_default();
                view.rename = Some((track, name, false));
            }
        } else if p.x >= lane.left() && ui.input(|i| i.modifiers.shift) {
            // Shift + 클릭: 구역을 여러 개 고른다. 재생 위치와 클립 선택은 그대로 둔다
            actions.extend(region_at(p).map(|region| Action::SelectRegion(region, true)));
        } else if p.x >= lane.left() {
            let region = region_at(p);
            actions.push(region.map_or(Action::DeselectRegions, |region| Action::SelectRegion(region, false)));
            if let Some(hit) = clip_at(p) {
                actions.push(Action::SelectClip(hit.clip));
            } else {
                if p.y > lane.top() + RULER_HEIGHT {
                    actions.push(Action::Deselect);
                }
                actions.push(Action::Seek(seek_at(p.x)));
            }
            // 구역 밖을 두 번 누르면 그 자리 양쪽에 새 구역을 만든다
            if response.double_clicked() && region.is_none() {
                actions.push(Action::AddRegion(seek_at(p.x - MARK_SPREAD), seek_at(p.x + MARK_SPREAD)));
            }
        }
    }
    // 우클릭: 그 자리에 있는 구역과 클립을 기억해 메뉴에 쓴다
    if let Some(p) = response.interact_pointer_pos().filter(|p| response.secondary_clicked() && p.x >= lane.left()) {
        let region = region_at(p);
        // 여러 개 고른 구역 위에서 누르면 선택을 그대로 두고 병합 메뉴를 낸다
        let merge = marks.picked.len() >= 2 && region.is_some_and(|r| marks.picked.contains(&r));
        let menu = Menu { region, merge, clip: clip_at(p).map(|h| h.clip), time: seek_at(p.x) };
        if !merge {
            actions.extend(menu.region.map(|region| Action::SelectRegion(region, false)));
        }
        actions.extend(menu.clip.map(Action::SelectClip));
        view.menu = Some(menu);
    }

    // 커서 모양으로 잡을 수 있는 곳을 알려 준다
    let hover = ui.input(|i| i.pointer.hover_pos()).filter(|_| response.hovered());
    if hover.is_some_and(|p| mutes.iter().any(|(_, r)| r.contains(p))) {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if view.edge_grabbed.is_some() || (view.grabbed.is_none() && hover.and_then(edge_at).is_some()) {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        return actions;
    }
    if view.copying.is_some_and(|copy| copy.over.is_some()) {
        ui.ctx().set_cursor_icon(CursorIcon::Copy);
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

/// 우클릭 메뉴: 구역 위에서는 구역 기능, 클립 위에서는 순서 바꾸기와 연결 해제.
fn context_menu(response: &egui::Response, view: &View, actions: &mut Vec<Action>) {
    let Some(menu) = view.menu else { return };
    let half = secs_to_flicks((MARK_SPREAD / view.px_per_sec) as f64);
    response.context_menu(|ui| {
        let mut item = |ui: &mut egui::Ui, text: &str, action: Action| {
            if ui.button(text).clicked() {
                actions.push(action);
                ui.close_menu();
            }
        };
        match menu.region {
            Some(_) if menu.merge => {
                ui.weak("선택한 구역들");
                item(ui, "병합 (하나의 구역으로 합치기)", Action::MergeRegions);
            }
            Some(region) => {
                ui.weak(format!("구역 {}", region + 1));
                item(ui, "구역 분할", Action::SplitRegion(region));
                item(ui, "편집본 끝에 복사", Action::CopyRegion(region));
                item(ui, "구역 삭제 (잘라내고 당겨 붙이기)", Action::CutRegion(region));
                item(ui, "구역 표시만 지우기", Action::RemoveRegion(region));
            }
            None => item(ui, "여기에 구역 만들기", Action::AddRegion((menu.time - half).max(0), menu.time + half)),
        }
        if let Some(clip) = menu.clip {
            ui.separator();
            item(ui, "클립을 앞으로 옮기기", Action::SwapClip(clip, false));
            item(ui, "클립을 뒤로 옮기기", Action::SwapClip(clip, true));
            item(ui, "연결 해제", Action::Unlink(clip));
        }
    });
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
