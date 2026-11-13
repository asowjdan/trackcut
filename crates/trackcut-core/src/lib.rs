//! trackcut 코어: 시간 계산, 미디어 분석, 프로젝트 모델, 편집 동작, Undo.
//! 화면(UI)과 무관하게 동작한다.

pub mod audio;
pub mod decode;
pub mod edit;
pub mod error;
pub mod export;
pub mod fx;
pub mod history;
pub mod labels;
pub mod media;
pub mod probe;
pub mod project;
pub mod time;

pub use error::{Error, Result};
