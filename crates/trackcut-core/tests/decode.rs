//! 디코더가 요청한 프레임을 정확히 돌려주는지 확인한다. ffmpeg가 없으면 건너뛴다.
//! 프레임 번호를 밝기로 새긴 영상을 만들어, 받은 프레임의 밝기로 번호를 되읽는다.

use std::time::Duration;

use trackcut_core::decode;
use trackcut_core::probe::run_tool;

const FRAMES: u64 = 100;

/// n번 프레임의 밝기(Y)는 20 + 2n이다.
fn expected_gray(frame: u64) -> f64 {
    (20.0 + 2.0 * frame as f64 - 16.0) * 255.0 / 219.0
}

#[test]
fn returns_exactly_the_requested_frame() {
    if run_tool("ffmpeg", ["-version"]).is_err() {
        eprintln!("ffmpeg가 없어 건너뜀");
        return;
    }
    let dir = std::env::temp_dir().join(format!("trackcut-decode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("프레임 번호.mp4");
    // B 프레임과 긴 GOP(키프레임 간격 40)를 넣어 실제 녹화본처럼 만든다
    let source = format!("nullsrc=s=64x64:r=60:d={},geq=lum='20+2*N':cb=128:cr=128", FRAMES as f64 / 60.0);
    let mut args: Vec<String> = ["-v", "error", "-y", "-f", "lavfi", "-i"].map(String::from).to_vec();
    args.push(source);
    args.extend("-c:v libx264 -g 40 -bf 2 -pix_fmt yuv420p".split(' ').map(String::from));
    args.push(path.to_string_lossy().into_owned());
    run_tool("ffmpeg", args).expect("샘플 파일 생성");

    let (info, decoder) = decode::open(&path, 64).unwrap();
    assert_eq!(info.video.as_ref().unwrap().grid.count(), FRAMES);
    assert_eq!((decoder.width, decoder.height), (64, 64));

    // 앞으로, 한 프레임씩 뒤로, 멀리 건너뛰기, 처음과 끝
    for frame in [0, 1, 2, 50, 49, 48, 47, 10, 99, 98, 41, 40, 39, 75, 0, 99] {
        let image = decoder.get(frame, Duration::from_secs(20)).unwrap_or_else(|| panic!("프레임 {frame} 시간 초과"));
        assert_eq!(image.rgb.len(), 64 * 64 * 3);
        let center = (32 * 64 + 32) * 3;
        let gray = image.rgb[center] as f64;
        let error = (gray - expected_gray(frame)).abs();
        assert!(error < 1.2, "프레임 {frame}: 밝기 {gray}, 기대 {:.1}", expected_gray(frame));
    }
    // 범위를 넘는 요청은 마지막 프레임으로 맞춘다
    decoder.request(FRAMES + 50);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(decoder.poll().unwrap().frame, FRAMES - 1);
    drop(decoder);
    std::fs::remove_dir_all(&dir).ok();
}
