//! 되돌리기 / 다시 하기. 편집 동작 하나(또는 AI 동작 하나)가 한 단계다.

use crate::error::Result;
use crate::project::{Project, Timeline};

struct Entry {
    label: String,
    state: Timeline,
}

pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    limit: usize,
    /// 바로 앞의 편집이 `apply_keyed`였다면 그 키
    last_key: Option<String>,
}

impl Default for History {
    fn default() -> Self {
        History { undo: Vec::new(), redo: Vec::new(), limit: 200, last_key: None }
    }
}

impl History {
    /// 편집을 실행하고 Undo 단계로 기록한다. 실패하면 프로젝트를 원래대로 돌려놓는다.
    pub fn apply<T>(
        &mut self,
        project: &mut Project,
        label: &str,
        edit: impl FnOnce(&mut Project) -> Result<T>,
    ) -> Result<T> {
        self.apply_inner(project, label, None, edit)
    }

    /// `apply`와 같지만, 같은 `key`로 연달아 실행한 편집은 Undo 한 단계로 묶는다.
    /// 숫자 칸을 끌어서 값을 바꿀 때처럼 한 가지 값을 여러 번 고치는 경우에 쓴다.
    pub fn apply_keyed<T>(
        &mut self,
        project: &mut Project,
        label: &str,
        key: &str,
        edit: impl FnOnce(&mut Project) -> Result<T>,
    ) -> Result<T> {
        self.apply_inner(project, label, Some(key), edit)
    }

    fn apply_inner<T>(
        &mut self,
        project: &mut Project,
        label: &str,
        key: Option<&str>,
        edit: impl FnOnce(&mut Project) -> Result<T>,
    ) -> Result<T> {
        let before = project.timeline.clone();
        let merge = key.is_some() && self.last_key.as_deref() == key && !self.undo.is_empty();
        if let (true, Some(base)) = (merge, self.undo.last()) {
            // 묶이는 편집은 처음 상태에서 다시 계산한다(중간 값들이 결과에 남지 않게)
            project.timeline = base.state.clone();
        }
        match edit(project) {
            Ok(value) => {
                if merge {
                    self.redo.clear();
                } else if project.timeline != before {
                    self.last_key = key.map(str::to_string);
                    self.undo.push(Entry { label: label.to_string(), state: before });
                    if self.undo.len() > self.limit {
                        self.undo.remove(0);
                    }
                    self.redo.clear();
                }
                Ok(value)
            }
            Err(e) => {
                project.timeline = before;
                Err(e)
            }
        }
    }

    /// 한 단계 되돌리고, 되돌린 동작의 이름을 돌려준다.
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        self.last_key = None;
        let entry = self.undo.pop()?;
        let current = std::mem::replace(&mut project.timeline, entry.state);
        self.redo.push(Entry { label: entry.label.clone(), state: current });
        Some(entry.label)
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        self.last_key = None;
        let entry = self.redo.pop()?;
        let current = std::mem::replace(&mut project.timeline, entry.state);
        self.undo.push(Entry { label: entry.label.clone(), state: current });
        Some(entry.label)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
