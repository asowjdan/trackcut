//! 편집 동작: 분할, 구간 삭제, 트림, 이동, 연결/해제, 재생 속도, 화면 자르기, 트랙 소리 설정.
//!
//! 시각은 호출하는 쪽에서 `Project::snap`으로 프레임 경계에 붙여 넘긴다.
//! 연결된 클립(같은 `link`)은 함께 잘리고 움직인다.

use std::collections::HashMap;

use crate::error::{edit_err, Result};
use crate::media::MediaInfo;
use crate::project::{Clip, Crop, MediaEntry, Project, Track, TrackKind, SPEED_MAX_PCT, SPEED_MIN_PCT, VOLUME_MAX_DB, VOLUME_MIN_DB};
use crate::time::{floor_frame, frame_time, Flicks};

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
        right.src_in = right.src_at(resume);
        right.start = place;
        if let Some(old) = right.link {
            right.link = Some(*links.entry(old).or_insert_with(|| self.timeline.alloc()));
        }
        let left = &mut self.timeline.clips[i];
        left.src_out = left.src_at(cut);
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

    /// `tracks`에 있는 클립만 시각 `t`에서 나눈다.
    fn split_in(&mut self, t: Flicks, tracks: &[u64]) {
        let clips = self.timeline.clips.iter().enumerate();
        let targets: Vec<usize> = clips.filter(|(_, c)| c.start < t && t < c.end() && tracks.contains(&c.track)).map(|(i, _)| i).collect();
        let mut links = HashMap::new();
        for i in targets {
            self.cut_right(i, t, t, t, &mut links);
        }
    }

    /// 구간 `[a, b)`의 양 끝에서 `tracks`의 클립을 나누고, 그 안에 들어온 조각을 전부 하나의
    /// 연결 묶음으로 만든다. 구간이 여러 영상에 걸쳐 있어도 한 덩어리로 선택되고 움직인다.
    /// 묶인 조각의 id를 돌려준다.
    pub fn split_range(&mut self, a: Flicks, b: Flicks, tracks: &[u64]) -> Result<Vec<u64>> {
        if a >= b {
            return edit_err("구간의 끝이 시작보다 뒤여야 한다");
        }
        self.split_in(a, tracks);
        self.split_in(b, tracks);
        let inside = |c: &Clip| tracks.contains(&c.track) && c.start >= a && c.end() <= b;
        if !self.timeline.clips.iter().any(inside) {
            return edit_err("구역 안에 클립이 없다");
        }
        let link = Some(self.timeline.alloc());
        let mut ids = Vec::new();
        for clip in self.timeline.clips.iter_mut().filter(|c| inside(c)) {
            clip.link = link;
            ids.push(clip.id);
        }
        Ok(ids)
    }

    /// 연결 묶음이 타임라인에서 차지하는 범위(가장 이른 시작, 가장 늦은 끝).
    pub fn group_span(&self, id: u64) -> Result<(Flicks, Flicks)> {
        let group = self.group(id)?;
        let members = || self.timeline.clips.iter().filter(|c| group.contains(&c.id));
        Ok((members().map(|c| c.start).min().unwrap_or(0), members().map(Clip::end).max().unwrap_or(0)))
    }

    /// 편집본 묶음에서 원본 트랙 `source`의 짝이 되는 트랙. 없으면 만든다.
    /// 편집본 트랙은 원본 트랙들 아래에 원본과 같은 순서로 놓인다.
    fn edit_track_for(&mut self, source: u64) -> Result<u64> {
        if let Some(track) = self.timeline.tracks.iter().find(|t| t.edit_of == Some(source)) {
            return Ok(track.id);
        }
        let order = |tracks: &[Track], id: u64| tracks.iter().position(|t| t.id == id);
        let Some(rank) = order(&self.timeline.tracks, source) else {
            return edit_err(format!("트랙 {source}이(가) 없다"));
        };
        let original = self.timeline.tracks[rank].clone();
        let id = self.timeline.alloc();
        let tracks = &self.timeline.tracks;
        // 짝인 원본이 더 아래에 있는 첫 편집본 트랙 앞에 넣는다
        let later = |t: &Track| t.edit_of.and_then(|s| order(tracks, s)).is_some_and(|r| r > rank);
        let at = tracks.iter().position(later).unwrap_or(tracks.len());
        let track = Track { id, name: format!("편집 · {}", original.name), solo: false, edit_of: Some(source), ..original };
        self.timeline.tracks.insert(at, track);
        Ok(id)
    }

    /// 원본 묶음의 구간 `[a, b)`를 편집본 묶음에 복사한다. 원본은 그대로 남는다.
    /// 복사한 조각들은 하나의 연결 묶음이 되어 `at`부터 놓이고, 그 자리가 다른 편집본 클립과
    /// 겹치면 편집본의 맨 끝에 놓인다. 새 클립의 id를 돌려준다.
    pub fn copy_to_edit(&mut self, a: Flicks, b: Flicks, at: Flicks) -> Result<Vec<u64>> {
        if a >= b {
            return edit_err("구간의 끝이 시작보다 뒤여야 한다");
        }
        let sources: Vec<Clip> = self.timeline.clips.iter().filter(|c| !self.in_edit(c) && c.start < b && a < c.end()).cloned().collect();
        if sources.is_empty() {
            return edit_err("복사할 내용이 없다");
        }
        let mut pieces = Vec::new();
        for source in sources {
            let (from, to) = (source.start.max(a), source.end().min(b));
            let mut piece = source.clone();
            // 잘리지 않은 쪽은 원본 값을 그대로 써서 반올림 오차가 생기지 않게 한다
            if from > source.start {
                piece.src_in = source.src_at(from);
            }
            if to < source.end() {
                piece.src_out = source.src_at(to);
            }
            piece.start = from - a;
            piece.track = self.edit_track_for(source.track)?;
            if !piece.is_empty() {
                pieces.push(piece);
            }
        }
        let clash = |p: &Project, at: Flicks| pieces.iter().any(|c| p.overlaps(c.track, at + c.start, at + c.end(), &[]));
        let at = if clash(self, at.max(0)) { self.group_duration(true) } else { at.max(0) };
        let link = Some(self.timeline.alloc());
        let mut ids = Vec::new();
        for mut piece in pieces {
            piece.id = self.timeline.alloc();
            piece.start += at;
            piece.link = link;
            ids.push(piece.id);
            self.timeline.clips.push(piece);
        }
        Ok(ids)
    }

    /// 구간 `[a, b)`를 지운다. `ripple`이면 뒤쪽 클립을 당겨 빈자리를 메운다.
    /// `tracks`를 주면 그 트랙만 편집하고(예: 마이크만 삭제), 건드린 클립은 연결이 풀린다.
    pub fn delete_range(&mut self, a: Flicks, b: Flicks, tracks: Option<&[u64]>, ripple: bool) -> Result<()> {
        self.delete_impl(a, b, tracks, ripple, tracks.is_some())
    }

    /// `delete_range`와 같지만 한 묶음(원본 또는 편집본)의 트랙 전체를 지우는 경우에 쓴다.
    /// 건드린 클립의 연결을 풀지 않는다.
    pub fn delete_range_in(&mut self, a: Flicks, b: Flicks, tracks: &[u64], ripple: bool) -> Result<()> {
        self.delete_impl(a, b, Some(tracks), ripple, false)
    }

    fn delete_impl(&mut self, a: Flicks, b: Flicks, tracks: Option<&[u64]>, ripple: bool, unlink: bool) -> Result<()> {
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
                clip.src_in = clip.src_at(b);
                clip.start = place;
            } else {
                let clip = &mut self.timeline.clips[i];
                clip.src_out = clip.src_at(a);
            }
        }
        self.timeline.clips.retain(|c| !removed.contains(&c.id));
        if unlink {
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
                    lo = lo.max(c.start - c.to_timeline(c.src_in - src_lo)).max(prev_end);
                    hi = hi.min(c.end() - 1);
                }
                Edge::End => {
                    let next_start = same_track().filter(|o| o.start >= c.end()).map(|o| o.start).min().unwrap_or(Flicks::MAX);
                    hi = hi.min(c.end() + c.to_timeline(src_hi - c.src_out)).min(next_start);
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
                    c.src_in = c.src_at(t);
                    c.start = t;
                }
                Edge::End => c.src_out = c.src_at(t),
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

    /// 클립(과 연결된 묶음)의 자리를 같은 트랙의 바로 뒤(`forward`)나 바로 앞 클립 묶음과 바꾼다.
    /// 묶음은 차지하는 범위 전체가 통째로 움직이고, 두 묶음 사이의 빈 구간은 그대로 둔다.
    pub fn swap_with_neighbor(&mut self, id: u64, forward: bool) -> Result<()> {
        let track = self.clip(id)?.track;
        let (group, span) = (self.group(id)?, self.group_span(id)?);
        let others = self.timeline.clips.iter().filter(|c| c.track == track && !group.contains(&c.id));
        let neighbor = if forward {
            others.filter(|c| c.start >= span.1).min_by_key(|c| c.start)
        } else {
            others.filter(|c| c.end() <= span.0).max_by_key(|c| c.start)
        };
        let Some(neighbor) = neighbor.map(|c| c.id) else {
            return edit_err("자리를 바꿀 이웃 클립이 없다");
        };
        let (other, other_span) = (self.group(neighbor)?, self.group_span(neighbor)?);
        let (front, back, first, second) = if forward { (group, other, span, other_span) } else { (other, group, other_span, span) };
        let (to_back, to_front) = (second.1 - first.1, first.0 - second.0);
        for c in self.timeline.clips.iter_mut() {
            if front.contains(&c.id) {
                c.start += to_back;
            } else if back.contains(&c.id) {
                c.start += to_front;
            }
        }
        let clips = &self.timeline.clips;
        let clash = clips.iter().any(|a| a.start < 0 || clips.iter().any(|b| a.id != b.id && a.track == b.track && a.start < b.end() && b.start < a.end()));
        if clash {
            return edit_err("자리를 바꾸면 다른 클립과 겹친다");
        }
        Ok(())
    }

    /// 보정한 소리 파일들을 새 트랙으로 올린다. `sources`는 (원본 미디어, 오디오 스트림 번호, 보정한 파일 정보).
    /// 새 트랙은 원본 트랙 바로 아래에 생기고 원본과 같은 자리에 같은 길이의 클립이 놓인다.
    /// 원본 트랙은 지우지 않고 음소거만 해서, 솔로·음소거로 전후를 비교할 수 있다.
    pub fn add_processed_track(&mut self, source_track: u64, sources: Vec<(u64, u32, MediaInfo)>) -> Result<u64> {
        let Some(position) = self.timeline.tracks.iter().position(|t| t.id == source_track) else {
            return edit_err(format!("트랙 {source_track}이(가) 없다"));
        };
        let source = self.timeline.tracks[position].clone();
        let track = self.timeline.alloc();
        let name = format!("{} (보정)", source.name);
        self.timeline.tracks.insert(position + 1, Track { id: track, kind: TrackKind::Audio, name, volume_db: source.volume_db, muted: false, solo: false, edit_of: source.edit_of });
        let mut added = Vec::new();
        for (media, stream, info) in sources {
            let old_start = self.media_info(media).and_then(|m| m.audio.iter().find(|a| a.audio_index == stream)).map(|a| a.start);
            let (Some(old_start), Some(new_start)) = (old_start, info.audio.first().map(|a| a.start)) else {
                return edit_err("보정한 파일에 소리가 없다");
            };
            let new_media = self.media.iter().map(|m| m.id).chain(added.iter().map(|m: &MediaEntry| m.id)).max().unwrap_or(0) + 1;
            let originals: Vec<Clip> =
                self.timeline.clips.iter().filter(|c| c.track == source_track && c.media == media && c.stream == stream).cloned().collect();
            for mut clip in originals {
                clip.id = self.timeline.alloc();
                clip.track = track;
                clip.media = new_media;
                clip.stream = 0;
                clip.src_in += new_start - old_start;
                clip.src_out += new_start - old_start;
                self.timeline.clips.push(clip);
            }
            added.push(MediaEntry { id: new_media, info });
        }
        if !self.timeline.clips.iter().any(|c| c.track == track) {
            return edit_err("보정할 클립이 없다");
        }
        self.track_mut(source_track)?.muted = true;
        self.media.extend(added);
        Ok(track)
    }

    /// 재생 속도(%)를 바꾼다. 연결된 클립도 같은 속도가 되고, 뒤쪽 클립은 달라진 길이만큼 밀리거나
    /// 당겨진다. 새 길이는 프레임 경계에 맞춘다(끝의 1프레임 미만은 잘린다).
    /// 소리는 속도와 함께 음높이도 바뀐다.
    pub fn set_speed(&mut self, id: u64, pct: u32) -> Result<()> {
        if !(SPEED_MIN_PCT..=SPEED_MAX_PCT).contains(&pct) {
            return edit_err(format!("속도는 {SPEED_MIN_PCT}% ~ {SPEED_MAX_PCT}% 사이여야 한다"));
        }
        let anchor = self.clip(id)?.clone();
        if anchor.speed_pct == pct {
            return Ok(());
        }
        let group = self.group(id)?;
        let fps = self.settings.fps;
        let old_end = anchor.end();
        let exact = Clip { speed_pct: pct, ..anchor.clone() }.len();
        let new_end = anchor.start + frame_time(floor_frame(exact, fps).max(1), fps);
        // 묶음 가운데 이 클립과 같은 시간대에 있는 것만 속도를 바꾼다(여러 조각을 묶은 구간은 조각마다 바꾼다)
        let beside = |c: &Clip| group.contains(&c.id) && c.start < old_end && anchor.start < c.end();
        // 뒤쪽 클립은 같은 묶음(원본 또는 편집본) 안에서만 밀린다
        let same_side = self.group_tracks(self.in_edit(&anchor));
        for c in self.timeline.clips.iter_mut() {
            if !same_side.contains(&c.track) {
                continue;
            }
            if beside(c) {
                let follows = c.id == id || c.end() == old_end;
                c.speed_pct = pct;
                if follows {
                    c.src_out = c.src_at(new_end).min(c.src_out);
                }
            } else if c.start >= old_end {
                c.start += new_end - old_end;
            }
        }
        Ok(())
    }

    /// 영상 클립의 화면을 자른다. 잘린 화면은 비율을 지킨 채 프로젝트 화면에 맞춰 키운다.
    pub fn set_crop(&mut self, id: u64, crop: Crop) -> Result<()> {
        let clip = self.clip(id)?;
        if self.track(clip.track).is_none_or(|t| t.kind != TrackKind::Video) {
            return edit_err("영상 클립만 화면을 자를 수 있다");
        }
        let video = self.media_info(clip.media).and_then(|m| m.video.as_ref());
        let Some((width, height)) = video.map(|v| (v.width, v.height)) else {
            return edit_err("영상 스트림이 없다");
        };
        if crop.left as u64 + crop.right as u64 + 16 > width as u64 || crop.top as u64 + crop.bottom as u64 + 16 > height as u64 {
            return edit_err("자르고 남는 화면이 너무 작다");
        }
        let i = self.clip_index(id)?;
        self.timeline.clips[i].crop = crop;
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

    /// 클립 하나만 소리를 끄거나 켠다. 같은 트랙의 다른 클립은 그대로다.
    pub fn set_clip_muted(&mut self, id: u64, muted: bool) -> Result<()> {
        let i = self.clip_index(id)?;
        self.timeline.clips[i].muted = muted;
        Ok(())
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
    /// 솔로는 같은 묶음(원본 또는 편집본) 안에서만 따진다.
    pub fn is_audible(&self, track: u64) -> bool {
        let Some(track) = self.track(track) else { return false };
        let any_solo = self.timeline.tracks.iter().any(|t| t.solo && t.is_edit() == track.is_edit());
        if any_solo {
            track.solo
        } else {
            !track.muted
        }
    }
}
