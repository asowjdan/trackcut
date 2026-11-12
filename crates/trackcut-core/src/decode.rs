//! 미리보기용 영상 디코더. 원하는 프레임 번호를 정확히 그 프레임으로 돌려준다.
//!
//! 별도 스레드가 ffmpeg 프로세스에서 RGB 프레임을 이어서 읽는다. 앞으로 가는 이동과 재생은
//! 같은 스트림을 계속 읽고, 최근 프레임은 캐시에 남겨 뒤로 가는 이동도 빠르게 한다.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::media::MediaInfo;
use crate::probe::{hide_console, tool_path};
use crate::time::{flicks_to_secs, Flicks, FrameGrid};

/// 이만큼 앞의 프레임까지는 스트림을 다시 열지 않고 이어서 읽는다.
const FORWARD_WINDOW: u64 = 90;
/// 스트림을 새로 열 때 목표보다 이만큼 앞에서 시작해 뒤로 가는 이동에 대비한다.
const BACK_FILL: u64 = 30;
const CACHE_FRAMES: usize = 120;

#[derive(Clone)]
pub struct FrameImage {
    /// 원본 파일 안에서의 프레임 번호(표시 순서)
    pub frame: u64,
    pub width: u32,
    pub height: u32,
    /// RGB 8비트, 행 순서
    pub rgb: Arc<Vec<u8>>,
}

struct Config {
    path: PathBuf,
    grid: FrameGrid,
    /// ffmpeg의 `-ss`가 기준으로 삼는 파일 시작 시각
    file_start: Flicks,
    width: u32,
    height: u32,
}

pub struct Decoder {
    requests: Sender<u64>,
    frames: Receiver<FrameImage>,
    pub width: u32,
    pub height: u32,
}

impl Decoder {
    /// `max_height`보다 크지 않게 줄여서 디코딩한다. 프레임이 준비될 때마다 `on_frame`을 부른다.
    pub fn new(info: &MediaInfo, max_height: u32, on_frame: impl Fn() + Send + 'static) -> Option<Self> {
        let video = info.video.as_ref()?;
        if video.width == 0 || video.height == 0 {
            return None;
        }
        let height = video.height.min(max_height).max(2) & !1;
        let width = ((video.width as u64 * height as u64 / video.height as u64) as u32).max(2) & !1;
        let file_start = info.audio.iter().map(|a| a.start).chain([video.grid.start()]).min().unwrap_or(0);
        let config = Config { path: PathBuf::from(&info.path), grid: video.grid.clone(), file_start, width, height };
        let (requests, request_rx) = channel();
        let (frame_tx, frames) = channel();
        std::thread::spawn(move || worker(config, request_rx, frame_tx, on_frame));
        Some(Decoder { requests, frames, width, height })
    }

    /// 프레임을 요청한다. 연달아 요청하면 가장 마지막 것만 처리한다.
    pub fn request(&self, frame: u64) {
        let _ = self.requests.send(frame);
    }

    /// 준비된 프레임 중 가장 최근 것.
    pub fn poll(&self) -> Option<FrameImage> {
        self.frames.try_iter().last()
    }

    /// 프레임을 요청하고 도착할 때까지 기다린다(테스트·내보내기용).
    pub fn get(&self, frame: u64, timeout: Duration) -> Option<FrameImage> {
        self.request(frame);
        let deadline = Instant::now() + timeout;
        loop {
            match self.frames.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(image) if image.frame == frame => return Some(image),
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

struct Stream {
    child: Child,
    stdout: ChildStdout,
    /// 다음에 읽을 프레임 번호
    next: u64,
    frame_bytes: usize,
}

impl Stream {
    fn open(config: &Config, start: u64) -> Option<Self> {
        let mut command = Command::new(tool_path("ffmpeg"));
        command.args(["-v", "error"]);
        if start > 0 {
            // 목표 프레임보다 반 프레임 앞을 가리켜서, 반올림 오차가 있어도 정확히 그 프레임부터 나온다
            let at = config.grid.time_of(start);
            let half = (config.grid.time_of(start + 1) - at) / 2;
            let secs = flicks_to_secs((at - half - config.file_start).max(0));
            command.args(["-ss", &format!("{secs:.6}")]);
        }
        command.arg("-i").arg(&config.path);
        let scale = format!("scale={}:{}:flags=bilinear", config.width, config.height);
        command.args(["-map", "0:v:0", "-an", "-sn", "-vf", &scale]);
        // passthrough: 디코딩된 프레임을 복제하거나 버리지 않고 하나씩 그대로 내보낸다
        command.args(["-pix_fmt", "rgb24", "-fps_mode", "passthrough", "-f", "rawvideo", "-"]);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        hide_console(&mut command);
        let mut child = command.spawn().ok()?;
        let stdout = child.stdout.take()?;
        let frame_bytes = config.width as usize * config.height as usize * 3;
        Some(Stream { child, stdout, next: start, frame_bytes })
    }

    fn read_frame(&mut self) -> Option<Vec<u8>> {
        let mut buffer = vec![0u8; self.frame_bytes];
        self.stdout.read_exact(&mut buffer).ok()?;
        Some(buffer)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Cache {
    frames: HashMap<u64, Arc<Vec<u8>>>,
    order: VecDeque<u64>,
}

impl Cache {
    fn put(&mut self, frame: u64, data: Vec<u8>) {
        if self.frames.insert(frame, Arc::new(data)).is_none() {
            self.order.push_back(frame);
        }
        while self.order.len() > CACHE_FRAMES {
            if let Some(old) = self.order.pop_front() {
                self.frames.remove(&old);
            }
        }
    }
}

fn worker(config: Config, requests: Receiver<u64>, frames: Sender<FrameImage>, on_frame: impl Fn()) {
    let last_frame = config.grid.count().saturating_sub(1);
    let mut cache = Cache::default();
    let mut stream: Option<Stream> = None;
    while let Ok(mut target) = requests.recv() {
        loop {
            // 밀린 요청은 건너뛰고 가장 최근 요청만 처리한다
            if let Some(newer) = requests.try_iter().last() {
                target = newer;
            }
            let target = target.min(last_frame);
            if let Some(rgb) = cache.frames.get(&target) {
                let image = FrameImage { frame: target, width: config.width, height: config.height, rgb: rgb.clone() };
                if frames.send(image).is_err() {
                    return;
                }
                on_frame();
                break;
            }
            let reusable = stream.as_ref().is_some_and(|s| s.next <= target && target - s.next <= FORWARD_WINDOW);
            if !reusable {
                stream = Stream::open(&config, target.saturating_sub(BACK_FILL));
            }
            let Some(active) = stream.as_mut() else { break };
            match active.read_frame() {
                Some(data) => {
                    cache.put(active.next, data);
                    active.next += 1;
                }
                None => {
                    // 파일 끝이거나 디코딩 실패
                    stream = None;
                    break;
                }
            }
        }
    }
}

/// 테스트에서 경로만으로 디코더를 만들 때 쓴다.
pub fn open(path: &Path, max_height: u32) -> crate::Result<(MediaInfo, Decoder)> {
    let info = crate::probe::probe(path)?;
    let decoder = Decoder::new(&info, max_height, || {})
        .ok_or_else(|| crate::Error::Probe("영상 스트림이 없다".into()))?;
    Ok((info, decoder))
}
