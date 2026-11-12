use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// ffmpeg / ffprobe 실행 실패
    Tool { tool: String, message: String },
    /// 미디어 분석 결과를 해석하지 못함
    Probe(String),
    /// 허용되지 않는 편집
    Edit(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "파일 오류: {e}"),
            Error::Json(e) => write!(f, "JSON 오류: {e}"),
            Error::Tool { tool, message } => write!(f, "{tool} 실행 실패: {message}"),
            Error::Probe(m) => write!(f, "미디어 분석 실패: {m}"),
            Error::Edit(m) => write!(f, "편집할 수 없음: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub(crate) fn edit_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Edit(msg.into()))
}
