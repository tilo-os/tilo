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
        "scenarios",
        "scenario install: boot an installer medium, install through its actions, verify first boot",
        include_str!("../guide/scenarios.md"),
    ),
    (
        "images",
        "Containerfile, unit, /etc and package changes: vm upgrade --from local or --from sha:<commit>, rollback-image",
        include_str!("../guide/images.md"),
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
        "devices",
        "boards and other real machines: vm add, the device slot, aarch64 builds",
        include_str!("../guide/devices.md"),
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

    /// Every topic referenced by errors exists; the quickstart teaches the headful path and both
    /// screenshots and names every command that takes a slot; guides use the current command shapes.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn guides_cover_every_error_topic_and_the_current_command_shapes() {
        // Phase: every_topic_referenced_by_errors_exists.
        {
            for name in [
                "quickstart",
                "concepts",
                "swapping",
                "driving",
                "scenarios",
                "images",
                "storage",
                "platforms",
                "devices",
                "troubleshooting",
                "fedora-loop",
            ] {
                assert!(topic(name).is_some_and(|t| t.starts_with("# ")), "{name}");
            }
            let scenarios = topic("scenarios").unwrap();
            for needle in [
                "tilo-vm scenario install",
                "tilo-control --socket",
                "confirmation_phrase",
                "org.tilo.console.0",
                "os/dev/test-installer-cycle.sh",
            ] {
                assert!(scenarios.contains(needle), "scenarios guide lacks {needle}");
            }
            assert!(index().contains("swapping"));
        }

        // Phase: quickstart_teaches_the_headful_path_and_both_screenshots.
        {
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

        // Phase: the_quickstart_rule_names_every_command_that_takes_a_slot.
        {
            use clap::CommandFactory as _;
            fn walk(command: &clap::Command, path: &str, out: &mut Vec<String>) {
                for sub in command.get_subcommands() {
                    let name = format!("{path} {}", sub.get_name()).trim().to_owned();
                    if sub.get_arguments().any(|a| a.get_long() == Some("slot")) {
                        out.push(name.clone());
                    }
                    walk(sub, &name, out);
                }
            }
            let mut commands = Vec::new();
            walk(&crate::cli::Cli::command(), "", &mut commands);
            assert!(commands.len() >= 14, "{commands:?}");
            let quickstart = topic("quickstart").unwrap();
            let rule = quickstart
                .split("accepts `--slot <name>`")
                .nth(1)
                .and_then(|rest| rest.split("\n- ").next())
                .expect("the quickstart states the --slot rule");
            for command in commands {
                let leaf = command.rsplit(' ').next().unwrap();
                assert!(
                    rule.contains(leaf),
                    "the --slot rule omits {command}: {rule}"
                );
            }
            let troubleshooting = topic("troubleshooting").unwrap();
            assert!(troubleshooting.contains("cargo build -p tilo-vm"));
            assert!(troubleshooting.contains("target/debug/tilo-vm"));
            let swapping = topic("swapping").unwrap();
            assert!(swapping.contains("## Verify your swap"));
            assert!(swapping.contains("tilo-vm slot exec -- sha256sum"));
        }

        // Phase: every_vm_upgrade_source_a_guide_shows_is_accepted.
        {
            let mut seen = 0;
            for (name, _, text) in TOPICS {
                for line in text.lines() {
                    let Some(rest) = line.split("vm upgrade").nth(1) else {
                        continue;
                    };
                    let Some(from) = rest.split("--from").nth(1) else {
                        continue;
                    };
                    let from = from
                        .trim_start_matches([' ', '='])
                        .split([' ', '`', ')', ','])
                        .next()
                        .unwrap_or_default()
                        .replace("<commit>", "1a2b3c4")
                        .replace("<sha7>", "1a2b3c4")
                        .replace("<tag>", "main-1a2b3c4");
                    if from.is_empty() {
                        continue;
                    }
                    seen += 1;
                    assert!(
                        crate::imageup::parse_from(&from, None).is_ok(),
                        "guide {name} shows `vm upgrade --from {from}`, which vm upgrade refuses: {line}"
                    );
                }
            }
            assert!(seen >= 5, "found only {seen} vm upgrade --from examples");
            let images = topic("images").unwrap();
            assert!(images.contains("## Pick the fastest loop"));
            assert!(images.contains("--from sha:<commit>"));
            assert!(images.contains("ref-<branch>-<sha7>-<target>"));
            assert!(images.contains("--ref <branch>"));
        }

        // Phase: guides_use_the_current_command_shapes.
        {
            for (name, _, text) in TOPICS {
                for stale in [
                    "swap vm ",
                    "swap my-slot",
                    "swap review-a",
                    "Every command takes `--json` and writes a receipt",
                    "`--slot` for `swap`",
                    "cargo install --locked --path crates/dev/tilo-vm",
                    "refusing to deploy",
                    "podman login code.tilo.so",
                ] {
                    assert!(!text.contains(stale), "guide {name} still says `{stale}`");
                }
            }
            let swapping = topic("swapping").unwrap();
            assert!(swapping.contains("--slot vm fs"));
            assert!(swapping.contains("--profile dev"));
            assert!(swapping.contains("dev-overrides"));
            let devices = topic("devices").unwrap();
            for needle in [
                "tilo-vm vm add opi --ssh root@",
                "tilo-vm swap --slot device shell --from sha:",
                "tilo-vm rollback --slot device --to 0",
                "tilo-vm slot exec --slot device -- sha256sum /usr/bin/shell",
                "TILO_VM_CARGO",
                "registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci",
                "vm upgrade opi",
                // A laptop: its owner's account through sudo, and its display through seat0.
                "tilo-vm vm add yoga --ssh me@",
                "--grant-sudo",
                "/etc/sudoers.d/tilo-vm-<user>",
                "tilo-vm seat claim",
                "ends whoever is logged in",
                "tilo-vm shot",
                "tilo-vm-ref:ref-<branch>-<sha7>-<board>",
                "credentials never reach the device",
                "passphrase",
            ] {
                assert!(devices.contains(needle), "devices guide lacks {needle}");
            }
            let platforms = topic("platforms").unwrap();
            assert!(platforms.contains("virt-manager"));
            assert!(platforms.contains("t dispatch publish-os-dev-disk-x86_64"));
            let fedora = topic("fedora-loop").unwrap();
            assert!(fedora.contains("dist/dev/fedora44-vm-iterate.sh"));
            assert!(fedora.contains("deploy-apps"));
        }
    }
}
