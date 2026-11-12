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
}

impl Default for History {
    fn default() -> Self {
        History { undo: Vec::new(), redo: Vec::new(), limit: 200 }
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
        let before = project.timeline.clone();
        match edit(project) {
            Ok(value) => {
                if project.timeline != before {
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
        let entry = self.undo.pop()?;
        let current = std::mem::replace(&mut project.timeline, entry.state);
        self.redo.push(Entry { label: entry.label.clone(), state: current });
        Some(entry.label)
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
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
