//! trackcut 편집 화면: 위쪽에 미리보기, 아래쪽에 타임라인, 오른쪽에 믹서.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod timeline;

use std::path::PathBuf;

use eframe::egui;

/// 실행 인자: `trackcut-app [미디어 또는 프로젝트] [--seek 시각] [--shot 화면캡처.png]`
#[derive(Default)]
pub struct Options {
    pub input: Option<PathBuf>,
    /// 시작할 때 이동할 시각
    pub seek: Option<String>,
    /// 화면을 한 번 그린 뒤 이 경로에 PNG로 저장하고 끝낸다(화면 확인용)
    pub shot: Option<PathBuf>,
}

fn parse_options() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seek" => options.seek = args.next(),
            "--shot" => options.shot = args.next().map(PathBuf::from),
            _ => options.input = Some(PathBuf::from(arg)),
        }
    }
    options
}

/// egui 기본 글꼴에는 한글이 없어서 Windows의 맑은 고딕을 추가한다.
fn setup_fonts(ctx: &egui::Context) {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let Ok(bytes) = std::fs::read(format!("{root}\\Fonts\\malgun.ttf")) else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("korean".into(), egui::FontData::from_owned(bytes));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("korean".into());
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    let options = parse_options();
    let viewport = egui::ViewportBuilder::default()
        .with_title("trackcut")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0])
        // 화면 확인용 실행은 사용자의 포커스를 가져가지 않는다
        .with_active(options.shot.is_none());
    let native = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "trackcut",
        native,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(app::TrackcutApp::new(cc.egui_ctx.clone(), options)))
        }),
    )
}
