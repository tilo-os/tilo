//! Built-in documentation. The text lives in `guide/*.md` so it doubles as
//! repository docs.

use std::fmt::Write as _;

pub const TOPICS: &[(&str, &str, &str)] = &[
    (
        "quickstart",
        "the five commands you need",
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
}
