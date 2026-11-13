//! 화면 배치: 패널의 위치와 크기, 창 크기. 설정 파일에 저장해 다음 실행에도 유지한다.
//!
//! 프로젝트 파일이 아니라 사용자 설정 폴더(`trackcut/layout.json`)에 둔다.
//! 파일이 없거나 깨져 있으면 기본 배치로 시작한다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 편집 패널 폭과 타임라인 높이의 허용 범위.
pub const PANEL_WIDTH: (f32, f32) = (220.0, 700.0);
pub const TIMELINE_HEIGHT: (f32, f32) = (150.0, 1200.0);
const WINDOW_MIN: [f32; 2] = [800.0, 500.0];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// 편집 패널을 오른쪽에 둔다(아니면 왼쪽)
    pub panel_right: bool,
    /// 편집 패널을 숨긴다
    pub panel_hidden: bool,
    /// 타임라인을 위에 둔다(아니면 아래)
    pub timeline_top: bool,
    pub panel_width: f32,
    pub timeline_height: f32,
    /// 창 안쪽 크기(가로, 세로)
    pub window: [f32; 2],
}

impl Default for Layout {
    fn default() -> Self {
        Layout { panel_right: false, panel_hidden: false, timeline_top: false, panel_width: 270.0, timeline_height: 320.0, window: [1280.0, 800.0] }
    }
}

impl Layout {
    /// 설정 파일 경로. 환경 변수 `TRACKCUT_CONFIG_DIR`이 있으면 그 폴더를 쓴다(확인·테스트용).
    pub fn path() -> Option<PathBuf> {
        let folder = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let base = folder("TRACKCUT_CONFIG_DIR")
            .or_else(|| folder("APPDATA").map(|dir| dir.join("trackcut")))
            .or_else(|| folder("XDG_CONFIG_HOME").map(|dir| dir.join("trackcut")))
            .or_else(|| folder("HOME").map(|dir| dir.join(".config").join("trackcut")))?;
        Some(base.join("layout.json"))
    }

    /// 확인·테스트용 설정 폴더를 따로 지정했는지.
    pub fn is_overridden() -> bool {
        std::env::var_os("TRACKCUT_CONFIG_DIR").is_some_and(|v| !v.is_empty())
    }

    /// 저장된 배치를 읽는다. 파일이 없거나 읽을 수 없으면 기본 배치.
    pub fn load(path: &Path) -> Self {
        let saved = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Layout>(&bytes).ok());
        saved.unwrap_or_default().sane()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        // 임시 파일에 쓴 뒤 이름을 바꿔서, 저장 중 중단돼도 기존 설정이 깨지지 않게 한다
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// 값이 쓸 수 없는 범위(너무 작거나 숫자가 아님)면 쓸 수 있는 값으로 고친다.
    pub fn sane(mut self) -> Self {
        let fallback = Layout::default();
        let fix = |value: f32, (low, high): (f32, f32), default: f32| if value.is_finite() { value.clamp(low, high) } else { default };
        self.panel_width = fix(self.panel_width, PANEL_WIDTH, fallback.panel_width);
        self.timeline_height = fix(self.timeline_height, TIMELINE_HEIGHT, fallback.timeline_height);
        for axis in 0..2 {
            self.window[axis] = fix(self.window[axis], (WINDOW_MIN[axis], 16_000.0), fallback.window[axis]);
        }
        self
    }

    /// 창 크기만 그대로 두고 나머지를 기본값으로 돌린 배치.
    pub fn reset(self) -> Self {
        Layout { window: self.window, ..Layout::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trackcut-layout-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("설정").join("layout.json")
    }

    #[test]
    fn saved_layout_comes_back_on_the_next_start() {
        let path = temp_file("roundtrip");
        let layout = Layout { panel_right: true, panel_hidden: false, timeline_top: true, panel_width: 412.0, timeline_height: 505.0, window: [1600.0, 900.0] };
        layout.save(&path).unwrap();
        assert_eq!(Layout::load(&path), layout);
        // 다시 저장해도(덮어쓰기) 그대로 읽힌다
        let changed = Layout { panel_width: 300.0, ..layout };
        changed.save(&path).unwrap();
        assert_eq!(Layout::load(&path), changed);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn missing_or_broken_file_gives_the_default_layout() {
        let path = temp_file("broken");
        assert_eq!(Layout::load(&path), Layout::default());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ 깨진 파일").unwrap();
        assert_eq!(Layout::load(&path), Layout::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn older_or_odd_files_are_filled_in_and_clamped() {
        let path = temp_file("partial");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 일부 값만 있는 파일: 없는 값은 기본값으로 채운다
        std::fs::write(&path, r#"{ "panel_right": true, "panel_width": 5.0, "timeline_height": 99999.0, "window": [10.0, 700.0] }"#).unwrap();
        let layout = Layout::load(&path);
        assert!(layout.panel_right && !layout.timeline_top);
        assert_eq!(layout.panel_width, PANEL_WIDTH.0);
        assert_eq!(layout.timeline_height, TIMELINE_HEIGHT.1);
        assert_eq!(layout.window, [800.0, 700.0]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn reset_keeps_the_window_size() {
        let layout = Layout { panel_right: true, panel_width: 500.0, window: [1600.0, 900.0], ..Layout::default() };
        assert_eq!(layout.reset(), Layout { window: [1600.0, 900.0], ..Layout::default() });
    }
}
