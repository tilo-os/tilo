//! freedesktop.org icon-theme lookup: turning an `Icon=` value into a file.
//!
//! An application that Tilo did not package names its icon rather than
//! shipping pixels — `Icon=firefox`, `Icon=org.gnome.Nautilus` — and the
//! Icon Theme Specification says where that name lives:
//!
//! - a theme is a directory under one of the *base directories*
//!   (`~/.icons`, `$XDG_DATA_HOME/icons`, each `$XDG_DATA_DIRS/icons`, and
//!   Flatpak's `exports/share/icons`), described by its `index.theme`;
//! - the index lists sub-directories with a nominal `Size`, a `Scale` and a
//!   `Type` (`Fixed`, `Scalable` or `Threshold`) saying which requested sizes
//!   each one serves;
//! - a theme `Inherits` others, and every chain ends in `hicolor`, where
//!   applications install their own icons;
//! - an icon found nowhere in a theme is looked for directly in the base
//!   directories and in `pixmaps`.
//!
//! Only PNG and SVG files are returned — the formats Tilo can draw. A name
//! that is an absolute path is taken as that file.
//!
//! Flatpak writes its exports without an `index.theme` for `hicolor`, and a
//! minimal host may not install the `hicolor-icon-theme` package at all, so
//! when no base directory carries hicolor's index its layout is read off the
//! directories that exist (`48x48/apps`, `24x24@2/apps`, `scalable/apps`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// The theme every inheritance chain ends in.
pub const FALLBACK_THEME: &str = "hicolor";

/// The theme consulted before `hicolor`. GTK's default, so a host app's
/// symbolic and generic icons resolve the way that app expects.
pub const DEFAULT_THEME: &str = "Adwaita";

/// Memoised answers kept per lookup before the table is dropped and rebuilt.
const MEMO_LIMIT: usize = 4096;

/// An inheritance chain deeper than this is a loop or a mistake.
const MAX_INHERIT_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconFormat {
    Png,
    Svg,
}

impl IconFormat {
    const ALL: [Self; 2] = [Self::Png, Self::Svg];

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Svg => "svg",
        }
    }

    fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        if ext.eq_ignore_ascii_case("png") {
            Some(Self::Png)
        } else if ext.eq_ignore_ascii_case("svg") {
            Some(Self::Svg)
        } else {
            None
        }
    }
}

/// A resolved icon: the file and how to decode it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IconFile {
    pub path: PathBuf,
    pub format: IconFormat,
}

/// Where themes and loose icons are looked for, in precedence order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IconSearchPath {
    /// Directories that contain theme directories (and loose icons).
    pub bases: Vec<PathBuf>,
    /// Directories of loose icons consulted last (`/usr/share/pixmaps`).
    pub pixmaps: Vec<PathBuf>,
}

impl IconSearchPath {
    /// The current session's search path, Flatpak exports included.
    pub fn system() -> Self {
        let mut path = Self::default();
        if let Some(home) = xdg_paths::home() {
            path.push_base(home.join(".icons"));
        }
        if let Some(data_home) = xdg_paths::xdg::data_home() {
            path.push_base(data_home.join("icons"));
        }
        let data_dirs = xdg_paths::xdg::data_dirs();
        for dir in &data_dirs {
            path.push_base(dir.join("icons"));
        }
        // Appended like the `applications` directories in `scan`: a session
        // that already lists the exports keeps their place, and a hand-installed
        // icon still overrides an exported one.
        if let Some(data_home) = xdg_paths::xdg::data_home() {
            path.push_base(data_home.join("flatpak/exports/share/icons"));
        }
        path.push_base(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
        for dir in &data_dirs {
            let pixmaps = dir.join("pixmaps");
            if !path.pixmaps.contains(&pixmaps) {
                path.pixmaps.push(pixmaps);
            }
        }
        path
    }

    /// This path with `dir` searched before everything else — an
    /// application's own icon directory.
    #[must_use]
    pub fn with_first_base(mut self, dir: PathBuf) -> Self {
        self.bases.retain(|base| base != &dir);
        self.bases.insert(0, dir);
        self
    }

    fn push_base(&mut self, dir: PathBuf) {
        if !self.bases.contains(&dir) {
            self.bases.push(dir);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirKind {
    Fixed,
    Scalable,
    Threshold,
}

/// One `index.theme` directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubDir {
    path: String,
    size: u32,
    scale: u32,
    min_size: u32,
    max_size: u32,
    threshold: u32,
    kind: DirKind,
}

impl SubDir {
    /// The spec's `DirectoryMatchesSize`.
    fn matches(&self, size: u32, scale: u32) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.kind {
            DirKind::Fixed => self.size == size,
            DirKind::Scalable => (self.min_size..=self.max_size).contains(&size),
            DirKind::Threshold => (self.size.saturating_sub(self.threshold)
                ..=self.size.saturating_add(self.threshold))
                .contains(&size),
        }
    }

    /// The spec's `DirectorySizeDistance`, in device pixels.
    fn distance(&self, size: u32, scale: u32) -> u32 {
        let want = size.saturating_mul(scale);
        let (low, high) = match self.kind {
            DirKind::Fixed => (self.size, self.size),
            DirKind::Scalable => (self.min_size, self.max_size),
            DirKind::Threshold => (
                self.size.saturating_sub(self.threshold),
                self.size.saturating_add(self.threshold),
            ),
        };
        let (low, high) = (
            low.saturating_mul(self.scale),
            high.saturating_mul(self.scale),
        );
        // Zero inside the band; otherwise the gap to its nearer edge.
        low.saturating_sub(want).max(want.saturating_sub(high))
    }

    /// Device pixels this directory's icons are drawn at, for breaking a
    /// distance tie towards the larger (downscaling beats upscaling).
    fn device_size(&self) -> u32 {
        self.size.saturating_mul(self.scale)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Theme {
    subdirs: Vec<SubDir>,
    inherits: Vec<String>,
}

fn comma_list(value: Option<&String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_index(contents: &str) -> Theme {
    let groups = crate::entry::unlocalised_groups(contents);
    let Some(header) = groups.get("Icon Theme") else {
        return Theme::default();
    };
    let mut names = comma_list(header.get("Directories"));
    for scaled in comma_list(header.get("ScaledDirectories")) {
        if !names.contains(&scaled) {
            names.push(scaled);
        }
    }
    let number = |group: &HashMap<&str, &String>, key: &str| {
        group
            .get(key)
            .and_then(|value| value.trim().parse::<u32>().ok())
    };
    let subdirs = names
        .into_iter()
        .filter_map(|name| {
            let group = groups.get(&name)?;
            let group: HashMap<&str, &String> = group
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect();
            let size = number(&group, "Size").filter(|size| *size > 0)?;
            let kind = match group.get("Type").map(|value| value.trim()) {
                Some("Fixed") => DirKind::Fixed,
                Some("Scalable") => DirKind::Scalable,
                _ => DirKind::Threshold,
            };
            Some(SubDir {
                size,
                scale: number(&group, "Scale")
                    .filter(|scale| *scale > 0)
                    .unwrap_or(1),
                min_size: number(&group, "MinSize").unwrap_or(size),
                max_size: number(&group, "MaxSize").unwrap_or(size),
                threshold: number(&group, "Threshold").unwrap_or(2),
                kind,
                path: name,
            })
        })
        .collect();
    Theme {
        subdirs,
        inherits: comma_list(header.get("Inherits")),
    }
}

/// `48x48` → (48, 1), `24x24@2` → (24, 2).
fn parse_size_dir(name: &str) -> Option<(u32, u32)> {
    let (dims, scale) = match name.split_once('@') {
        Some((dims, scale)) => (dims, scale.parse::<u32>().ok().filter(|s| *s > 0)?),
        None => (name, 1),
    };
    let (width, height) = dims.split_once('x')?;
    let width = width.parse::<u32>().ok().filter(|w| *w > 0)?;
    (height.parse::<u32>().ok() == Some(width)).then_some((width, scale))
}

fn sorted_dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// hicolor's layout read off the directories present under `bases`.
fn discovered_layout(bases: &[PathBuf], theme: &str) -> Theme {
    let mut subdirs: Vec<SubDir> = Vec::new();
    for base in bases {
        let root = base.join(theme);
        for size_dir in sorted_dir_names(&root) {
            let shape = if size_dir == "scalable" || size_dir == "symbolic" {
                None
            } else if let Some(size) = parse_size_dir(&size_dir) {
                Some(size)
            } else {
                continue;
            };
            for context in sorted_dir_names(&root.join(&size_dir)) {
                let path = format!("{size_dir}/{context}");
                if subdirs.iter().any(|known| known.path == path) {
                    continue;
                }
                subdirs.push(match shape {
                    Some((size, scale)) => SubDir {
                        path,
                        size,
                        scale,
                        min_size: size,
                        max_size: size,
                        threshold: 2,
                        kind: DirKind::Threshold,
                    },
                    None => SubDir {
                        path,
                        size: 128,
                        scale: 1,
                        min_size: 1,
                        max_size: 512,
                        threshold: 2,
                        kind: DirKind::Scalable,
                    },
                });
            }
        }
    }
    Theme {
        subdirs,
        inherits: Vec::new(),
    }
}

/// Reject names that would walk out of a theme directory.
fn clean_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    // A handful of entries still write `Icon=foo.png`; the spec says the
    // value is a name, and every toolkit strips the extension.
    let stem = [".png", ".svg", ".xpm"]
        .iter()
        .find_map(|ext| {
            name.len()
                .checked_sub(ext.len())
                .filter(|cut| {
                    name.get(*cut..)
                        .is_some_and(|tail| tail.eq_ignore_ascii_case(ext))
                })
                .and_then(|cut| name.get(..cut))
        })
        .unwrap_or(name);
    (!stem.is_empty()).then_some(stem)
}

type MemoKey = (String, u32, u32);

/// A theme-aware icon resolver with its answers memoised.
#[derive(Debug)]
pub struct IconLookup {
    search: IconSearchPath,
    theme: String,
    themes: Mutex<HashMap<String, Option<Arc<Theme>>>>,
    memo: Mutex<HashMap<MemoKey, Option<IconFile>>>,
}

impl IconLookup {
    pub fn new(search: IconSearchPath, theme: impl Into<String>) -> Self {
        Self {
            search,
            theme: theme.into(),
            themes: Mutex::new(HashMap::new()),
            memo: Mutex::new(HashMap::new()),
        }
    }

    pub fn search_path(&self) -> &IconSearchPath {
        &self.search
    }

    /// The file for icon `name` drawn `size` logical pixels square at
    /// integer `scale`, or `None` when no theme, base or pixmaps directory
    /// has a PNG or SVG of that name.
    pub fn find(&self, name: &str, size: u32, scale: u32) -> Option<IconFile> {
        let size = size.max(1);
        let scale = scale.max(1);
        if Path::new(name.trim()).is_absolute() {
            let path = PathBuf::from(name.trim());
            let format = IconFormat::of(&path)?;
            return path.is_file().then_some(IconFile { path, format });
        }
        let name = clean_name(name)?;
        let key = (name.to_string(), size, scale);
        if let Some(hit) = self
            .memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return hit.clone();
        }
        let found = self.find_uncached(name, size, scale);
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        if memo.len() >= MEMO_LIMIT {
            memo.clear();
        }
        memo.insert(key, found.clone());
        found
    }

    fn find_uncached(&self, name: &str, size: u32, scale: u32) -> Option<IconFile> {
        let mut visited: Vec<String> = Vec::new();
        self.find_in_chain(name, size, scale, &self.theme, &mut visited, 0)
            .or_else(|| self.find_in_chain(name, size, scale, FALLBACK_THEME, &mut visited, 0))
            .or_else(|| self.find_loose(name))
    }

    fn find_in_chain(
        &self,
        name: &str,
        size: u32,
        scale: u32,
        theme_name: &str,
        visited: &mut Vec<String>,
        depth: usize,
    ) -> Option<IconFile> {
        if depth > MAX_INHERIT_DEPTH || visited.iter().any(|seen| seen == theme_name) {
            return None;
        }
        visited.push(theme_name.to_string());
        let theme = self.theme(theme_name)?;
        if let Some(found) = self.find_in_theme(name, size, scale, theme_name, &theme) {
            return Some(found);
        }
        theme.inherits.iter().find_map(|parent| {
            self.find_in_chain(name, size, scale, parent, visited, depth.saturating_add(1))
        })
    }

    /// The spec's `LookupIcon`: an exact size match first, else the
    /// nearest directory.
    fn find_in_theme(
        &self,
        name: &str,
        size: u32,
        scale: u32,
        theme_name: &str,
        theme: &Theme,
    ) -> Option<IconFile> {
        let file_in = |subdir: &SubDir| {
            self.search.bases.iter().find_map(|base| {
                IconFormat::ALL.into_iter().find_map(|format| {
                    let path = base
                        .join(theme_name)
                        .join(&subdir.path)
                        .join(format!("{name}.{}", format.extension()));
                    path.is_file().then_some(IconFile { path, format })
                })
            })
        };
        if let Some(found) = theme
            .subdirs
            .iter()
            .filter(|subdir| subdir.matches(size, scale))
            .find_map(file_in)
        {
            return Some(found);
        }
        let mut best: Option<(u32, u32, IconFile)> = None;
        for subdir in &theme.subdirs {
            let distance = subdir.distance(size, scale);
            let better = best.as_ref().is_none_or(|(best_distance, best_size, _)| {
                distance < *best_distance
                    || (distance == *best_distance && subdir.device_size() > *best_size)
            });
            if better && let Some(file) = file_in(subdir) {
                best = Some((distance, subdir.device_size(), file));
            }
        }
        best.map(|(_, _, file)| file)
    }

    /// The spec's `LookupFallbackIcon`.
    fn find_loose(&self, name: &str) -> Option<IconFile> {
        self.search
            .bases
            .iter()
            .chain(&self.search.pixmaps)
            .find_map(|dir| {
                IconFormat::ALL.into_iter().find_map(|format| {
                    let path = dir.join(format!("{name}.{}", format.extension()));
                    path.is_file().then_some(IconFile { path, format })
                })
            })
    }

    fn theme(&self, name: &str) -> Option<Arc<Theme>> {
        if let Some(known) = self
            .themes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            return known.clone();
        }
        let loaded = self.load_theme(name).map(Arc::new);
        self.themes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string(), loaded.clone());
        loaded
    }

    fn load_theme(&self, name: &str) -> Option<Theme> {
        if clean_name(name) != Some(name) {
            return None;
        }
        let index = self
            .search
            .bases
            .iter()
            .map(|base| base.join(name).join("index.theme"))
            .find(|path| path.is_file());
        match index {
            Some(index) => std::fs::read_to_string(index)
                .ok()
                .map(|text| parse_index(&text)),
            None if name == FALLBACK_THEME => Some(discovered_layout(&self.search.bases, name)),
            None => None,
        }
    }
}

struct Shared {
    generation: u64,
    lookup: Arc<IconLookup>,
}

fn shared() -> &'static Mutex<Option<Shared>> {
    static SHARED: OnceLock<Mutex<Option<Shared>>> = OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(None))
}

/// The session's icon resolver. Rebuilt — dropping every memoised answer —
/// whenever the application scan in [`crate::cache`] notices something was
/// installed or removed, since that is when new icons appear.
pub fn system_lookup() -> Arc<IconLookup> {
    let generation = crate::cache::cached_scan_generation();
    let mut guard = shared().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(current) = guard.as_ref()
        && current.generation == generation
    {
        return current.lookup.clone();
    }
    let lookup = Arc::new(IconLookup::new(IconSearchPath::system(), DEFAULT_THEME));
    *guard = Some(Shared {
        generation,
        lookup: lookup.clone(),
    });
    lookup
}

/// [`IconLookup::find`] against the session's themes.
pub fn lookup(name: &str, size: u32, scale: u32) -> Option<IconFile> {
    system_lookup().find(name, size, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"icon").unwrap();
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A theme `Fixture` inheriting `Parent`, with sized, scaled and
    /// scalable directories, and an index-less hicolor under a second base.
    fn fixture() -> (tempfile::TempDir, IconSearchPath) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("share/icons");
        let exports = dir.path().join("flatpak/exports/share/icons");
        let pixmaps = dir.path().join("share/pixmaps");
        write(
            &base.join("Fixture/index.theme"),
            "[Icon Theme]\nName=Fixture\nInherits=Parent\n\
             Directories=16x16/apps,48x48/apps,scalable/apps\n\
             ScaledDirectories=24x24@2/apps\n\n\
             [16x16/apps]\nSize=16\nType=Fixed\n\n\
             [48x48/apps]\nSize=48\nType=Threshold\n\n\
             [24x24@2/apps]\nSize=24\nScale=2\nType=Fixed\n\n\
             [scalable/apps]\nSize=64\nMinSize=24\nMaxSize=40\nType=Scalable\n",
        );
        touch(&base.join("Fixture/16x16/apps/editor.png"));
        touch(&base.join("Fixture/48x48/apps/editor.png"));
        touch(&base.join("Fixture/24x24@2/apps/editor.png"));
        touch(&base.join("Fixture/scalable/apps/editor.svg"));
        touch(&base.join("Fixture/16x16/apps/tiny.png"));
        touch(&base.join("Fixture/48x48/apps/tiny.png"));
        touch(&base.join("Fixture/16x16/apps/tie.png"));
        touch(&base.join("Fixture/24x24@2/apps/tie.png"));
        write(
            &base.join("Parent/index.theme"),
            "[Icon Theme]\nName=Parent\nInherits=Fixture\nDirectories=32x32/apps\n\n\
             [32x32/apps]\nSize=32\n",
        );
        touch(&base.join("Parent/32x32/apps/inherited.png"));
        touch(&exports.join("hicolor/128x128/apps/org.mozilla.firefox.png"));
        touch(&exports.join("hicolor/scalable/apps/com.valvesoftware.Steam.svg"));
        touch(&pixmaps.join("legacy.png"));
        let search = IconSearchPath {
            bases: vec![base, exports],
            pixmaps: vec![pixmaps],
        };
        (dir, search)
    }

    fn found(lookup: &IconLookup, name: &str, size: u32, scale: u32) -> String {
        let file = lookup.find(name, size, scale).expect("icon resolves");
        let path = file.path.to_string_lossy().into_owned();
        let at = path
            .find("icons/")
            .or_else(|| path.find("pixmaps/"))
            .unwrap();
        path[at..].to_string()
    }

    #[test]
    fn the_directory_serving_the_requested_size_wins() {
        let (_dir, search) = fixture();
        let lookup = IconLookup::new(search, "Fixture");
        assert_eq!(
            found(&lookup, "editor", 16, 1),
            "icons/Fixture/16x16/apps/editor.png"
        );
        assert_eq!(
            found(&lookup, "editor", 48, 1),
            "icons/Fixture/48x48/apps/editor.png"
        );
        // Inside the threshold of the 48 directory.
        assert_eq!(
            found(&lookup, "editor", 50, 1),
            "icons/Fixture/48x48/apps/editor.png"
        );
        assert_eq!(
            found(&lookup, "editor", 24, 2),
            "icons/Fixture/24x24@2/apps/editor.png"
        );
    }

    #[test]
    fn a_scalable_directory_serves_its_range_as_svg() {
        let (_dir, search) = fixture();
        let lookup = IconLookup::new(search, "Fixture");
        let file = lookup.find("editor", 32, 1).unwrap();
        assert_eq!(file.format, IconFormat::Svg);
        assert!(file.path.ends_with("Fixture/scalable/apps/editor.svg"));
    }

    #[test]
    fn with_no_exact_match_the_nearest_size_is_used_preferring_larger() {
        let (_dir, search) = fixture();
        let lookup = IconLookup::new(search, "Fixture");
        // 32 is nearer the 48 directory's threshold band than 16.
        assert_eq!(
            found(&lookup, "tiny", 32, 1),
            "icons/Fixture/48x48/apps/tiny.png"
        );
        assert_eq!(
            found(&lookup, "tiny", 8, 1),
            "icons/Fixture/16x16/apps/tiny.png"
        );
        // 32 device pixels is 16 from both 16x16 and 24x24@2 (48 device
        // pixels): downscaling the larger looks better.
        assert_eq!(
            found(&lookup, "tie", 32, 1),
            "icons/Fixture/24x24@2/apps/tie.png"
        );
    }

    #[test]
    fn inherited_themes_and_hicolor_are_searched_even_through_a_loop() {
        let (_dir, search) = fixture();
        let lookup = IconLookup::new(search, "Fixture");
        assert_eq!(
            found(&lookup, "inherited", 32, 1),
            "icons/Parent/32x32/apps/inherited.png"
        );
        assert_eq!(lookup.find("absent-everywhere", 32, 1), None);
    }

    #[test]
    fn flatpak_exports_resolve_without_a_hicolor_index() {
        let (_dir, search) = fixture();
        let lookup = IconLookup::new(search, "Fixture");
        assert_eq!(
            found(&lookup, "org.mozilla.firefox", 32, 1),
            "icons/hicolor/128x128/apps/org.mozilla.firefox.png"
        );
        let steam = lookup.find("com.valvesoftware.Steam", 32, 2).unwrap();
        assert_eq!(steam.format, IconFormat::Svg);
    }

    #[test]
    fn a_missing_user_theme_still_falls_back_to_hicolor() {
        let (_dir, search) = fixture();
        let lookup = IconLookup::new(search, "NotInstalled");
        assert_eq!(
            found(&lookup, "org.mozilla.firefox", 48, 1),
            "icons/hicolor/128x128/apps/org.mozilla.firefox.png"
        );
    }

    #[test]
    fn pixmaps_absolute_paths_and_extensions_are_honoured() {
        let (dir, search) = fixture();
        let lookup = IconLookup::new(search, "Fixture");
        assert_eq!(found(&lookup, "legacy", 32, 1), "pixmaps/legacy.png");
        assert_eq!(found(&lookup, "legacy.png", 32, 1), "pixmaps/legacy.png");
        let absolute = dir.path().join("share/pixmaps/legacy.png");
        assert_eq!(
            lookup
                .find(absolute.to_str().unwrap(), 32, 1)
                .map(|f| f.path),
            Some(absolute)
        );
        assert_eq!(lookup.find("/etc/passwd", 32, 1), None);
        assert_eq!(lookup.find("../share/pixmaps/legacy", 32, 1), None);
        assert_eq!(lookup.find("apps/editor", 32, 1), None);
        assert_eq!(lookup.find("", 32, 1), None);
    }

    #[test]
    fn an_application_icon_directory_is_searched_first() {
        let (dir, search) = fixture();
        let own = dir.path().join("opt/app/icons");
        touch(&own.join("legacy.svg"));
        let lookup = IconLookup::new(search.with_first_base(own.clone()), "Fixture");
        assert_eq!(
            lookup.find("legacy", 22, 1).map(|f| f.path),
            Some(own.join("legacy.svg"))
        );
    }

    #[test]
    fn the_system_path_includes_both_flatpak_exports() {
        let bases = IconSearchPath::system().bases;
        assert!(
            bases
                .iter()
                .any(|b| b.ends_with("var/lib/flatpak/exports/share/icons")),
            "{bases:?}"
        );
        assert!(
            bases
                .iter()
                .any(|b| b.ends_with("flatpak/exports/share/icons") && !b.starts_with("/var/lib")),
            "{bases:?}"
        );
    }
}
