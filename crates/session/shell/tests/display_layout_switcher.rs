//! Display layout and identification, end to end through the real overlay
//! host over a compositor-published display snapshot: Super+P opens the
//! layout sheet, keys choose a layout, which the sheet makes through the
//! published `display.layout.*` action, and the staged display transaction
//! the compositor applies is read back from the settings file it watches;
//! an Identify request from Settings puts a numbered card on every display.

use std::collections::BTreeMap;

use settings_store::display_identify::IdentifyRequest;
use settings_store::hardware::{
    DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction, DisplayTransactionState,
    LiveOutputPlacement, OutputDescriptor, OutputMode, publisher_identity,
};
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::OutputGeometry;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::WorkspaceTree;
use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Role};
use vello::kurbo::Rect;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::widget::Widget as _;

use crate::support::host::{layout_host, paint, press_key};

fn mode(width: u32, height: u32) -> OutputMode {
    OutputMode {
        width,
        height,
        refresh_mhz: 60_000,
    }
}

fn output(connector: &str, builtin: bool, modes: &[OutputMode]) -> DisplayOutputSnapshot {
    DisplayOutputSnapshot {
        descriptor: OutputDescriptor {
            connector: connector.into(),
            connector_label: connector.into(),
            modes: modes.to_vec(),
            hdr: false,
            vrr: false,
            color_profiles: Vec::new(),
            monitor: None,
        },
        current_mode: modes[0],
        scale: 1.0,
        builtin,
        sink_hdr: false,
        hdr_unavailable_reason: None,
        sink_vrr: false,
        vrr_unavailable_reason: None,
        color_profile_labels: BTreeMap::new(),
    }
}

fn placement(connector: &str, x: i32, width: u32, height: u32) -> LiveOutputPlacement {
    LiveOutputPlacement {
        connector: connector.into(),
        x,
        y: 0,
        width,
        height,
    }
}

/// Publish a laptop panel and an HDMI monitor exactly as the compositor
/// does: the runtime snapshot, stamped with a live publisher identity.
fn publish_laptop_with_monitor() {
    let (publisher_pid, publisher_start_ticks) =
        publisher_identity().expect("hold the publisher lease, as the compositor does");
    DisplayHardwareSnapshot {
        generated_unix_ms: 1,
        backend: "drm".into(),
        publisher_pid,
        publisher_start_ticks,
        outputs: vec![
            output("eDP-1", true, &[mode(2560, 1600), mode(1920, 1080)]),
            output("HDMI-A-1", false, &[mode(3840, 2160), mode(1920, 1080)]),
        ],
        layout: vec![
            placement("eDP-1", 0, 2560, 1600),
            placement("HDMI-A-1", 2560, 3840, 2160),
        ],
    }
    .save()
    .expect("write display snapshot");
    assert!(
        DisplayHardwareSnapshot::load().is_some(),
        "the published snapshot must be trusted, or the test observes nothing"
    );
}

/// Route a pressed logical key through the host dispatcher.
pub(crate) fn press(
    host: &mut ShellOverlayHost,
    layout: &(TaffyTree<()>, NodeId),
    key: Key,
    mods: ModifiersState,
) {
    let what = format!("{key:?}");
    let consumed = press_key(host, &layout.0, layout.1, key, mods);
    assert!(consumed, "{what} must be consumed by the shell");
}

fn super_p(host: &mut ShellOverlayHost, layout: &(TaffyTree<()>, NodeId)) {
    press(
        host,
        layout,
        Key::Character("p".into()),
        ModifiersState::SUPER,
    );
}

fn staged() -> DisplayTransaction {
    shell::shell::durable_writer::global().flush();
    let transaction = DisplayTransaction::load().expect("a display transaction was staged");
    assert_eq!(transaction.state, DisplayTransactionState::Requested);
    transaction
}

fn owner_chrome() -> shell::shell::chrome_state::Chrome {
    owner_host().1
}

/// The real host over a workspace its person owns, so the published
/// display actions are theirs to invoke.
fn owner_host() -> (ShellOverlayHost, shell::shell::chrome_state::Chrome) {
    host_owned_by(None)
}

/// The real host over a workspace `owner` owns; `None` is the person at
/// this machine.
fn host_owned_by(owner: Option<&str>) -> (ShellOverlayHost, shell::shell::chrome_state::Chrome) {
    use shell::shell::state::{FreshId as _, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "Display fixture");
    // The public host keeps the real session registry principal.
    // Give that actual person ownership rather than inventing a fixture actor.
    let person = shell::shell::state::local_user_id_for_room(&workspace.room);
    workspace.set_owner(owner.map_or(person, str::to_owned));
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    build_tilo_overlay_host_with_chrome(&tree)
}

/// Begin the real published layout action; it must retain its original job
/// until the captured request obtains its conditional durability receipt.
fn begin_mirror(
    chrome: &shell::shell::chrome_state::Chrome,
) -> shell::shell::search::PendingInvocation {
    begin_display_action(chrome, "display.layout.mirror")
}

/// The published action `id`, as the actions provider serves it to the
/// control socket, search and the palette.
fn display_action(
    chrome: &shell::shell::chrome_state::Chrome,
    id: &str,
) -> shell::shell::search::SearchResult {
    use shell::shell::search::{ResourceKind, SearchContext, SearchProvider as _};
    let provider = shell::shell::search::providers::actions::ActionsProvider::new();
    let doc = automerge::AutoCommit::new();
    provider
        .query(SearchContext {
            query_scope: None,
            include_programmatic: true,
            needle: id,
            wanted: &[ResourceKind::Action],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
        .into_iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("{id} is a declared owner action"))
}

/// Begin the real published display action `id` through its lifecycle.
fn begin_display_action(
    chrome: &shell::shell::chrome_state::Chrome,
    id: &str,
) -> shell::shell::search::PendingInvocation {
    let mut doc = automerge::AutoCommit::new();
    let row = display_action(chrome, id);
    let disposition = row.on_begin_invoke.as_ref().expect("display lifecycle")(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        chrome,
        &shell::shell::search::action_api::ActionArgs::none(),
    )
    .unwrap();
    let shell::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
        panic!("a queued display action must retain its published job");
    };
    ticket
}

/// The display sheet in the host's semantic tree.
fn display_sheet(host: &ShellOverlayHost) -> AccessNode {
    host.accessibility()
        .expect("the host publishes a semantic tree")
        .children
        .into_iter()
        .find(|node| node.role == Role::Dialog && node.name.as_deref() == Some("Display"))
        .expect("the open display sheet is in the semantic tree")
}

fn highlighted_layout(host: &ShellOverlayHost) -> String {
    display_sheet(host)
        .children
        .into_iter()
        .find(|node| node.state.selected)
        .and_then(|node| node.name)
        .expect("one layout is highlighted")
}

#[test]
fn super_p_arrow_enter_stages_a_mirror_transaction() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let layout = layout_host(&mut host);

    super_p(&mut host, &layout);
    assert!(
        chrome.borrow().overlays.projection,
        "Super+P opens the sheet"
    );
    assert_eq!(
        highlighted_layout(&host),
        "Extend",
        "a fresh laptop-plus-monitor is extended, so the current layout is highlighted"
    );

    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::empty(),
    );
    assert_eq!(highlighted_layout(&host), "Mirror");
    assert!(
        DisplayTransaction::load().is_none(),
        "moving the highlight changes nothing on the displays"
    );
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );

    let transaction = staged();
    let monitor = &transaction.candidate.outputs["HDMI-A-1"];
    let panel = &transaction.candidate.outputs["eDP-1"];
    assert_eq!(monitor.mirror_of.as_deref(), Some("eDP-1"));
    assert!(panel.enabled && monitor.enabled && panel.primary);
    assert_eq!(panel.mode, Some(mode(1920, 1080)));
    assert_eq!(monitor.mode, Some(mode(1920, 1080)));

    // The compositor tick: the host update takes the stored change into the
    // sheet; painting only draws what it took.
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    paint(&mut host);
    let sheet = display_sheet(&host);
    for name in ["Keep", "Revert"] {
        assert!(
            sheet
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some(name)),
            "a staged layout offers {name} until it restores itself"
        );
    }
}

#[test]
fn layout_dispatch_remains_responsive_until_the_request_is_durable() {
    use settings_store::SettingsSource as _;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let paths = settings_store::SessionPaths::configured().unwrap();
    let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
    settings_store::install_source(Some(session.host_view().unwrap()));
    publish_laptop_with_monitor();
    let (mut host, _chrome) = owner_host();
    let layout = layout_host(&mut host);
    super_p(&mut host, &layout);
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::empty(),
    );
    let lock_path = paths.copy.with_file_name("settings.json.lock");
    let (dir, name) = tilo_files::file_at(&lock_path).unwrap();
    let lock = tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive).unwrap();
    let (sent, received) = mpsc::channel();
    let release = std::thread::spawn(move || {
        let responsive = received.recv_timeout(Duration::from_millis(500)).is_ok();
        drop(lock);
        responsive
    });
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );
    let observed_before_release = DisplayTransaction::load();
    let _ = sent.send(());
    let responsive = release.join().unwrap();
    shell::shell::durable_writer::global().flush();
    settings_store::install_source(None);
    let reopened = settings_store::SessionSettings::open(paths).unwrap();
    assert!(
        responsive,
        "layout key dispatch waited for the actual storage lock"
    );
    assert_eq!(
        observed_before_release, None,
        "a queued request cannot become effective before durability"
    );
    assert_eq!(reopened.load()["display_transaction"]["state"], "requested");
    assert_eq!(
        reopened.load()["display_transaction"]["candidate"]["outputs"]["HDMI-A-1"]["mirror_of"],
        "eDP-1"
    );
}

#[test]
fn layout_storage_failure_is_observed_by_the_ordinary_host_update_without_paint() {
    use std::sync::Arc;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let paths = settings_store::SessionPaths::configured().unwrap();
    let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
    settings_store::install_source(Some(session.host_view().unwrap()));
    publish_laptop_with_monitor();
    let (mut host, _) = owner_host();
    let layout = layout_host(&mut host);
    super_p(&mut host, &layout);
    std::fs::create_dir_all(paths.device.parent().unwrap()).unwrap();
    std::fs::create_dir(&paths.device).unwrap();
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );
    let disabled = |host: &ShellOverlayHost| {
        display_sheet(host)
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some("Mirror"))
            .unwrap()
            .state
            .disabled
    };
    assert!(
        disabled(&host),
        "choices wait for the actual storage receipt"
    );
    shell::shell::durable_writer::global().flush();
    assert!(
        host.update(&automerge::AutoCommit::new(), &automerge::ROOT),
        "the normal host update must invalidate on failed completion"
    );
    assert!(
        !disabled(&host),
        "the failed request leaves a reachable retry without forcing a paint"
    );
    assert!(DisplayTransaction::load().is_none());
    std::fs::remove_dir(&paths.device).unwrap();
    settings_store::install_source(None);
}

#[test]
fn published_layout_completes_only_after_its_actual_durable_request() {
    use settings_store::SettingsSource as _;
    use std::sync::Arc;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let paths = settings_store::SessionPaths::configured().unwrap();
    let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
    settings_store::install_source(Some(session.host_view().unwrap()));
    publish_laptop_with_monitor();
    let chrome = owner_chrome();
    let lock_path = paths.copy.with_file_name("settings.json.lock");
    let (dir, name) = tilo_files::file_at(&lock_path).unwrap();
    let lock = tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive).unwrap();
    let ticket = begin_mirror(&chrome);
    assert!(ticket().unwrap().is_none());
    assert!(DisplayTransaction::load().is_none());
    drop(lock);
    shell::shell::durable_writer::global().flush();
    assert!(
        ticket().unwrap().is_some(),
        "published completion needs an actual durable receipt"
    );
    let transaction = DisplayTransaction::load().unwrap();
    settings_store::install_source(None);
    let reopened = settings_store::SessionSettings::open(paths).unwrap();
    assert_eq!(reopened.load()["display_transaction"]["id"], transaction.id);
    assert_eq!(transaction.state, DisplayTransactionState::Requested);
    assert_eq!(
        transaction.candidate.outputs["HDMI-A-1"]
            .mirror_of
            .as_deref(),
        Some("eDP-1")
    );
}

#[test]
fn published_layout_waits_for_durability_and_refuses_external_profile_replacement() {
    use settings_store::SettingsSource as _;
    use std::sync::Arc;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let paths = settings_store::SessionPaths::configured().unwrap();
    let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
    settings_store::install_source(Some(session.host_view().unwrap()));
    publish_laptop_with_monitor();
    let chrome = owner_chrome();
    let lock_path = paths.copy.with_file_name("settings.json.lock");
    let (dir, name) = tilo_files::file_at(&lock_path).unwrap();
    let lock = tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive).unwrap();
    let ticket = begin_mirror(&chrome);
    assert!(
        ticket().unwrap().is_none(),
        "held storage cannot complete a job"
    );
    let mut copy = settings_store::load_at(&paths.copy);
    copy.insert(
        "display_profiles".into(),
        serde_json::json!({"profiles":{},"monitors":{},"external_revision":1}),
    );
    std::fs::write(&paths.copy, serde_json::to_vec_pretty(&copy).unwrap()).unwrap();
    drop(lock);
    shell::shell::durable_writer::global().flush();
    let result = ticket();
    settings_store::install_source(None);
    let reopened = settings_store::SessionSettings::open(paths).unwrap();
    assert!(matches!(
        result,
        Err(shell::shell::search::action_api::ActionError::Refused(_))
    ));
    assert!(
        !reopened.load().contains_key("display_transaction"),
        "a stale layout must never become effective"
    );
    assert_eq!(reopened.load()["display_profiles"]["external_revision"], 1);
}

#[test]
fn published_layout_reports_actual_record_failure_without_publishing_intent() {
    use std::sync::Arc;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let paths = settings_store::SessionPaths::configured().unwrap();
    let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
    settings_store::install_source(Some(session.host_view().unwrap()));
    publish_laptop_with_monitor();
    let chrome = owner_chrome();
    assert!(
        !paths.device.exists(),
        "a fresh session has no device edits yet"
    );
    std::fs::create_dir_all(paths.device.parent().unwrap()).unwrap();
    std::fs::create_dir(&paths.device).unwrap();
    let ticket = begin_mirror(&chrome);
    shell::shell::durable_writer::global().flush();
    let result = ticket();
    let document = settings_store::load();
    std::fs::remove_dir(&paths.device).unwrap();
    settings_store::install_source(None);
    assert!(matches!(
        result,
        Err(shell::shell::search::action_api::ActionError::Refused(_))
    ));
    assert!(!document.contains_key("display_transaction"));
}

#[test]
fn super_p_again_cycles_and_enter_applies_the_highlighted_layout() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let layout = layout_host(&mut host);

    super_p(&mut host, &layout);
    let mut cycle = Vec::new();
    for _ in 0..4 {
        super_p(&mut host, &layout);
        assert!(
            chrome.borrow().overlays.projection,
            "Super+P while open cycles instead of closing"
        );
        cycle.push(highlighted_layout(&host));
    }
    assert_eq!(
        cycle,
        ["Mirror", "Only this screen", "Only external", "Extend"]
    );
    assert!(
        DisplayTransaction::load().is_none(),
        "cycling alone stages nothing"
    );

    super_p(&mut host, &layout);
    super_p(&mut host, &layout);
    assert_eq!(highlighted_layout(&host), "Only this screen");
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );

    let transaction = staged();
    assert!(transaction.candidate.outputs["eDP-1"].enabled);
    assert!(!transaction.candidate.outputs["HDMI-A-1"].enabled);
}

/// The sheet is a caller of the published layout action, not a second door
/// beside it: where the workspace does not let this person change the
/// displays, Super+P, Mirror, Enter is refused exactly as the action is,
/// stages nothing, and says why.
#[test]
fn a_sheet_choice_is_refused_where_the_published_action_is() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = host_owned_by(Some("foreign-owner"));
    let layout = layout_host(&mut host);
    let mut doc = automerge::AutoCommit::new();
    assert!(
        display_action(&chrome, "display.layout.mirror")
            .on_begin_invoke
            .as_ref()
            .expect("display lifecycle")(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &shell::shell::search::action_api::ActionArgs::none(),
        )
        .is_err(),
        "the published action is refused to a person this workspace does not let change displays"
    );

    super_p(&mut host, &layout);
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::empty(),
    );
    assert_eq!(highlighted_layout(&host), "Mirror");
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );
    shell::shell::durable_writer::global().flush();
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    assert!(
        DisplayTransaction::load().is_none(),
        "the sheet stages nothing the action would refuse"
    );
    let description = display_sheet(&host).description.unwrap_or_default();
    assert!(
        description.contains("no longer permits"),
        "the sheet says why the choice was refused: {description}"
    );
}

/// The open sheet takes the compositor's displays from what it published in
/// memory: painting it, again and again, reads no file at all, and it still
/// shows the displays once their runtime file is gone.
#[test]
fn painting_the_open_sheet_reads_no_storage() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let layout = layout_host(&mut host);
    super_p(&mut host, &layout);
    // The compositor tick takes the open-time state; it may read settings.
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    std::fs::remove_file(settings_store::hardware::display_snapshot_path().unwrap())
        .expect("the published snapshot file");
    let before = settings_store::published::file_reads_on_this_thread();
    for _ in 0..10 {
        paint(&mut host);
    }
    assert_eq!(
        settings_store::published::file_reads_on_this_thread(),
        before,
        "painting the open display sheet must not read settings or runtime files"
    );
    assert!(chrome.borrow().overlays.projection);
    let sheet = display_sheet(&host);
    let mirror = sheet
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Mirror"))
        .expect("the Mirror layout row");
    assert!(
        !mirror.state.disabled,
        "two published displays can be mirrored, though their file is gone"
    );
}

#[test]
fn identify_puts_a_numbered_card_on_every_display() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo());
    let outputs = [
        OutputGeometry::unscaled(0.0, 0.0, 2560.0, 1600.0),
        OutputGeometry::unscaled(2560.0, 0.0, 3840.0, 2160.0),
    ];
    host.set_output_geometry(&outputs);
    let _layout = layout_host(&mut host);
    assert!(host.display_identify_labels().is_empty());
    assert!(!host.poll_display_identify(), "nothing was asked yet");

    IdentifyRequest::request().expect("Settings writes the request");
    assert!(host.poll_display_identify());
    let labels = host.display_identify_labels();
    assert_eq!(labels.len(), outputs.len(), "one card per display");
    for (label, output) in labels.iter().zip(&outputs) {
        assert_eq!(
            label.output,
            Rect::new(output.x, 0.0, output.x + output.width, output.height),
            "each card is centred on its own display"
        );
    }
    let named: Vec<(usize, &str)> = labels
        .iter()
        .map(|label| (label.number, label.name.as_str()))
        .collect();
    assert_eq!(named, [(1, "Built-in display"), (2, "HDMI-A-1")]);
    assert!(
        !host.poll_display_identify(),
        "one request shows the numbers once"
    );
}

/// Run an action's job to its durable receipt, as the control socket does.
fn complete(ticket: &shell::shell::search::PendingInvocation) {
    shell::shell::durable_writer::global().flush();
    assert!(
        ticket().expect("the display action succeeds").is_some(),
        "a flushed request has its receipt"
    );
}

/// Write the transaction's next state as the compositor does when it
/// applies, confirms, reverts or refuses a staged layout.
fn compositor_moves_to(state: DisplayTransactionState) {
    let mut transaction = DisplayTransaction::load().expect("a staged display change");
    transaction.state = state;
    if state == DisplayTransactionState::AwaitingConfirmation {
        transaction.deadline_unix_ms = Some(u64::MAX);
    }
    transaction
        .save()
        .expect("the compositor records the change");
}

fn sheet_open(chrome: &shell::shell::chrome_state::Chrome) -> bool {
    chrome.borrow().overlays.projection
}

/// Drive the ordinary host update, as the compositor tick does, until the
/// sheet closes or `window` (several of its 250 ms storage polls) passes.
fn updates_close_the_sheet_within(
    host: &mut ShellOverlayHost,
    chrome: &shell::shell::chrome_state::Chrome,
    window: std::time::Duration,
) -> bool {
    let doc = automerge::AutoCommit::new();
    let started = std::time::Instant::now();
    while started.elapsed() < window {
        host.update(&doc, &automerge::ROOT);
        if !sheet_open(chrome) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    false
}

/// The sheet closes within a few storage polls.
fn updates_close_the_sheet(
    host: &mut ShellOverlayHost,
    chrome: &shell::shell::chrome_state::Chrome,
) -> bool {
    updates_close_the_sheet_within(host, chrome, std::time::Duration::from_secs(2))
}

/// The sheet is still open after three storage polls.
fn updates_keep_the_sheet(
    host: &mut ShellOverlayHost,
    chrome: &shell::shell::chrome_state::Chrome,
) -> bool {
    !updates_close_the_sheet_within(host, chrome, std::time::Duration::from_millis(800))
}

fn assert_sheet_gone(host: &ShellOverlayHost, chrome: &shell::shell::chrome_state::Chrome) {
    assert!(!sheet_open(chrome), "the display sheet closed");
    assert!(
        !host
            .accessibility()
            .expect("the host publishes a semantic tree")
            .children
            .iter()
            .any(|node| node.role == Role::Dialog && node.name.as_deref() == Some("Display")),
        "no display dialog is left to take the keyboard"
    );
}

/// An agent mirrors the displays through the action door: the sheet shows
/// the change with its Keep/Revert countdown while it waits.
fn mirror_through_the_action(
    host: &mut ShellOverlayHost,
    chrome: &shell::shell::chrome_state::Chrome,
) {
    let ticket = begin_mirror(chrome);
    assert!(sheet_open(chrome), "the layout action shows its change");
    complete(&ticket);
    compositor_moves_to(DisplayTransactionState::AwaitingConfirmation);
    assert!(
        updates_keep_the_sheet(host, chrome),
        "a change still waiting for Keep or Revert keeps its sheet"
    );
}

#[test]
fn an_action_opened_sheet_closes_when_the_revert_action_answers_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let _layout = layout_host(&mut host);
    mirror_through_the_action(&mut host, &chrome);

    complete(&begin_display_action(&chrome, "display.change.revert"));
    assert_eq!(
        DisplayTransaction::load().unwrap().state,
        DisplayTransactionState::RevertRequested
    );
    assert!(
        updates_close_the_sheet(&mut host, &chrome),
        "an answered change leaves no modal sheet behind"
    );
    assert_sheet_gone(&host, &chrome);
}

#[test]
fn an_action_opened_sheet_closes_once_the_kept_change_is_confirmed() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let _layout = layout_host(&mut host);
    mirror_through_the_action(&mut host, &chrome);

    complete(&begin_display_action(&chrome, "display.change.keep"));
    compositor_moves_to(DisplayTransactionState::Confirmed);
    assert!(updates_close_the_sheet(&mut host, &chrome));
    assert_sheet_gone(&host, &chrome);
}

#[test]
fn an_action_opened_sheet_closes_when_its_change_times_out_or_is_refused() {
    for settled in [
        DisplayTransactionState::Reverted,
        DisplayTransactionState::Rejected,
    ] {
        let _home = tilo_test_env::TestHome::new().unwrap();
        publish_laptop_with_monitor();
        let (mut host, chrome) = owner_host();
        let _layout = layout_host(&mut host);
        mirror_through_the_action(&mut host, &chrome);

        compositor_moves_to(settled);
        assert!(
            updates_close_the_sheet(&mut host, &chrome),
            "a change that is {settled:?} with nobody answering closes its sheet"
        );
        assert_sheet_gone(&host, &chrome);
    }
}

#[test]
fn an_action_opened_sheet_closes_at_once_when_storage_refuses_the_layout() {
    use std::sync::Arc;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let paths = settings_store::SessionPaths::configured().unwrap();
    let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
    settings_store::install_source(Some(session.host_view().unwrap()));
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let _layout = layout_host(&mut host);
    std::fs::create_dir_all(paths.device.parent().unwrap()).unwrap();
    std::fs::create_dir(&paths.device).unwrap();
    let ticket = begin_mirror(&chrome);
    assert!(sheet_open(&chrome));
    shell::shell::durable_writer::global().flush();
    let result = ticket();
    std::fs::remove_dir(&paths.device).unwrap();
    settings_store::install_source(None);
    assert!(matches!(
        result,
        Err(shell::shell::search::action_api::ActionError::Refused(_))
    ));
    assert_sheet_gone(&host, &chrome);
}

#[test]
fn an_action_opened_sheet_closes_by_escape_and_by_the_close_action() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let layout = layout_host(&mut host);
    mirror_through_the_action(&mut host, &chrome);
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Escape),
        ModifiersState::empty(),
    );
    assert_sheet_gone(&host, &chrome);

    compositor_moves_to(DisplayTransactionState::Reverted);
    mirror_through_the_action(&mut host, &chrome);
    let mut doc = automerge::AutoCommit::new();
    (display_action(&chrome, "projection.close").on_open)(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &chrome,
        &shell::shell::search::action_api::ActionArgs::none(),
    )
    .expect("closing the display sheet is an action, not only a click");
    assert_sheet_gone(&host, &chrome);
}

#[test]
fn a_sheet_the_person_opened_or_used_stays_when_its_change_settles() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = owner_host();
    let layout = layout_host(&mut host);

    // Super+P, Mirror, Enter: the person's own picker.
    super_p(&mut host, &layout);
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::empty(),
    );
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );
    staged();
    compositor_moves_to(DisplayTransactionState::AwaitingConfirmation);
    compositor_moves_to(DisplayTransactionState::Reverted);
    assert!(
        updates_keep_the_sheet(&mut host, &chrome),
        "a picker the person opened stays until they close it"
    );
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Escape),
        ModifiersState::empty(),
    );
    assert!(!sheet_open(&chrome));

    // An action opens it, then the person takes it over with a key.
    mirror_through_the_action(&mut host, &chrome);
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::ArrowUp),
        ModifiersState::empty(),
    );
    compositor_moves_to(DisplayTransactionState::Reverted);
    assert!(
        updates_keep_the_sheet(&mut host, &chrome),
        "a sheet the person used is theirs to close"
    );
}
