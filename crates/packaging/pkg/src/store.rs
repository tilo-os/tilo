//! Where installed packages live, and in what order readers see them.
//!
//! A qOS machine has **two** package stores and every reader has to see
//! both:
//!
//! * the **system store** at [`SYSTEM_DATA_DIR`], written during the
//!   image build and shipped read-only inside the immutable rootfs. It
//!   is what gives a first boot a populated command palette without the
//!   user installing anything.
//! * the **user store** under `$XDG_DATA_HOME/qos-apps`, which is the
//!   only one anything at runtime can write to.
//!
//! A user entry **shadows** a system entry with the same id, so building
//! your own copy of a preinstalled app replaces it in the palette rather
//! than producing two rows for one app.
//!
//! This module exists because that rule was implemented twice and only
//! once correctly. `qos-launcher` merged both stores; `qos-install list`
//! read the user store alone, so on a machine whose apps are *all*
//! preinstalled it printed "(no packages installed)" next to a launcher
//! showing forty-five. Two readers of one store disagreeing about what
//! is installed is the bug; one implementation of the lookup order is
//! the fix.
//!
//! Only path resolution and the merge order live here. Neither reader's
//! row shape does: the launcher decodes three fields and the installer
//! decodes twelve, and forcing them into one struct would make every
//! field either optional or a lie. [`merge_registries`] therefore takes
//! the decode step as a closure and is generic over the row.

use std::path::{Path, PathBuf};

/// The image-wide app store. Read-only at runtime: it is populated
/// during the image build and ships inside the immutable rootfs.
pub const SYSTEM_DATA_DIR: &str = "/usr/share/qos-apps";

/// Filename of a store's registry, in both stores.
pub const REGISTRY_FILENAME: &str = "registry.toml";

/// Which of the two stores a row came from.
///
/// Worth carrying to the caller rather than discarding after the merge:
/// "installed" and "shipped with the machine" differ in whether the
/// running system can remove it, and a reader that cannot tell them
/// apart will offer to uninstall a file in a read-only `/usr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Store {
    /// [`SYSTEM_DATA_DIR`] — preinstalled, not writable at runtime.
    System,
    /// The per-user store — installed by this user, removable.
    User,
}

impl Store {
    /// Single word for a column in a listing.
    pub fn label(self) -> &'static str {
        match self {
            Store::System => "system",
            Store::User => "user",
        }
    }

    /// Whether anything running on this machine may modify the store.
    pub fn is_writable(self) -> bool {
        matches!(self, Store::User)
    }
}

/// Root of the system store, or `None` when there is no system store to
/// read.
///
/// `QOS_APPS_SYSTEM_DIR` names it explicitly. Otherwise it is
/// [`SYSTEM_DATA_DIR`] — *unless* `QOS_APPS_DATA_DIR` is set, which
/// suppresses it entirely.
///
/// That last rule is what makes the override usable for its documented
/// purpose. `QOS_APPS_DATA_DIR` means "the store is here", and a test
/// that says so and then asserts on how many packages exist has to get
/// the answer it set up — otherwise the same test passes on a dev box
/// with no `/usr/share/qos-apps` and fails inside a qOS image, where
/// every preinstalled package would be counted alongside its fixtures.
/// Production sets neither variable and reads both stores; a test that
/// genuinely wants both sets `QOS_APPS_SYSTEM_DIR` too.
pub fn system_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_APPS_SYSTEM_DIR") {
        return Some(PathBuf::from(dir));
    }
    if std::env::var_os("QOS_APPS_DATA_DIR").is_some() {
        return None;
    }
    Some(PathBuf::from(SYSTEM_DATA_DIR))
}

/// `registry.toml` of the system store, when there is one. Unlike the
/// per-user path this needs no `HOME`: it resolves identically for every
/// user and for a launch with no home at all.
pub fn system_registry_path() -> Option<PathBuf> {
    Some(system_data_dir()?.join(REGISTRY_FILENAME))
}

/// Root of the per-user store: `QOS_APPS_DATA_DIR`, else
/// `$XDG_DATA_HOME/qos-apps`, else `$HOME/.local/share/qos-apps`.
///
/// `None` means none of those resolve — a process with no `HOME` and no
/// override. That is not "nothing installed": it is "this process cannot
/// tell", and callers that need the distinction get it from the option.
pub fn user_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("qos-apps"));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".local/share/qos-apps"))
}

/// `registry.toml` of the per-user store, when one can be located.
pub fn user_registry_path() -> Option<PathBuf> {
    Some(user_data_dir()?.join(REGISTRY_FILENAME))
}

/// Every registry a reader must consult, **in precedence order**: system
/// first, user second, so a later row overwrites an earlier one with the
/// same id.
///
/// A store that cannot be located is absent from the list rather than
/// present-and-empty, which is what lets a caller report "no home, so
/// the user store was not consulted" instead of "you have no apps".
pub fn registry_paths() -> Vec<(Store, PathBuf)> {
    let mut out = Vec::with_capacity(2);
    if let Some(p) = system_registry_path() {
        out.push((Store::System, p));
    }
    if let Some(p) = user_registry_path() {
        out.push((Store::User, p));
    }
    out
}

/// Read both stores with `read`, and merge the rows by id with the user
/// store shadowing the system store.
///
/// `read` is handed one `registry.toml` path and returns its rows; it is
/// the caller's decoder, so each reader keeps its own row shape. It is
/// called for paths that may not exist — a store with no registry is
/// "nothing in that store", and returning an empty vec for a missing
/// file is the decoder's job, not this function's, because "absent" and
/// "unparseable" must not collapse into each other.
///
/// Shadowing replaces in place rather than appending, which keeps the
/// system store's ordering stable instead of moving a row to the end of
/// the list the moment the user rebuilds that app.
pub fn merge_registries<T, E>(
    read: impl Fn(&Path) -> Result<Vec<T>, E>,
    id_of: impl Fn(&T) -> &str,
) -> Result<Vec<(Store, T)>, E> {
    let mut out: Vec<(Store, T)> = Vec::new();
    for (store, path) in registry_paths() {
        for row in read(&path)? {
            match out.iter().position(|(_, e)| id_of(e) == id_of(&row)) {
                Some(at) => out[at] = (store, row),
                None => out.push((store, row)),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Every test here mutates process-wide environment, so they are
    /// serialised against each other by this lock rather than by luck.
    static ENV: Mutex<()> = Mutex::new(());

    #[derive(Debug, PartialEq, Eq)]
    struct Row {
        id: String,
        marker: &'static str,
    }

    fn rows(spec: &[(&str, &'static str)]) -> Vec<Row> {
        spec.iter()
            .map(|(id, marker)| Row {
                id: (*id).to_string(),
                marker,
            })
            .collect()
    }

    /// Run `f` with both store roots pinned, putting every variable
    /// back afterwards — including the ones that were already unset.
    /// A test that leaves `HOME` or `XDG_DATA_HOME` pointing somewhere
    /// of its own devising is a flake in whichever test runs next.
    fn with_stores<T>(system: Option<&Path>, user: Option<&Path>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            ["QOS_APPS_SYSTEM_DIR", "QOS_APPS_DATA_DIR", "XDG_DATA_HOME"]
                .into_iter()
                .map(|k| (k, std::env::var_os(k)))
                .collect();
        // SAFETY: serialised by ENV above; nothing else in this crate
        // reads these variables concurrently.
        unsafe {
            for (k, _) in &saved {
                std::env::remove_var(k);
            }
            if let Some(p) = system {
                std::env::set_var("QOS_APPS_SYSTEM_DIR", p);
            }
            if let Some(p) = user {
                std::env::set_var("QOS_APPS_DATA_DIR", p);
            }
        }
        let out = f();
        unsafe {
            for (k, v) in saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        out
    }

    #[test]
    fn system_store_is_read_before_the_user_store() {
        let paths = with_stores(Some(Path::new("/sys")), Some(Path::new("/usr-store")), || {
            registry_paths()
        });
        assert_eq!(
            paths,
            vec![
                (Store::System, PathBuf::from("/sys/registry.toml")),
                (Store::User, PathBuf::from("/usr-store/registry.toml")),
            ]
        );
    }

    #[test]
    fn a_bare_data_dir_override_suppresses_the_system_store() {
        // `QOS_APPS_DATA_DIR` alone means "the store is here" — a test
        // fixture must not be joined by /usr/share/qos-apps.
        let paths = with_stores(None, Some(Path::new("/fixture")), registry_paths);
        assert_eq!(
            paths,
            vec![(Store::User, PathBuf::from("/fixture/registry.toml"))]
        );
    }

    #[test]
    fn the_user_store_shadows_a_system_row_of_the_same_id_in_place() {
        let merged = with_stores(Some(Path::new("/sys")), Some(Path::new("/user")), || {
            merge_registries::<Row, std::convert::Infallible>(
                |p| {
                    Ok(if p.starts_with("/sys") {
                        rows(&[("ai.qos.notes", "sys"), ("ai.qos.maps", "sys")])
                    } else {
                        rows(&[("ai.qos.maps", "user"), ("ai.qos.extra", "user")])
                    })
                },
                |r| r.id.as_str(),
            )
            .unwrap()
        });
        // maps keeps the position it had in the system store, and its
        // row is the user's; notes stays; extra is appended.
        assert_eq!(
            merged
                .iter()
                .map(|(s, r)| (*s, r.id.as_str(), r.marker))
                .collect::<Vec<_>>(),
            vec![
                (Store::System, "ai.qos.notes", "sys"),
                (Store::User, "ai.qos.maps", "user"),
                (Store::User, "ai.qos.extra", "user"),
            ]
        );
    }

    #[test]
    fn a_read_error_is_propagated_rather_than_becoming_an_empty_store() {
        let out = with_stores(Some(Path::new("/sys")), Some(Path::new("/user")), || {
            merge_registries::<Row, &'static str>(|_| Err("registry parse"), |r| r.id.as_str())
        });
        assert_eq!(out, Err("registry parse"));
    }
}
