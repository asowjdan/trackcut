//! 구역: 타임라인에서 지정한 시작~끝. 여러 개를 두고, 여러 개를 함께 선택해 하나로 합칠 수 있다.
//! 편집 결과가 아니라 화면 상태라서 프로젝트 파일에는 저장하지 않는다.

use trackcut_core::time::Flicks;

/// 구역 하나: (시작, 끝).
pub type Region = (Flicks, Flicks);

#[derive(Default)]
pub struct Regions {
    list: Vec<Region>,
    /// 선택한 구역 번호들. 마지막이 가장 최근에 고른 것이다
    picked: Vec<usize>,
}

impl Regions {
    pub fn list(&self) -> &[Region] {
        &self.list
    }

    pub fn picked(&self) -> &[usize] {
        &self.picked
    }

    pub fn is_picked(&self, index: usize) -> bool {
        self.picked.contains(&index)
    }

    /// 가장 최근에 고른 구역. 재생과 단축키는 이 구역을 따른다.
    pub fn current(&self) -> Option<usize> {
        self.picked.last().copied()
    }

    pub fn get(&self, index: usize) -> Option<Region> {
        self.list.get(index).copied()
    }

    pub fn clear(&mut self) {
        self.list.clear();
        self.picked.clear();
    }

    pub fn deselect(&mut self) {
        self.picked.clear();
    }

    /// 구역을 만들고 그것만 선택한다. 길이가 없으면 만들지 않는다.
    pub fn add(&mut self, region: Region) {
        if region.1 > region.0 {
            self.list.push(region);
            self.picked = vec![self.list.len() - 1];
        }
    }

    /// 구역을 고른다. `extend`(Shift + 클릭)면 이미 고른 것에 더하고, 다시 누르면 뺀다.
    pub fn select(&mut self, index: usize, extend: bool) {
        if index >= self.list.len() {
            return;
        }
        match self.picked.iter().position(|&i| i == index) {
            _ if !extend => self.picked = vec![index],
            Some(at) => {
                self.picked.remove(at);
            }
            None => self.picked.push(index),
        }
    }

    /// 구역의 시작(또는 끝)을 옮긴다. 시작과 끝이 서로를 넘어가지 않는다.
    pub fn set_edge(&mut self, index: usize, is_end: bool, t: Flicks) {
        match self.list.get_mut(index) {
            Some(region) if is_end => region.1 = t.max(region.0),
            Some(region) => region.0 = t.min(region.1),
            None => {}
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.list.len() {
            self.list.remove(index);
            self.picked.clear();
        }
    }

    /// 선택한 구역들을 하나로 합친다. 가장 이른 시작부터 가장 늦은 끝까지가 한 구역이 되고
    /// (사이의 빈 곳도 들어간다), 그 구역이 선택된다. 둘 이상 골랐을 때만 합친다.
    pub fn merge(&mut self) -> Option<Region> {
        if self.picked.len() < 2 {
            return None;
        }
        let chosen = self.picked.iter().filter_map(|&i| self.list.get(i));
        let merged = (chosen.clone().map(|r| r.0).min()?, chosen.map(|r| r.1).max()?);
        let first = *self.picked.iter().min()?;
        let picked = std::mem::take(&mut self.picked);
        let mut index = 0;
        self.list.retain(|_| {
            index += 1;
            !picked.contains(&(index - 1))
        });
        self.list.insert(first.min(self.list.len()), merged);
        self.picked = vec![first.min(self.list.len() - 1)];
        Some(merged)
    }

    /// 구역 `index`의 내용을 잘라내고 뒤를 당겨 붙인 뒤의 정리: 그 구역은 없애고,
    /// 뒤쪽 구역은 함께 당기고, 걸쳐 있던 구역은 잘린 만큼 줄인다.
    pub fn after_cut(&mut self, index: usize) {
        let Some((a, b)) = self.get(index) else { return };
        self.list.remove(index);
        // 잘린 곳 안쪽의 시각은 잘린 자리로 모이고, 그 뒤의 시각은 잘린 길이만큼 당겨진다
        let moved = |t: Flicks| if t >= b { t - (b - a) } else { t.min(a) };
        for region in &mut self.list {
            *region = (moved(region.0), moved(region.1));
        }
        self.list.retain(|r| r.1 > r.0);
        self.picked.clear();
    }
}

/// 재생을 시작할 위치와 멈출 위치. 구역을 선택한 채 재생하면 보고 있는 프레임부터 그 구역의
/// 끝까지만 재생한다. 재생 위치가 구역 밖이면 구역의 시작부터 재생한다.
/// 구역이 없으면 끝까지 재생하고, 이미 끝에 있으면 처음부터 다시 재생한다.
pub fn play_span(playhead: Flicks, region: Option<Region>, duration: Flicks) -> (Flicks, Option<Flicks>) {
    match region.map(|(a, b)| (a, b.min(duration))).filter(|(a, b)| b > a) {
        Some((a, b)) if a <= playhead && playhead < b => (playhead, Some(b)),
        Some((a, b)) => (a, Some(b)),
        None if playhead >= duration => (0, None),
        None => (playhead, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions(list: &[Region]) -> Regions {
        let mut regions = Regions::default();
        for &region in list {
            regions.add(region);
        }
        regions.deselect();
        regions
    }

    #[test]
    fn playing_with_a_region_stops_at_its_end() {
        assert_eq!(play_span(150, Some((100, 200)), 1000), (150, Some(200)), "보고 있는 프레임부터 구역 끝까지");
        assert_eq!(play_span(100, Some((100, 200)), 1000), (100, Some(200)));
        assert_eq!(play_span(50, Some((100, 200)), 1000), (100, Some(200)), "구역 밖이면 구역의 시작부터");
        assert_eq!(play_span(200, Some((100, 200)), 1000), (100, Some(200)), "구역 끝에서 다시 누르면 구역을 다시 재생");
        assert_eq!(play_span(900, Some((800, 1200)), 1000), (900, Some(1000)), "타임라인 끝을 넘지 않는다");
    }

    #[test]
    fn playing_without_a_region_runs_to_the_end() {
        assert_eq!(play_span(150, None, 1000), (150, None));
        assert_eq!(play_span(1000, None, 1000), (0, None), "끝에서는 처음부터");
        assert_eq!(play_span(150, Some((300, 300)), 1000), (150, None), "길이가 없는 구역은 무시한다");
    }

    #[test]
    fn shift_click_adds_and_removes_regions_from_the_selection() {
        let mut r = regions(&[(0, 10), (20, 30), (40, 50)]);
        r.select(0, false);
        r.select(2, true);
        assert_eq!(r.picked(), [0, 2]);
        assert_eq!(r.current(), Some(2));
        r.select(2, true);
        assert_eq!(r.picked(), [0], "고른 구역을 Shift로 다시 누르면 빠진다");
        r.select(1, false);
        assert_eq!(r.picked(), [1], "그냥 누르면 그것만 선택된다");
        r.select(9, true);
        assert_eq!(r.picked(), [1], "없는 구역은 무시한다");
    }

    #[test]
    fn merge_joins_the_selected_regions_into_one() {
        let mut r = regions(&[(0, 10), (40, 50), (20, 30), (60, 70)]);
        r.select(2, false);
        r.select(1, true);
        assert_eq!(r.merge(), Some((20, 50)));
        assert_eq!(r.list(), [(0, 10), (20, 50), (60, 70)]);
        assert_eq!(r.picked(), [1], "합친 구역이 선택된다");
        assert_eq!(r.merge(), None, "하나만 골랐으면 합치지 않는다");
    }

    #[test]
    fn merging_everything_leaves_one_region() {
        let mut r = regions(&[(0, 10), (5, 30), (40, 50)]);
        for index in 0..3 {
            r.select(index, true);
        }
        assert_eq!(r.merge(), Some((0, 50)));
        assert_eq!(r.list(), [(0, 50)]);
        assert_eq!(r.picked(), [0]);
    }

    #[test]
    fn cutting_a_region_pulls_later_regions_forward() {
        let mut r = regions(&[(100, 200), (300, 400), (150, 250), (0, 50)]);
        r.after_cut(0);
        assert_eq!(r.list(), [(200, 300), (100, 150), (0, 50)], "뒤쪽은 당겨지고, 걸쳐 있던 구역은 잘린 만큼 줄어든다");
        let mut r = regions(&[(100, 200), (120, 180)]);
        r.after_cut(0);
        assert!(r.list().is_empty(), "잘린 곳 안에만 있던 구역은 사라진다");
        assert!(r.picked().is_empty());
    }
}
