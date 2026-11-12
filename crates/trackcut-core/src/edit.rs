//! 편집 동작: 분할, 구간 삭제, 트림, 이동, 연결/해제, 트랙 소리 설정.
//!
//! 시각은 호출하는 쪽에서 `Project::snap`으로 프레임 경계에 붙여 넘긴다.
//! 연결된 클립(같은 `link`)은 함께 잘리고 움직인다.

use std::collections::HashMap;

use crate::error::{edit_err, Result};
use crate::project::{Clip, Project, Track, VOLUME_MAX_DB, VOLUME_MIN_DB};
use crate::time::Flicks;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

impl Project {
    fn clip_index(&self, id: u64) -> Result<usize> {
        match self.timeline.clips.iter().position(|c| c.id == id) {
            Some(i) => Ok(i),
            None => edit_err(format!("클립 {id}이(가) 없다")),
        }
    }

    pub fn clip(&self, id: u64) -> Result<&Clip> {
        self.clip_index(id).map(|i| &self.timeline.clips[i])
    }

    fn track_mut(&mut self, id: u64) -> Result<&mut Track> {
        match self.timeline.tracks.iter_mut().find(|t| t.id == id) {
            Some(t) => Ok(t),
            None => edit_err(format!("트랙 {id}이(가) 없다")),
        }
    }

    /// 클립과 연결된 클립 전부(자기 자신 포함).
    pub fn group(&self, id: u64) -> Result<Vec<u64>> {
        Ok(match self.clip(id)?.link {
            Some(link) => self.timeline.clips.iter().filter(|c| c.link == Some(link)).map(|c| c.id).collect(),
            None => vec![id],
        })
    }

    /// `i`번 클립을 둘로 나눈다. 왼쪽은 `cut`에서 끝나고, 오른쪽은 타임라인 `resume` 시각의
    /// 내용부터 시작해 `place`에 놓인다. 오른쪽 조각들은 새 연결 묶음을 이룬다.
    fn cut_right(&mut self, i: usize, cut: Flicks, resume: Flicks, place: Flicks, links: &mut HashMap<u64, u64>) -> u64 {
        let mut right = self.timeline.clips[i].clone();
        right.id = self.timeline.alloc();
        right.src_in += resume - right.start;
        right.start = place;
        if let Some(old) = right.link {
            right.link = Some(*links.entry(old).or_insert_with(|| self.timeline.alloc()));
        }
        let left = &mut self.timeline.clips[i];
        left.src_out = left.src_in + (cut - left.start);
        let id = right.id;
        self.timeline.clips.push(right);
        id
    }

    /// 시각 `t`에서 나눈다. `only`가 없으면 그 위치에 걸친 모든 클립을, 있으면 그 클립과
    /// 연결된 클립만 나눈다. 새로 생긴 오른쪽 클립 id를 돌려준다.
    pub fn split_at(&mut self, t: Flicks, only: Option<u64>) -> Result<Vec<u64>> {
        let scope = only.map(|id| self.group(id)).transpose()?;
        let targets: Vec<usize> = self
            .timeline
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| c.start < t && t < c.end())
            .filter(|(_, c)| scope.as_ref().is_none_or(|s| s.contains(&c.id)))
            .map(|(i, _)| i)
            .collect();
        if let Some(id) = only {
            if !targets.iter().any(|&i| self.timeline.clips[i].id == id) {
                return edit_err("분할 위치가 클립 안쪽이 아니다");
            }
        }
        let mut links = HashMap::new();
        Ok(targets.into_iter().map(|i| self.cut_right(i, t, t, t, &mut links)).collect())
    }

    /// 구간 `[a, b)`를 지운다. `ripple`이면 뒤쪽 클립을 당겨 빈자리를 메운다.
    /// `tracks`를 주면 그 트랙만 편집하고(예: 마이크만 삭제), 건드린 클립은 연결이 풀린다.
    pub fn delete_range(&mut self, a: Flicks, b: Flicks, tracks: Option<&[u64]>, ripple: bool) -> Result<()> {
        if a >= b {
            return edit_err("구간의 끝이 시작보다 뒤여야 한다");
        }
        let (len, place) = (b - a, if ripple { a } else { b });
        let mut links = HashMap::new();
        let mut removed = Vec::new();
        let mut touched = Vec::new();
        for i in 0..self.timeline.clips.len() {
            let (id, track) = (self.timeline.clips[i].id, self.timeline.clips[i].track);
            let (start, end) = (self.timeline.clips[i].start, self.timeline.clips[i].end());
            if tracks.is_some_and(|t| !t.contains(&track)) || end <= a {
                continue;
            }
            touched.push(id);
            if start >= b {
                if ripple {
                    self.timeline.clips[i].start -= len;
                } else {
                    touched.pop();
                }
            } else if start >= a && end <= b {
                removed.push(id);
            } else if start < a && end > b {
                let right = self.cut_right(i, a, b, place, &mut links);
                touched.push(right);
            } else if start >= a {
                let clip = &mut self.timeline.clips[i];
                clip.src_in += b - start;
                clip.start = place;
            } else {
                self.timeline.clips[i].src_out -= end - a;
            }
        }
        self.timeline.clips.retain(|c| !removed.contains(&c.id));
        if tracks.is_some() {
            for clip in self.timeline.clips.iter_mut().filter(|c| touched.contains(&c.id)) {
                clip.link = None;
            }
        }
        Ok(())
    }

    /// 같은 트랙에서 `[start, end)`와 겹치는 다른 클립이 있는지.
    fn overlaps(&self, track: u64, start: Flicks, end: Flicks, ignore: &[u64]) -> bool {
        self.timeline
            .clips
            .iter()
            .any(|c| c.track == track && !ignore.contains(&c.id) && c.start < end && start < c.end())
    }

    /// 클립 경계를 `t`로 옮긴다(트림). 연결된 클립 중 같은 경계를 가진 것도 함께 움직인다.
    /// 원본 길이와 이웃 클립을 넘지 않게 조정하고, 실제로 적용된 시각을 돌려준다.
    pub fn trim(&mut self, id: u64, edge: Edge, t: Flicks) -> Result<Flicks> {
        let anchor = self.clip(id)?.clone();
        let edge_time = |c: &Clip| if edge == Edge::Start { c.start } else { c.end() };
        let group = self.group(id)?;
        let members: Vec<usize> = (0..self.timeline.clips.len())
            .filter(|&i| {
                let c = &self.timeline.clips[i];
                group.contains(&c.id) && edge_time(c) == edge_time(&anchor)
            })
            .collect();

        let (mut lo, mut hi) = (0, Flicks::MAX);
        for &i in &members {
            let c = &self.timeline.clips[i];
            let (src_lo, src_hi) = self.source_bounds(c);
            let same_track = || self.timeline.clips.iter().filter(|o| o.track == c.track && o.id != c.id);
            match edge {
                Edge::Start => {
                    let prev_end = same_track().filter(|o| o.end() <= c.start).map(Clip::end).max().unwrap_or(0);
                    lo = lo.max(c.start - (c.src_in - src_lo)).max(prev_end);
                    hi = hi.min(c.end() - 1);
                }
                Edge::End => {
                    let next_start = same_track().filter(|o| o.start >= c.end()).map(|o| o.start).min().unwrap_or(Flicks::MAX);
                    hi = hi.min(c.end() + (src_hi - c.src_out)).min(next_start);
                    lo = lo.max(c.start + 1);
                }
            }
        }
        if lo > hi {
            return edit_err("트림할 수 있는 범위가 없다");
        }
        let t = t.clamp(lo, hi);
        for i in members {
            let c = &mut self.timeline.clips[i];
            match edge {
                Edge::Start => {
                    c.src_in += t - c.start;
                    c.start = t;
                }
                Edge::End => c.src_out = c.src_in + (t - c.start),
            }
        }
        Ok(t)
    }

    /// 클립을 `new_start`로 옮긴다. 연결된 클립은 같은 만큼 함께 움직인다.
    pub fn move_clip(&mut self, id: u64, new_start: Flicks) -> Result<()> {
        let delta = new_start - self.clip(id)?.start;
        let group = self.group(id)?;
        for c in self.timeline.clips.iter().filter(|c| group.contains(&c.id)) {
            if c.start + delta < 0 {
                return edit_err("타임라인 시작보다 앞으로 옮길 수 없다");
            }
            if self.overlaps(c.track, c.start + delta, c.end() + delta, &group) {
                return edit_err("같은 트랙의 다른 클립과 겹친다");
            }
        }
        for c in self.timeline.clips.iter_mut().filter(|c| group.contains(&c.id)) {
            c.start += delta;
        }
        Ok(())
    }

    /// 클립 하나의 연결을 푼다. 이후 따로 자르거나 옮길 수 있다.
    pub fn unlink(&mut self, id: u64) -> Result<()> {
        let i = self.clip_index(id)?;
        self.timeline.clips[i].link = None;
        Ok(())
    }

    /// 클립들을 하나의 연결 묶음으로 만든다.
    pub fn link(&mut self, ids: &[u64]) -> Result<()> {
        let indices = ids.iter().map(|&id| self.clip_index(id)).collect::<Result<Vec<_>>>()?;
        let link = Some(self.timeline.alloc());
        for i in indices {
            self.timeline.clips[i].link = link;
        }
        Ok(())
    }

    /// 트랙 볼륨(dB). −60 ~ +12 범위로 맞춰 적용하고 적용된 값을 돌려준다.
    pub fn set_volume(&mut self, track: u64, db: f32) -> Result<f32> {
        if !db.is_finite() {
            return edit_err("볼륨 값이 올바르지 않다");
        }
        let db = db.clamp(VOLUME_MIN_DB, VOLUME_MAX_DB);
        self.track_mut(track)?.volume_db = db;
        Ok(db)
    }

    pub fn set_muted(&mut self, track: u64, muted: bool) -> Result<()> {
        self.track_mut(track)?.muted = muted;
        Ok(())
    }

    pub fn set_solo(&mut self, track: u64, solo: bool) -> Result<()> {
        self.track_mut(track)?.solo = solo;
        Ok(())
    }

    pub fn rename_track(&mut self, track: u64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return edit_err("트랙 이름이 비어 있다");
        }
        self.track_mut(track)?.name = name.to_string();
        Ok(())
    }

    /// 지금 소리가 나는 트랙인지. 솔로인 트랙이 하나라도 있으면 솔로 트랙만 들린다.
    pub fn is_audible(&self, track: u64) -> bool {
        let any_solo = self.timeline.tracks.iter().any(|t| t.solo);
        self.track(track).is_some_and(|t| if any_solo { t.solo } else { !t.muted })
    }
}
