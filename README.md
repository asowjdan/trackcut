# trackcut

다중 오디오 트랙 녹화본을 **프레임 단위**로 자르고, 트랙마다 소리를 따로 다듬기 위한 개인용 편집기다. Rust로 만든다.

## 만든 이유

OBS로 게임을 녹화하면 게임, 마이크, 통화 소리가 각각 다른 오디오 트랙에 담긴다. 이런 파일을 편하게 편집할 프로그램이 필요했는데, 기존 편집기는 트랙을 자동으로 나눠 주지 않거나 트랙을 그대로 내보내지 못했다. 그래서 **오픈소스(FFmpeg, Rust 생태계)와 AI(Claude)를 활용해** 직접 만들고 있다. 설계와 코드 작성 모두 AI와 함께 진행한다.

## 지금 되는 것

화면까지 전부 Rust로 만든다. 편집 화면(`trackcut-app`)과 명령줄 도구(`trackcut`)가 있다.

- **편집 화면**: 위쪽 미리보기, 아래쪽 타임라인, 오른쪽 믹서. 프레임 단위 이동, 휠 확대·축소, 분할, 구간 삭제, 되돌리기를 지원한다.
- **소리 재생과 파형**: 트랙별 볼륨·음소거·솔로가 재생에 바로 반영되고, 타임라인에 트랙별 파형이 보인다. 화면은 소리가 실제로 나간 위치를 따라간다.
- **내보내기**: 섞어서 하나로(MP4 + AAC 320k), 트랙을 각각 담은 다중 트랙 MP4, 트랙별 WAV. NVENC를 쓰고 안 되면 x264로 바꾼다. 진행률·남은 시간·취소를 지원하고, 기존 파일은 덮어쓰지 않는다. 지금은 전체를 다시 인코딩한다.
- **프레임 정확한 미리보기**: 요청한 프레임 번호와 화면에 나오는 프레임이 정확히 일치한다(테스트로 확인).

- **미디어 분석**: `ffprobe`로 트랙 수, 실제 프레임 시각, 키프레임 위치, 트랙별 시작 시각을 읽는다. 프레임 간격이 고르지 않은 파일(VFR)도 실제 프레임에 맞춘다.
- **트랙 이름표**: 오디오 트랙에 이름을 넣은 새 파일과 `*.tracks.json` 설명 파일을 만든다. 다시 인코딩하지 않는다.
- **하나의 타임라인**: 영상과 모든 오디오가 같은 시간축에 놓이고, 처음에는 서로 연결돼 함께 잘린다.
- **편집**: 분할, 구간 삭제(뒤쪽 당겨 붙이기 선택), 트림, 이동, 연결/해제, 트랙별 볼륨·음소거·솔로.
- **되돌리기 / 다시 하기**
- **프로젝트 파일(JSON)**: 원본 미디어는 절대 수정하거나 덮어쓰지 않는다.

시간은 flick(1/705,600,000초) 정수로 계산한다. 60fps 프레임과 48kHz 샘플이 모두 정수로 떨어져서, 오디오 편집점이 프레임당 정확히 800샘플 경계에 놓인다.

## 사용법

[Rust](https://rustup.rs)와 [FFmpeg](https://ffmpeg.org)(`ffmpeg`, `ffprobe`가 PATH에 있어야 함)가 필요하다.

```bash
cargo build --release
```

```bash
trackcut probe 녹화.mp4
trackcut tag 녹화.mp4 --names 게임,마이크,통화 --mute 3
trackcut new 녹화.tagged.mp4
trackcut split 녹화.tagged.trackcut.json 00:12:03:30
trackcut delete 녹화.tagged.trackcut.json 90f 150f
trackcut delete 녹화.tagged.trackcut.json 10.5 12 --tracks 3 --keep-gap
trackcut show 녹화.tagged.trackcut.json
trackcut export 녹화.tagged.trackcut.json --audio mix --quality high
```

시각은 `90f`(프레임), `00:12:03:30`(시:분:초:프레임), `12.5`(초)로 쓰고, 자동으로 프레임 경계에 붙는다.

편집 화면은 이렇게 연다. 영상 파일을 창에 끌어다 놓아도 된다.

```bash
trackcut-app 녹화.tagged.mp4
```

| 키 | 동작 |
|---|---|
| Space | 재생 / 정지 |
| ← / → | 1프레임 이동 |
| Shift + ← / → | 1초 이동 |
| I / O | 시작점 / 끝점 |
| S | 현재 위치에서 분할 |
| Delete | 시작점~끝점 구간 삭제 |
| Ctrl+Z / Ctrl+Y | 되돌리기 / 다시 하기 |
| Ctrl+S | 프로젝트 저장 |
| Ctrl+휠 / 휠 | 타임라인 확대·축소 / 좌우 이동 |

미디어 파일과 프로젝트 폴더 경로에 한글이 있어도 된다. 다만 Windows GNU 툴체인의 도구(ld, dlltool)는 한글 경로의 파일을 열지 못하므로, 소스가 한글 경로에 있으면 **빌드 출력만** 영문 경로로 돌린다. 상위 폴더의 `.cargo/config.toml`에 `[build] target-dir`을 적거나 `CARGO_TARGET_DIR` 환경 변수를 쓴다.

## 구조

| 경로 | 내용 |
|---|---|
| `crates/trackcut-core` | 시간 계산, 미디어 분석, 프로젝트 모델, 편집 동작, Undo |
| `crates/trackcut-app` | 편집 화면 `trackcut-app` (egui) |
| `crates/trackcut-cli` | 명령줄 도구 `trackcut` |
| `scripts/check-private.sh` | 개인정보·컴퓨터 정보 검사 |

## 앞으로 할 것

1. 클립 선택과 드래그(트림·이동), 연결 해제, 썸네일, 자동 저장
2. 트랙별 노이즈 제거와 음성 보정(FFmpeg 필터)
3. 다른 파일 추가와 화면 전환 효과
4. 스마트 렌더: 손대지 않은 구간은 원본을 그대로 복사하고 바뀐 구간만 다시 인코딩
5. AI 조작(MCP): AI가 편집기를 조작하고 그 과정을 화면에 표시

## 개인정보 보호

공개 저장소라서 개인정보나 컴퓨터 정보가 들어간 변경은 자동으로 거절한다.

- 커밋할 때(`.githooks/pre-commit`)와 푸시할 때(CI) `scripts/check-private.sh`가 검사한다.
- 거절 대상: 사용자 폴더 경로, 이메일 주소, IP 주소, 토큰·비밀 키, 이 컴퓨터의 사용자·컴퓨터 이름, 녹화본과 프로젝트 파일, GitHub noreply가 아닌 커밋 이메일.

저장소를 받은 뒤 한 번 실행해 훅을 켠다.

```bash
git config core.hooksPath .githooks
```

## 사용한 오픈소스

- [FFmpeg](https://ffmpeg.org): 분석, 디코딩, 필터, 인코딩
- [Rust](https://www.rust-lang.org), [serde](https://serde.rs), [egui / eframe](https://github.com/emilk/egui), [cpal](https://github.com/RustAudio/cpal)

## 라이선스

MIT
