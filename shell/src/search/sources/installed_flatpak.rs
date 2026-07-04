//! `InstalledFlatpakSource` — shells out to `flatpak list --user
//! --app --columns=application,name,description` and turns each row
//! into a `SearchItem`.
//!
//! The CLI is asked to produce tab-separated output (`--columns`
//! guarantees the order), so parsing is `split_terminator('\t')`
//! without a shell.
//!
//! Refresh is a synchronous `Command::output`. If `flatpak` isn't
//! installed we surface `SourceError::Unavailable` — the aggregator
//! logs it and moves on.

use std::process::Command;
use std::time::Instant;

use super::{SearchAction, SearchItem, SearchSource, SourceError, SourceKind};

pub struct InstalledFlatpakSource {
    runner: RunnerBox,
    items: Vec<SearchItem>,
    last_refresh: Option<Instant>,
}

/// Shell-out result: `Ok(stdout)` on a clean run, `Err` on
/// missing-binary / non-zero exit.
type RunnerBox = Box<dyn Fn() -> Result<String, SourceError> + Send + Sync>;

impl InstalledFlatpakSource {
    pub fn new() -> Self {
        Self {
            runner: Box::new(default_runner),
            items: Vec::new(),
            last_refresh: None,
        }
    }

    pub fn with_runner<F>(mut self, runner: F) -> Self
    where
        F: Fn() -> Result<String, SourceError> + Send + Sync + 'static,
    {
        self.runner = Box::new(runner);
        self
    }
}

impl Default for InstalledFlatpakSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchSource for InstalledFlatpakSource {
    fn kind(&self) -> SourceKind {
        SourceKind::InstalledFlatpak
    }

    fn items(&self) -> &[SearchItem] {
        &self.items
    }

    fn refresh(&mut self) -> Result<(), SourceError> {
        let stdout = match (self.runner)() {
            Ok(s) => s,
            Err(SourceError::Unavailable(_)) => {
                // No flatpak installed → treat as empty, not fatal.
                self.items.clear();
                self.last_refresh = Some(Instant::now());
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut items = Vec::new();
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut cols = line.split('\t');
            let Some(app_id) = cols.next() else { continue };
            let name = cols.next().unwrap_or(app_id);
            let description = cols.next().unwrap_or("");
            items.push(SearchItem {
                id: app_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                icon: None,
                source: SourceKind::InstalledFlatpak,
                categories: vec!["Flatpak".into()],
                action: SearchAction::LaunchInstalled {
                    pkg_id: app_id.to_string(),
                    kind: SourceKind::InstalledFlatpak,
                },
            });
        }
        self.items = items;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn last_refreshed_at(&self) -> Option<Instant> {
        self.last_refresh
    }
}

fn default_runner() -> Result<String, SourceError> {
    // Best-effort. On systems without flatpak the binary lookup
    // fails; we surface that as `Unavailable` so the refresh call
    // still succeeds with zero items.
    let out = Command::new("flatpak")
        .args([
            "list",
            "--user",
            "--app",
            "--columns=application,name,description",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(SourceError::Fetch(format!(
            "flatpak list exited {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr)
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(SourceError::Unavailable("flatpak binary not found".into()))
        }
        Err(e) => Err(SourceError::Io(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsv() {
        let mut src = InstalledFlatpakSource::new().with_runner(|| {
            Ok("org.mozilla.firefox\tFirefox\tFast and safe browser\nio.neovim.nvim\tNeovim\tVim, but better\n".into())
        });
        src.refresh().unwrap();
        assert_eq!(src.items().len(), 2);
        assert_eq!(src.items()[0].name, "Firefox");
        assert_eq!(src.items()[1].id, "io.neovim.nvim");
    }

    #[test]
    fn no_flatpak_binary_is_empty_not_error() {
        let mut src = InstalledFlatpakSource::new()
            .with_runner(|| Err(SourceError::Unavailable("no flatpak".into())));
        src.refresh().unwrap();
        assert!(src.items().is_empty());
    }
}
