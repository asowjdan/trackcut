//! trackcut 편집 화면: 위쪽에 미리보기, 아래쪽에 타임라인, 오른쪽에 믹서.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod drag;
mod engine;
mod export_ui;
mod regions;
mod sound;
mod timeline;

use std::path::PathBuf;

use eframe::egui;

/// 실행 인자: `trackcut-app [미디어 또는 프로젝트 ...] [--seek 시각] [--play] [--select 클립순번] [--region 시작초,끝초 ...] [--merge] [--split] [--to-edit] [--edit-view] [--crop] [--pos x,y] [--shot 화면캡처.png]`
#[derive(Default)]
pub struct Options {
    /// 열 파일들. 첫 파일로 프로젝트를 시작하고 나머지는 타임라인 끝에 이어 붙인다
    pub inputs: Vec<PathBuf>,
    /// 시작할 때 이동할 시각
    pub seek: Option<String>,
    /// 화면을 한 번 그린 뒤 이 경로에 PNG로 저장하고 끝낸다(화면 확인용)
    pub shot: Option<PathBuf>,
    /// 열자마자 재생한다. `--shot`과 함께 쓰면 2초 재생한 뒤(구역을 지정했으면 구역 끝에서 멈춘 뒤)의
    /// 화면을 저장한다(재생 확인용)
    pub play: bool,
    /// 열자마자 이 순번(0부터)의 클립을 선택한다(선택 표시 확인용)
    pub select: Option<usize>,
    /// 열자마자 이 구역(`시작초,끝초`)들을 지정한다(구역 표시 확인용). 여러 번 쓸 수 있다
    pub regions: Vec<[f64; 2]>,
    /// 열자마자 구역을 전부 하나로 합친다(병합 확인용)
    pub merge: bool,
    /// 열자마자 첫 구역을 분할한다(분할 결과 확인용)
    pub split: bool,
    /// 열자마자 구역들을 차례로 편집본에 복사한다(편집본 확인용)
    pub to_edit: bool,
    /// 열자마자 미리보기를 편집본 묶음으로 바꾼다
    pub edit_view: bool,
    /// 열자마자 화면 자르기 편집을 켠다(`--select`로 영상 클립을 골라야 보인다)
    pub crop: bool,
    /// 창을 띄울 화면 위치 `x,y`(다른 모니터에 띄울 때)
    pub pos: Option<[f32; 2]>,
}

fn parse_options() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seek" => options.seek = args.next(),
            "--shot" => options.shot = args.next().map(PathBuf::from),
            "--play" => options.play = true,
            "--select" => options.select = args.next().and_then(|n| n.parse().ok()),
            "--crop" => options.crop = true,
            "--split" => options.split = true,
            "--merge" => options.merge = true,
            "--to-edit" => options.to_edit = true,
            "--edit-view" => options.edit_view = true,
            "--region" => {
                let text = args.next().unwrap_or_default();
                let mut ends = text.split(',').filter_map(|n| n.trim().parse().ok());
                options.regions.extend(ends.next().zip(ends.next()).map(|(a, b)| [a, b]));
            }
            "--pos" => {
                let text = args.next().unwrap_or_default();
                let mut xy = text.split(',').filter_map(|n| n.trim().parse().ok());
                options.pos = xy.next().zip(xy.next()).map(|(x, y)| [x, y]);
            }
            _ => options.inputs.push(PathBuf::from(arg)),
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
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("trackcut")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0])
        // 화면 확인용 실행은 사용자의 포커스를 가져가지 않는다
        .with_active(options.shot.is_none());
    if let Some(pos) = options.pos {
        viewport = viewport.with_position(pos);
    }
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
