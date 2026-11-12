//! 내보내기 창: 방식과 화질을 고르고, 진행률을 보며 취소할 수 있다.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use trackcut_core::export::{export, AudioMode, ExportOptions, Quality};
use trackcut_core::project::Project;

struct Job {
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    done: Receiver<Result<Vec<PathBuf>, String>>,
    started: Instant,
}

pub struct ExportUi {
    pub open: bool,
    mode: AudioMode,
    quality: Quality,
    job: Option<Job>,
    message: String,
}

impl Default for ExportUi {
    fn default() -> Self {
        ExportUi { open: false, mode: AudioMode::Mix, quality: Quality::High, job: None, message: String::new() }
    }
}

/// 프로젝트 파일 옆에 겹치지 않는 출력 경로를 정한다: `이름.export.mp4`, `이름.export (2).mp4`…
fn output_path(project_path: &Path, mode: AudioMode) -> PathBuf {
    let stem = project_path.file_stem().unwrap_or_default().to_string_lossy();
    let stem = stem.strip_suffix(".trackcut").unwrap_or(&stem).to_string();
    if mode == AudioMode::WavPerTrack {
        return project_path.with_file_name(format!("{stem}.wav"));
    }
    (1..)
        .map(|n| match n {
            1 => project_path.with_file_name(format!("{stem}.export.mp4")),
            n => project_path.with_file_name(format!("{stem}.export ({n}).mp4")),
        })
        .find(|p| !p.exists())
        .expect("빈 파일 이름")
}

impl ExportUi {
    fn start(&mut self, ctx: &egui::Context, project: &Project, project_path: &Path) {
        let options = ExportOptions { audio: self.mode, quality: self.quality, ..ExportOptions::new(output_path(project_path, self.mode)) };
        let (progress, cancel) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        let (tx, done) = channel();
        let (project, ctx) = (project.clone(), ctx.clone());
        let (thread_progress, thread_cancel) = (progress.clone(), cancel.clone());
        std::thread::spawn(move || {
            let result = export(&project, &options, &thread_cancel, |p| {
                thread_progress.store(p.to_bits(), Ordering::Relaxed);
                ctx.request_repaint();
            });
            let _ = tx.send(result.map_err(|e| e.to_string()));
            ctx.request_repaint();
        });
        self.job = Some(Job { progress, cancel, done, started: Instant::now() });
        self.message.clear();
    }

    pub fn show(&mut self, ctx: &egui::Context, project: Option<&Project>, project_path: &Path) {
        if let Some(result) = self.job.as_ref().and_then(|job| job.done.try_recv().ok()) {
            self.job = None;
            self.message = match result {
                Ok(files) => {
                    let names: Vec<_> = files.iter().filter_map(|f| f.file_name()).map(|n| n.to_string_lossy()).collect();
                    format!("완료: {}", names.join(", "))
                }
                Err(e) => e,
            };
        }
        let Some(project) = project else { return };
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("내보내기").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let busy = self.job.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                ui.label("소리");
                ui.radio_value(&mut self.mode, AudioMode::Mix, "들리는 트랙을 섞어서 하나로 (공유용)");
                ui.radio_value(&mut self.mode, AudioMode::Tracks, "트랙을 섞지 않고 각각 담기");
                ui.radio_value(&mut self.mode, AudioMode::WavPerTrack, "트랙마다 WAV 파일 (영상 없음)");
                ui.add_space(6.0);
                ui.add_enabled_ui(self.mode != AudioMode::WavPerTrack, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("화질");
                        ui.selectable_value(&mut self.quality, Quality::High, "높음");
                        ui.selectable_value(&mut self.quality, Quality::Standard, "보통");
                        ui.selectable_value(&mut self.quality, Quality::Small, "작은 용량");
                    });
                });
            });
            ui.add_space(6.0);
            ui.weak(format!("저장 위치: {}", output_path(project_path, self.mode).display()));
            ui.add_space(6.0);
            match &self.job {
                Some(job) => {
                    let fraction = f32::from_bits(job.progress.load(Ordering::Relaxed));
                    ui.add(egui::ProgressBar::new(fraction).show_percentage());
                    // 남은 시간: 지금까지 걸린 시간으로 추정
                    let elapsed = job.started.elapsed().as_secs_f32();
                    if fraction > 0.02 {
                        ui.weak(format!("남은 시간 약 {:.0}초", elapsed / fraction * (1.0 - fraction)));
                    }
                    if ui.button("취소").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                }
                None => {
                    if ui.button("내보내기 시작").clicked() {
                        self.start(ctx, project, project_path);
                    }
                }
            }
            if !self.message.is_empty() {
                ui.label(&self.message);
            }
        });
        self.open = open;
    }
}
