//! Built-in documentation. The text lives in `guide/*.md` so it doubles as
//! repository docs.

use std::fmt::Write as _;

pub const TOPICS: &[(&str, &str, &str)] = &[
    (
        "quickstart",
        "get a VM, swap, screenshot; the headful and seat0 recipe",
        include_str!("../guide/quickstart.md"),
    ),
    (
        "concepts",
        "VM, slot, layer, generation, seat, headful vs headless",
        include_str!("../guide/concepts.md"),
    ),
    (
        "swapping",
        "swap, rollback, and what restarts",
        include_str!("../guide/swapping.md"),
    ),
    (
        "kernel",
        "change the kernel: incremental build, swap into the VM, reboot, roll back",
        include_str!("../guide/kernel.md"),
    ),
    (
        "driving",
        "control, shot, logs, seat0",
        include_str!("../guide/driving.md"),
    ),
    (
        "storage",
        "disk use, caps and gc",
        include_str!("../guide/storage.md"),
    ),
    (
        "platforms",
        "hosts, hypervisors, architectures",
        include_str!("../guide/platforms.md"),
    ),
    (
        "troubleshooting",
        "when something does not work",
        include_str!("../guide/troubleshooting.md"),
    ),
    (
        "fedora-loop",
        "fedora44-vm-iterate.sh commands and their equivalents",
        include_str!("../guide/fedora-loop.md"),
    ),
];

pub fn index() -> String {
    let mut text = String::from("tilo-vm guide <topic>\n\n");
    for (name, summary, _) in TOPICS {
        let _ = writeln!(text, "  {name:<16} {summary}");
    }
    text
}

pub fn topic(name: &str) -> Option<&'static str> {
    TOPICS
        .iter()
        .find(|(topic, _, _)| *topic == name)
        .map(|(_, _, text)| *text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_referenced_by_errors_exists() {
        for name in [
            "quickstart",
            "concepts",
            "swapping",
            "driving",
            "storage",
            "platforms",
            "troubleshooting",
            "fedora-loop",
        ] {
            assert!(topic(name).is_some_and(|t| t.starts_with("# ")), "{name}");
        }
        assert!(index().contains("swapping"));
    }

    #[test]
    fn quickstart_teaches_the_headful_path_and_both_screenshots() {
        let text = topic("quickstart").unwrap();
        for needle in [
            "tilo-vm slot create --headful",
            "tilo-vm swap shell",
            "tilo-vm shot",
            "tilo-vm seat claim",
            "tilo-vm vm shot",
            "compositor",
            "seat0",
            "qemu:///session",
        ] {
            assert!(text.contains(needle), "quickstart lacks {needle}");
        }
    }

    #[test]
    fn guides_use_the_current_command_shapes() {
        for (name, _, text) in TOPICS {
            for stale in [
                "swap vm ",
                "swap my-slot",
                "swap review-a",
                "Every command takes `--json` and writes a receipt",
            ] {
                assert!(!text.contains(stale), "guide {name} still says `{stale}`");
            }
        }
        let swapping = topic("swapping").unwrap();
        assert!(swapping.contains("--slot vm fs"));
        assert!(swapping.contains("--profile dev"));
        assert!(swapping.contains("dev-overrides"));
        let platforms = topic("platforms").unwrap();
        assert!(platforms.contains("virt-manager"));
        assert!(platforms.contains("Release / Dev VM disk"));
        let fedora = topic("fedora-loop").unwrap();
        assert!(fedora.contains("dist/dev/fedora44-vm-iterate.sh"));
        assert!(fedora.contains("deploy-apps"));
    }
}
