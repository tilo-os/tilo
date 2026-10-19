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

mod support;

use std::fs;
use std::process::Command;

use serde_json::{Value, json};
use support::{BASE_PACKAGES, Fake, Machine, Publish, ROGUE, catalogue_dir, repo, rpm, rpm_with};
use tilo_extension::{Catalogue, Error, Layout, base_packages_sha256, parse_command};

const HELPER: &str = env!("CARGO_BIN_EXE_tilo-extension");

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

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[test]
fn accepts_verb_and_catalogue_id() {
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
    for phase in ["system", "session", "status"] {
        assert_eq!(
            parse_command(&args(&[&format!("live:{phase}")])).unwrap(),
            ("live".into(), phase.into())
        );
    }
}

#[test]
fn rejects_everything_else() {
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

#[test]
fn the_command_line_reports_usage_with_status_2() {
    for argv in [
        &[][..],
        &["install:Media"],
        &["upgrade:media-codecs"],
        &["install", "media-codecs"],
    ] {
        let out = Command::new(HELPER).args(argv).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{argv:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).starts_with("tilo-extension: "),
            "{argv:?}"
        );
        assert!(out.stdout.is_empty());
    }
    let out = Command::new(HELPER).arg("install:Media").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "tilo-extension: invalid extension id\n"
    );
    let out = Command::new(HELPER).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "tilo-extension: expected exactly one argument: activate, refresh:all, live:<phase> or <verb>:<id>\n"
    );
}

#[test]
fn status_of_an_id_this_image_lacks_is_a_usage_error() {
    let out = Command::new(HELPER)
        .arg("status:no-such-extension-anywhere")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "tilo-extension: this image has no extension no-such-extension-anywhere\n"
    );
}

#[test]
fn polkit_rule_matches_the_helper_and_the_unit() {
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

#[test]
fn unknown_id_is_refused() {
    let machine = Machine::new();
    assert_eq!(machine.run_on("install", "nvidia"), 1);
    assert!(!machine.layout.desired("nvidia").exists());
}

#[test]
fn status_prints_the_status_file_or_not_installed() {
    let machine = Machine::new();
    assert_eq!(machine.run("status"), 0);
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    assert_eq!(machine.run("install"), 0);
    assert_eq!(machine.run("status"), 0);
    assert_eq!(machine.run_on("status", "nvidia"), 2);
}

#[test]
fn the_status_file_keeps_its_python_form() {
    let machine = Machine::new();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    assert_eq!(machine.run("install"), 0);
    let text = fs::read_to_string(machine.layout.status("media-codecs")).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    // json.dumps(indent=2, sort_keys=True) and a newline.
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
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

#[test]
fn order_and_gpg_keys_do_not_matter() {
    let mut shuffled: Vec<&str> = BASE_PACKAGES.iter().rev().skip(1).copied().collect();
    shuffled.push("gpg-pubkey 0:aaaa-bbbb.(none)");
    assert_eq!(
        base_packages_sha256(&BASE_PACKAGES),
        base_packages_sha256(&shuffled)
    );
}

#[test]
fn any_package_change_changes_it() {
    let changed: Vec<String> = BASE_PACKAGES
        .iter()
        .map(|l| l.replace("25.2.4-1", "25.2.4-2"))
        .collect();
    assert_ne!(
        base_packages_sha256(&BASE_PACKAGES),
        base_packages_sha256(&changed)
    );
}

#[test]
fn definition_is_pinned() {
    let lines = ["b 0:1-1.x86_64\n", "a 1:2-3.noarch\n"];
    assert_eq!(
        base_packages_sha256(&lines),
        support::sha256_hex(b"tilo.base-packages.v1\na 1:2-3.noarch\nb 0:1-1.x86_64\n")
    );
}

#[test]
fn the_device_computes_the_publishers_fingerprint() {
    // The same rpm output and digest as tools/tests/test_extension_resolve.py
    // pins for the resolver that publishes manifests under this fingerprint.
    let rpm = "bash 0:5.3.0-2.fc44.x86_64\ngpg-pubkey 0:6d9f90a6-6470bd3b.(none)\n\
               zlib-ng-compat 0:2.2.5-2.fc44.x86_64\naudit-libs 0:4.1.1-1.fc44.x86_64\n";
    let lines: Vec<&str> = rpm.lines().collect();
    assert_eq!(
        base_packages_sha256(&lines),
        "5a21779b0a9aca5bb5638fa2177fbbd69c074ed1611aaab0071468cbb6c24e13"
    );
    assert_eq!(
        base_packages_sha256::<&str>(&[]),
        support::sha256_hex(b"tilo.base-packages.v1\n")
    );
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

#[test]
fn shipped_entries_parse() {
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
            // Tilo builds and signs this payload itself (os/Containerfile
            // windows-compat-sysext); nothing else may host it.
            assert_eq!(entry.hosts.iter().collect::<Vec<_>>(), ["code.tilo.so"]);
            assert!(entry.download_bytes > 0);
            assert!(entry.installed_bytes > entry.download_bytes);
        } else {
            assert!(
                !entry.hosts.contains("code.tilo.so"),
                "Tilo must not serve extension payloads"
            );
        }
    }
}

#[test]
fn entry_must_name_itself_and_pin_the_base() {
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

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

#[test]
fn installs_verified_parts_and_reports_status() {
    let machine = Machine::new();
    machine.manifest(
        &[
            machine.artifact(rpm("libavcodec-freeworld", "7.1.1")),
            machine.artifact(rpm("x265-libs", "4.1")),
        ],
        None,
    );
    assert_eq!(machine.run("install"), 0);
    assert_eq!(
        fs::read_to_string(machine.codecs().join("usr/lib64/libavcodec-freeworld.so")).unwrap(),
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
    let objects = machine.layout.objects("media-codecs");
    assert_eq!(
        fs::read_dir(objects).unwrap().count(),
        2,
        "verified downloads are kept for activation"
    );
    assert!(
        !machine
            .layout
            .cache("media-codecs")
            .join("partial")
            .exists()
    );
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
}

#[test]
fn refresh_without_changes_neither_downloads_nor_remerges() {
    let machine = Machine::new();
    let codecs = vec![machine.artifact(rpm("libavcodec-freeworld", "1.0"))];
    machine.manifest(&codecs, None);
    assert_eq!(machine.run("install"), 0);
    machine.host.requests.borrow_mut().clear();
    machine.manifest(&codecs, None);
    assert_eq!(machine.run("refresh"), 0);
    assert!(machine.rpm_requests().is_empty());
    assert_eq!(machine.host.merges.get(), 1);
    assert_eq!(machine.status()["revision"], 2);
}

#[test]
fn refresh_without_opt_in_does_nothing() {
    let machine = Machine::new();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    assert_eq!(machine.run("refresh"), 0);
    assert!(machine.host.requests.borrow().is_empty());
    assert_eq!(machine.status()["state"], "not-installed");
}

#[test]
fn mesa_part_arrives_when_published() {
    let machine = Machine::new();
    let codecs = vec![machine.artifact(rpm("libavcodec-freeworld", "1.0"))];
    machine.manifest(&codecs, None);
    machine.run("install");
    machine.manifest(
        &codecs,
        Some(&[machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))]),
    );
    assert_eq!(machine.run("refresh"), 0);
    assert!(machine.part("tilo-codecs-mesa").is_dir());
    assert_eq!(machine.status()["parts"]["tilo-codecs-mesa"], "enabled");
    assert_eq!(machine.status()["message"], "");
}

// ---------------------------------------------------------------------------
// Refusals: every mismatch refuses the whole transaction and changes nothing.
// ---------------------------------------------------------------------------

fn assert_refused(machine: &Machine, fragment: &str) {
    assert_eq!(machine.run("install"), 1);
    let status = machine.status();
    assert_eq!(status["state"], "failed");
    let error = status["last_error"].as_str().unwrap();
    assert!(error.contains(fragment), "{error:?} lacks {fragment:?}");
    assert!(!machine.codecs().exists());
    assert_eq!(machine.host.merges.get(), 0);
}

#[test]
fn manifest_signed_by_another_key() {
    let machine = Machine::new();
    let publish = Publish {
        key: ROGUE,
        ..Publish::default()
    };
    machine.manifest_with(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
        &publish,
    );
    assert_refused(&machine, "signature is not valid");
}

fn replace_bytes(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let text = String::from_utf8(data.to_vec()).unwrap();
    text.replace(
        std::str::from_utf8(from).unwrap(),
        std::str::from_utf8(to).unwrap(),
    )
    .into_bytes()
}

#[test]
fn manifest_changed_after_signing() {
    let machine = Machine::new();
    let data = machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    machine.host.web.borrow_mut().insert(
        machine.manifest_url(None),
        replace_bytes(&data, b"\"size\": ", b"\"size\": 1"),
    );
    assert_refused(&machine, "signature is not valid");
}

#[test]
fn artifact_bytes_do_not_match_the_pin() {
    let machine = Machine::new();
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
    assert_refused(&machine, "does not match the signed manifest");
}

#[test]
fn artifact_larger_than_the_pin() {
    let machine = Machine::new();
    let entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
    machine.manifest(std::slice::from_ref(&entry), None);
    machine
        .host
        .web
        .borrow_mut()
        .get_mut(entry["urls"][0].as_str().unwrap())
        .unwrap()
        .extend_from_slice(b"more");
    assert_refused(&machine, "larger than the signed manifest");
}

#[test]
fn manifest_for_another_base() {
    let machine = Machine::new();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
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
    drop(web);
    assert_refused(&machine, "for another base");
}

#[test]
fn manifest_for_another_release() {
    let machine = Machine::new();
    let publish = Publish {
        version_id: "45",
        ..Publish::default()
    };
    machine.manifest_with(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
        &publish,
    );
    assert_refused(&machine, "for another base");
}

#[test]
fn artifact_from_a_host_the_image_does_not_allow() {
    let machine = Machine::new();
    let entry = machine.artifact_at(
        rpm("libavcodec-freeworld", "1.0"),
        "https://code.tilo.so/mirror/libavcodec.rpm",
    );
    machine.manifest(&[entry], None);
    assert_refused(&machine, "not allowed for this extension");
}

#[test]
fn artifact_over_plain_http() {
    let machine = Machine::new();
    let entry = machine.artifact_at(
        rpm("libavcodec-freeworld", "1.0"),
        "http://download1.rpmfusion.org/x.rpm",
    );
    machine.manifest(&[entry], None);
    assert_refused(&machine, "not plain HTTPS");
}

#[test]
fn artifact_url_that_climbs_or_carries_a_query() {
    for url in [
        "https://download1.rpmfusion.org/free/../x.rpm",
        "https://download1.rpmfusion.org/free/%2e%2E/x.rpm",
        "https://download1.rpmfusion.org/x.rpm?mirror=1",
        "https://download1.rpmfusion.org/x.rpm#f",
    ] {
        let machine = Machine::new();
        machine.manifest(
            &[machine.artifact_at(rpm("libavcodec-freeworld", "1.0"), url)],
            None,
        );
        assert_refused(&machine, "has an unexpected form");
    }
    let machine = Machine::new();
    let entry = machine.artifact_at(
        rpm("libavcodec-freeworld", "1.0"),
        "https://u@download1.rpmfusion.org/x.rpm",
    );
    machine.manifest(&[entry], None);
    assert_refused(&machine, "not plain HTTPS");
    let machine = Machine::new();
    let entry = machine.artifact_at(
        rpm("libavcodec-freeworld", "1.0"),
        "https://download1.rpmfusion.org:x/x.rpm",
    );
    machine.manifest(&[entry], None);
    assert_refused(&machine, "Port could not be cast to integer value as 'x'");
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

#[test]
fn part_the_image_does_not_know() {
    let machine = Machine::new();
    republish_with(&machine, |data| {
        data["parts"][0]["name"] = json!("tilo-anything");
    });
    assert_refused(&machine, "part this image does not allow");
}

#[test]
fn unknown_fields_such_as_commands() {
    let machine = Machine::new();
    republish_with(&machine, |data| {
        data["parts"][0]["post_install"] = json!("rm -rf /");
    });
    assert_refused(&machine, "unknown fields");
}

#[test]
fn package_that_replaces_a_base_package() {
    let machine = Machine::new();
    machine.manifest(&[machine.artifact(rpm("glibc", "2.99"))], None);
    assert_refused(&machine, "would replace a package of this system");
}

#[test]
fn file_that_is_not_the_named_package() {
    let machine = Machine::new();
    let mut entry = machine.artifact(rpm("x265-libs", "1.0"));
    entry["package"] = json!("libavcodec-freeworld-1.0-1.fc44.x86_64");
    machine.manifest(&[entry], None);
    assert_refused(&machine, "the file is x265-libs");
}

#[test]
fn older_revision_than_already_accepted() {
    let machine = Machine::new();
    let codecs = vec![machine.artifact(rpm("libavcodec-freeworld", "1.0"))];
    machine.manifest_with(
        &codecs,
        None,
        &Publish {
            revision: Some(5),
            ..Publish::default()
        },
    );
    assert_eq!(machine.run("install"), 0);
    machine.manifest_with(
        &codecs,
        None,
        &Publish {
            revision: Some(4),
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
}

#[test]
fn failed_refresh_keeps_the_working_extension() {
    let machine = Machine::new();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    machine.run("install");
    let publish = Publish {
        key: ROGUE,
        ..Publish::default()
    };
    machine.manifest_with(
        &[machine.artifact(rpm("libavcodec-freeworld", "2.0"))],
        None,
        &publish,
    );
    assert_eq!(machine.run("refresh"), 1);
    assert_eq!(machine.status()["state"], "installed");
    assert_eq!(
        fs::read_to_string(machine.codecs().join("usr/lib64/libavcodec-freeworld.so")).unwrap(),
        "libavcodec-freeworld 1.0"
    );
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

#[test]
fn resumes_a_partial_download_by_offset() {
    let machine = Machine::new();
    let entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
    machine.manifest(std::slice::from_ref(&entry), None);
    let partial = machine.layout.cache("media-codecs").join("partial");
    fs::create_dir_all(&partial).unwrap();
    let url = entry["urls"][0].as_str().unwrap().to_owned();
    let data = machine.host.web.borrow()[&url].clone();
    fs::write(partial.join(entry["sha256"].as_str().unwrap()), &data[..10]).unwrap();
    assert_eq!(machine.run("install"), 0);
    assert!(machine.host.requests.borrow().contains(&(url, 10)));
}

#[test]
fn falls_back_to_the_next_url() {
    let machine = Machine::new();
    let mut entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
    entry["urls"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!("https://download1.rpmfusion.org/gone.rpm"));
    machine.manifest(&[entry], None);
    assert_eq!(machine.run("install"), 0);
}

#[test]
fn every_failed_url_is_reported() {
    let machine = Machine::new();
    let mut entry = machine.artifact(rpm("libavcodec-freeworld", "1.0"));
    entry["urls"] = json!([
        "https://download1.rpmfusion.org/gone.rpm",
        "https://download1.rpmfusion.org/also-gone.rpm"
    ]);
    machine.manifest(&[entry], None);
    assert_refused(
        &machine,
        "download1.rpmfusion.org: Not Found; download1.rpmfusion.org: Not Found",
    );
}

#[test]
fn unpublished_base_is_reported_and_retried_later() {
    let machine = Machine::new();
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
}

#[test]
fn offline_opt_in_waits_without_an_error() {
    let machine = Machine::new();
    *machine.host.online.borrow_mut() = "none".to_owned();
    assert_eq!(machine.run("install"), 0);
    let status = machine.status();
    assert_eq!(status["state"], "waiting-for-network");
    assert_eq!(status["last_error"], "");
    assert!(machine.host.requests.borrow().is_empty());
}

#[test]
fn a_connection_lost_mid_install_waits_instead_of_failing() {
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
            *self.0.online.borrow_mut() = "limited".to_owned();
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
    let machine = Machine::new();
    let host = DropsOff(Fake::default());
    *host.0.packages.borrow_mut() = machine.host.packages.borrow().clone();
    *host.0.online.borrow_mut() = "full".to_owned();
    assert_eq!(
        tilo_extension::main(&host, &machine.layout, &args(&["install:media-codecs"])),
        0
    );
    let status = machine.status();
    assert_eq!(
        (status["state"].as_str(), status["last_error"].as_str()),
        (Some("waiting-for-network"), Some(""))
    );
}

// ---------------------------------------------------------------------------
// Offline start-up
// ---------------------------------------------------------------------------

#[test]
fn empty_activation_reads_neither_rpm_nor_the_network() {
    let machine = Machine::new();
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(machine.host.rpm_queries.get(), 0);
    assert!(machine.host.requests.borrow().is_empty());
    assert_eq!(
        (machine.host.merges.get(), machine.host.unmerges.get()),
        (0, 0)
    );
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Images with the release root take certified manifests only.
// ---------------------------------------------------------------------------

#[test]
fn an_image_with_the_release_root_installs_from_a_certified_bundle() {
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
    // Activation verifies the kept bundle again.
    fs::remove_dir_all(machine.codecs()).unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir());
}

#[test]
fn an_image_with_the_release_root_refuses_a_bare_signature() {
    let machine = Machine::new();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
    // Published for key-pinned images only: no bundle beside the manifest.
    machine.certify();
    assert_eq!(machine.run("install"), 1);
    assert!(!machine.codecs().exists());
    assert_eq!(machine.host.merges.get(), 0);
}

#[test]
fn a_kept_bare_signature_does_not_activate_on_an_image_with_the_root() {
    let machine = Machine::new();
    install(&machine, false);
    // The next image carries the root: what the old one kept no longer counts.
    machine.certify();
    machine.run("activate");
    assert!(!machine.codecs().exists());
}

fn install(machine: &Machine, mesa: bool) {
    let codecs = vec![machine.artifact(rpm("libavcodec-freeworld", "1.0"))];
    let mesa = mesa.then(|| vec![machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))]);
    machine.manifest(&codecs, mesa.as_deref());
    assert_eq!(machine.run("install"), 0);
}

#[test]
fn remove_deletes_everything_and_unmerges() {
    let machine = Machine::new();
    install(&machine, true);
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
    assert_eq!(machine.host.merges.get(), 2);
}

#[test]
fn activation_rebuilds_parts_from_verified_bytes() {
    let machine = Machine::new();
    install(&machine, true);
    let library = machine.codecs().join("usr/lib64/libavcodec-freeworld.so");
    fs::write(&library, "replaced behind the helper's back").unwrap();
    fs::write(
        machine.codecs().join("usr/lib64/extra.so"),
        "added behind its back",
    )
    .unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(
        fs::read_to_string(&library).unwrap(),
        "libavcodec-freeworld 1.0"
    );
    assert!(!machine.codecs().join("usr/lib64/extra.so").exists());
}

#[test]
fn rebuilding_keeps_the_times_media_processes_compare() {
    let machine = Machine::new();
    install(&machine, true);
    let release = machine
        .codecs()
        .join("usr/lib/extension-release.d/extension-release.tilo-codecs");
    let before = fs::metadata(&release).unwrap().modified().unwrap();
    machine.run("activate");
    assert_eq!(fs::metadata(&release).unwrap().modified().unwrap(), before);
}

#[test]
fn activation_refuses_a_tampered_object() {
    let machine = Machine::new();
    install(&machine, false);
    for entry in fs::read_dir(machine.layout.objects("media-codecs")).unwrap() {
        let path = entry.unwrap().path();
        let data = fs::read(&path).unwrap();
        fs::write(&path, replace_bytes(&data, b"1.0", b"6.6")).unwrap();
    }
    machine.run("activate");
    assert!(!machine.codecs().exists());
    assert_eq!(machine.status()["parts"]["tilo-codecs"], "waiting");
}

#[test]
fn activation_refuses_a_tampered_kept_manifest() {
    let machine = Machine::new();
    install(&machine, true);
    let kept = fs::read_dir(machine.layout.bases("media-codecs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("manifest.json");
    let data = fs::read(&kept).unwrap();
    fs::write(
        &kept,
        replace_bytes(&data, b"\"license\": \"GPL", b"\"license\": \"MIT GPL"),
    )
    .unwrap();
    machine.run("activate");
    assert!(!machine.codecs().exists());
    assert!(!machine.part("tilo-codecs-mesa").exists());
}

#[test]
fn activation_merges_nothing_beside_a_foreign_extension() {
    let machine = Machine::new();
    install(&machine, true);
    let foreign = machine.layout.extensions().join("evil");
    fs::create_dir_all(foreign.join("usr/lib/extension-release.d")).unwrap();
    let merges = machine.host.merges.get();
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
        (1, 0)
    );
    let status = machine.status();
    assert_eq!(status["state"], "failed");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("unexpected system extensions")
    );
    // Once the foreign extension is gone, activation merges again and the
    // refusal no longer shows.
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
}

#[test]
fn verified_downloads_move_across_mounts() {
    let machine = Machine::new();
    machine.host.cross_device.set(true);
    install(&machine, true);
    assert!(machine.codecs().is_dir());
    assert!(
        !machine
            .layout
            .cache("media-codecs")
            .join("partial")
            .exists()
    );
}

#[test]
fn development_image_merges_its_own_layers_too() {
    let machine = Machine::new();
    install(&machine, true);
    fs::create_dir_all(machine.layout.extensions().join("tilo-vm-layer/usr")).unwrap();
    machine.host.masked.set(false);
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir());
}

#[test]
fn parts_stay_across_an_update_that_keeps_their_base_packages() {
    let machine = Machine::new();
    install(&machine, true);
    machine
        .host
        .packages
        .borrow_mut()
        .push("new-app 0:1-1.fc44.x86_64".to_owned());
    machine.run("activate");
    assert!(machine.codecs().is_dir());
    assert!(machine.part("tilo-codecs-mesa").is_dir());
}

#[test]
fn plugin_without_codec_is_refused_before_download_on_exact_base() {
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
}

#[test]
fn retained_plugin_loses_readiness_when_real_codec_becomes_stub() {
    let machine = Machine::new();
    machine
        .host
        .packages
        .borrow_mut()
        .push("openh264 0:2.6.0-3.fc44.x86_64".to_owned());
    machine.manifest(
        &[machine.artifact(rpm("gstreamer1-plugin-openh264", "1.0"))],
        None,
    );
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
    *machine.host.packages.borrow_mut() = previous;
    assert_eq!(machine.run("activate"), 0);
    assert!(
        machine.codecs().exists(),
        "the authenticated part returns on rollback"
    );
}

#[test]
fn plugin_with_signed_codec_library_can_be_installed() {
    let machine = Machine::new();
    machine
        .host
        .packages
        .borrow_mut()
        .push("noopenh264 0:2.6.0-4.fc44.x86_64".to_owned());
    machine.manifest(
        &[
            machine.artifact(rpm("gstreamer1-plugin-openh264", "1.0")),
            machine.artifact(rpm_with(
                "openh264",
                "1.0",
                &[("usr/lib64/libopenh264.so.8", "codec")],
            )),
        ],
        None,
    );
    assert_eq!(machine.run("install"), 0);
    assert_eq!(
        fs::read_to_string(machine.codecs().join("usr/lib64/libopenh264.so.8")).unwrap(),
        "codec"
    );
}

#[test]
fn a_part_whose_base_package_changed_waits_and_returns_on_rollback() {
    let machine = Machine::new();
    install(&machine, true);
    let booted = machine.host.packages.borrow().clone();
    *machine.host.packages.borrow_mut() = booted
        .iter()
        .map(|p| p.replace("mesa-dri-drivers 0:25.2.4", "mesa-dri-drivers 0:25.3.0"))
        .collect();
    machine.run("activate");
    assert!(machine.codecs().is_dir(), "the software codecs still fit");
    assert!(!machine.part("tilo-codecs-mesa").exists());
    assert_eq!(machine.status()["parts"]["tilo-codecs-mesa"], "waiting");
    // Rolling back to the deployment it was built for brings it back, offline.
    *machine.host.packages.borrow_mut() = booted;
    machine.run("activate");
    assert!(machine.part("tilo-codecs-mesa").is_dir());
    assert_eq!(machine.status()["parts"]["tilo-codecs-mesa"], "enabled");
}

#[test]
fn a_part_whose_package_the_base_now_ships_waits() {
    let machine = Machine::new();
    install(&machine, false);
    machine
        .host
        .packages
        .borrow_mut()
        .push("libavcodec-freeworld 0:1.0-1.fc44.x86_64".to_owned());
    machine.run("activate");
    assert!(!machine.codecs().exists());
}

#[test]
fn nothing_is_merged_on_another_os_release() {
    let machine = Machine::new();
    install(&machine, true);
    fs::write(
        machine.root.join("usr/lib/os-release"),
        "ID=fedora\nVERSION_ID=45\n",
    )
    .unwrap();
    machine.run("activate");
    assert!(!machine.codecs().exists());
    assert!(!machine.part("tilo-codecs-mesa").exists());
}

#[test]
fn an_unreadable_base_merges_nothing() {
    let machine = Machine::new();
    install(&machine, true);
    fs::write(machine.root.join("usr/lib/os-release"), "ID=Fedora Linux\n").unwrap();
    let unmerges = machine.host.unmerges.get();
    assert_eq!(machine.run("activate"), 1);
    assert!(!machine.codecs().exists());
    assert_eq!(machine.host.unmerges.get(), unmerges + 1);
}

#[test]
fn parts_nothing_vouches_for_are_removed() {
    let machine = Machine::new();
    let stray = machine.codecs();
    fs::create_dir_all(stray.join("usr/lib/extension-release.d")).unwrap();
    machine.run("activate");
    assert!(!stray.exists());
}

#[test]
fn refresh_after_an_update_rebuilds_for_the_new_base() {
    let machine = Machine::new();
    install(&machine, true);
    let updated: Vec<String> = machine
        .host
        .packages
        .borrow()
        .iter()
        .map(|p| p.replace("gstreamer1 0:1.26.5", "gstreamer1 0:1.26.6"))
        .collect();
    *machine.host.packages.borrow_mut() = updated;
    machine.run("activate");
    assert!(!machine.codecs().exists());
    let mut data: Value = serde_json::from_slice(&machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.1"))],
        Some(&[machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))]),
    ))
    .unwrap();
    data["parts"][0]["requires_base"]["gstreamer1"] = json!("0:1.26.6-1.fc44");
    machine.publish(&serde_json::to_vec(&data).unwrap(), support::TRUSTED, None);
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
}

#[test]
fn only_the_last_three_bases_are_kept() {
    let machine = Machine::new();
    install(&machine, false);
    for version in ["1.26.6", "1.26.7", "1.26.8"] {
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
        let mut data: Value = serde_json::from_slice(&machine.manifest(
            &[machine.artifact(rpm("libavcodec-freeworld", version))],
            None,
        ))
        .unwrap();
        data["parts"][0]["requires_base"]["gstreamer1"] = json!(format!("0:{version}-1.fc44"));
        machine.publish(&serde_json::to_vec(&data).unwrap(), support::TRUSTED, None);
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
        3
    );
}

#[test]
fn installer_seed_is_adopted_once() {
    let machine = Machine::new();
    let seed = machine.layout.seed("media-codecs");
    fs::create_dir_all(seed.parent().unwrap()).unwrap();
    fs::write(&seed, "owner opted in during installation\n").unwrap();
    machine.manifest(
        &[machine.artifact(rpm("libavcodec-freeworld", "1.0"))],
        None,
    );
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
// Shipped
// ---------------------------------------------------------------------------

#[test]
fn activation_unit_merges_in_the_host_namespace() {
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

#[test]
fn manifests_verify_only_against_the_extension_key() {
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

#[test]
fn sealed_image_still_masks_the_boot_time_merge() {
    assert!(read("os/Containerfile").contains(
        "systemctl mask systemd-sysext.service systemd-confext.service systemd-sysext.socket"
    ));
    assert!(!sources().contains("enable systemd-sysext"));
}

#[test]
fn timer_and_update_hook_refresh_every_extension() {
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

#[test]
fn dispatcher_starts_refresh_for_a_pending_opt_in() {
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

#[test]
fn installer_seeds_the_path_the_helper_adopts() {
    let script = read("os/usr/libexec/tilo-install");
    assert!(script.contains(r#"SEED_DIR="$DEPLOY_ETC/tilo/extensions/opt-in""#));
    assert!(script.contains("/usr/share/tilo/system-extensions/sysext.d/${2}.json"));
}

#[test]
fn the_image_installs_the_compiled_helper_where_its_units_run_it() {
    let components = read("os/components.toml");
    assert!(components.contains(
        r#"{ from = "tilo-extension", to = "/usr/libexec/tilo-extension", mode = "0755" }"#
    ));
    assert!(!read("os/Containerfile").contains("os/usr/libexec/tilo-extension"));
}

// ---------------------------------------------------------------------------
// A prebuilt, manifest-pinned sysext image (format "sysext-raw")
// ---------------------------------------------------------------------------

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

/// Publish a manifest whose one part is `image` (or `artifacts` instead);
/// returns the image's artifact entry.
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

fn raw_status(machine: &Machine) -> Value {
    serde_json::from_slice(&fs::read(machine.layout.status(RAW_ID)).unwrap()).unwrap()
}

#[test]
fn install_links_the_verified_image_and_merges_it() {
    let machine = raw_machine();
    publish_raw(&machine, &raw_image(), None);
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
    assert_eq!(
        machine.run("activate"),
        0,
        "the image is accepted again at the next boot"
    );
    assert_eq!(*machine.host.merged.borrow(), [format!("{RAW_PART}.raw")]);
}

#[test]
fn catalogue_declares_the_size_the_owner_is_told() {
    let entry = Catalogue::parse(
        &fs::read(catalogue_dir().join("windows-compat.json")).unwrap(),
        RAW_ID,
    )
    .unwrap();
    assert!(entry.installed_bytes >= 3_000_000_000);
}

#[test]
fn a_changed_image_is_not_merged_at_activation() {
    let machine = raw_machine();
    let artifact = publish_raw(&machine, &raw_image(), None);
    assert_eq!(machine.run_on("install", RAW_ID), 0);
    let object = machine
        .layout
        .objects(RAW_ID)
        .join(artifact["sha256"].as_str().unwrap());
    fs::write(object, b"tampered").unwrap();
    machine.run("activate");
    assert!(!raw_live(&machine).exists());
    assert_eq!(raw_status(&machine)["parts"], json!({RAW_PART: "waiting"}));
}

#[test]
fn remove_deletes_the_image_and_its_record() {
    let machine = raw_machine();
    publish_raw(&machine, &raw_image(), None);
    assert_eq!(machine.run_on("install", RAW_ID), 0);
    assert_eq!(machine.run_on("remove", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());
    assert!(!raw_record(&machine).exists());
    assert_eq!(raw_status(&machine)["state"], "not-installed");
}

#[test]
fn an_image_with_the_wrong_digest_is_refused() {
    let machine = raw_machine();
    let artifact = publish_raw(&machine, &raw_image(), None);
    machine.host.web.borrow_mut().insert(
        artifact["urls"][0].as_str().unwrap().to_owned(),
        b"not what was signed".repeat(10),
    );
    assert_ne!(machine.run_on("install", RAW_ID), 0);
    assert!(!raw_live(&machine).exists());
}

#[test]
fn a_sysext_image_part_is_exactly_one_artifact() {
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
}

#[test]
fn an_unmanaged_image_in_the_search_path_still_refuses_the_merge() {
    let machine = raw_machine();
    publish_raw(&machine, &raw_image(), None);
    fs::create_dir_all(machine.layout.extensions()).unwrap();
    fs::write(machine.layout.extensions().join("evil.raw"), b"x").unwrap();
    machine.run_on("install", RAW_ID);
    assert!(!raw_live(&machine).exists());
    assert!(machine.host.unmerges.get() >= 1);
}

#[test]
fn an_image_stays_across_an_update_that_keeps_the_release() {
    let machine = raw_machine();
    publish_raw(&machine, &raw_image(), None);
    assert_eq!(machine.run_on("install", RAW_ID), 0);
    machine
        .host
        .packages
        .borrow_mut()
        .push("new-app 0:1-1.fc44.x86_64".to_owned());
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(fs::read(raw_live(&machine)).unwrap(), raw_image());
    assert_eq!(raw_status(&machine)["parts"], json!({RAW_PART: "enabled"}));
}
