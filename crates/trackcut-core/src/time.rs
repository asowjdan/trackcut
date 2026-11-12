//! 시간 단위와 프레임 격자.
//!
//! 모든 시각은 flick(1/705,600,000초) 정수로 다룬다. 60fps 프레임(11,760,000)과
//! 48kHz 샘플(14,700)이 모두 정수로 떨어져서 프레임·샘플 경계를 오차 없이 계산한다.

use serde::{Deserialize, Serialize};

pub type Flicks = i64;
pub const FLICKS_PER_SEC: i64 = 705_600_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new(num: i64, den: i64) -> Self {
        Rational { num, den }
    }

    /// "60/1", "1/15360", "60" 형태를 읽는다. 0 이하 값은 거부한다.
    pub fn parse(s: &str) -> Option<Self> {
        let (n, d) = match s.split_once('/') {
            Some((n, d)) => (n.trim().parse().ok()?, d.trim().parse().ok()?),
            None => (s.trim().parse().ok()?, 1),
        };
        (n > 0 && d > 0).then_some(Rational { num: n, den: d })
    }

    pub fn reduced(self) -> Self {
        let g = gcd(self.num, self.den).max(1);
        Rational::new(self.num / g, self.den / g)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn div_round(n: i128, d: i128) -> i128 {
    if n >= 0 {
        (n + d / 2) / d
    } else {
        -((-n + d / 2) / d)
    }
}

/// 스트림 타임스탬프(time_base 단위)를 flick으로 바꾼다.
pub fn ts_to_flicks(ts: i64, tb: Rational) -> Flicks {
    div_round(ts as i128 * tb.num as i128 * FLICKS_PER_SEC as i128, tb.den as i128) as i64
}

pub fn secs_to_flicks(secs: f64) -> Flicks {
    (secs * FLICKS_PER_SEC as f64).round() as i64
}

pub fn flicks_to_secs(t: Flicks) -> f64 {
    t as f64 / FLICKS_PER_SEC as f64
}

/// 고정 fps에서 n번째 프레임의 시작 시각.
pub fn frame_time(n: i64, fps: Rational) -> Flicks {
    div_round(n as i128 * FLICKS_PER_SEC as i128 * fps.den as i128, fps.num as i128) as i64
}

pub fn nearest_frame(t: Flicks, fps: Rational) -> i64 {
    div_round(t as i128 * fps.num as i128, FLICKS_PER_SEC as i128 * fps.den as i128) as i64
}

pub fn floor_frame(t: Flicks, fps: Rational) -> i64 {
    (t as i128 * fps.num as i128).div_euclid(FLICKS_PER_SEC as i128 * fps.den as i128) as i64
}

/// 시각을 샘플 번호로 바꾼다. 48kHz·60fps면 프레임당 정확히 800샘플이다.
pub fn to_samples(t: Flicks, sample_rate: u32) -> i64 {
    div_round(t as i128 * sample_rate as i128, FLICKS_PER_SEC as i128) as i64
}

/// `hh:mm:ss:ff` 타임코드.
pub fn timecode(t: Flicks, fps: Rational) -> String {
    let per_sec = ((fps.num + fps.den / 2) / fps.den).max(1);
    let frame = floor_frame(t.max(0), fps);
    let (secs, ff) = (frame / per_sec, frame % per_sec);
    format!("{:02}:{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60, ff)
}

/// 사용자가 입력한 시각을 읽는다: `123f`(프레임), `hh:mm:ss:ff`, `mm:ss`, `12.5`(초).
pub fn parse_time(s: &str, fps: Rational) -> Option<Flicks> {
    let s = s.trim();
    if let Some(frames) = s.strip_suffix('f') {
        return Some(frame_time(frames.parse().ok()?, fps));
    }
    if s.contains(':') {
        let parts: Vec<i64> = s.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
        let per_sec = ((fps.num + fps.den / 2) / fps.den).max(1);
        let (h, m, sec, ff) = match parts[..] {
            [h, m, sec, ff] => (h, m, sec, ff),
            [h, m, sec] => (h, m, sec, 0),
            [m, sec] => (0, m, sec, 0),
            _ => return None,
        };
        return Some(frame_time((h * 3600 + m * 60 + sec) * per_sec + ff, fps));
    }
    let secs: f64 = s.parse().ok()?;
    secs.is_finite().then(|| secs_to_flicks(secs))
}

/// 원본 파일의 실제 프레임 시각. 편집점은 이 경계에 붙는다.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameGrid {
    /// 프레임 간격이 일정한 파일
    Cfr { start: Flicks, fps: Rational, count: u64 },
    /// 프레임 간격이 고르지 않은 파일(VFR). `pts`는 정렬된 프레임 시작 시각.
    Vfr { pts: Vec<Flicks>, end: Flicks },
}

impl FrameGrid {
    pub fn count(&self) -> u64 {
        match self {
            FrameGrid::Cfr { count, .. } => *count,
            FrameGrid::Vfr { pts, .. } => pts.len() as u64,
        }
    }

    pub fn is_cfr(&self) -> bool {
        matches!(self, FrameGrid::Cfr { .. })
    }

    /// n번째 프레임의 시작 시각. n이 프레임 수와 같으면 끝 시각.
    pub fn time_of(&self, n: u64) -> Flicks {
        let n = n.min(self.count());
        match self {
            FrameGrid::Cfr { start, fps, .. } => start + frame_time(n as i64, *fps),
            FrameGrid::Vfr { pts, end } => pts.get(n as usize).copied().unwrap_or(*end),
        }
    }

    pub fn start(&self) -> Flicks {
        self.time_of(0)
    }

    pub fn end(&self) -> Flicks {
        self.time_of(self.count())
    }

    /// 시각 t에 화면에 보이는 프레임 번호.
    pub fn frame_at(&self, t: Flicks) -> u64 {
        let last = self.count().saturating_sub(1);
        match self {
            FrameGrid::Cfr { start, fps, .. } => (floor_frame(t - start, *fps).max(0) as u64).min(last),
            FrameGrid::Vfr { pts, .. } => {
                (pts.partition_point(|&p| p <= t).saturating_sub(1) as u64).min(last)
            }
        }
    }

    /// 가장 가까운 프레임 경계(끝 시각 포함)로 붙인다.
    pub fn snap(&self, t: Flicks) -> Flicks {
        match self {
            FrameGrid::Cfr { start, fps, count } => {
                let n = nearest_frame(t - start, *fps).clamp(0, *count as i64);
                start + frame_time(n, *fps)
            }
            FrameGrid::Vfr { pts, .. } => {
                let i = pts.partition_point(|&p| p <= t);
                let before = self.time_of(i.saturating_sub(1) as u64);
                let after = self.time_of(i as u64);
                if (t - before).abs() <= (after - t).abs() {
                    before
                } else {
                    after
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS60: Rational = Rational::new(60, 1);

    #[test]
    fn frame_is_800_samples_at_48k_60fps() {
        assert_eq!(to_samples(frame_time(1, FPS60), 48_000), 800);
        assert_eq!(to_samples(frame_time(7_201, FPS60), 48_000), 7_201 * 800);
    }

    #[test]
    fn timestamps_convert_exactly() {
        // OBS mp4의 흔한 time_base 1/15360에서 60fps 프레임은 256틱
        assert_eq!(ts_to_flicks(256, Rational::new(1, 15_360)), frame_time(1, FPS60));
    }

    #[test]
    fn timecode_roundtrip() {
        let t = parse_time("00:12:03:59", FPS60).unwrap();
        assert_eq!(timecode(t, FPS60), "00:12:03:59");
        assert_eq!(parse_time("120f", FPS60), parse_time("2", FPS60));
        assert_eq!(parse_time("abc", FPS60), None);
    }

    #[test]
    fn cfr_snap_clamps_to_range() {
        let g = FrameGrid::Cfr { start: 0, fps: FPS60, count: 120 };
        assert_eq!(g.snap(frame_time(10, FPS60) + 5), frame_time(10, FPS60));
        assert_eq!(g.snap(i64::MAX / 4), g.end());
        assert_eq!(g.frame_at(g.end()), 119);
    }

    #[test]
    fn vfr_snap_uses_real_frame_times() {
        let g = FrameGrid::Vfr { pts: vec![0, 100, 350, 400], end: 500 };
        assert_eq!(g.snap(240), 350);
        assert_eq!(g.snap(200), 100);
        assert_eq!(g.snap(480), 500);
        assert_eq!(g.frame_at(349), 1);
    }
}
