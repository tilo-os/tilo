//! tilo-extension without network, rpm tooling or root: the whole state
//! machine against a temporary root (see `support`), the command line as the
//! units run it, and the shipped units and keys it depends on.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "test helpers fail the test on any I/O error"
)]
#![allow(
    clippy::too_many_lines,
    reason = "a journey asserts every step of one person's use in order"
)]

mod support;

use std::fs;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

use serde_json::{Value, json};
use support::{BASE_PACKAGES, Fake, Machine, Publish, ROGUE, catalogue_dir, repo, rpm, rpm_with};
use tilo_extension::{Catalogue, Error, Layout, base_packages_sha256, parse_command};

fn helper() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-extension")
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn read(path: &str) -> String {
    fs::read_to_string(repo().join(path)).unwrap()
}

/// The helper's own sources, its module directories included, for what it
/// must never contain.
fn sources() -> String {
    fn collect(dir: &std::path::Path, all: &mut String) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, all);
            } else {
                all.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    let mut all = String::new();
    collect(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut all,
    );
    all
}

tilo_test_env::scenario! {
    fn the_command_takes_one_verb_and_id_and_reports_status;

    phase accepts_verb_and_catalogue_id;
    {
        assert_eq!(
            parse_command(&args(&["install:media-codecs"])).unwrap(),
            ("install".into(), "media-codecs".into())
        );
        assert_eq!(
            parse_command(&args(&["refresh:all"])).unwrap(),
            ("refresh".into(), "all".into())
        );
        assert_eq!(
            parse_command(&args(&["activate"])).unwrap(),
            ("activate".into(), String::new())
        );
        for phase in ["system", "session", "prune", "status"] {
            assert_eq!(
                parse_command(&args(&[&format!("live:{phase}")])).unwrap(),
                ("live".into(), phase.into())
            );
        }
    }

    phase rejects_everything_else;
    {
        let cases: &[&[&str]] = &[
            &[],
            &["install"],
            &["install", "media-codecs"],
            &["Install:media-codecs"],
            &["install:../x"],
            &["install:Media"],
            &["install:all"],
            &["remove:all"],
            &["upgrade:media-codecs"],
            &["install:media-codecs;rm"],
            &["activate:x"],
            &["live:everything"],
            &["live"],
            &["guard"],
        ];
        for argv in cases {
            assert!(
                matches!(parse_command(&args(argv)), Err(Error::Pack(_))),
                "{argv:?}"
            );
        }
    }

    phase the_command_line_reports_usage_with_status_2;
    {
        for argv in [
            &[][..],
            &["install:Media"],
            &["upgrade:media-codecs"],
            &["install", "media-codecs"],
        ] {
            let out = Command::new(helper()).args(argv).output_locked().unwrap();
            assert_eq!(out.status.code(), Some(2), "{argv:?}");
            assert!(
                String::from_utf8_lossy(&out.stderr).starts_with("tilo-extension: "),
                "{argv:?}"
            );
            assert!(out.stdout.is_empty());
        }
        let out = Command::new(helper())
            .arg("install:Media")
            .output_locked()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "tilo-extension: invalid extension id\n"
        );
        let out = Command::new(helper()).output_locked().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "tilo-extension: expected exactly one argument: activate, refresh:all, boot:<phase>, live:<phase> or <verb>:<id>\n"
        );
    }

    phase status_of_an_id_this_image_lacks_is_a_usage_error;
    {
        let out = Command::new(helper())
            .arg("status:no-such-extension-anywhere")
            .output_locked()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "tilo-extension: this image has no extension no-such-extension-anywhere\n"
        );
    }
}

tilo_test_env::scenario! {
    fn the_shipped_units_keys_and_image_match_the_helper;

    phase polkit_rule_matches_the_helper_and_the_unit;
    {
        let rule = read("os/etc/polkit-1/rules.d/49-tilo-extension.rules");
        let start = rule.find("tilo-extension@(").unwrap() + "tilo-extension@(".len();
        let end = start + rule[start..].find(')').unwrap();
        let verbs: Vec<&str> = rule[start..end].split('|').collect();
        let mut sorted = verbs.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, ["install", "refresh", "remove"]);
        assert!(verbs.iter().all(|v| tilo_extension::VERBS.contains(v)));
        let unit = read("os/usr/lib/systemd/system/tilo-extension@.service");
        assert!(unit.contains("ExecStart=/usr/libexec/tilo-extension %i\n"));
    }

    phase activation_unit_merges_in_the_host_namespace;
    {
        let unit = read("os/usr/lib/systemd/system/tilo-extension-activate.service");
        assert!(unit.contains("ExecStart=/usr/libexec/tilo-extension activate\n"));
        let settings: Vec<&str> = unit
            .lines()
            .filter(|l| l.contains('=') && !l.starts_with('#'))
            .map(|l| l.split('=').next().unwrap())
            .collect();
        for namespaced in [
            "ProtectSystem",
            "PrivateTmp",
            "ProtectHome",
            "PrivateDevices",
            "ReadWritePaths",
            "PrivateMounts",
            "ProtectKernelModules",
            "TemporaryFileSystem",
        ] {
            assert!(
                !settings.contains(&namespaced),
                "{namespaced} would hide the merge from the host"
            );
        }
        assert!(sources().contains(r#"OsStr::new("tilo-extension-activate.service")"#));
    }

    phase a_trial_starts_before_any_merge_and_ends_at_the_graphical_target;
    {
        let start = read("os/usr/lib/systemd/system/tilo-extension-trial.service");
        assert!(start.contains("ExecStart=/usr/libexec/tilo-extension boot:start\n"));
        assert!(start.contains("DefaultDependencies=no\n"));
        assert!(start.contains(
            "Before=systemd-sysext.service tilo-extension-activate.service sysinit.target\n"
        ));
        let done = read("os/usr/lib/systemd/system/tilo-extension-booted.service");
        assert!(done.contains("ExecStart=/usr/libexec/tilo-extension boot:done\n"));
        assert!(done.contains("After=graphical.target\n") && done.contains("WantedBy=graphical.target\n"));
        for unit in [&start, &done] {
            assert!(unit.contains("ConditionPathExistsGlob=/var/lib/tilo/extensions/*/trial\n"));
        }
        assert_eq!(
            Layout::default().trial("windows-compat"),
            std::path::Path::new("/var/lib/tilo/extensions/windows-compat/trial")
        );
    }

    phase a_kept_image_that_would_change_usr_is_taken_out_before_the_merge;
    {
        let unit = read("os/usr/lib/systemd/system/tilo-extension-quarantine.service");
        assert!(unit.contains("ExecStart=/usr/libexec/tilo-extension boot:quarantine\n"));
        assert!(unit.contains("DefaultDependencies=no\n"));
        assert!(unit.contains(
            "Before=systemd-sysext.service tilo-extension-activate.service sysinit.target\n"
        ));
        assert!(unit.contains("ConditionDirectoryNotEmpty=/var/lib/extensions\n"));
        let image = read("os/Containerfile");
        assert!(image.contains("systemctl enable tilo-extension-quarantine.service"));
    }

    phase manifests_verify_only_against_the_extension_key;
    {
        assert_eq!(
            Layout::default().trust_key(),
            std::path::Path::new("/usr/share/tilo/extensions.pub")
        );
        assert_ne!(read("keys/extensions.pub"), read("keys/cosign.pub"));
        assert!(
            read("os/Containerfile")
                .contains("COPY keys/extensions.pub /usr/share/tilo/extensions.pub")
        );
        assert!(!sources().contains("cosign.pub"));
    }

    phase sealed_image_still_masks_the_boot_time_merge;
    {
        assert!(read("os/Containerfile").contains(
            "systemctl mask systemd-sysext.service systemd-confext.service systemd-sysext.socket"
        ));
        assert!(!sources().contains("enable systemd-sysext"));
    }

    phase timer_and_update_hook_refresh_every_extension;
    {
        assert!(
            read("os/usr/lib/systemd/system/tilo-extension-refresh.timer")
                .contains("Unit=tilo-extension@refresh:all.service")
        );
        assert!(
            read(
                "os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/40-tilo-extension.conf"
            )
            .contains("tilo-extension@refresh:all.service")
        );
    }

    phase dispatcher_starts_refresh_for_a_pending_opt_in;
    {
        let script = read("os/usr/lib/NetworkManager/dispatcher.d/90-tilo-extension");
        assert!(script.contains("tilo-extension@refresh:all.service"));
        assert!(script.contains("SEED_DIR=/etc/tilo/extensions/opt-in"));
        let layout = Layout::default();
        assert_eq!(
            layout.seed("x").parent().unwrap(),
            std::path::Path::new("/etc/tilo/extensions/opt-in")
        );
        assert_eq!(
            layout.state_root(),
            std::path::Path::new("/var/lib/tilo/extensions")
        );
    }

    phase installer_seeds_the_path_the_helper_adopts;
    {
        let script = read("os/usr/libexec/tilo-install");
        assert!(script.contains(r#"SEED_DIR="$DEPLOY_ETC/tilo/extensions/opt-in""#));
        assert!(script.contains("/usr/share/tilo/system-extensions/sysext.d/${2}.json"));
    }

    phase the_image_installs_the_compiled_helper_where_its_units_run_it;
    {
        let components = read("os/components.toml");
        assert!(components.contains(
            r#"{ from = "tilo-extension", to = "/usr/libexec/tilo-extension", mode = "0755" }"#
        ));
        assert!(!read("os/Containerfile").contains("os/usr/libexec/tilo-extension"));
    }
}

tilo_test_env::scenario! {
    fn the_base_packages_fingerprint_follows_its_pinned_definition;

    phase order_and_gpg_keys_do_not_matter;
    {
        let mut shuffled: Vec<&str> = BASE_PACKAGES.iter().rev().skip(1).copied().collect();
        shuffled.push("gpg-pubkey 0:aaaa-bbbb.(none)");
        assert_eq!(
            base_packages_sha256(&BASE_PACKAGES),
            base_packages_sha256(&shuffled)
        );
    }

    phase any_package_change_changes_it;
    {
        let changed: Vec<String> = BASE_PACKAGES
            .iter()
            .map(|l| l.replace("25.2.4-1", "25.2.4-2"))
            .collect();
        assert_ne!(
            base_packages_sha256(&BASE_PACKAGES),
            base_packages_sha256(&changed)
        );
    }

    phase definition_is_pinned;
    {
        let lines = ["b 0:1-1.x86_64\n", "a 1:2-3.noarch\n"];
        assert_eq!(
            base_packages_sha256(&lines),
            support::sha256_hex(b"tilo.base-packages.v1\na 1:2-3.noarch\nb 0:1-1.x86_64\n")
        );
        assert_eq!(
            base_packages_sha256::<&str>(&[]),
            support::sha256_hex(b"tilo.base-packages.v1\n")
        );
    }
}

tilo_test_env::scenario! {
    fn catalogue_entries_are_parsed_strictly;

    phase shipped_entries_parse;
    {
        let mut ids: Vec<String> = fs::read_dir(catalogue_dir())
            .unwrap()
            .filter_map(|e| {
                e.unwrap()
                    .file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".json"))
                    .map(str::to_owned)
            })
            .collect();
        ids.sort();
        assert!(ids.contains(&"media-codecs".to_owned()));
        for ident in ids {
            let entry = Catalogue::parse(
                &fs::read(catalogue_dir().join(format!("{ident}.json"))).unwrap(),
                &ident,
            )
            .unwrap();
            assert!(entry.manifest.starts_with("https://code.tilo.so/"));
            if entry.kind == "windows-compat" {
                assert_eq!(entry.hosts.iter().collect::<Vec<_>>(), ["code.tilo.so"]);
                assert!(entry.download_bytes > 0);
                // The image is linked into the search path as it was downloaded.
                assert!(entry.installed_bytes >= entry.download_bytes);
                // The size the owner is told before downloading it.
                assert!(entry.installed_bytes >= 2_000_000_000);
            } else {
                assert!(
                    !entry.hosts.contains("code.tilo.so"),
                    "Tilo must not serve extension payloads"
                );
            }
        }
    }

    phase entry_must_name_itself_and_pin_the_base;
    {
        let good: Value =
            serde_json::from_slice(&fs::read(catalogue_dir().join("media-codecs.json")).unwrap())
                .unwrap();
        for change in [
            json!({"id": "other"}),
            json!({"manifest": "http://code.tilo.so/{arch}-{base}"}),
            json!({"manifest": "https://code.tilo.so/{arch}/manifest.json"}),
            json!({"hosts": ["evil host"]}),
            json!({"parts": ["Bad Part"]}),
            json!({"run": "sh"}),
            json!({"size": null}),
            json!({"size": {"download_bytes": 0}}),
            json!({"size": {"installed_bytes": true}}),
            json!({"size": {"installed_bytes": 30_000_000_000_u64}}),
            json!({"size": {"shown_bytes": 1}}),
        ] {
            let mut entry = good.clone();
            for (k, v) in change.as_object().unwrap() {
                entry[k] = v.clone();
            }
            assert!(
                Catalogue::parse(&serde_json::to_vec(&entry).unwrap(), "media-codecs").is_err(),
                "{change}"
            );
        }
    }
}

fn replace_bytes(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let text = String::from_utf8(data.to_vec()).unwrap();
    text.replace(
        std::str::from_utf8(from).unwrap(),
        std::str::from_utf8(to).unwrap(),
    )
    .into_bytes()
}

/// Every file in `dir`, with its bytes, so a test can damage them and put them back.
fn snapshot(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let data = fs::read(&path).unwrap();
            (path, data)
        })
        .collect()
}

fn restore(files: &[(std::path::PathBuf, Vec<u8>)]) {
    for (path, data) in files {
        fs::write(path, data).unwrap();
    }
}

/// The base with gstreamer at `version`, as an update that changes it boots.
fn boot_gstreamer(machine: &Machine, version: &str) {
    let updated: Vec<String> = machine
        .host
        .packages
        .borrow()
        .iter()
        .map(|p| {
            if p.starts_with("gstreamer1 ") {
                format!("gstreamer1 0:{version}-1.fc44.x86_64")
            } else {
                p.clone()
            }
        })
        .collect();
    *machine.host.packages.borrow_mut() = updated;
}

/// Publish the codecs resolved for gstreamer `version`, with the Mesa part.
fn publish_for_gstreamer(machine: &Machine, version: &str, mesa: &[Value]) {
    let mut data: Value = serde_json::from_slice(&machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", version))],
        Some(mesa),
    ))
    .unwrap();
    data["parts"][0]["requires_base"]["gstreamer1"] = json!(format!("0:{version}-1.fc44"));
    machine.publish(&serde_json::to_vec(&data).unwrap(), support::TRUSTED, None);
}

/// A host whose connection drops as soon as a download starts.
struct DropsOff(Fake);

impl tilo_extension::Host for DropsOff {
    fn rpm_query_all(&self, layout: &Layout) -> tilo_extension::Result<Vec<String>> {
        self.0.rpm_query_all(layout)
    }
    fn http_open(
        &self,
        url: &str,
        offset: u64,
    ) -> Result<Box<dyn tilo_extension::Response>, tilo_extension::HttpError> {
        "limited".clone_into(&mut self.0.online.borrow_mut());
        Err(tilo_extension::HttpError::Unreachable(format!(
            "{url} {offset}: Network is unreachable"
        )))
    }
    fn connectivity(&self) -> String {
        self.0.connectivity()
    }
    fn machine(&self) -> String {
        "x86_64".to_owned()
    }
    fn euid(&self) -> u32 {
        0
    }
}

/// The owner turns the media codecs on, keeps them through refreshes, repairs,
/// updates and rollbacks of the base, and turns them off again: every state the
/// status file reports, and every refusal on the way that must leave the
/// working extension in place.
#[test]
fn an_owner_keeps_codecs_through_refreshes_updates_and_rollbacks_then_removes_them() {
    let machine = Machine::new();
    // An id this image has no entry for is refused before anything is chosen.
    assert_eq!(machine.run_on("install", "nvidia"), 1);
    assert!(!machine.layout.desired("nvidia").exists());

    // Nothing chosen yet: status says so, and activation and refresh read
    // neither rpm nor the network.
    assert_eq!(
        machine.run("status"),
        0,
        "nothing installed, and it says so"
    );
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(machine.host.rpm_queries.get(), 0);
    assert_eq!(
        (machine.host.merges.get(), machine.host.unmerges.get()),
        (0, 0)
    );
    assert_eq!(machine.run("refresh"), 0);
    assert!(machine.host.requests.borrow().is_empty());
    assert_eq!(machine.status()["state"], "not-installed");

    // Opting in offline waits without an error.
    "none".clone_into(&mut machine.host.online.borrow_mut());
    assert_eq!(machine.run("install"), 0);
    let status = machine.status();
    assert_eq!(status["state"], "waiting-for-network");
    assert_eq!(status["last_error"], "");
    assert!(machine.host.requests.borrow().is_empty());
    "full".clone_into(&mut machine.host.online.borrow_mut());

    // Online, but nothing published for this base yet: reported, and the
    // choice kept for the daily retry.
    assert_eq!(machine.run("install"), 1);
    let status = machine.status();
    assert_eq!(status["state"], "failed");
    assert_eq!(
        status["desired"], true,
        "the choice is kept for the daily retry"
    );
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("Not available for this version of Tilo yet")
    );

    // A connection lost mid-install waits instead of failing.
    let drops = DropsOff(Fake::default());
    *drops.0.packages.borrow_mut() = machine.host.packages.borrow().clone();
    "full".clone_into(&mut drops.0.online.borrow_mut());
    assert_eq!(
        tilo_extension::main(&drops, &machine.layout, &args(&["install:media-codecs"])),
        0
    );
    let status = machine.status();
    assert_eq!(
        (status["state"].as_str(), status["last_error"].as_str()),
        (Some("waiting-for-network"), Some(""))
    );

    // Published now. The first artifact's first mirror is gone, part of it
    // was downloaded before, and the cache sits on another mount than the
    // verified store.
    let mut libavcodec = machine.artifact(rpm("libavcodec-freeworld", "7.1.1"));
    let url = libavcodec["urls"][0].as_str().unwrap().to_owned();
    libavcodec["urls"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!("https://download1.rpmfusion.org/gone.rpm"));
    let codecs = vec![libavcodec, machine.artifact(rpm("x265-libs", "4.1"))];
    machine.manifest(&codecs, None);
    let partial = machine.layout.cache("media-codecs").join("partial");
    fs::create_dir_all(&partial).unwrap();
    let bytes = machine.host.web.borrow()[&url].clone();
    fs::write(
        partial.join(codecs[0]["sha256"].as_str().unwrap()),
        &bytes[..10],
    )
    .unwrap();
    machine.host.cross_device.set(true);
    assert_eq!(machine.run("install"), 0, "{}", machine.status());
    assert!(
        machine.host.requests.borrow().contains(&(url, 10)),
        "the partial download resumes by offset, from the next mirror"
    );
    let library = machine.codecs().join("usr/lib64/libavcodec-freeworld.so");
    assert_eq!(
        fs::read_to_string(&library).unwrap(),
        "libavcodec-freeworld 7.1.1"
    );
    let release = machine.release("tilo-codecs");
    assert_eq!(
        (
            release["ID"].as_str(),
            release["VERSION_ID"].as_str(),
            release["ARCHITECTURE"].as_str()
        ),
        ("fedora", "44", "x86-64")
    );
    assert_eq!(release["TILO_BASE_PACKAGES"], machine.fingerprint());
    let status = machine.status();
    assert_eq!(status["state"], "installed");
    assert_eq!(status["desired"], true);
    assert_eq!(
        status["parts"],
        json!({"tilo-codecs": "enabled", "tilo-codecs-mesa": "waiting"})
    );
    assert!(
        status["message"]
            .as_str()
            .unwrap()
            .contains("waiting for a matching Mesa build")
    );
    assert_eq!(status["generation"], 1);
    assert_eq!(machine.host.merges.get(), 1);
    assert!(!machine.part("tilo-codecs-mesa").exists());
    assert_eq!(
        fs::read_dir(machine.layout.objects("media-codecs"))
            .unwrap()
            .count(),
        2,
        "verified downloads are kept for activation"
    );
    assert!(!partial.exists(), "verified downloads move across mounts");
    let record: Value = serde_json::from_slice(
        &fs::read(
            machine
                .codecs()
                .join("usr/share/tilo-extension/tilo-codecs.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        record["packages"],
        json!(["libavcodec-freeworld", "x265-libs"])
    );
    assert_eq!(record["requires_base"]["gstreamer1"], "0:1.26.5-1.fc44");

    // The status file keeps the form the desktop has always read.
    assert_eq!(machine.run("status"), 0);
    let text = fs::read_to_string(machine.layout.status("media-codecs")).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        text,
        tilo_extension::py::Dumps::INDENTED.dumps(&value) + "\n"
    );
    assert!(
        text.starts_with("{\n  \"base\": {\n    \"architecture\": \"x86_64\",\n"),
        "{text}"
    );
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    let updated = value["updated"].as_str().unwrap();
    assert_eq!(updated.len(), "2026-10-14T00:00:00Z".len());
    assert!(
        updated.ends_with('Z') && updated.as_bytes()[10] == b'T',
        "{updated}"
    );

    // A refresh that finds nothing new neither downloads nor merges again.
    machine.host.requests.borrow_mut().clear();
    machine.manifest(&codecs, None);
    assert_eq!(machine.run("refresh"), 0);
    assert!(machine.rpm_requests().is_empty());
    assert_eq!(machine.host.merges.get(), 1);
    assert_eq!(machine.status()["revision"], 2);

    // The Mesa part arrives once it is published.
    let mesa = vec![machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))];
    machine.manifest(&codecs, Some(&mesa));
    assert_eq!(machine.run("refresh"), 0);
    assert!(machine.part("tilo-codecs-mesa").is_dir());
    assert_eq!(machine.status()["parts"]["tilo-codecs-mesa"], "enabled");
    assert_eq!(machine.status()["message"], "");

    // Activation rebuilds the parts from the verified bytes, keeping the
    // times media processes compare.
    let release_file = machine
        .codecs()
        .join("usr/lib/extension-release.d/extension-release.tilo-codecs");
    let before = fs::metadata(&release_file).unwrap().modified().unwrap();
    fs::write(&library, "replaced behind the helper's back").unwrap();
    fs::write(
        machine.codecs().join("usr/lib64/extra.so"),
        "added behind its back",
    )
    .unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(
        fs::read_to_string(&library).unwrap(),
        "libavcodec-freeworld 7.1.1"
    );
    assert!(!machine.codecs().join("usr/lib64/extra.so").exists());
    assert_eq!(
        fs::metadata(&release_file).unwrap().modified().unwrap(),
        before
    );

    // A changed verified object, or a changed kept manifest, is never
    // activated; once the bytes are as verified again, the parts return.
    let objects = snapshot(&machine.layout.objects("media-codecs"));
    for (path, data) in &objects {
        fs::write(path, replace_bytes(data, b"7.1.1", b"6.6.6")).unwrap();
    }
    machine.run("activate");
    assert!(!machine.codecs().exists());
    assert_eq!(machine.status()["parts"]["tilo-codecs"], "waiting");
    restore(&objects);
    let kept_dir = machine
        .layout
        .bases("media-codecs")
        .join(machine.fingerprint());
    let kept = snapshot(&kept_dir);
    let manifest = kept_dir.join("manifest.json");
    let data = fs::read(&manifest).unwrap();
    fs::write(
        &manifest,
        replace_bytes(&data, b"\"license\": \"GPL", b"\"license\": \"MIT GPL"),
    )
    .unwrap();
    machine.run("activate");
    assert!(!machine.codecs().exists());
    assert!(!machine.part("tilo-codecs-mesa").exists());
    restore(&kept);
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir() && machine.part("tilo-codecs-mesa").is_dir());

    // An update that keeps the parts' base packages keeps the parts.
    machine
        .host
        .packages
        .borrow_mut()
        .push("new-app 0:1-1.fc44.x86_64".to_owned());
    machine.run("activate");
    assert!(machine.codecs().is_dir());
    assert!(machine.part("tilo-codecs-mesa").is_dir());

    // A part whose base package changed waits, and returns on rollback.
    let booted = machine.host.packages.borrow().clone();
    *machine.host.packages.borrow_mut() = booted
        .iter()
        .map(|p| p.replace("mesa-dri-drivers 0:25.2.4", "mesa-dri-drivers 0:25.3.0"))
        .collect();
    machine.run("activate");
    assert!(machine.codecs().is_dir(), "the software codecs still fit");
    assert!(!machine.part("tilo-codecs-mesa").exists());
    assert_eq!(machine.status()["parts"]["tilo-codecs-mesa"], "waiting");
    *machine.host.packages.borrow_mut() = booted.clone();
    machine.run("activate");
    assert!(machine.part("tilo-codecs-mesa").is_dir());
    assert_eq!(machine.status()["parts"]["tilo-codecs-mesa"], "enabled");

    // A part whose package the base now ships waits.
    machine
        .host
        .packages
        .borrow_mut()
        .push("x265-libs 0:4.1-1.fc44.x86_64".to_owned());
    machine.run("activate");
    assert!(!machine.codecs().exists());
    *machine.host.packages.borrow_mut() = booted;

    // Nothing is merged on another release, or on a base that cannot be read.
    let os_release = machine.root.join("usr/lib/os-release");
    let good_release = fs::read(&os_release).unwrap();
    fs::write(&os_release, "ID=fedora\nVERSION_ID=45\n").unwrap();
    machine.run("activate");
    assert!(!machine.codecs().exists());
    assert!(!machine.part("tilo-codecs-mesa").exists());
    fs::write(&os_release, "ID=Fedora Linux\n").unwrap();
    let unmerges = machine.host.unmerges.get();
    assert_eq!(machine.run("activate"), 1);
    assert!(!machine.codecs().exists());
    assert_eq!(machine.host.unmerges.get(), unmerges + 1);
    fs::write(&os_release, &good_release).unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir());

    // Beside an extension nothing vouches for, activation merges nothing.
    let foreign = machine.layout.extensions().join("evil");
    fs::create_dir_all(foreign.join("usr/lib/extension-release.d")).unwrap();
    let (merges, unmerges) = (machine.host.merges.get(), machine.host.unmerges.get());
    assert_eq!(machine.run("activate"), 1);
    assert!(!machine.codecs().exists());
    assert_eq!(
        machine.host.merges.get(),
        merges,
        "a refusal must not merge what it refused"
    );
    assert_eq!(
        (
            machine.host.unmerges.get(),
            machine.host.merged.borrow().len()
        ),
        (unmerges + 1, 0)
    );
    let status = machine.status();
    assert_eq!(status["state"], "failed");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("unexpected system extensions")
    );
    fs::remove_dir_all(&foreign).unwrap();
    assert_eq!(machine.run("activate"), 0);
    let status = machine.status();
    assert_eq!(
        (status["state"].as_str(), status["last_error"].as_str()),
        (Some("installed"), Some(""))
    );
    assert!(
        machine
            .host
            .merged
            .borrow()
            .contains(&"tilo-codecs".to_owned())
    );

    // A development image merges its own layers beside the parts.
    let layer = machine.layout.extensions().join("tilo-vm-layer");
    fs::create_dir_all(layer.join("usr")).unwrap();
    machine.host.masked.set(false);
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir());
    fs::remove_dir_all(&layer).unwrap();
    machine.host.masked.set(true);

    // An update of a base package takes the parts out until a refresh
    // rebuilds them for the new base; the previous base's manifest stays for a
    // rollback, and only the last three bases (and their downloads) are kept.
    boot_gstreamer(&machine, "1.26.6");
    machine.run("activate");
    assert!(!machine.codecs().exists());
    publish_for_gstreamer(&machine, "1.26.6", &mesa);
    assert_eq!(machine.run("refresh"), 0);
    assert_eq!(
        machine.release("tilo-codecs")["TILO_BASE_PACKAGES"],
        machine.fingerprint()
    );
    assert_eq!(
        machine.status()["parts"],
        json!({"tilo-codecs": "enabled", "tilo-codecs-mesa": "enabled"})
    );
    assert_eq!(
        fs::read_dir(machine.layout.bases("media-codecs"))
            .unwrap()
            .count(),
        2,
        "the previous base's manifest stays for a rollback"
    );
    for version in ["1.26.7", "1.26.8"] {
        boot_gstreamer(&machine, version);
        publish_for_gstreamer(&machine, version, &mesa);
        assert_eq!(machine.run("refresh"), 0);
    }
    assert_eq!(
        fs::read_dir(machine.layout.bases("media-codecs"))
            .unwrap()
            .count(),
        3
    );
    assert_eq!(
        fs::read_dir(machine.layout.objects("media-codecs"))
            .unwrap()
            .count(),
        4,
        "three libavcodec builds and the Mesa part they share"
    );

    // An older revision than the one accepted, or a manifest signed by
    // another key, fails the refresh and keeps the working extension.
    machine.manifest_with(
        &codecs,
        None,
        &Publish {
            revision: Some(1),
            ..Publish::default()
        },
    );
    assert_eq!(machine.run("refresh"), 1);
    let status = machine.status();
    assert_eq!(status["state"], "installed", "the working extension stays");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("refusing a downgrade")
    );
    assert!(machine.codecs().is_dir());
    machine.manifest_with(
        &[machine.artifact(rpm("libavcodec-freeworld", "2.0"))],
        None,
        &Publish {
            key: ROGUE,
            ..Publish::default()
        },
    );
    assert_eq!(machine.run("refresh"), 1);
    assert_eq!(machine.status()["state"], "installed");
    assert_eq!(
        fs::read_to_string(&library).unwrap(),
        "libavcodec-freeworld 1.26.8"
    );

    // Removing deletes everything and merges again without the parts.
    let merges = machine.host.merges.get();
    assert_eq!(machine.run("remove"), 0);
    assert!(!machine.codecs().exists());
    assert!(!machine.part("tilo-codecs-mesa").exists());
    assert!(!machine.layout.desired("media-codecs").exists());
    assert!(!machine.layout.objects("media-codecs").exists());
    assert!(!machine.layout.bases("media-codecs").exists());
    let status = machine.status();
    assert_eq!(status["state"], "not-installed");
    assert_eq!(
        status["parts"],
        json!({"tilo-codecs": "off", "tilo-codecs-mesa": "off"})
    );
    assert_eq!(status["revision"], Value::Null);
    assert_eq!(machine.host.merges.get(), merges + 1);

    // A choice made in the installer is adopted once; a removal after it sticks.
    let seed = machine.layout.seed("media-codecs");
    fs::create_dir_all(seed.parent().unwrap()).unwrap();
    fs::write(&seed, "owner opted in during installation\n").unwrap();
    publish_for_gstreamer(&machine, "1.26.8", &mesa);
    assert_eq!(machine.run_on("refresh", "all"), 0);
    assert_eq!(machine.status()["state"], "installed");
    machine.run("remove");
    assert_eq!(machine.run_on("refresh", "all"), 0);
    assert_eq!(
        machine.status()["state"],
        "not-installed",
        "a removal sticks"
    );
}

// ---------------------------------------------------------------------------
// Refusals: every mismatch refuses the whole transaction and changes nothing.
// ---------------------------------------------------------------------------

fn assert_refused(machine: &Machine, fragment: &str) {
    assert_eq!(machine.run("install"), 1, "{fragment}");
    let status = machine.status();
    assert_eq!(status["state"], "failed");
    let error = status["last_error"].as_str().unwrap();
    assert!(error.contains(fragment), "{error:?} lacks {fragment:?}");
    assert!(!machine.codecs().exists());
    assert_eq!(machine.host.merges.get(), 0);
}

fn republish_with(machine: &Machine, change: impl Fn(&mut Value)) {
    let mut data: Value = serde_json::from_slice(&machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    ))
    .unwrap();
    change(&mut data);
    machine.publish(&serde_json::to_vec(&data).unwrap(), support::TRUSTED, None);
}

fn publish_at(machine: &Machine, url: &str) {
    machine.manifest(
        &[machine.artifact_at(rpm("libavcodec-freeworld", "1.0"), url)],
        None,
    );
}

#[test]
fn every_mismatch_refuses_the_whole_install_and_changes_nothing() {
    type Publishes = fn(&Machine);
    let cases: &[(&str, Publishes)] = &[
        ("does not match the signed manifest", |machine| {
            let entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
            machine.manifest(std::slice::from_ref(&entry), None);
            let size = usize::try_from(entry["size"].as_u64().unwrap()).unwrap();
            let evil = serde_json::to_vec(
                &json!({"name": "libavcodec-freeworld", "nvra": entry["package"],
                "files": {"usr/lib64/libavcodec.so": "evil"}}),
            )
            .unwrap();
            let mut forged = evil[..evil.len().min(size)].to_vec();
            forged.resize(size, b' ');
            machine
                .host
                .web
                .borrow_mut()
                .insert(entry["urls"][0].as_str().unwrap().to_owned(), forged);
        }),
        ("larger than the signed manifest", |machine| {
            let entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
            machine.manifest(std::slice::from_ref(&entry), None);
            machine
                .host
                .web
                .borrow_mut()
                .get_mut(entry["urls"][0].as_str().unwrap())
                .unwrap()
                .extend_from_slice(b"more");
        }),
        ("for another base", |machine| {
            // The manifest and signature published for another base, served
            // at this base's address.
            let mut other_lines: Vec<&str> = BASE_PACKAGES.to_vec();
            other_lines.push("extra 0:1-1.x86_64");
            let other = base_packages_sha256(&other_lines);
            let publish = Publish {
                fingerprint: Some(other.clone()),
                ..Publish::default()
            };
            machine.manifest_with(
                &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
                None,
                &publish,
            );
            let mine = machine.manifest_url(None);
            let theirs = machine.manifest_url(Some(&other));
            let mut web = machine.host.web.borrow_mut();
            let (data, sig) = (web[&theirs].clone(), web[&format!("{theirs}.sig")].clone());
            web.insert(mine.clone(), data);
            web.insert(format!("{mine}.sig"), sig);
        }),
        ("for another base", |machine| {
            let publish = Publish {
                version_id: "45",
                ..Publish::default()
            };
            machine.manifest_with(
                &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
                None,
                &publish,
            );
        }),
        ("not allowed for this extension", |machine| {
            publish_at(machine, "https://code.tilo.so/mirror/libavcodec.rpm");
        }),
        ("not plain HTTPS", |machine| {
            publish_at(machine, "http://download1.rpmfusion.org/x.rpm");
        }),
        ("not plain HTTPS", |machine| {
            publish_at(machine, "https://u@download1.rpmfusion.org/x.rpm");
        }),
        ("has an unexpected form", |machine| {
            publish_at(machine, "https://download1.rpmfusion.org/free/../x.rpm");
        }),
        ("has an unexpected form", |machine| {
            publish_at(machine, "https://download1.rpmfusion.org/free/%2e%2E/x.rpm");
        }),
        ("has an unexpected form", |machine| {
            publish_at(machine, "https://download1.rpmfusion.org/x.rpm?mirror=1");
        }),
        ("has an unexpected form", |machine| {
            publish_at(machine, "https://download1.rpmfusion.org/x.rpm#f");
        }),
        (
            "Port could not be cast to integer value as 'x'",
            |machine| {
                publish_at(machine, "https://download1.rpmfusion.org:x/x.rpm");
            },
        ),
        (
            "download1.rpmfusion.org: Not Found; download1.rpmfusion.org: Not Found",
            |machine| {
                let mut entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
                entry["urls"] = json!([
                    "https://download1.rpmfusion.org/gone.rpm",
                    "https://download1.rpmfusion.org/also-gone.rpm"
                ]);
                machine.manifest(&[entry], None);
            },
        ),
        ("part this image does not allow", |machine| {
            republish_with(machine, |data| {
                data["parts"][0]["name"] = json!("tilo-anything");
            });
        }),
        ("unknown fields", |machine| {
            republish_with(machine, |data| {
                data["parts"][0]["post_install"] = json!("rm -rf /");
            });
        }),
        ("would replace a package of this system", |machine| {
            machine.manifest(&[machine.artifact(rpm("glibc", "2.99"))], None);
        }),
        ("the file is x265-libs", |machine| {
            let mut entry = machine.artifact(rpm("x265-libs", "1.0"));
            entry["package"] = json!("libavcodec-freeworld-1.0-1.fc44.x86_64");
            machine.manifest(&[entry], None);
        }),
    ];
    for (fragment, publish) in cases {
        let machine = Machine::new();
        publish(&machine);
        assert_refused(&machine, fragment);
    }
}

/// An image that carries the release root takes certified manifests only, and
/// what an image without it accepted does not activate on one with it.
#[test]
fn images_with_the_release_root_take_certified_manifests_only() {
    let machine = Machine::new();
    machine.certify();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    assert_eq!(machine.run("install"), 0, "{}", machine.status());
    let url = machine.manifest_url(None);
    let requests: Vec<String> = machine
        .host
        .requests
        .borrow()
        .iter()
        .map(|(u, _)| u.clone())
        .collect();
    assert!(requests.contains(&format!("{url}.bundle")), "{requests:?}");
    assert!(!requests.contains(&format!("{url}.sig")), "{requests:?}");
    let kept = machine
        .layout
        .bases("media-codecs")
        .join(machine.fingerprint());
    assert!(kept.join("manifest.json.bundle").is_file());
    assert!(!kept.join("manifest.json.sig").exists());
    fs::remove_dir_all(machine.codecs()).unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir());

    // Installed under a bare signature, then the image gains the root.
    let machine = Machine::new();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    assert_eq!(machine.run("install"), 0);
    machine.certify();
    machine.run("activate");
    assert!(
        !machine.codecs().exists(),
        "a kept bare signature does not activate"
    );
    let merges = machine.host.merges.get();
    assert_eq!(
        machine.run("install"),
        1,
        "published for key-pinned images only: no bundle beside the manifest"
    );
    assert!(!machine.codecs().exists());
    assert_eq!(machine.host.merges.get(), merges);
}

/// The `OpenH264` plugin is installed only beside Cisco's real codec library:
/// refused before any download without it, taken out when the base swaps the
/// library for its stub, back on rollback, and installable with the library
/// from the manifest.
#[test]
fn the_openh264_plugin_is_ready_only_with_the_real_codec_library() {
    let machine = Machine::new();
    let plugin = machine.artifact(rpm("gstreamer1-plugin-openh264", "1.0"));
    machine.manifest(std::slice::from_ref(&plugin), None);
    assert_eq!(machine.run("install"), 1);
    assert!(!machine.codecs().exists());
    assert!(
        machine.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("Cisco's codec library")
    );
    let url = plugin["urls"][0].as_str().unwrap();
    assert!(!machine.host.requests.borrow().iter().any(|(u, _)| u == url));

    machine
        .host
        .packages
        .borrow_mut()
        .push("openh264 0:2.6.0-3.fc44.x86_64".to_owned());
    machine.manifest(std::slice::from_ref(&plugin), None);
    assert_eq!(machine.run("install"), 0);
    assert!(machine.codecs().exists());
    let previous = machine.host.packages.borrow().clone();
    let mut stub: Vec<String> = previous
        .iter()
        .filter(|p| !p.starts_with("openh264 "))
        .cloned()
        .collect();
    stub.push("noopenh264 0:2.6.0-4.fc44.x86_64".to_owned());
    *machine.host.packages.borrow_mut() = stub;
    assert_eq!(machine.run("activate"), 0);
    assert!(!machine.codecs().exists());
    assert_eq!(machine.status()["parts"]["tilo-codecs"], "waiting");
    *machine.host.packages.borrow_mut() = previous.clone();
    assert_eq!(machine.run("activate"), 0);
    assert!(
        machine.codecs().exists(),
        "the authenticated part returns on rollback"
    );

    // On the stub's base, a manifest that brings the signed library too.
    let mut stub: Vec<String> = previous
        .iter()
        .filter(|p| !p.starts_with("openh264 "))
        .cloned()
        .collect();
    stub.push("noopenh264 0:2.6.0-4.fc44.x86_64".to_owned());
    *machine.host.packages.borrow_mut() = stub;
    machine.manifest(
        &[
            plugin,
            machine.artifact(rpm_with(
                "openh264",
                "1.0",
                &[("usr/lib64/libopenh264.so.8", "codec")],
            )),
        ],
        None,
    );
    assert_eq!(machine.run("refresh"), 0);
    assert_eq!(
        fs::read_to_string(machine.codecs().join("usr/lib64/libopenh264.so.8")).unwrap(),
        "codec"
    );
}

const RAW_ID: &str = "windows-compat";
const RAW_PART: &str = "tilo-windows-compat";
const RAW_BASE_URL: &str =
    "https://code.tilo.so/api/packages/tilo/generic/tilo-extension-windows-compat";

fn raw_machine() -> Machine {
    let machine = Machine::new();
    fs::copy(
        catalogue_dir().join("windows-compat.json"),
        machine.layout.catalogue_dir().join("windows-compat.json"),
    )
    .unwrap();
    machine
}

fn raw_image() -> Vec<u8> {
    b"erofs image bytes".repeat(100)
}

fn publish_raw(machine: &Machine, image: &[u8], artifacts: Option<Value>) -> Value {
    let fingerprint = machine.fingerprint();
    let url = format!("{RAW_BASE_URL}/x86_64-{fingerprint}/{RAW_PART}.raw");
    machine
        .host
        .web
        .borrow_mut()
        .insert(url.clone(), image.to_vec());
    let artifact = json!({
        "package": format!("{RAW_PART}.raw"), "urls": [url], "size": image.len(),
        "sha256": support::sha256_hex(image), "publisher": "Tilo",
        "license": "see /usr/share/licenses inside the image",
    });
    let document = json!({
        "type": tilo_extension::MANIFEST_TYPE, "schema": 1, "id": RAW_ID, "revision": 1,
        "created": "2026-10-03T12:00:00Z",
        "base": {"os_id": "fedora", "version_id": "44", "architecture": "x86_64",
                 "packages_sha256": fingerprint},
        "parts": [{"name": RAW_PART, "format": "sysext-raw", "state": "available",
                   "requires_base": {},
                   "artifacts": artifacts.unwrap_or_else(|| json!([artifact.clone()]))}],
    });
    let data = serde_json::to_vec(&document).unwrap();
    let manifest_url = format!("{RAW_BASE_URL}/x86_64-{fingerprint}/manifest.json");
    let mut web = machine.host.web.borrow_mut();
    web.insert(
        format!("{manifest_url}.sig"),
        support::sign(support::TRUSTED, &data),
    );
    web.insert(manifest_url, data);
    artifact
}

fn raw_live(machine: &Machine) -> std::path::PathBuf {
    machine.layout.extensions().join(format!("{RAW_PART}.raw"))
}

fn raw_record(machine: &Machine) -> std::path::PathBuf {
    machine
        .layout
        .extensions()
        .join(format!(".{RAW_PART}.record.json"))
}

fn boot(machine: &Machine, phase: &str) -> i32 {
    tilo_extension::main(&machine.host, &machine.layout, &[format!("boot:{phase}")])
}

fn raw_status(machine: &Machine) -> Value {
    serde_json::from_slice(&fs::read(machine.layout.status(RAW_ID)).unwrap()).unwrap()
}

fn unlabelled_image() -> tilo_extension::image::Inspection {
    let mut image = support::sound_image();
    for entry in image.image.values_mut() {
        entry.label = Some("system_u:object_r:unlabeled_t:s0".to_owned());
    }
    image
}

/// A Windows-compatibility image: installed, kept through starts that finish
/// and updates that keep its release, never merged once changed, removed
/// whole, turned off by a start that never finished, and taken out at the next
/// start when it would damage /usr.
#[test]
fn a_raw_sysext_image_lives_through_starts_updates_and_removal() {
    let machine = raw_machine();
    let artifact = publish_raw(&machine, &raw_image(), None);
    assert_eq!(machine.run_on("install", RAW_ID), 0);
    assert_eq!(fs::read(raw_live(&machine)).unwrap(), raw_image());
    assert_eq!(*machine.host.merged.borrow(), [format!("{RAW_PART}.raw")]);
    let status = raw_status(&machine);
    assert_eq!(
        (status["state"].clone(), status["parts"].clone()),
        (json!("installed"), json!({RAW_PART: "enabled"}))
    );
    let record: Value = serde_json::from_slice(&fs::read(raw_record(&machine)).unwrap()).unwrap();
    assert_eq!(record["format"], "sysext-raw");
    assert_eq!(record["packages"], json!([]));

    // A start that finishes keeps it; the start that installed it proves nothing.
    let trial = machine.layout.trial(RAW_ID);
    assert_eq!(fs::read_to_string(&trial).unwrap(), "new\n");
    assert_eq!(boot(&machine, "done"), 0);
    assert!(trial.exists());
    assert_eq!(boot(&machine, "start"), 0);
    assert_eq!(fs::read_to_string(&trial).unwrap(), "booting\n");
    assert_eq!(
        machine.run("activate"),
        0,
        "the image is accepted again at the next boot"
    );
    assert_eq!(*machine.host.merged.borrow(), [format!("{RAW_PART}.raw")]);
    assert_eq!(fs::read_to_string(&trial).unwrap(), "booting\n");
    assert_eq!(boot(&machine, "done"), 0);
    assert!(!trial.exists());
    assert_eq!(boot(&machine, "start"), 0);
    assert_eq!(fs::read(raw_live(&machine)).unwrap(), raw_image());
    assert_eq!(raw_status(&machine)["state"], "installed");

    // A sound image kept from an earlier start is left alone at the next one.
    assert_eq!(boot(&machine, "quarantine"), 0);
    assert_eq!(fs::read(raw_live(&machine)).unwrap(), raw_image());
    assert!(machine.layout.desired(RAW_ID).exists());
    assert_eq!(raw_status(&machine)["state"], "installed");

    // It stays across an update that keeps the release.
    machine
        .host
        .packages
        .borrow_mut()
        .push("new-app 0:1-1.fc44.x86_64".to_owned());
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(fs::read(raw_live(&machine)).unwrap(), raw_image());
    assert_eq!(raw_status(&machine)["parts"], json!({RAW_PART: "enabled"}));

    // A changed image is not merged at activation.
    let object = machine
        .layout
        .objects(RAW_ID)
        .join(artifact["sha256"].as_str().unwrap());
    fs::write(&object, b"tampered").unwrap();
    machine.run("activate");
    assert!(!raw_live(&machine).exists());
    assert_eq!(raw_status(&machine)["parts"], json!({RAW_PART: "waiting"}));

    // Removing deletes the image and its record.
    assert_eq!(machine.run_on("remove", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());
    assert!(!raw_record(&machine).exists());
    assert_eq!(raw_status(&machine)["state"], "not-installed");

    // Installed again, a start that does not finish turns it off for good.
    publish_raw(&machine, &raw_image(), None);
    assert_eq!(machine.run_on("install", RAW_ID), 0);
    assert_eq!(fs::read_to_string(&trial).unwrap(), "new\n");
    assert_eq!(boot(&machine, "start"), 0);
    assert_eq!(machine.run("activate"), 0, "a start activates as usual");
    assert_eq!(fs::read_to_string(&trial).unwrap(), "booting\n");
    assert_eq!(
        boot(&machine, "start"),
        0,
        "the next start finds it never finished"
    );
    assert!(!raw_live(&machine).exists());
    assert!(!trial.exists());
    assert!(!machine.layout.desired(RAW_ID).exists());
    let status = raw_status(&machine);
    assert_eq!(status["state"], "failed");
    assert_eq!(status["parts"], json!({RAW_PART: "off"}));
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .starts_with("Turned off: this computer did not finish starting with it.")
    );
    assert_eq!(machine.run("activate"), 0);
    assert!(!raw_live(&machine).exists(), "and it stays off");

    // Installed again by a version that did not inspect images, an image that
    // would leave /usr unlabelled is taken out before the next merge.
    assert_eq!(machine.run_on("install", RAW_ID), 0);
    assert_eq!(fs::read(raw_live(&machine)).unwrap(), raw_image());
    *machine.host.image.borrow_mut() = Some(unlabelled_image());
    assert_eq!(boot(&machine, "quarantine"), 0);
    assert!(
        !raw_live(&machine).exists(),
        "the merge would have left /usr unlabelled"
    );
    assert!(!machine.layout.desired(RAW_ID).exists());
    assert!(!trial.exists());
    assert_eq!(machine.run("activate"), 0);
    assert!(
        !raw_live(&machine).exists(),
        "and its downloads are gone with it"
    );
    let status = raw_status(&machine);
    assert_eq!(
        (status["state"].clone(), status["parts"].clone()),
        (json!("failed"), json!({RAW_PART: "off"}))
    );
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .starts_with("Turned off: merging it would damage the system: ")
    );
}

#[test]
fn a_raw_sysext_image_that_is_not_exactly_what_was_signed_and_safe_is_never_merged() {
    // The image served is not the one signed.
    let machine = raw_machine();
    let artifact = publish_raw(&machine, &raw_image(), None);
    machine.host.web.borrow_mut().insert(
        artifact["urls"][0].as_str().unwrap().to_owned(),
        b"not what was signed".repeat(10),
    );
    assert_ne!(machine.run_on("install", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());

    // A sysext image part is exactly one artifact.
    let machine = raw_machine();
    let artifact = publish_raw(&machine, &raw_image(), None);
    publish_raw(
        &machine,
        &raw_image(),
        Some(json!([artifact.clone(), artifact])),
    );
    assert_ne!(machine.run_on("install", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());
    assert!(
        raw_status(&machine)["last_error"]
            .as_str()
            .unwrap()
            .contains("a sysext image part is exactly one artifact")
    );

    // An unmanaged image in the search path still refuses the merge.
    let machine = raw_machine();
    publish_raw(&machine, &raw_image(), None);
    fs::create_dir_all(machine.layout.extensions()).unwrap();
    fs::write(machine.layout.extensions().join("evil.raw"), b"x").unwrap();
    machine.run_on("install", RAW_ID);
    assert!(!raw_live(&machine).exists());
    assert!(machine.host.unmerges.get() >= 1);

    // An image that would damage /usr is never merged, now or at the next start.
    let machine = raw_machine();
    let mut image = support::sound_image();
    let usr = image.image.get_mut("/usr").unwrap();
    (usr.uid, usr.gid) = (99, 65534);
    *machine.host.image.borrow_mut() = Some(image);
    publish_raw(&machine, &raw_image(), None);
    assert_ne!(machine.run_on("install", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());
    assert!(machine.host.merged.borrow().is_empty());
    let status = raw_status(&machine);
    assert_eq!(status["state"], "failed");
    assert_eq!(
        status["last_error"],
        format!(
            "{RAW_PART}: not turned on, it would damage the system: /usr is owned by 99:65534, not root"
        )
    );
    assert_ne!(
        machine.run("activate"),
        0,
        "and not at the next start either"
    );
    assert!(!raw_live(&machine).exists());
    assert!(!machine.layout.trial(RAW_ID).exists());

    // An unlabelled image is never merged on a labelled system.
    let machine = raw_machine();
    *machine.host.image.borrow_mut() = Some(unlabelled_image());
    publish_raw(&machine, &raw_image(), None);
    assert_ne!(machine.run_on("install", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());
    assert!(
        raw_status(&machine)["last_error"]
            .as_str()
            .unwrap()
            .ends_with(": /usr is labelled system_u:object_r:unlabeled_t:s0")
    );
}
