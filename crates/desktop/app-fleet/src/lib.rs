//! What the Tilo fleet's signed manifests say about each app, compiled in, so an app that is not on
//! this device yet is known as fully as one that is: where it runs, the names and words people know
//! it by, the documents it creates and the actions it offers while closed. Gathered at build time
//! from the catalogue entry every app commits beside itself (`fleet.json`, written by `tilolang
//! describe --fleet --write`), so a change to one app never rewrites a file another app shares.

/// An action an app offers while closed (`[[offers]]`), as `tilolang surface` writes it from
/// what the app's code publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetAction {
    pub resource: &'static str,
    /// The kind of that resource; empty when it is the resource itself.
    pub resource_kind: &'static str,
    pub action: &'static str,
    pub label: &'static str,
    /// Empty when the manifest gives none.
    pub description: &'static str,
    /// What it may do: `read`, `undoable` or `outside`.
    pub kind: &'static str,
    /// How it acts: `document`, `effect` or `view`.
    pub operation: &'static str,
    /// A person confirms it first.
    pub confirm: bool,
    /// The values it asks for, encoded ([`Self::asks`]); empty when it asks for none.
    pub asks: &'static str,
    /// The other words people use for it, each ended by a unit separator ([`Self::keywords`]).
    keywords: &'static str,
    /// Its picture, by its Lucide name; empty when it has none.
    pub icon: &'static str,
}

/// One value an offered action asks for: its name, the type a form field takes and the words
/// beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetParam {
    pub name: &'static str,
    /// As a form's parameter type spells it (`text`, `secret`, `integer`…).
    pub ty: &'static str,
    /// The words a person reads beside the field; empty when the manifest gives none.
    pub description: &'static str,
}

impl FleetAction {
    /// The values it asks for before it runs, in the order a form gives them.
    pub fn asks(&self) -> impl Iterator<Item = FleetParam> {
        self.asks.split('\u{1e}').filter_map(|part| {
            let mut fields = part.split('\u{1d}');
            let name = fields.next().unwrap_or_default();
            (!name.is_empty()).then(|| FleetParam {
                name,
                ty: fields.next().filter(|ty| !ty.is_empty()).unwrap_or("text"),
                description: fields.next().unwrap_or_default(),
            })
        })
    }

    /// The other words people use for it.
    pub fn keywords(&self) -> impl Iterator<Item = &'static str> {
        self.keywords
            .split('\u{1f}')
            .filter(|word| !word.is_empty())
    }

    /// One line of an app's [`FleetApp::offers`]: its fields in order, each after a tab.
    fn read(line: &'static str) -> Option<Self> {
        let mut fields = line.split('\t');
        let mut next = || fields.next();
        Some(Self {
            resource: next()?,
            resource_kind: next()?,
            action: next()?,
            label: next()?,
            description: next()?,
            kind: next()?,
            operation: next()?,
            confirm: next()? == "1",
            asks: next()?,
            keywords: next()?,
            icon: next()?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetApp {
    pub id: &'static str,
    pub name: &'static str,
    /// Its manifest's category ("Office"); empty when it has none.
    pub category: &'static str,
    /// The platforms it declares (`linux`, `macos`, `web`, `tilo-os`).
    pub platforms: &'static [&'static str],
    /// The registry's tag line; empty when it has none.
    pub tagline: &'static str,
    /// Its name in other languages (`[package.names]`).
    pub names: &'static [&'static str],
    /// Every language's `search-keywords`.
    pub keywords: &'static [&'static str],
    /// `(kind, label)` of each document it creates.
    pub resource_types: &'static [(&'static str, &'static str)],
    /// The actions it offers while closed, one line each ([`Self::actions`]): one string, so
    /// the hundreds the fleet offers cost the program what their text does.
    offers: &'static str,
    /// The actions its windows run on keys, one line each ([`Self::commands`]).
    commands: &'static str,
}

impl FleetApp {
    /// The actions it offers while closed (`[[offers]]`).
    pub fn actions(&self) -> impl Iterator<Item = FleetAction> {
        self.offers.lines().filter_map(FleetAction::read)
    }

    /// The actions its windows run on keys, with the keys it gives each, in the order it declares
    /// them: what a person picks from to move one to other keys.
    pub fn commands(&self) -> impl Iterator<Item = FleetCommand> {
        self.commands.lines().filter_map(|line| {
            let mut fields = line.split('\t');
            Some(FleetCommand {
                action: fields.next()?,
                label: fields.next()?,
                key: fields.next()?,
            })
        })
    }
}

/// One action an app's windows run on keys (`key: "Ctrl+Enter"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetCommand {
    /// The action's id, as it is published.
    pub action: &'static str,
    pub label: &'static str,
    /// The keys the app gives it.
    pub key: &'static str,
}

mod default_apps;

/// The fleet, read from each app's committed catalogue entry by `build.rs`.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/fleet.rs"));
}

#[must_use]
pub fn apps() -> &'static [FleetApp] {
    generated::APPS
}

/// The apps a workspace is created with: the default collection's package ids, the same list
/// the release's `package-apps` builds and the OS image bakes (`dist/apps/app-collections.toml`,
/// via `tools/apps/generate_default_apps.py`). [`metadata`] has each one's name, keywords and
/// closed actions.
#[must_use]
pub fn default_apps() -> &'static [&'static str] {
    default_apps::DEFAULT
}

/// The fleet's metadata for `package_id`. A development build publishes the same apps under
/// `dev.tilo.`, which resolve to their `so.tilo.` entry.
#[must_use]
pub fn metadata(package_id: &str) -> Option<&'static FleetApp> {
    let wanted = canonical(package_id);
    apps().iter().find(|app| app.id == wanted)
}

/// The compiled surface of `package_id`, as its package ships it: the declarations a program in
/// a workspace compiles that app against there.
#[cfg(feature = "surfaces")]
#[must_use]
pub fn surface(package_id: &str) -> Option<&'static str> {
    let wanted = canonical(package_id);
    generated::SURFACES
        .iter()
        .find(|(app, _)| *app == wanted)
        .map(|(_, surface)| *surface)
}

/// Every package that ships a surface, by its id, with the name people call it: the apps of
/// [`apps`] and the services whose resources and actions the workspace holds all the same (the
/// on-screen keyboard's, the launcher's). What `tilo sdk gen` writes types for and the API's
/// description describes, so neither leaves out an action a program may invoke.
#[cfg(feature = "surfaces")]
pub fn surfaced() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    generated::SURFACES.iter().map(|(id, surface)| {
        let name = generated::SURFACE_NAMES
            .iter()
            .find(|(named, _)| named == id)
            .map_or(*id, |(_, name)| *name);
        (*id, name, *surface)
    })
}

/// Whether a window of `package_id` is the person's own, as the app declares it
/// (`ResourceApp::WINDOWS_ARE_PERSONAL`): it shows what the person keeps for themselves, and
/// never stands on a workspace's shared desktop.
#[must_use]
pub fn windows_are_own(package_id: &str) -> bool {
    let app = canonical(package_id);
    generated::OWN_WINDOWS
        .iter()
        .any(|own| own.eq_ignore_ascii_case(&app))
}

/// Whether each window of `package_id` shows a document of its own (a workbook, a terminal's
/// session), as the app itself declares (`ResourceApp::WINDOW_RESOURCE`). Such an app keeps none
/// for the workspace: one of its documents is named by its own id.
#[must_use]
pub fn windows_hold_documents(package_id: &str) -> bool {
    let app = canonical(package_id);
    generated::DOCUMENT_WINDOWS
        .iter()
        .any(|held| held.eq_ignore_ascii_case(&app))
}

/// The kinds of record `package_id`'s manifest declares each member of a workspace keeps one of
/// that only they read and change (`[resources] private`).
pub fn private_kinds(package_id: &str) -> impl Iterator<Item = &'static str> {
    let app = canonical(package_id).into_owned();
    generated::PRIVATE_KINDS
        .iter()
        .filter(move |(declared, _)| declared.eq_ignore_ascii_case(&app))
        .map(|(_, kind)| *kind)
}

/// Every kind of record [`private_kinds`] names, by the app that declares it.
pub fn every_private_kind() -> impl Iterator<Item = (&'static str, &'static str)> {
    generated::PRIVATE_KINDS.iter().copied()
}

/// Whether records of `kind` are kept under keys of their own from their first write, as the
/// app that declares the kind says (`[resources] sealed`, a tilolang root's `sealed: true`).
#[must_use]
pub fn is_sealed(kind: &str) -> bool {
    generated::SEALED_KINDS
        .iter()
        .any(|(_, declared)| *declared == kind)
}

/// The kind of `package_id`'s own record, which a tilolang package keeps its root in.
#[must_use]
pub fn package_kind(package_id: &str) -> String {
    format!("tl.{}", canonical(package_id))
}

/// `package_id` as the fleet names it: a development build's `dev.tilo.` app is its `so.tilo.` one,
/// and an id an app was published under before names that app.
fn canonical(package_id: &str) -> std::borrow::Cow<'_, str> {
    let canonical = package_id
        .strip_prefix("dev.tilo.")
        .map_or_else(|| package_id.to_owned(), |name| format!("so.tilo.{name}"));
    let previous = generated::PREVIOUS
        .iter()
        .find(|(old, _)| old.eq_ignore_ascii_case(&canonical));
    previous.map_or(std::borrow::Cow::Owned(canonical), |(_, app)| {
        std::borrow::Cow::Borrowed(*app)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_build_and_a_previous_id_name_the_app_the_fleet_knows() {
        assert_eq!(
            metadata("dev.tilo.sheets").map(|app| app.id),
            Some("so.tilo.sheets")
        );
        assert_eq!(
            metadata("so.tilo.lang.chat").map(|app| app.id),
            Some("so.tilo.chat")
        );
        for app in apps() {
            assert_eq!(metadata(app.id).map(|known| known.id), Some(app.id));
        }
        assert!(metadata("so.tilo.no-such-app").is_none());
    }

    #[test]
    fn windows_private_records_and_closed_actions_read_per_app() {
        // Settings shows what the person keeps for themselves, so its windows are theirs.
        assert!(windows_are_own("dev.tilo.settings"));
        assert!(!windows_are_own("so.tilo.chat"));
        assert!(windows_hold_documents("so.tilo.sheets"));
        // Each window of PDF Viewer holds one document of the workspace's shared library.
        assert!(windows_hold_documents("so.tilo.pdf-viewer"));
        assert!(!windows_hold_documents("so.tilo.tasks"));
        for (app, kind) in every_private_kind() {
            assert!(
                private_kinds(app).any(|declared| declared == kind),
                "{app} names {kind}"
            );
        }
        for app in apps() {
            for action in app.actions() {
                assert!(!action.action.is_empty(), "{}", app.id);
                assert!(!action.label.is_empty(), "{}", app.id);
                for word in action.keywords() {
                    assert!(!word.is_empty(), "{}", app.id);
                }
            }
        }
    }

    // The surfaces are compiled in only for a server that runs hosted programs.
    #[cfg(feature = "surfaces")]
    #[test]
    fn every_surface_is_the_one_its_app_ships() {
        let mut checked = 0;
        for (id, _, text) in surfaced() {
            assert!(!text.is_empty(), "{id}");
            assert_eq!(surface(id), Some(text), "{id}");
            checked += 1;
        }
        assert!(checked > 0, "no surface is compiled in");
    }
}
