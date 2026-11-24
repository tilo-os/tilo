//! The styles this device's installed packages offer (`docs/designs/app-language/shell.md` §6):
//! one resource the device publishes in its person's own workspace ([`STYLES_APP`], record
//! empty), read through the one record read like every resource, and followed by revision.
//!
//! A style is a view of the person's layout, and the layout's type is the one whose origin is
//! `session.layout`. Any installed tilolang package may declare one: the packages are read by
//! the language runtime the daemon's program mounts ([`Reader`]), and every view of theirs that
//! takes the layout and nothing else is listed. There is no list of styles in code and no
//! keyword: the type is the contract, as it is for every view.
//!
//! The device reads its package store again whenever it moves, so a style a person or their
//! agent installs is offered by every reader of the resource without a restart: the person's
//! shell, their desktop page, an agent or a script. Picking one is the shell's own action
//! (`shell#show-style`), never this resource's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReads, ResourceWake,
};
use tilo_lock::MutexRecover as _;

/// The resource the device's styles are read at, in its person's own workspace (`record`
/// empty): a reading of [`StyleList`], read by whoever that workspace lets read it. It is also
/// the package a tilolang view reads it through (`use tilo.styles as styles`, then
/// `styles.installed.offered`).
pub const STYLES_APP: &str = "tilo.styles";

/// A style: a view of the layout an installed package declares (shell.md §6).
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Style {
    /// The view reference: a package and a view (`so.tilo.shell:strips`), as `Viewing.style`
    /// names it.
    pub style: String,
    /// The package that declares it.
    pub package: String,
    /// The view's own name, as its declaration writes it.
    pub name: String,
    /// The view's doc line, in its author's words: what the switch lists it by.
    pub line: String,
    /// Whether the style loads: false for one whose package does not compile against the shell
    /// and the session, or whose view failed when it ran (`shell.md` §6).
    pub loads: bool,
    /// Why the style does not load, in the runtime's words; empty when it does.
    pub reason: String,
}

/// The reading of [`STYLES_APP`]: every style the device's installed packages offer.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct StyleList {
    pub offered: Vec<Style>,
}

/// The styles as a view of another language reads them: the surface `styles-surface.json`
/// beside this crate keeps, which a tilolang view compiles against.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<StyleList, tilo_resource::NoEffect>(
        "styles", "Styles",
    )
}

/// What a device's installed packages offer as styles, read by the language runtime the daemon's
/// program mounts: each package's module is read, and every view of its that takes the person's
/// layout and nothing else is a style. A service its program mounted nothing on offers the
/// styles its store already carries, and reads no package for more.
pub trait Reader: Send + Sync {
    /// The styles the installed packages `apps` (each its id and its payload's directory, in the
    /// order the store lists them) offer, the shell this build carries standing in where the
    /// store has none.
    fn installed(&self, apps: &[(String, std::path::PathBuf)]) -> Vec<Style>;
}

/// The styles the device publishes, as it last read them from its package store.
#[derive(Clone, Default)]
pub struct Styles(Arc<Shown>);

#[derive(Default)]
struct Shown {
    offered: Mutex<Vec<Style>>,
    revision: AtomicU64,
    /// The reads waiting for the styles to move.
    waiting: Mutex<Vec<ResourceWake>>,
}

impl Styles {
    /// Offer `offered` from now on; every reader following the resource is woken when it moved.
    pub fn offer(&self, offered: Vec<Style>) {
        let mut now = self.0.offered.lock_recover();
        if *now == offered {
            return;
        }
        *now = offered;
        drop(now);
        self.0.revision.fetch_add(1, Ordering::Relaxed);
        for wake in std::mem::take(&mut *self.0.waiting.lock_recover()) {
            wake.wake();
        }
    }

    /// The styles offered now.
    #[must_use]
    pub fn offered(&self) -> Vec<Style> {
        self.0.offered.lock_recover().clone()
    }
}

impl ResourceReads for Styles {
    fn read<'a>(
        &'a self,
        _caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ResourceCopy> {
        tilo_actions::remote::reads_answer(async move {
            if address.app != STYLES_APP || !address.record.is_empty() {
                return Ok(ResourceCopy::Absent);
            }
            // The revision first: styles that move while they are read move it past this one.
            let revision = self.0.revision.load(Ordering::Relaxed).to_string();
            serde_json::to_value(StyleList {
                offered: self.offered(),
            })
            .map(|value| ResourceCopy::reading(value, Some(revision)))
            .map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })
        })
    }

    fn watch<'a>(
        &'a self,
        _caller: &'a Caller,
        _address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        tilo_actions::remote::reads_answer(async move {
            let mut waiting = self.0.waiting.lock_recover();
            waiting.retain(ResourceWake::waiting);
            waiting.push(wake.clone());
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file a tilolang view compiles the styles against is what the device publishes.
    /// `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_styles_surface_file_is_what_the_device_publishes() {
        let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": [surface()] }))
            .expect("a surface writes as JSON")
            + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("styles-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/tilo-workspace-service/styles-surface.json is not what the device \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }
}
