//! 소리 출력 엔진. 타임라인의 오디오 클립을 실시간으로 섞어 기본 출력 장치로 내보낸다.
//!
//! 믹서 스레드가 캐시된 PCM 파일에서 읽어 섞은 소리를 버퍼에 채우고, 출력 장치의 콜백이
//! 버퍼에서 꺼내 간다. 재생 위치는 실제로 장치에 넘긴 샘플 수로 계산해서 화면과 소리가 맞는다.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const RATE: u32 = 48_000;
/// 한 번에 섞는 프레임 수
const BLOCK: usize = 1024;
/// 클립 경계에서 "딱" 소리가 나지 않게 하는 페이드 길이(5ms)
const FADE_FRAMES: f32 = 240.0;

/// 타임라인 위치는 모두 48kHz 샘플 단위다.
pub struct PlanClip {
    pub pcm: PathBuf,
    pub start: i64,
    /// PCM 파일 안에서 이 클립이 시작하는 프레임
    pub offset: i64,
    pub len: i64,
}

pub struct PlanTrack {
    /// 0이면 들리지 않는 트랙
    pub gain: f32,
    pub clips: Vec<PlanClip>,
}

struct Shared {
    plan: Arc<Vec<PlanTrack>>,
    playing: bool,
    /// 다음에 섞을 타임라인 위치
    render_pos: i64,
    /// 장치에 넘긴 위치
    played: f64,
    /// 출력 장치 샘플레이트의 스테레오 샘플
    ring: VecDeque<f32>,
    /// 탐색하거나 구성이 바뀌면 올려서, 그 전에 섞던 블록을 버린다
    generation: u64,
    closed: bool,
}

pub struct Engine {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    _stream: cpal::Stream,
}

impl Engine {
    pub fn new() -> Result<Self, String> {
        let device = cpal::default_host().default_output_device().ok_or("소리 출력 장치가 없다")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("지원하지 않는 출력 형식: {:?}", supported.sample_format()));
        }
        let config: cpal::StreamConfig = supported.into();
        let (rate, channels) = (config.sample_rate.0, config.channels as usize);
        let shared = Arc::new((
            Mutex::new(Shared {
                plan: Arc::new(Vec::new()),
                playing: false,
                render_pos: 0,
                played: 0.0,
                ring: VecDeque::new(),
                generation: 0,
                closed: false,
            }),
            Condvar::new(),
        ));
        let step = RATE as f64 / rate as f64;
        let callback = shared.clone();
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut state = callback.0.lock().unwrap();
                    for frame in data.chunks_mut(channels) {
                        let (left, right) = if state.playing && state.ring.len() >= 2 {
                            state.played += step;
                            (state.ring.pop_front().unwrap_or(0.0), state.ring.pop_front().unwrap_or(0.0))
                        } else {
                            (0.0, 0.0)
                        };
                        frame.fill(0.0);
                        if frame.len() == 1 {
                            frame[0] = (left + right) * 0.5;
                        } else {
                            (frame[0], frame[1]) = (left, right);
                        }
                    }
                    drop(state);
                    callback.1.notify_one();
                },
                |_error| {},
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        let worker = shared.clone();
        std::thread::spawn(move || mixer(worker, rate));
        Ok(Engine { shared, _stream: stream })
    }

    fn update(&self, change: impl FnOnce(&mut Shared)) {
        let mut state = self.shared.0.lock().unwrap();
        change(&mut state);
        state.ring.clear();
        state.generation += 1;
        drop(state);
        self.shared.1.notify_all();
    }

    /// 섞을 구성을 바꾼다. 재생 중이면 지금 위치에서 이어진다.
    pub fn set_plan(&self, plan: Vec<PlanTrack>) {
        self.update(|s| {
            s.plan = Arc::new(plan);
            s.render_pos = s.played.round() as i64;
        });
    }

    pub fn play(&self, from: i64) {
        self.update(|s| {
            s.playing = true;
            s.render_pos = from;
            s.played = from as f64;
        });
    }

    pub fn pause(&self) {
        self.update(|s| s.playing = false);
    }

    /// 지금 장치로 나가고 있는 타임라인 위치(샘플).
    pub fn position(&self) -> i64 {
        self.shared.0.lock().unwrap().played as i64
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap().closed = true;
        self.shared.1.notify_all();
    }
}

fn mixer(shared: Arc<(Mutex<Shared>, Condvar)>, rate: u32) {
    // 버퍼는 0.15초 분량까지만 채운다(음소거·볼륨 변경이 빨리 반영되게)
    let target = rate as usize * 2 * 15 / 100;
    let mut files: HashMap<PathBuf, File> = HashMap::new();
    let mut carry = 0.0;
    loop {
        let (plan, position, generation) = {
            let mut state = shared.0.lock().unwrap();
            loop {
                if state.closed {
                    return;
                }
                if state.playing && state.ring.len() < target {
                    break;
                }
                state = shared.1.wait_timeout(state, Duration::from_millis(20)).unwrap().0;
            }
            (state.plan.clone(), state.render_pos, state.generation)
        };
        let block = render(&plan, position, &mut files);
        let output = if rate == RATE { block } else { resample(&block, rate, &mut carry) };
        let mut state = shared.0.lock().unwrap();
        if state.generation == generation {
            state.ring.extend(output);
            state.render_pos += BLOCK as i64;
        }
    }
}

/// 타임라인 `position`부터 한 블록을 섞는다(48kHz 스테레오).
pub fn render(plan: &[PlanTrack], position: i64, files: &mut HashMap<PathBuf, File>) -> Vec<f32> {
    let mut out = vec![0.0f32; BLOCK * 2];
    let mut raw = vec![0u8; BLOCK * 4];
    for track in plan.iter().filter(|t| t.gain > 0.0) {
        for clip in &track.clips {
            let from = position.max(clip.start);
            let to = (position + BLOCK as i64).min(clip.start + clip.len);
            if from >= to {
                continue;
            }
            if !files.contains_key(&clip.pcm) {
                match File::open(&clip.pcm) {
                    Ok(file) => files.insert(clip.pcm.clone(), file),
                    Err(_) => continue,
                };
            }
            let Some(file) = files.get_mut(&clip.pcm) else { continue };
            let source = (clip.offset + from - clip.start).max(0) as u64;
            if file.seek(SeekFrom::Start(source * 4)).is_err() {
                continue;
            }
            let wanted = (to - from) as usize * 4;
            let mut filled = 0;
            while filled < wanted {
                match file.read(&mut raw[filled..wanted]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => filled += n,
                }
            }
            for (i, sample) in raw[..filled - filled % 4].chunks_exact(4).enumerate() {
                let t = from + i as i64;
                let edge = (t - clip.start).min(clip.start + clip.len - 1 - t) as f32;
                let gain = track.gain * (edge / FADE_FRAMES).min(1.0) / 32768.0;
                let at = ((from - position) as usize + i) * 2;
                out[at] += i16::from_le_bytes([sample[0], sample[1]]) as f32 * gain;
                out[at + 1] += i16::from_le_bytes([sample[2], sample[3]]) as f32 * gain;
            }
        }
    }
    out.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
    out
}

/// 출력 장치가 48kHz가 아닐 때 선형 보간으로 맞춘다.
fn resample(block: &[f32], rate: u32, carry: &mut f64) -> Vec<f32> {
    let exact = BLOCK as f64 * rate as f64 / RATE as f64 + *carry;
    let frames = exact.floor() as usize;
    *carry = exact - frames as f64;
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let at = i as f64 * BLOCK as f64 / frames as f64;
        let index = (at as usize).min(BLOCK - 1);
        let next = (index + 1).min(BLOCK - 1);
        let mix = (at - index as f64) as f32;
        for channel in 0..2 {
            out.push(block[index * 2 + channel] * (1.0 - mix) + block[next * 2 + channel] * mix);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 값이 프레임 번호인 PCM 파일(왼쪽 = n, 오른쪽 = -n)
    fn pcm_file(name: &str, frames: i16) -> PathBuf {
        let path = std::env::temp_dir().join(format!("trackcut-engine-{}-{name}.pcm", std::process::id()));
        let bytes: Vec<u8> = (0..frames).flat_map(|n| [n.to_le_bytes(), (-n).to_le_bytes()].concat()).collect();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn renders_clips_at_their_timeline_position() {
        let pcm = pcm_file("a", 4000);
        // 타임라인 500에서 시작, 파일의 1000번 프레임부터 2000프레임
        let plan = vec![PlanTrack { gain: 1.0, clips: vec![PlanClip { pcm: pcm.clone(), start: 500, offset: 1000, len: 2000 }] }];
        let mut files = HashMap::new();
        let block = render(&plan, 0, &mut files);
        assert_eq!(block[499 * 2], 0.0, "클립 앞은 무음");
        // 페이드가 끝난 뒤(240프레임 이후)에는 원본 값 그대로
        let at = 800;
        assert!((block[at * 2] * 32768.0 - 1300.0).abs() < 0.01);
        assert!((block[at * 2 + 1] * 32768.0 + 1300.0).abs() < 0.01);
        // 클립 시작은 페이드 인
        assert!(block[501 * 2].abs() < 1001.0 / 32768.0 * 0.01);
        // 음소거된 트랙(gain 0)은 섞이지 않는다
        let muted = vec![PlanTrack { gain: 0.0, clips: vec![PlanClip { pcm: pcm.clone(), start: 0, offset: 0, len: 4000 }] }];
        assert!(render(&muted, 0, &mut files).iter().all(|&s| s == 0.0));
        std::fs::remove_file(pcm).ok();
    }

    #[test]
    fn resample_keeps_the_average_rate() {
        let block = vec![0.25f32; BLOCK * 2];
        let mut carry = 0.0;
        let total: usize = (0..100).map(|_| resample(&block, 44_100, &mut carry).len() / 2).sum();
        assert!((total as i64 - (BLOCK * 100 * 44_100 / 48_000) as i64).abs() <= 1);
        assert!(resample(&block, 44_100, &mut carry).iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }
}
