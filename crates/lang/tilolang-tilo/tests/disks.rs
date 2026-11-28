//! Disks end to end: this computer's drives over the device's `disks` reading, its map, its
//! health, its partitions and the verbs that open, erase, measure and eject them.

use crate::fixtures::Pane;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};

const SOURCE: &str = include_str!("../../../../apps/tl/disks/app.tl");
const APP: &str = "so.tilo.disks";

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading() -> ResourceRef {
    ResourceRef::parse_id("disks", "so.tilo.device#Disks").expect("a resource id")
}

/// What the device publishes: one healthy system drive with two partitions, and a failing
/// external drive without a partition table.
const READING: &str = r#"{
  "serving": true,
  "read_error": null,
  "drives": [
    {"name": "nvme0n1", "model": "Samsung SSD", "serial": "S1", "transport": "nvme",
     "kind": "disk", "size_bytes": 1000000000000, "rotational": false, "removable": false,
     "read_only": false, "fstype": null, "mountpoint": null,
     "partitions": [
       {"name": "nvme0n1p1", "label": "boot", "fstype": "vfat", "part_type_name": "EFI System",
        "size_bytes": 500000000, "used_bytes": 200000000, "mountpoint": "/boot/efi", "uuid": "u1"},
       {"name": "nvme0n1p2", "label": "root", "fstype": "btrfs",
        "part_type_name": "Linux filesystem", "size_bytes": 800000000000,
        "used_bytes": 400000000000, "mountpoint": "/", "uuid": "u2"},
       {"name": "nvme0n1p3", "label": null, "fstype": null,
        "part_type_name": "Linux filesystem", "size_bytes": 50000000000,
        "used_bytes": null, "mountpoint": null, "uuid": "u3",
        "encryption": {"scheme": "LUKS2", "unlocked_as": null}},
       {"name": "nvme0n1p4", "label": "home", "fstype": "ext4",
        "part_type_name": "Linux filesystem", "size_bytes": 50000000000,
        "used_bytes": 10000000000, "mountpoint": "/home", "uuid": "u4",
        "encryption": {"scheme": "LUKS2", "unlocked_as": "dm-1"}}
     ],
     "health": {"supported": true, "failing": false, "warnings": [], "temperature_c": 40,
                "power_on_hours": 1200, "power_cycles": 300, "percent_used": 4,
                "available_spare": 100, "bytes_written": 9000000000, "bytes_read": 12000000000,
                "media_errors": 0, "bad_sectors": 0, "self_test": "success",
                "attributes": [
                  {"number": 5, "name": "Reallocated_Sector_Ct", "value": 100,
                   "worst": 100, "threshold": 10, "raw": "0"},
                  {"number": 194, "name": "Temperature_Celsius", "value": 60,
                   "worst": 40, "threshold": 0, "raw": "40"}
                ]}},
    {"name": "sda", "model": "USB Disk", "serial": "S2", "transport": "usb", "kind": "disk",
     "size_bytes": 64000000000, "rotational": true, "removable": true, "read_only": false,
     "fstype": "exfat", "mountpoint": null, "partitions": [],
     "health": {"supported": true, "failing": true, "warnings": ["temperature"],
                "temperature_c": 60, "power_on_hours": 9000, "power_cycles": 20,
                "percent_used": 90, "available_spare": 5, "bytes_written": 1000000000,
                "bytes_read": 2000000000, "media_errors": 3, "bad_sectors": 2,
                "self_test": "failed"}}
  ],
  "benchmark": null,
  "pending": [],
  "problem": null
}"#;

/// A machine whose drives have not been read yet.
const UNREAD: &str = r#"{
  "serving": false,
  "read_error": null,
  "drives": [],
  "benchmark": null,
  "pending": [],
  "problem": null
}"#;

/// The window with its view state staged, its rows drawn and its patches answered.
fn window_showing(app: &Pane, states: &[(&str, &str)]) -> ViewInstance {
    for (state, value) in states {
        app.host
            .runtime()
            .stage_view_state(APP, state, value)
            .expect("the window's state");
    }
    let mut window = app.window();
    window
        .set_lazy_rows(false)
        .expect("the window draws its rows");
    window.poll_patches().expect("the window answers");
    window
}

fn installed() -> Pane {
    Pane::install(&me(), SOURCE)
        .delegating()
        .reading(&reading(), READING)
        .rooted_of(APP)
}

/// Every text under `node` that the window shows, in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for prop in [
        "value",
        "title",
        "label",
        "message",
        "action",
        "description",
        "secondary",
        "meta",
        "caption",
    ] {
        if let Some(Value::Text(text)) = node.props.get(prop) {
            into.push(text.clone());
        }
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    out
}

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

#[test]
fn a_reading_that_has_not_arrived_says_it_is_loading() {
    let app = Pane::install(&me(), SOURCE)
        .delegating()
        .reading(&reading(), UNREAD)
        .rooted_of(APP);
    let window = app.window();
    assert!(has(&window, "Loading"), "{:?}", shown(&window));
    assert!(
        !has(&window, "No drives on this device"),
        "an unread machine is not called empty: {:?}",
        shown(&window)
    );
}

#[test]
fn the_drive_list_names_the_drives_and_their_health() {
    let app = installed();
    let window = app.window();
    assert!(contains(&window, "Samsung SSD"));
    assert!(contains(&window, "USB Disk"));
    assert!(contains(&window, "Predicted to fail"));
}

#[test]
fn a_chosen_drive_shows_its_partitions_and_unallocated_space() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1")]);
    for wanted in [
        "How it is divided",
        "nvme0n1p1 boot",
        "Unallocated",
        "Open at /",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn the_health_tab_shows_what_the_drive_reports() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "health")]);
    for wanted in [
        "Verdict",
        "Temperature",
        "Life used",
        "Media errors",
        "Last self-test",
    ] {
        assert!(
            has(&window, wanted) || contains(&window, wanted),
            "{wanted} shows"
        );
    }
}

#[test]
fn the_health_tab_shows_the_drives_smart_attributes() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "health")]);
    let words = shown(&window);
    assert!(
        !of_kind(window.tree(), "ui.DataTable").is_empty(),
        "the attributes are a table: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "Reallocated_Sector_Ct"),
        "each attribute is a row: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "194") && words.iter().any(|text| text == "40"),
        "a row shows its number and raw value: {words:?}"
    );
    // A drive that reports no table shows no table.
    let usb = window_showing(&app, &[("chosen", "sda"), ("tab", "health")]);
    assert!(
        of_kind(usb.tree(), "ui.DataTable").is_empty(),
        "a drive with no attributes has no table: {:?}",
        shown(&usb)
    );
}

#[test]
fn the_partitions_tab_offers_each_partition_its_verbs() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "partitions")]);
    let words = shown(&window);
    for wanted in ["nvme0n1p1", "nvme0n1p2", "Close", "Erase…"] {
        assert!(
            words.iter().any(|text| text.contains(wanted)),
            "{wanted} shows: {words:?}"
        );
    }
}

#[test]
fn the_whole_drive_is_one_choice_of_its_own() {
    let app = installed();
    let mut window = window_showing(
        &app,
        &[
            ("chosen", "nvme0n1"),
            ("tab", "overview"),
            ("chosen_part", "nvme0n1p4"),
        ],
    );
    let chip = of_kind(window.tree(), "ui.Chip")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text("The whole drive")))
        .expect("the whole drive is a choice of its own");
    window
        .fire(&chip.key, "press")
        .expect("the whole drive is chosen");
    assert_eq!(window.view_state("chosen_part"), Some(Value::text("")));
    window.poll_patches().expect("the window answers");
    assert!(
        contains(&window, "Serial number"),
        "the drive's own details show: {:?}",
        shown(&window)
    );
}

#[test]
fn a_drive_is_named_in_the_workspace() {
    let app = installed();
    // The buttons are the reading's verbs; each names the device it is called on.
    let id = app.action_id("name_drive").to_owned();
    app.host
        .runtime()
        .invoke_action(
            &me(),
            (
                id.as_str(),
                vec![app.root_value(), Value::text("S1"), Value::text("System")],
            ),
        )
        .expect("the rename runs");
    let window = app.window();
    assert!(contains(&window, "System"), "{:?}", shown(&window));
}

#[test]
fn a_blank_name_forgets_the_drive_and_a_padded_one_is_refused() {
    let app = installed();
    app.call(
        "name_drive",
        vec![app.root_value(), Value::text("S2"), Value::text("Stick")],
    );
    assert!(contains(&app.window(), "Stick"), "a name shows on the row");
    app.call(
        "name_drive",
        vec![app.root_value(), Value::text("S2"), Value::text("  ")],
    );
    assert!(
        !contains(&app.window(), "Stick"),
        "a blank name forgets it: {:?}",
        shown(&app.window())
    );
    let refused = app
        .try_call(
            "name_drive",
            vec![app.root_value(), Value::text("S2"), Value::text(" Stick ")],
        )
        .unwrap_err();
    assert!(refused.contains("not blank and not padded"), "{refused}");
}

#[test]
fn the_details_name_the_drive_and_what_one_partition_is() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "overview")]);
    let words = shown(&window);
    for wanted in [
        "Details",
        "/dev/nvme0n1",
        "NVMe solid state drive",
        "Capacity",
        "Interface",
        "NVMe",
        "Serial number",
        "S1",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    let chosen = window_showing(
        &app,
        &[
            ("chosen", "nvme0n1"),
            ("tab", "overview"),
            ("chosen_part", "nvme0n1p4"),
        ],
    );
    let words = shown(&chosen);
    for wanted in [
        "/dev/nvme0n1p4",
        "ext4",
        "LUKS2, unlocked",
        "/home",
        "Home folders",
        "Used",
        "Free",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows for the chosen partition: {words:?}"
        );
    }
    assert!(
        words.iter().any(|text| text.contains("10 GB")),
        "the partition's used and free sizes show: {words:?}"
    );
}

#[test]
fn the_filesystems_say_what_is_mounted_and_what_is_left() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "overview")]);
    let words = shown(&window);
    assert!(words.iter().any(|text| text == "Filesystems"), "{words:?}");
    for wanted in ["vfat at /boot/efi", "btrfs at /", "ext4 at /home"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    assert!(
        words.iter().any(|text| text.ends_with(" free")),
        "what is left shows: {words:?}"
    );
    let none = window_showing(&app, &[("chosen", "sda"), ("tab", "overview")]);
    assert!(
        has(&none, "No filesystems are mounted."),
        "a drive with none says so: {:?}",
        shown(&none)
    );
}

/// The button labelled `label`, where it stands in `tree`.
fn button<'a>(tree: &'a UiNode, label: &str) -> &'a UiNode {
    of_kind(tree, "ui.Button")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("no {label} button"))
}

/// Whether a control in `tree` stands refused with `reason`.
fn refused_with(tree: &UiNode, reason: &str) -> bool {
    of_kind(tree, "ui.Button").into_iter().any(|node| {
        node.props.get("enabled") == Some(&Value::Bool(false))
            && node.props.get("description") == Some(&Value::text(reason))
    })
}

#[test]
fn the_running_system_refuses_to_be_erased_or_closed() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "overview")]);
    let erase = button(window.tree(), "Erase…");
    assert_eq!(
        erase.props.get("enabled"),
        Some(&Value::Bool(false)),
        "the whole system drive cannot be erased: {:?}",
        erase.props
    );
    assert_eq!(
        erase.props.get("description"),
        Some(&Value::text("Holds the running system"))
    );
    // A removable drive with nothing of the system on it is erased.
    let usb = window_showing(&app, &[("chosen", "sda"), ("tab", "overview")]);
    assert_eq!(
        button(usb.tree(), "Erase…").props.get("enabled"),
        Some(&Value::Bool(true)),
        "the external drive erases: {:?}",
        shown(&usb)
    );
    // The home partition keeps its files; a locked encrypted one keeps its encryption; the
    // root keeps the running system.
    let parts = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "partitions")]);
    assert!(
        refused_with(parts.tree(), "Holds the home folders"),
        "closing and erasing the home partition are refused: {:?}",
        shown(&parts)
    );
    assert!(
        refused_with(parts.tree(), "Formatting would remove its encryption"),
        "erasing a locked encrypted partition would remove its encryption: {:?}",
        shown(&parts)
    );
    assert!(
        of_kind(parts.tree(), "ui.Button")
            .into_iter()
            .filter(|node| {
                node.props.get("description") == Some(&Value::text("Holds the running system"))
            })
            .count()
            >= 2,
        "the root partition's Close and Erase are both refused: {:?}",
        shown(&parts)
    );
}

#[test]
fn erasing_says_what_it_erases() {
    let app = installed();
    // A partition with files on it says what would be lost, and names the partition.
    let mut window = window_showing(
        &app,
        &[
            ("chosen", "nvme0n1"),
            ("tab", "overview"),
            ("chosen_part", "nvme0n1p1"),
        ],
    );
    let erase = button(window.tree(), "Erase…");
    window.fire(&erase.key, "press").expect("the erase asks");
    window.poll_patches().expect("the window answers");
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text == "The 200 MB of files on “boot” are erased for good."),
        "the question names what it erases: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "Erase boot?"),
        "the question names the partition: {words:?}"
    );
}

#[test]
fn erasing_asks_for_the_filesystem_and_then_asks_the_device() {
    let app = installed();
    // The confirmation calls `format_device`, which the window's Erase answers with.
    app.call(
        "format_device",
        vec![Value::text("sda"), Value::text("ext4")],
    );
    let asked = app.sent.asked("format");
    assert_eq!(asked[1], Value::text("sda"));
    assert_eq!(asked[2], Value::text("ext4"));
}

#[test]
fn a_failing_drive_says_so_and_offers_its_eject() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "sda")]);
    for wanted in ["Eject", "Measure", "Erase…", "Predicted to fail"] {
        assert!(
            has(&window, wanted) || contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

/// The first node under `node` whose words satisfy `wanted`.
fn find<'a>(node: &'a UiNode, wanted: &dyn Fn(&str) -> bool) -> Option<&'a UiNode> {
    let own = ["value", "title", "label", "text", "message", "action"]
        .iter()
        .filter_map(|prop| node.props.get(*prop))
        .filter_map(Value::as_text)
        .any(wanted);
    if own {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, wanted))
}

/// The nodes of `kind`, in order.
fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            found.push(node);
        }
        for child in &node.children {
            walk(child, kind, found);
        }
    }
    let mut found = Vec::new();
    walk(tree, kind, &mut found);
    found
}

#[test]
fn a_mounted_filesystem_offers_both_ways_on_its_files() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "overview")]);
    for wanted in ["Open in Files", "Disk Usage"] {
        assert!(
            has(&window, wanted),
            "{wanted} shows for a mounted filesystem: {:?}",
            shown(&window)
        );
    }
    let partitions = window_showing(
        &app,
        &[
            ("chosen", "nvme0n1"),
            ("tab", "partitions"),
            ("chosen_part", "nvme0n1p2"),
        ],
    );
    for wanted in ["Open in Files", "Disk Usage"] {
        assert!(
            has(&partitions, wanted),
            "{wanted} shows on a mounted partition: {:?}",
            shown(&partitions)
        );
    }
    // Nothing mounted on the external drive: neither way on is offered.
    let unmounted = window_showing(&app, &[("chosen", "sda"), ("tab", "overview")]);
    for wanted in ["Open in Files", "Disk Usage"] {
        assert!(
            !has(&unmounted, wanted),
            "{wanted} is not offered for a filesystem that is not mounted: {:?}",
            shown(&unmounted)
        );
    }
}

#[test]
fn a_locked_partition_is_encrypted_and_shows_nothing_inside() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "partitions")]);
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text.contains("LUKS2, locked")),
        "the locked partition says its scheme: {words:?}"
    );
    assert!(
        words.iter().any(|text| text.contains("nothing inside")),
        "a locked partition shows nothing inside: {words:?}"
    );
    assert!(
        has(&window, "Locked; unlock it first"),
        "it says why it cannot open: {words:?}"
    );
    // The unlocked mapping shows what it holds and is offered its ways on.
    assert!(
        words
            .iter()
            .any(|text| text.contains("LUKS2") && text.contains("home")),
        "the unlocked mapping shows its filesystem: {words:?}"
    );
}

#[test]
fn blocks_tile_the_track_and_slivers_stay_clickable() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "overview")]);
    let blocks: Vec<&UiNode> = of_kind(window.tree(), "ui.Drawing")
        .into_iter()
        .filter(|node| {
            let mut said = Vec::new();
            words(node, &mut said);
            said.iter().any(|text| text.contains("nvme0n1p"))
        })
        .collect();
    assert_eq!(
        blocks.len(),
        4,
        "one block per partition: {:?}",
        shown(&window)
    );
    for block in &blocks {
        assert!(
            block.events.contains_key("press"),
            "a block is a control: {:?}",
            block.props
        );
        let label = block.props.get("label");
        assert!(
            matches!(label, Some(Value::Text(text)) if text.contains("nvme0n1p")),
            "assistive technology reads the block's name: {label:?}"
        );
    }
    let second = blocks
        .iter()
        .find(|node| {
            let label = node.props.get("label");
            matches!(label, Some(Value::Text(text)) if text.starts_with("nvme0n1p2"))
        })
        .expect("the root partition's block");
    window
        .fire(&second.key, "press")
        .expect("the block is pressed");
    assert_eq!(
        window.view_state("chosen_part"),
        Some(Value::text("nvme0n1p2")),
        "pressing a block chooses its partition"
    );
}

#[test]
fn blocks_legend_entries_and_the_chip_report_the_choice() {
    let app = installed();
    let window = window_showing(&app, &[("chosen", "nvme0n1"), ("tab", "overview")]);
    let chip = find(window.tree(), &|text| text.starts_with("nvme0n1p4 · "))
        .expect("the legend names each piece");
    window
        .fire(&chip.key, "press")
        .expect("the legend entry is pressed");
    assert_eq!(
        window.view_state("chosen_part"),
        Some(Value::text("nvme0n1p4"))
    );
    let window = window_showing(
        &app,
        &[
            ("chosen", "nvme0n1"),
            ("tab", "overview"),
            ("chosen_part", "nvme0n1p4"),
        ],
    );
    assert!(
        contains(&window, "LUKS2"),
        "the legend reports the chosen partition's facts: {:?}",
        shown(&window)
    );
}

/// What the system's History says naming a drive did, in English and in German: the name it
/// was given.
#[test]
fn history_says_the_name_a_drive_was_given_in_the_readers_language() {
    let app = Pane::install_with_ids(
        &me(),
        SOURCE,
        include_str!("../../../../apps/tl/disks/app.ids"),
    )
    .delegating()
    .reading(&reading(), READING)
    .rooted_of(APP);
    app.call(
        "name_drive",
        vec![app.root_value(), Value::text("S2"), Value::text("Stick")],
    );
    let german = include_str!("../../../../apps/tl/disks/locales/de-DE.ftl");
    let said = app.history(app.root(), "en-US", "");
    let gesagt = app.history(app.root(), "de-DE", german);
    assert!(
        said.last()
            .is_some_and(|said| said.starts_with("Named a drive “Stick” in")),
        "{said:?}"
    );
    assert!(
        gesagt
            .last()
            .is_some_and(|said| said.starts_with("Benannte ein Laufwerk „Stick“")),
        "{gesagt:?}"
    );
}
