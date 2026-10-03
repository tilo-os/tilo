//! On-disk layout of bases and layers.
//!
//! ```text
//! <root>/bases/<name>/            an unpacked base root filesystem, owned by the user
//! <root>/layers/<name>/upper/     the layer's writes — this is also its diff
//! <root>/layers/<name>/work/      overlayfs scratch, must share a filesystem with upper
//! <root>/layers/<name>/home/      the layer's own home directory
//! <root>/layers/<name>/layer.toml metadata
//! ```
//!
//! An ephemeral layer is an ordinary on-disk layer marked for deletion when its
//! session ends. It can be promoted to a kept layer before teardown.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;

/// Whether a layer survives the end of its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerKind {
    /// Deleted when the session that opened it exits, unless promoted first.
    Ephemeral,
    /// Kept until explicitly discarded.
    Kept,
}

impl fmt::Display for LayerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayerKind::Ephemeral => f.write_str("ephemeral"),
            LayerKind::Kept => f.write_str("kept"),
        }
    }
}

/// A layer's metadata, as persisted in `layer.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    /// Name of the base under `<root>/bases`.
    pub base: String,
    pub kind: LayerKind,
    /// Seconds since the Unix epoch.
    pub created: u64,
    /// Names of layers stacked underneath this one, base-most first. Their upper
    /// directories become additional overlay lower directories, which is how a
    /// layer can be built on top of a kept one without copying it.
    #[serde(default)]
    pub stacked_on: Vec<String>,
    /// Free-text note, shown in listings. Set with `tinker keep --note`.
    #[serde(default)]
    pub note: String,
}

/// The layer store.
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Store rooted at an explicit path.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into() }
    }

    /// The user's store.
    ///
    /// `TILO_TINKER_ROOT` overrides, which is what the tests and the demo script
    /// use. Otherwise layers live under the user's data directory: per-user, on
    /// writable storage, and outside any path the OS image owns.
    ///
    /// # Errors
    ///
    /// Returns an error if `TILO_TINKER_ROOT` is unset and neither
    /// `XDG_DATA_HOME` nor `HOME` can be resolved.
    pub fn user() -> Result<Self> {
        if let Some(explicit) = std::env::var_os("TILO_TINKER_ROOT") {
            return Ok(Store::at(PathBuf::from(explicit)));
        }
        let data = dirs_data_home().context("cannot determine a data directory for the store")?;
        Ok(Store::at(data.join("tilo/tinker")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn bases_dir(&self) -> PathBuf {
        self.root.join("bases")
    }

    pub fn layers_dir(&self) -> PathBuf {
        self.root.join("layers")
    }

    pub fn base_path(&self, base: &str) -> PathBuf {
        self.bases_dir().join(base)
    }

    pub fn layer_dir(&self, name: &str) -> PathBuf {
        self.layers_dir().join(name)
    }

    pub fn upper_dir(&self, name: &str) -> PathBuf {
        self.layer_dir(name).join("upper")
    }

    pub fn work_dir(&self, name: &str) -> PathBuf {
        self.layer_dir(name).join("work")
    }

    pub fn home_dir(&self, name: &str) -> PathBuf {
        self.layer_dir(name).join("home")
    }

    fn meta_path(&self, name: &str) -> PathBuf {
        self.layer_dir(name).join("layer.toml")
    }

    /// Create a layer. Fails rather than clobbering an existing one.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` fails `validate_name`, a layer by that
    /// name already exists, any `stacked_on` parent does not exist, or the
    /// layer's directories or metadata can't be written.
    pub fn create(
        &self,
        name: &str,
        base: &str,
        kind: LayerKind,
        stacked_on: Vec<String>,
    ) -> Result<Layer> {
        validate_name(name)?;
        if self.meta_path(name).exists() {
            bail!("layer '{name}' already exists (tinker diff {name} to see what is in it)");
        }
        for parent in &stacked_on {
            if !self.meta_path(parent).exists() {
                bail!("cannot stack on '{parent}': no such layer");
            }
        }
        std::fs::create_dir_all(self.upper_dir(name))?;
        std::fs::create_dir_all(self.work_dir(name))?;
        std::fs::create_dir_all(self.home_dir(name))?;
        let layer = Layer {
            name: name.to_string(),
            base: base.to_string(),
            kind,
            created: unix_now(),
            stacked_on,
            note: String::new(),
        };
        self.write_meta(&layer)?;
        Ok(layer)
    }

    /// # Errors
    ///
    /// Returns an error if `layer` can't be serialised to TOML or its
    /// metadata file can't be written.
    pub fn write_meta(&self, layer: &Layer) -> Result<()> {
        let text = toml::to_string_pretty(layer).context("serialising layer metadata")?;
        std::fs::write(self.meta_path(&layer.name), text)
            .with_context(|| format!("writing metadata for layer '{}'", layer.name))?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if no layer named `name` exists, or its metadata
    /// file is not valid TOML for [`Layer`].
    pub fn load(&self, name: &str) -> Result<Layer> {
        let path = self.meta_path(name);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("no layer named '{name}' (tinker list)"))?;
        toml::from_str(&text)
            .with_context(|| format!("layer metadata at {} is unreadable", path.display()))
    }

    /// # Errors
    ///
    /// Returns an error if the layers directory exists but can't be read.
    /// An individual layer whose metadata fails to load is skipped rather
    /// than failing the whole listing.
    pub fn list(&self) -> Result<Vec<Layer>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(self.layers_dir()) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in rd.filter_map(std::result::Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(layer) = self.load(&name) {
                out.push(layer);
            }
        }
        out.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    /// Promote an ephemeral layer to a kept one, optionally renaming it.
    ///
    /// This is the retroactive decision the ephemeral-on-disk design exists to
    /// allow. Promoting an already-kept layer is not an error; it just updates
    /// the note, so `tinker keep` is safe to type twice.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` does not exist, `new_name` fails
    /// `validate_name` or is already taken, or the rename or the rewritten
    /// metadata can't be written.
    pub fn promote(&self, name: &str, new_name: Option<&str>, note: Option<&str>) -> Result<Layer> {
        let mut layer = self.load(name)?;
        if let Some(new) = new_name.filter(|n| *n != name) {
            validate_name(new)?;
            if self.meta_path(new).exists() {
                bail!("cannot rename to '{new}': a layer by that name already exists");
            }
            std::fs::rename(self.layer_dir(name), self.layer_dir(new))
                .with_context(|| format!("renaming layer '{name}' to '{new}'"))?;
            // Anything stacked on the old name would now dangle. Rewrite the
            // references so a rename cannot silently break a stack.
            self.rewrite_stack_references(name, new)?;
            layer.name = new.to_string();
        }
        layer.kind = LayerKind::Kept;
        if let Some(n) = note {
            layer.note = n.to_string();
        }
        self.write_meta(&layer)?;
        Ok(layer)
    }

    fn rewrite_stack_references(&self, old: &str, new: &str) -> Result<()> {
        for mut other in self.list()? {
            if other.stacked_on.iter().any(|s| s == old) {
                for s in &mut other.stacked_on {
                    if s == old {
                        *s = new.to_string();
                    }
                }
                self.write_meta(&other)?;
            }
        }
        Ok(())
    }

    /// Layers that would dangle if `name` were removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the layers directory exists but can't be read.
    pub fn dependents(&self, name: &str) -> Result<Vec<String>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|l| l.stacked_on.iter().any(|s| s == name))
            .map(|l| l.name)
            .collect())
    }

    /// Delete a layer.
    ///
    /// Refuses while another layer is stacked on it, because removing the lower
    /// out from under a stack would silently change what the upper layers see.
    /// The layer's files can belong to subordinate ids, so this must run inside
    /// the layer id mapping — see [`crate::idmap`].
    ///
    /// # Errors
    ///
    /// Returns an error if another layer still depends on `name`, no layer
    /// named `name` exists, or its directory can't be removed.
    pub fn discard(&self, name: &str) -> Result<()> {
        let dependents = self.dependents(name)?;
        if !dependents.is_empty() {
            bail!(
                "layer '{name}' is underneath {}; discard those first",
                dependents.join(", ")
            );
        }
        let dir = self.layer_dir(name);
        if !dir.exists() {
            bail!("no layer named '{name}'");
        }
        std::fs::remove_dir_all(&dir).with_context(|| {
            format!(
                "removing {}. Layer contents can be owned by subordinate ids; \
                 this command has to run inside the layer id mapping.",
                dir.display()
            )
        })?;
        Ok(())
    }

    /// Delete one path's worth of change from a layer, so the base version shows
    /// through again.
    ///
    /// Removing a whiteout un-deletes; removing a copied-up file reverts it.
    /// Either way the base is untouched, which is what makes selective revert
    /// safe to offer at all.
    ///
    /// # Errors
    ///
    /// Returns an error if `logical` contains a `..` component, the layer
    /// never changed that path, or removing it fails.
    pub fn revert_path(&self, name: &str, logical: &Path) -> Result<()> {
        let rel = logical.strip_prefix("/").unwrap_or(logical);
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("path must not contain '..'");
        }
        let target = self.upper_dir(name).join(rel);
        if target.symlink_metadata().is_err() {
            bail!("layer '{name}' did not change {}", logical.display());
        }
        if target.symlink_metadata()?.is_dir() {
            std::fs::remove_dir_all(&target)?;
        } else {
            std::fs::remove_file(&target)?;
        }
        Ok(())
    }
}

/// Layer names become directory names and appear in shell prompts, so keep them
/// boring: no separators, no leading dot, nothing that needs quoting.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("layer name must not be empty");
    }
    if name.len() > 64 {
        bail!("layer name is too long (max 64 characters)");
    }
    if name.starts_with('.') || name.starts_with('-') {
        bail!("layer name must not start with '.' or '-'");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("layer name may only contain letters, digits, '-', '_' and '.'");
    }
    Ok(())
}

fn dirs_data_home() -> Option<PathBuf> {
    xdg_paths::xdg::data_home()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::at(d.path());
        (d, s)
    }

    #[test]
    fn layers_are_created_promoted_and_renamed_without_clobbering() {
        // Phase `a_created_layer_round_trips`.
        {
            let (_d, s) = store();
            s.create("scratch", "debian", LayerKind::Ephemeral, vec![])
                .unwrap();
            let back = s.load("scratch").unwrap();
            assert_eq!(back.name, "scratch");
            assert_eq!(back.base, "debian");
            assert_eq!(back.kind, LayerKind::Ephemeral);
            assert!(s.upper_dir("scratch").is_dir());
            assert!(s.work_dir("scratch").is_dir());
            assert!(s.home_dir("scratch").is_dir());
        }
        // Phase `creating_twice_refuses_rather_than_clobbering`.
        {
            let (_d, s) = store();
            s.create("a", "debian", LayerKind::Kept, vec![]).unwrap();
            let err = s
                .create("a", "debian", LayerKind::Kept, vec![])
                .unwrap_err();
            assert!(err.to_string().contains("already exists"), "{err}");
        }
        // Phase `promotion_is_retroactive_and_can_rename`.
        {
            let (_d, s) = store();
            s.create("scratch", "debian", LayerKind::Ephemeral, vec![])
                .unwrap();
            std::fs::write(s.upper_dir("scratch").join("marker"), b"work").unwrap();

            let kept = s
                .promote("scratch", Some("rust-dev"), Some("cargo + clang"))
                .unwrap();
            assert_eq!(kept.kind, LayerKind::Kept);
            assert_eq!(kept.name, "rust-dev");
            assert_eq!(kept.note, "cargo + clang");
            // The work done before the decision has to survive the decision.
            assert_eq!(
                std::fs::read(s.upper_dir("rust-dev").join("marker")).unwrap(),
                b"work"
            );
            assert!(s.load("scratch").is_err());
        }
        // Phase `promoting_twice_is_not_an_error`.
        {
            let (_d, s) = store();
            s.create("a", "debian", LayerKind::Ephemeral, vec![])
                .unwrap();
            s.promote("a", None, None).unwrap();
            let again = s.promote("a", None, Some("note")).unwrap();
            assert_eq!(again.kind, LayerKind::Kept);
            assert_eq!(again.note, "note");
        }
        // Phase `renaming_rewrites_stack_references`.
        {
            let (_d, s) = store();
            s.create("bottom", "debian", LayerKind::Kept, vec![])
                .unwrap();
            s.create("top", "debian", LayerKind::Ephemeral, vec!["bottom".into()])
                .unwrap();
            s.promote("bottom", Some("renamed"), None).unwrap();
            assert_eq!(
                s.load("top").unwrap().stacked_on,
                vec!["renamed".to_string()]
            );
        }
        // Phase `listing_an_empty_store_is_empty_not_an_error`.
        {
            let d = tempfile::tempdir().unwrap();
            let s = Store::at(d.path().join("does-not-exist-yet"));
            assert!(s.list().unwrap().is_empty());
        }
    }

    #[test]
    fn stacks_reverts_and_names_are_contained() {
        // Phase `stacking_on_a_missing_layer_is_refused`.
        {
            let (_d, s) = store();
            let err = s
                .create("top", "debian", LayerKind::Ephemeral, vec!["ghost".into()])
                .unwrap_err();
            assert!(err.to_string().contains("no such layer"), "{err}");
        }
        // Phase `discard_refuses_to_pull_a_layer_out_from_under_a_stack`.
        {
            let (_d, s) = store();
            s.create("bottom", "debian", LayerKind::Kept, vec![])
                .unwrap();
            s.create("top", "debian", LayerKind::Ephemeral, vec!["bottom".into()])
                .unwrap();

            let err = s.discard("bottom").unwrap_err();
            assert!(err.to_string().contains("top"), "{err}");
            assert!(s.load("bottom").is_ok());

            s.discard("top").unwrap();
            s.discard("bottom").unwrap();
            assert!(s.list().unwrap().is_empty());
        }
        // Phase `reverting_one_path_leaves_the_rest_of_the_layer_alone`.
        {
            let (_d, s) = store();
            s.create("a", "debian", LayerKind::Kept, vec![]).unwrap();
            std::fs::create_dir_all(s.upper_dir("a").join("etc")).unwrap();
            std::fs::write(s.upper_dir("a").join("etc/hosts"), b"bad").unwrap();
            std::fs::write(s.upper_dir("a").join("etc/keepme"), b"good").unwrap();

            s.revert_path("a", Path::new("/etc/hosts")).unwrap();
            assert!(!s.upper_dir("a").join("etc/hosts").exists());
            assert!(s.upper_dir("a").join("etc/keepme").exists());
        }
        // Phase `reverting_something_the_layer_never_touched_says_so`.
        {
            let (_d, s) = store();
            s.create("a", "debian", LayerKind::Kept, vec![]).unwrap();
            let err = s.revert_path("a", Path::new("/etc/nope")).unwrap_err();
            assert!(err.to_string().contains("did not change"), "{err}");
        }
        // Phase `revert_paths_cannot_escape_the_layer`.
        {
            let (_d, s) = store();
            s.create("a", "debian", LayerKind::Kept, vec![]).unwrap();
            assert!(s.revert_path("a", Path::new("/../../etc/passwd")).is_err());
        }
        // Phase `hostile_layer_names_are_refused`.
        {
            let (_d, s) = store();
            for bad in ["", ".hidden", "-flag", "a/b", "a b", "a;b", "..", "a\nb"] {
                assert!(
                    s.create(bad, "debian", LayerKind::Kept, vec![]).is_err(),
                    "accepted {bad:?}"
                );
            }
            // '..' must be rejected by the name rules, not merely by the filesystem.
            assert!(validate_name("..").is_err());
        }
    }
}
