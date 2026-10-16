//! Reading what a file says, for a caller with no screen.
//!
//! A person reads a file by opening it; an agent asked to summarise,
//! quote or mail what the selected files say reads them here. The answer is
//! the file's text in parts of [`MAX_READ_CHARS`], each saying where the
//! next starts, so a long book reads in a few calls and a short note in
//! one.

use std::path::PathBuf;

use super::FileExplorerWidget;

/// The most characters one read answers with.
pub const MAX_READ_CHARS: usize = 64 * 1024;

impl FileExplorerWidget {
    /// The file `file` names: an absolute path, a name in the folder shown,
    /// or the row id of an entry this window publishes.
    pub(super) fn named_file(&self, file: &str) -> Result<PathBuf, String> {
        let file = file.trim();
        if file.is_empty() {
            return Err("a file is named".into());
        }
        let path = PathBuf::from(file);
        if path.is_absolute() {
            return Ok(path);
        }
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| Self::entry_resource_id(entry) == file || entry.name == file)
        {
            return Ok(entry.path.clone());
        }
        Err(format!(
            "`{file}` is neither an absolute path nor an entry of the folder shown"
        ))
    }

    /// What `file` says, from character `from` on, at most
    /// [`MAX_READ_CHARS`] of it.
    pub(super) fn read_text_answer(
        &self,
        file: &str,
        from: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        let path = self.named_file(file)?;
        let read = crate::documents::read_text(&path)?;
        Ok(page(&path, &read, from.unwrap_or(0)))
    }

    /// The bytes of `file`, kept in this window's content store; their
    /// content hash.
    pub(crate) fn hand_over(&self, file: &str) -> Result<String, String> {
        let path = self.named_file(file)?;
        let bytes = crate::documents::read_file(&path)?;
        let store = ui::tilo_resource::content::current()
            .ok_or("this window has no content store to hand the file over through")?;
        store
            .put(&bytes)
            .map_err(|error| format!("{} could not be handed over: {error}", path.display()))
    }

    /// The selected entries, with their paths, changing nothing.
    pub(super) fn selected_entries_answer(&self) -> serde_json::Value {
        let mut answer = self.selection_answer();
        let paths: Vec<String> = self
            .selected_paths()
            .iter()
            .take(super::MAX_ANSWERED_ENTRIES)
            .map(|path| path.display().to_string())
            .collect();
        if let Some(object) = answer.as_object_mut() {
            object.insert(
                "folder".into(),
                self.current_path.display().to_string().into(),
            );
            object.insert("paths".into(), paths.into());
        }
        answer
    }
}

/// The part of `read` from character `from` on.
fn page(
    path: &std::path::Path,
    read: &crate::documents::FileText,
    from: usize,
) -> serde_json::Value {
    let total = read.text.chars().count();
    let from = from.min(total);
    let to = from.saturating_add(MAX_READ_CHARS).min(total);
    let byte = |n: usize| {
        read.text
            .char_indices()
            .nth(n)
            .map_or(read.text.len(), |(at, _)| at)
    };
    let text = read.text.get(byte(from)..byte(to)).unwrap_or_default();
    serde_json::json!({
        "file": path.display().to_string(),
        "format": read.format,
        "text": text,
        "from": from,
        "next": (to < total).then_some(to),
        "total": total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_text_reads_in_parts_that_join_up() {
        let text: String = "ab\u{e9}".repeat(MAX_READ_CHARS);
        let read = crate::documents::FileText {
            format: "text",
            text: text.clone(),
        };
        let path = std::path::Path::new("/tmp/book.txt");
        let mut joined = String::new();
        let mut from = Some(0);
        while let Some(at) = from {
            let part = page(path, &read, at);
            joined.push_str(part["text"].as_str().unwrap());
            from = part["next"]
                .as_u64()
                .map(|next| usize::try_from(next).unwrap());
        }
        assert_eq!(joined, text);
        let past = page(path, &read, usize::MAX);
        assert_eq!(past["text"], "");
        assert_eq!(past["next"], serde_json::Value::Null);
    }
}
