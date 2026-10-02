#![allow(
    clippy::cast_lossless,
    clippy::cloned_ref_to_slice_refs,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::double_ended_iterator_last,
    clippy::drop_non_drop,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::ignored_unit_patterns,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_clamp,
    clippy::manual_let_else,
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_const_for_thread_local,
    clippy::missing_errors_doc,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_lifetimes,
    clippy::needless_option_as_deref,
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    clippy::nonminimal_bool,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::suspicious_operation_groupings,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_mut_passed,
    clippy::unnecessary_semicolon,
    clippy::unnecessary_wraps,
    clippy::unreachable,
    clippy::unreadable_literal,
    clippy::unused_io_amount,
    clippy::unused_self,
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    clippy::wildcard_imports,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Disk Imager — Tilo native app for writing an OS image to a drive, and for
//! installing Tilo (CRDT-native, built on the `ui` toolkit).
//!
//! A single guided **wizard** with a six-station step indicator, covering both
//! "write this image to a USB stick" and "install Tilo onto a disk":
//!
//!   1. **Source** — pick a local `.iso` / `.img` via the platform's native
//!      file picker, or a known Tilo release.
//!   2. **Drive** — choose a target from real platform block-device discovery;
//!      internal disks are hidden behind an explicit opt-in, the **system disk**
//!      needs a second acknowledgement before it can even be selected, and an
//!      honest empty state asks the user to insert a drive.
//!   3. **Customize** (Tilo only) — the guided-customization step (hostname,
//!      first user and region), shared by the write and install flows. Plain
//!      ISOs skip it. Only the secret-free settings the live installer applies
//!      are offered; encryption and login secrets stay on the target device.
//!   4. **Confirm** — a read-only review of every choice, an erase warning that
//!      escalates for an internal / system disk, and the typed device-name gate.
//!   5. **Write / Install** — a real, byte-for-byte streaming copy with live
//!      progress, then a read-back **verify** pass. (The caption reads "Write"
//!      for a removable drive, "Install" for a disk install.)
//!   6. **Done** — the verified / failed outcome, with a fresh-start affordance.
//!
//! Writing is doubly gated: the typed confirmation, then platform-native
//! authentication for a short-lived helper. The wizard, downloads, and
//! customization remain in the user's unprivileged session.
//!
//! `app_root()` is the live integration point; `app_root_for_shot()` stages
//! only peer-local capture state before returning the same widget. The live binary sets
//! `IMAGER_LIVE=1` so the drive list comes from a real `lsblk` probe and the
//! app starts genuinely blank; `--shots` / `--demo` seed sample content so the
//! screenshot is deterministic offline. Immediately before writing, the live
//! inventory is read again and its stable device identity, size, and system-
//! disk status must still match the confirmed target.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::panic,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod customize;
pub mod doc;
mod export_dialog;
pub mod handoff;
mod host;
pub mod model;
pub mod probe;
pub mod profile;
pub mod profile_cli;
pub mod release;
pub mod resource;
pub mod setup;
pub mod write;
pub mod write_setup;

/// The imager's spacing rhythm (HIG §7b): the same relationship always gets
/// the same step, and every step is a token of `Spacing::BASE` — the
/// compile-time reference the wizard's immediate-mode hit layout and the
/// setup pages' retained compositions both need, because neither receives a
/// `Theme` when its geometry is declared. Declared once here; every view
/// imports the steps it uses instead of restating them.
pub(crate) mod rhythm {
    use ui::tokens::Spacing;
    const S: Spacing = Spacing::BASE;
    /// A label and its control; an icon and its label; two lines of one block.
    pub const LABEL: f64 = S.s1 as f64;
    /// Related rows that read as one group: a title and its explanation, a
    /// section caption and its list, help copy under a field.
    pub const RELATED: f64 = S.s2 as f64;
    /// Siblings of equal weight: list rows, cards in a list, the two footer
    /// actions, a card and the banner that follows it.
    pub const SIBLING: f64 = S.s3 as f64;
    /// Section padding: the inset inside a card or row; the compact page gutter.
    pub const SECTION: f64 = S.s4 as f64;
    /// Before the next group; the page gutter.
    pub const GROUP: f64 = S.s6 as f64;
    /// Between major regions: the step rail and the step body, the progress
    /// ring and its bars, the hero disc and its headline.
    pub const REGION: f64 = S.s8 as f64;
    /// The Done hero's stand-off from the content top.
    pub const HERO: f64 = S.s16 as f64;

    /// The same steps as `f32`, for the retained setup pages whose taffy
    /// styles and `Column` / `Row` gaps take `f32`.
    pub mod px {
        /// A title and its explanation.
        pub const RELATED: f32 = super::RELATED as f32;
        /// Siblings: the cards of one section, the page's own bands.
        pub const SIBLING: f32 = super::SIBLING as f32;
        /// Before the next group of the page; between the saved folders of
        /// one card.
        pub const GROUP: f32 = super::GROUP as f32;
        /// The page gutter, all four sides.
        pub const PAGE_GUTTER: f32 = super::GROUP as f32;
        /// The scrollbar's footprint beside the content.
        pub const SCROLLBAR_GUTTER: f32 = super::SECTION as f32;
        /// A sticky footer's clean edge inside the scroll viewport.
        pub const FOOTER_INSET: f32 = super::LABEL as f32;
    }
}

/// Below a 520 px content width — scaled by the text scale, so a 1000 px
/// column at 2× (the room of a 500 px one) counts as compact too — the
/// wizard's steps and the customization panel take their compact form. One
/// decision for the chrome, the heading floor and the form's column split, so
/// no width exists where one is compact and the other is not.
pub(crate) fn compact_content(width: f64, text_scale: f64) -> bool {
    width < 520.0 * text_scale
}

pub use app::ImagerApp;
pub use customize::CustomizePanel;
pub use model::{
    Action, Filesystem, InstallMode, InstallProfile, Source, SourceKind, Step, TargetDrive,
    TargetKind,
};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImagerApp::new())
}

/// Build the same widget as [`app_root`], with only peer-local shot state
/// staged outside the collaborative document.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    if scenario.starts_with("import-") {
        return Box::new(setup::import_view::ImportApp::for_shot(scenario));
    }
    if scenario.starts_with("setup-") {
        return Box::new(setup::view::SetupApp::for_shot(scenario));
    }
    let mut app = ImagerApp::new();
    app.seed_local_shot_state(scenario);
    if scenario == "confirm" {
        app.seed_confirmation_for_shot("sdb");
    } else if scenario == "export" {
        app.seed_export_for_shot();
    } else if scenario == "customize-timezone-open" {
        app.shot_open_customize_timezone();
    }
    Box::new(app)
}

/// The ordered `--shots` scenarios: one per meaningful wizard station, named so
/// the offscreen renderer can walk them and the gallery reads top-to-bottom.
pub const SHOT_SCENARIOS: &[&str] = &[
    "source",
    "source-empty",
    "drive",
    "drive-empty",
    "drive-internal",
    "customize",
    "customize-timezone-open",
    "export",
    "confirm",
    "progress",
    "done",
    "done-failed",
    "setup-experience-checking",
    "setup-experience",
    "setup-experience-failed",
    "setup-folders-copy",
    "setup-folders-connected",
    "setup-folders-later",
    "setup-review",
    "setup-desktop-installed",
    "setup-full-review",
    "setup-full-restart",
    "import-first-login",
    "import-first-login-waiting",
    "import-first-login-later",
    "import-progress",
    "import-retry-progress",
    "import-partial",
    "import-conflict",
    "import-complete",
    "import-issue-pages",
];

/// Pose the document for one named `--shots` scenario. Sample content is seeded
/// here via `ImagerDoc::seed_demo` — never by the live app — so a screenshot
/// is the only place this state is set. Unknown names fall back to the source
/// step.
pub fn seed_scenario(doc: &mut automerge::AutoCommit, scenario: &str) {
    use crate::doc::ImagerDoc;

    // The empty-drive scenario is the one case with no sample inventory: seed an
    // empty document so the honest "insert a drive" state renders.
    if scenario == "drive-empty" {
        seed_scenario_drive_empty(doc);
        return;
    }
    // The Source step as the live app actually opens it: nothing browsed yet,
    // no release known to be on this machine. This is the state a real run
    // starts in, so it is the one worth looking at.
    if scenario == "source-empty" {
        seed_scenario_source_empty(doc);
        return;
    }
    // Internal-disks-revealed: a USB stick plus the *system* disk, shown with
    // the guard active (the system disk locked, both acknowledgements visible).
    if scenario == "drive-internal" {
        seed_scenario_drive_internal(doc);
        return;
    }

    // Every other scenario builds on the demo document (sample drives + a Tilo
    // image + recent history), then poses the wizard station on top.
    ImagerDoc::seed_demo(doc);
    seed_scenario_common(doc, scenario);
}

/// The ISO source shared by the `drive-empty` and `drive` scenarios.
fn demo_iso_source() -> crate::model::Source {
    use crate::model::{Source, SourceKind};
    Source {
        kind: SourceKind::IsoFile,
        path: "/home/quinten/Downloads/Fedora-Workstation-Live-42.iso".into(),
        name: "Fedora-Workstation-Live-42.iso".into(),
        size: 2_113_929_216,
        label: "Fedora-WS-Live-42-1-1".into(),
        installable: false,
        identity: None,
        expected_sha256: None,
    }
}

fn select_source(doc: &mut automerge::AutoCommit, source: crate::model::Source) {
    let _ = ui::tilo_actions::Action::apply(
        &crate::actions::ImagerAction::SelectSource {
            source: Box::new(source),
            previous: None,
            recent_rank: 0,
        },
        doc,
    );
}

fn ensure_empty_records(doc: &mut automerge::AutoCommit) {
    let _ = ui::tilo_actions::Action::apply(&crate::actions::ImagerAction::EnsureRecords, doc);
}

fn seed_scenario_drive_empty(doc: &mut automerge::AutoCommit) {
    ensure_empty_records(doc);
    select_source(doc, demo_iso_source());
}

fn seed_scenario_source_empty(doc: &mut automerge::AutoCommit) {
    ensure_empty_records(doc);
}

fn seed_scenario_drive_internal(doc: &mut automerge::AutoCommit) {
    use crate::model::sample_source;
    ensure_empty_records(doc);
    select_source(doc, sample_source());
}

/// Pose the "drive", "customize", "confirm", "progress", "done" and
/// "done-failed" stations (plus the "source"/unknown default) on top of the
/// already-seeded demo document.
fn seed_scenario_common(doc: &mut automerge::AutoCommit, scenario: &str) {
    if scenario == "drive" {
        select_source(doc, demo_iso_source());
    }
}
pub use resource::{Imager, ImagerShell};

#[cfg(test)]
mod tests {
    #[test]
    fn drive_scenario_selects_the_iso_through_the_source_action() {
        let mut doc = ui::automerge::AutoCommit::new();
        super::seed_scenario(&mut doc, "drive-empty");
        assert!(
            !crate::doc::ImagerDoc::is_empty(&doc),
            "the scenario record is created by EnsureRecords"
        );
        let source = crate::doc::ImagerDoc::source(&doc).expect("iso selected");
        assert!(source.name.contains("Fedora"));
    }
}
