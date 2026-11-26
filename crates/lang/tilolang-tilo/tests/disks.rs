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
                "media_errors": 0, "bad_sectors": 0, "self_test": "success"}},
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
