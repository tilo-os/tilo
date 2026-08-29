//! `.flatpakref` file handling.
//!
//! The Software store registers itself as the handler for
//! `application/vnd.flatpak.ref` files (see the `.desktop` entry shipped in
//! `os/`). When the compositor hands a `.flatpakref` path to the store on
//! launch — e.g. the user opened one from the file explorer or a download —
//! the store parses it here and drives the install-confirmation flow for
//! the referenced app instead of opening the browse home.
//!
//! A `.flatpakref` is a small INI/keyfile under a `[Flatpak Ref]` group:
//!
//! ```ini
//! [Flatpak Ref]
//! Name=org.gimp.GIMP
//! Branch=stable
//! Title=GIMP
//! Url=https://dl.flathub.org/repo/
//! Homepage=https://www.gimp.org/
//! Comment=Create images and edit photographs
//! Icon=https://dl.flathub.org/repo/appstream/x86_64/icons/128x128/org.gimp.GIMP.png
//! RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo
//! IsRuntime=false
//! ```
//!
//! We only need a few of these to render the confirm sheet and run the
//! install; everything else is ignored.

use std::path::Path;

/// A parsed `.flatpakref`. Carries enough to render the install
/// confirmation and to hand the original file to `flatpak install`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatpakRef {
    /// The reverse-DNS app id (`Name=` key — confusingly named in the spec).
    pub app_id: String,
    /// Human title, if the file provides one (`Title=`); falls back to
    /// the app id.
    pub title: String,
    /// Short comment / tagline (`Comment=`).
    pub comment: String,
    /// The repo URL the ref installs from (`Url=`).
    pub url: String,
    /// The branch (`Branch=`), usually `stable`.
    pub branch: String,
    /// The original file path on disk, passed verbatim to
    /// `flatpak install <path>`.
    pub source_path: String,
}

impl FlatpakRef {
    /// Parse a `.flatpakref` from its text content. `source_path` is the
    /// on-disk path the ref came from (used to drive the install).
    /// Returns `None` if there's no `Name=` key — the one field we can't
    /// proceed without.
    pub fn parse(content: &str, source_path: &str) -> Option<FlatpakRef> {
        let mut in_group = false;
        let mut app_id = None;
        let mut title = String::new();
        let mut comment = String::new();
        let mut url = String::new();
        let mut branch = String::new();

        for raw in content.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                in_group = line.eq_ignore_ascii_case("[Flatpak Ref]");
                continue;
            }
            if !in_group {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().to_string();
            match key {
                "Name" => app_id = Some(value),
                "Title" => title = value,
                "Comment" => comment = value,
                "Url" => url = value,
                "Branch" => branch = value,
                _ => {}
            }
        }

        let app_id = app_id?;
        let title = if title.is_empty() {
            app_id.clone()
        } else {
            title
        };
        let branch = if branch.is_empty() {
            "stable".to_string()
        } else {
            branch
        };
        Some(FlatpakRef {
            app_id,
            title,
            comment,
            url,
            branch,
            source_path: source_path.to_string(),
        })
    }

    /// Read and parse a `.flatpakref` from a path on disk.
    pub fn from_path(path: &Path) -> Option<FlatpakRef> {
        let content = std::fs::read_to_string(path).ok()?;
        FlatpakRef::parse(&content, &path.to_string_lossy())
    }
}

/// Scan a process argument list for the first `.flatpakref` path. Used by
/// `main` so `software_store some/app.flatpakref` opens straight into the
/// install-confirmation flow. Ignores flags (anything starting with `-`).
pub fn ref_path_from_args(args: &[String]) -> Option<std::path::PathBuf> {
    args.iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .find(|a| a.to_lowercase().ends_with(".flatpakref"))
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Flatpak Ref]
Name=org.gimp.GIMP
Branch=stable
Title=GIMP
Url=https://dl.flathub.org/repo/
Comment=Create images and edit photographs
IsRuntime=false
";

    #[test]
    fn parses_core_fields() {
        let r = FlatpakRef::parse(SAMPLE, "/tmp/gimp.flatpakref").unwrap();
        assert_eq!(r.app_id, "org.gimp.GIMP");
        assert_eq!(r.title, "GIMP");
        assert_eq!(r.branch, "stable");
        assert_eq!(r.comment, "Create images and edit photographs");
        assert_eq!(r.url, "https://dl.flathub.org/repo/");
        assert_eq!(r.source_path, "/tmp/gimp.flatpakref");
    }

    #[test]
    fn missing_name_is_rejected() {
        let bad = "[Flatpak Ref]\nTitle=Nameless\n";
        assert!(FlatpakRef::parse(bad, "x").is_none());
    }

    #[test]
    fn title_falls_back_to_app_id() {
        let min = "[Flatpak Ref]\nName=org.x.Y\n";
        let r = FlatpakRef::parse(min, "x").unwrap();
        assert_eq!(r.title, "org.x.Y");
        assert_eq!(r.branch, "stable");
    }

    #[test]
    fn ignores_keys_outside_the_group() {
        let txt = "Name=should.be.ignored\n[Flatpak Ref]\nName=real.app.Id\n";
        let r = FlatpakRef::parse(txt, "x").unwrap();
        assert_eq!(r.app_id, "real.app.Id");
    }

    #[test]
    fn arg_scan_finds_flatpakref() {
        let args = vec![
            "software_store".to_string(),
            "--foo".to_string(),
            "/home/me/GIMP.flatpakref".to_string(),
        ];
        assert_eq!(
            ref_path_from_args(&args).unwrap().to_string_lossy(),
            "/home/me/GIMP.flatpakref"
        );
    }

    #[test]
    fn arg_scan_ignores_non_refs() {
        let args = vec![
            "software_store".to_string(),
            "--shots".to_string(),
            "out.png".to_string(),
        ];
        assert!(ref_path_from_args(&args).is_none());
    }
}
