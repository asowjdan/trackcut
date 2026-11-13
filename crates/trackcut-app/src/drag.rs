//! 타임라인에서 마우스로 끄는 편집(이동, 트림).
//!
//! 끄는 동안에는 시작할 때의 상태에서 매번 다시 계산해 미리 보여 주고,
//! 놓을 때 Undo 한 단계로 남긴다. 그래서 한 번 끈 것은 한 번에 되돌려진다.

use trackcut_core::edit::Edge;
use trackcut_core::history::History;
use trackcut_core::project::{Project, Timeline};
use trackcut_core::time::{frame_time, nearest_frame, Flicks};

/// 클립의 어디를 잡았는지.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    Move,
    Trim(Edge),
}

impl Grab {
    pub fn label(self) -> &'static str {
        match self {
            Grab::Move => "클립 이동",
            Grab::Trim(_) => "트림",
        }
    }
}

pub struct Drag {
    pub clip: u64,
    pub grab: Grab,
    base: Timeline,
    /// 마지막으로 적용에 성공한 시각(이동: 클립의 새 시작, 트림: 경계의 새 위치)
    applied: Option<Flicks>,
}

fn apply(project: &mut Project, clip: u64, grab: Grab, t: Flicks) -> trackcut_core::Result<Flicks> {
    match grab {
        Grab::Move => project.move_clip(clip, t).map(|_| t),
        Grab::Trim(edge) => project.trim(clip, edge, t),
    }
}

impl Drag {
    pub fn begin(project: &Project, clip: u64, grab: Grab) -> Self {
        Drag { clip, grab, base: project.timeline.clone(), applied: None }
    }

    /// 포인터가 가리키는 목표 시각을 프레임에 맞춰 미리 적용한다.
    /// 적용할 수 없는 위치(다른 클립과 겹침 등)면 마지막으로 성공한 위치에 머문다.
    pub fn update(&mut self, project: &mut Project, target: Flicks) {
        project.timeline = self.base.clone();
        let t = match self.grab {
            Grab::Move => self.move_target(project, target),
            Grab::Trim(_) => Some(project.snap(target)),
        };
        if let Some(t) = t {
            if let Ok(done) = apply(project, self.clip, self.grab, t) {
                self.applied = Some(done);
                return;
            }
        }
        project.timeline = self.base.clone();
        if let Some(t) = self.applied {
            let _ = apply(project, self.clip, self.grab, t);
        }
    }

    /// 이동량을 프레임 단위로 맞춘 새 시작 시각. 묶음 전체가 타임라인 시작을 넘지 않게 한다.
    fn move_target(&self, project: &Project, target: Flicks) -> Option<Flicks> {
        let start = project.clip(self.clip).ok()?.start;
        let group = project.group(self.clip).ok()?;
        let earliest = project.timeline.clips.iter().filter(|c| group.contains(&c.id)).map(|c| c.start).min()?;
        let fps = project.settings.fps;
        let delta = frame_time(nearest_frame(target - start, fps), fps).max(-earliest);
        Some(start + delta)
    }

    /// 끌기를 없던 일로 하고 시작할 때의 상태로 돌려놓는다.
    pub fn cancel(self, project: &mut Project) {
        project.timeline = self.base;
    }

    /// 끌기를 끝내고 결과를 Undo 한 단계로 남긴다. 바뀐 것이 있으면 true.
    pub fn finish(self, project: &mut Project, history: &mut History) -> trackcut_core::Result<bool> {
        project.timeline = self.base;
        let Some(t) = self.applied else { return Ok(false) };
        let before = project.timeline.clone();
        history.apply(project, self.grab.label(), |p| apply(p, self.clip, self.grab, t))?;
        Ok(project.timeline != before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trackcut_core::media::{AudioStream, MediaInfo, VideoStream};
    use trackcut_core::time::{FrameGrid, Rational};

    const FPS: Rational = Rational::new(60, 1);

    fn f(n: i64) -> Flicks {
        frame_time(n, FPS)
    }

    /// 60fps 영상 600프레임과 같은 길이의 오디오 1개.
    fn project() -> Project {
        let audio = AudioStream {
            index: 1,
            audio_index: 0,
            codec: "aac".into(),
            sample_rate: 48_000,
            channels: 2,
            title: None,
            start: 0,
            duration: f(600),
        };
        let video = VideoStream {
            index: 0,
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            fps: FPS,
            grid: FrameGrid::Cfr { start: 0, fps: FPS, count: 600 },
            keyframes: vec![0],
        };
        let info = MediaInfo { path: "recording.mp4".into(), duration: f(600), video: Some(video), audio: vec![audio] };
        Project::from_media(info, None)
    }

    fn spans(p: &Project, track_number: usize) -> Vec<(Flicks, Flicks)> {
        let track = p.timeline.tracks[track_number].id;
        let mut v: Vec<_> = p.timeline.clips.iter().filter(|c| c.track == track).map(|c| (c.start, c.end())).collect();
        v.sort();
        v
    }

    #[test]
    fn trim_drag_previews_live_and_is_one_undo_step() {
        let mut p = project();
        let mut history = History::default();
        let clip = p.timeline.clips[0].id;
        let mut drag = Drag::begin(&p, clip, Grab::Trim(Edge::End));
        // 프레임 사이를 가리켜도 프레임 경계에 붙는다
        drag.update(&mut p, f(400) + 1000);
        assert_eq!(spans(&p, 0), [(0, f(400))]);
        assert_eq!(spans(&p, 1), [(0, f(400))], "연결된 오디오도 함께 줄어든다");
        drag.update(&mut p, f(300));
        assert!(drag.finish(&mut p, &mut history).unwrap());
        assert_eq!(spans(&p, 0), [(0, f(300))]);
        history.undo(&mut p);
        assert_eq!(spans(&p, 0), [(0, f(600))]);
        assert!(!history.can_undo());
    }

    #[test]
    fn move_drag_stays_at_last_valid_position_when_blocked() {
        let mut p = project();
        let mut history = History::default();
        p.delete_range(f(100), f(200), None, false).unwrap();
        let right = p.timeline.clips.iter().find(|c| c.start == f(200)).unwrap().id;
        let mut drag = Drag::begin(&p, right, Grab::Move);
        drag.update(&mut p, f(150));
        assert_eq!(spans(&p, 0), [(0, f(100)), (f(150), f(550))]);
        drag.update(&mut p, f(50)); // 왼쪽 클립과 겹친다
        assert_eq!(spans(&p, 0), [(0, f(100)), (f(150), f(550))]);
        assert!(drag.finish(&mut p, &mut history).unwrap());
        assert_eq!(spans(&p, 1), [(0, f(100)), (f(150), f(550))]);
        history.undo(&mut p);
        assert_eq!(spans(&p, 0), [(0, f(100)), (f(200), f(600))]);
    }

    #[test]
    fn move_drag_stops_at_timeline_start() {
        let mut p = project();
        p.delete_range(0, f(100), None, false).unwrap();
        let clip = p.timeline.clips[0].id;
        let mut drag = Drag::begin(&p, clip, Grab::Move);
        drag.update(&mut p, -f(500));
        assert_eq!(spans(&p, 0), [(0, f(500))]);
    }

    #[test]
    fn cancelled_drag_restores_the_timeline() {
        let mut p = project();
        p.delete_range(0, f(100), None, false).unwrap();
        let before = p.timeline.clone();
        let clip = p.timeline.clips[0].id;
        let mut drag = Drag::begin(&p, clip, Grab::Move);
        drag.update(&mut p, f(30));
        assert_ne!(p.timeline, before);
        drag.cancel(&mut p);
        assert_eq!(p.timeline, before);
    }

    #[test]
    fn drag_without_change_leaves_no_undo_step() {
        let mut p = project();
        let mut history = History::default();
        let clip = p.timeline.clips[0].id;
        let mut drag = Drag::begin(&p, clip, Grab::Move);
        drag.update(&mut p, 0);
        assert!(!drag.finish(&mut p, &mut history).unwrap());
        assert!(!history.can_undo());
    }
}
