use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
    pub mime: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy { Name, Size, Modified, Type }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder { Ascending, Descending }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode { List, Grid }

pub fn sort_entries(sort_by: SortBy, sort_order: SortOrder, entries: &mut Vec<Entry>) {
    entries.sort_by(|a, b| {
        let cmp = match sort_by {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Type => a.mime.cmp(&b.mime),
        };
        let final_cmp = if sort_order == SortOrder::Descending { cmp.reverse() } else { cmp };
        // Directories always first, regardless of order direction.
        b.is_dir.cmp(&a.is_dir).then(final_cmp)
    });
}

pub fn read_dir_sync(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let meta = entry.metadata().ok();
        let path = entry.path();
        let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
        out.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.as_ref().and_then(|m| m.modified().ok()).map(DateTime::from),
            mime,
        });
    }
    Ok(out)
}

pub fn format_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home { return "~".to_string(); }
        if let Ok(rel) = path.strip_prefix(home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub fn initial_path() -> PathBuf {
    let workspace = PathBuf::from("/workspace");
    if workspace.exists() { return workspace; }
    if let Some(home) = dirs::home_dir() { return home; }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}
