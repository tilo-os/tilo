//! Typed operations on the same scoped picker used by pointer and AT input.
use super::{FilePicker, FilePickerMode};

#[derive(Clone, Debug)]
pub enum PickerOperation {
    Inspect { page: usize },
    Select { entry: String },
    Navigate { entry: String },
    Up,
    Confirm,
    Cancel,
    SetFilename { filename: String },
    Replace,
    KeepExisting,
    NextFilter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerRefusal {
    Stale,
    InvalidPage,
    UnavailableEntry,
    OutsideScope,
    Unavailable,
}

fn entry_id(path: &std::path::Path) -> String {
    blake3::hash(path.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string()
}

impl FilePicker {
    /// Binds an operation to the actual listing, selection and Save prompt.
    fn semantic_state(&self) -> String {
        let mut hash = blake3::Hasher::new();
        for part in [
            self.current_dir.as_os_str().as_encoded_bytes(),
            self.active_filter_label().as_bytes(),
            self.filename().as_bytes(),
        ] {
            hash.update(&part.len().to_le_bytes());
            hash.update(part);
        }
        for (index, entry) in self.entries.iter().enumerate() {
            let path = entry.path.as_os_str().as_encoded_bytes();
            hash.update(&path.len().to_le_bytes());
            hash.update(path);
            hash.update(&[
                u8::from(entry.is_dir),
                u8::from(self.selection.is_selected(index)),
            ]);
        }
        if let Some(path) = &self.replace_prompt {
            hash.update(path.as_os_str().as_encoded_bytes());
        }
        hash.finalize().to_hex().to_string()
    }

    /// A bounded view of the actual displayed entries; never enumerates another root.
    pub fn semantic_inspection(&self, page: usize) -> Result<serde_json::Value, PickerRefusal> {
        let pages = self.entries.len().div_ceil(32).max(1);
        if page >= pages {
            return Err(PickerRefusal::InvalidPage);
        }
        let entries: Vec<_> = self.entries.iter().enumerate().skip(page * 32).take(32).map(|(index, entry)| {
            serde_json::json!({"id": entry_id(&entry.path), "name": entry.name.chars().take(256).collect::<String>(),
                "directory": entry.is_dir, "selected": self.selection.is_selected(index)})
        }).collect();
        Ok(
            serde_json::json!({"state": self.semantic_state(), "page": page, "pages": pages,
            "total": self.entries.len(), "entries": entries, "can_confirm": self.can_commit_open(),
            "can_up": self.can_go_up(), "filter": self.active_filter_label().chars().take(256).collect::<String>(), "filename": self.filename().chars().take(256).collect::<String>(),
            "mode": match self.mode { FilePickerMode::Open => "open", FilePickerMode::Save => "save", FilePickerMode::Directory => "directory" },
            "replace_pending": self.replace_prompt.is_some(), "completed": self.pending_outcome.is_some()}),
        )
    }

    /// Apply through existing selection, navigation and confirmation validation.
    pub fn perform_semantic(
        &mut self,
        state: Option<&str>,
        operation: PickerOperation,
    ) -> Result<serde_json::Value, PickerRefusal> {
        if let PickerOperation::Inspect { page } = operation {
            return self.semantic_inspection(page);
        }
        if !matches!(operation, PickerOperation::Cancel)
            && state != Some(self.semantic_state().as_str())
        {
            return Err(PickerRefusal::Stale);
        }
        if self.pending_outcome.is_some() {
            return Err(PickerRefusal::Unavailable);
        }
        if self.replace_prompt.is_some()
            && !matches!(
                operation,
                PickerOperation::Cancel | PickerOperation::Replace | PickerOperation::KeepExisting
            )
        {
            return Err(PickerRefusal::Unavailable);
        }
        let navigate = matches!(operation, PickerOperation::Navigate { .. });
        match operation {
            PickerOperation::Select { entry } | PickerOperation::Navigate { entry } => {
                let row = self
                    .entries
                    .iter()
                    .find(|row| entry_id(&row.path) == entry)
                    .ok_or(PickerRefusal::UnavailableEntry)?
                    .clone();
                let within_scope = if row.is_dir {
                    self.safe_existing_directory(&row.path).is_some()
                } else {
                    self.safe_existing_file(&row.path).is_some()
                };
                if !within_scope {
                    return Err(PickerRefusal::OutsideScope);
                }
                // Select is the ordinary single-click selection. Directory traversal is explicit.
                if navigate {
                    if !row.is_dir {
                        return Err(PickerRefusal::UnavailableEntry);
                    }
                    self.navigate_to(&row.path);
                } else {
                    self.select_by_path(&row.path);
                }
            }
            PickerOperation::Up => {
                if !self.can_go_up() {
                    return Err(PickerRefusal::OutsideScope);
                }
                self.go_up();
            }
            PickerOperation::Confirm => {
                if !self.can_commit_open() {
                    return Err(PickerRefusal::Unavailable);
                }
                self.click_open();
            }
            PickerOperation::Cancel => self.click_cancel(),
            PickerOperation::SetFilename { filename } => {
                if self.mode != FilePickerMode::Save
                    || filename.len() > 255
                    || (!filename.is_empty() && self.safe_save_target(&filename).is_none())
                {
                    return Err(PickerRefusal::Unavailable);
                }
                self.set_filename(&filename);
            }
            PickerOperation::Replace => {
                if self.replace_prompt.is_none() {
                    return Err(PickerRefusal::Unavailable);
                }
                self.confirm_replace();
            }
            PickerOperation::KeepExisting => {
                if self.replace_prompt.is_none() {
                    return Err(PickerRefusal::Unavailable);
                }
                self.keep_existing();
            }
            PickerOperation::NextFilter => self.cycle_filter(),
            PickerOperation::Inspect { .. } => {}
        }
        self.semantic_inspection(0)
    }
}
