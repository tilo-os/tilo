use super::super::{
    data::{self, APPS, HELPER, HOME, NATIVE, ORIGINAL, Store, TOOLS},
    json,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    home: Value,
    cap: Value,
    native: Value,
    original: Value,
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let store = Store::new(root.clone(), uid());
        let mut paths = vec![
            HELPER,
            "/usr/libexec/tilo-firstboot-user",
            "/usr/libexec/tilo-firstboot-home-seeds",
            "/usr/libexec/tilo-firstboot-files",
            "/usr/bin/tar",
        ];
        paths.extend(TOOLS);
        for name in paths {
            let path = store.path(name).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(store.path("/usr/share/tilo").unwrap()).unwrap();
        let home = data::home_facts(&store).unwrap();
        let facts = data::apps_artifacts(&store).unwrap();
        let native = json!({
            "schema": 1, "subset_protocol": 2, "source_protocol": 6,
            "prepared_helper_sha256": facts.helper, "tools": facts.tools,
            "source_read_claim_sha256": "a".repeat(64),
            "source_completion_receipt_sha256": "a".repeat(64),
            "setup_ciphertext_sha256": "a".repeat(64),
            "applications_ciphertext_sha256": "a".repeat(64),
            "receiving": {"schema": 1, "subset_protocol": 2, "deployments": 2, "install_commands": 2,
                          "commits": {"app/org.tilo.Fixture/x86_64/stable": "b".repeat(64),
                                      "runtime/org.tilo.FixtureRuntime/x86_64/stable": "c".repeat(64)},
                          "selected_data_sha256": "d".repeat(64)},
        });
        let cap = json!({
            "schema": 1, "subset_protocol": 2,
            "prepared_helper_sha256": facts.helper,
            "qualification_script_sha256": facts.helper,
            "native_qualification_sha256": hex::encode(Sha256::digest(json::canonical(&native))),
        });
        // Explicit mocked unit data; these files never constitute native evidence.
        let original = json!({
            "schema": 1, "mocked_unit_test_only": true, "original_source_protocol": 6,
            "prepared_helper_sha256": facts.helper, "capture_pins": {"worker_protocol": 6},
        });
        let fixture = Self {
            _temp: temp,
            root,
            store,
            home,
            cap,
            native,
            original,
        };
        fixture.save(HOME, &fixture.home, true);
        fixture.save(APPS, &fixture.cap, true);
        fixture.save(NATIVE, &fixture.native, false);
        fixture.save(ORIGINAL, &fixture.original, true);
        fixture
    }

    fn receipt(&self, name: &str) -> PathBuf {
        self.store.path(&format!("/usr/share/tilo/{name}")).unwrap()
    }

    fn save(&self, name: &str, value: &Value, newline: bool) {
        let mut bytes = json::canonical(value);
        if newline {
            bytes.push(b'\n');
        }
        std::fs::write(self.receipt(name), bytes).unwrap();
    }

    fn answer(&self, command: &[OsString]) -> Vec<u8> {
        let mode = command.last().and_then(|mode| mode.to_str()).unwrap_or("");
        match mode {
            "--home-consumer-capability" => json::canonical(&self.home),
            "--capabilities" => {
                assert_eq!(
                    command,
                    [
                        self.store
                            .path("/usr/libexec/tilo-firstboot-home-seeds")
                            .unwrap()
                            .into_os_string(),
                        "--capabilities".into()
                    ]
                );
                br#"{"schema":1,"seed_protocol":1}"#.to_vec()
            }
            "--reattest-original-source-consumer-capability" => json::canonical(&self.original),
            "--applications-consumer-capability" => json::canonical(&json!({
                "capability": self.cap, "native_qualification": self.native,
                "original_source": self.original,
            })),
            _ => panic!("unexpected production command: {command:?}"),
        }
    }

    fn runner(&self) -> impl FnMut(&[OsString], usize, Duration) -> io::Result<Vec<u8>> + '_ {
        |command, _, _| Ok(self.answer(command))
    }

    fn home_check(&self) -> io::Result<Value> {
        data::home(&self.store, true, &mut self.runner())
    }

    fn apps_check(&self) -> io::Result<Value> {
        data::applications(&self.store, true, &mut self.runner())
    }
}

fn snapshot(fixture: &Fixture) -> Vec<(PathBuf, Vec<u8>, i64, i64)> {
    let mut files = std::fs::read_dir(fixture.store.path("/usr/share/tilo").unwrap())
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let info = std::fs::metadata(&path).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes, info.mtime(), info.mtime_nsec())
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[test]
fn closed_home_and_applications_checks_are_read_only() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture);
    assert_eq!(fixture.home_check().unwrap(), fixture.home);
    assert_eq!(fixture.apps_check().unwrap(), fixture.cap);
    assert_eq!(snapshot(&fixture), before);
}

#[test]
fn receipt_mutations() {
    let fixture = Fixture::new();
    for (name, proof, key) in [
        (HOME, fixture.home.clone(), "archive_protocol"),
        (APPS, fixture.cap.clone(), "subset_protocol"),
    ] {
        let mutations: [&dyn Fn(&mut Value); 5] = [
            &|p| p["unknown"] = true.into(),
            &|p| {
                p.as_object_mut().unwrap().remove("schema");
            },
            &|p| p["schema"] = true.into(),
            &|p| p[key] = 1.into(),
            &|p| p["prepared_helper_sha256"] = "A".repeat(64).into(),
        ];
        for mutate in mutations {
            let mut value = proof.clone();
            mutate(&mut value);
            fixture.save(name, &value, true);
            let result = if name == HOME {
                fixture.home_check()
            } else {
                fixture.apps_check()
            };
            assert!(result.is_err(), "{name}: {value}");
        }
        fixture.save(name, &proof, true);
    }
}

#[test]
fn duplicate_and_oversized_proofs() {
    let fixture = Fixture::new();
    for payload in [
        br#"{"schema":1,"schema":1}"#.to_vec(),
        vec![b' '; 4097],
        b"[]".to_vec(),
    ] {
        std::fs::write(fixture.receipt(HOME), payload).unwrap();
        assert!(fixture.home_check().is_err());
    }
}

#[test]
fn every_actual_artifact_change_refuses() {
    let fixture = Fixture::new();
    let change = |name: &str, check: &dyn Fn() -> io::Result<Value>| {
        let path = fixture.store.path(name).unwrap();
        let data = std::fs::read(&path).unwrap();
        std::fs::write(&path, [data.as_slice(), b"changed"].concat()).unwrap();
        assert!(check().is_err(), "{name}");
        std::fs::write(&path, data).unwrap();
    };
    for name in [
        HELPER,
        "/usr/bin/tar",
        "/usr/libexec/tilo-firstboot-user",
        "/usr/libexec/tilo-firstboot-home-seeds",
    ] {
        change(name, &|| fixture.home_check());
    }
    for name in std::iter::once(HELPER).chain(TOOLS) {
        change(name, &|| fixture.apps_check());
    }
    assert!(fixture.home_check().is_ok() && fixture.apps_check().is_ok());
}

#[test]
fn compiled_firstboot_helper_substitution_during_probe_refuses() {
    let fixture = Fixture::new();
    let mut changed = |command: &[OsString], _: usize, _: Duration| -> io::Result<Vec<u8>> {
        let result = Ok(fixture.answer(command));
        if command
            .last()
            .is_some_and(|mode| mode == "--home-consumer-capability")
        {
            let path = fixture
                .store
                .path("/usr/libexec/tilo-firstboot-files")
                .unwrap();
            std::fs::write(path, "replaced ELF").unwrap();
        }
        result
    };
    let error = data::home(&fixture.store, true, &mut changed).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Home artifacts changed during probe"),
        "{error}"
    );
}

#[test]
fn namespace_redirect_and_mutable_receipt_refuse() {
    let fixture = Fixture::new();
    let path = fixture.receipt(APPS);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(fixture.apps_check().is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let actual = path.with_extension("actual");
    std::fs::rename(&path, &actual).unwrap();
    std::os::unix::fs::symlink(actual.file_name().unwrap(), &path).unwrap();
    assert!(fixture.apps_check().is_err());
    // A foreign-owned namespace edge refuses too.
    let foreign = Store::new(fixture.root.clone(), uid() + 1);
    assert!(data::home(&foreign, true, &mut fixture.runner()).is_err());
}

#[test]
fn native_report_mutations_even_with_recomputed_digest() {
    let fixture = Fixture::new();
    let mutations: [&dyn Fn(&mut Value); 6] = [
        &|p| p["source_protocol"] = 5.into(),
        &|p| p["receiving"]["deployments"] = 1.into(),
        &|p| p["receiving"]["install_commands"] = 4.into(),
        &|p| p["receiving"]["commits"] = json!([]),
        &|p| p["receiving"]["selected_data_sha256"] = "g".repeat(64).into(),
        &|p| p["tools"]["/usr/bin/flatpak"] = "f".repeat(64).into(),
    ];
    for mutate in mutations {
        let mut native = fixture.native.clone();
        mutate(&mut native);
        fixture.save(NATIVE, &native, false);
        let mut cap = fixture.cap.clone();
        cap["native_qualification_sha256"] =
            hex::encode(Sha256::digest(json::canonical(&native))).into();
        fixture.save(APPS, &cap, true);
        assert!(fixture.apps_check().is_err(), "{native}");
    }
    // A newline or other noncanonical native bytes refuse as well.
    fixture.save(APPS, &fixture.cap, true);
    fixture.save(NATIVE, &fixture.native, true);
    assert!(fixture.apps_check().is_err());
}

#[test]
fn signed_cohort_failure_and_during_check_substitution_refuse() {
    let fixture = Fixture::new();
    let mut changed = |command: &[OsString], _: usize, _: Duration| -> io::Result<Vec<u8>> {
        if command
            .last()
            .is_some_and(|mode| mode == "--reattest-original-source-consumer-capability")
        {
            std::fs::write(fixture.store.path("/usr/bin/flatpak").unwrap(), "replaced").unwrap();
        }
        Ok(fixture.answer(command))
    };
    assert!(data::applications(&fixture.store, true, &mut changed).is_err());
    let mut failed = |_: &[OsString], _: usize, _: Duration| -> io::Result<Vec<u8>> {
        Err(io::Error::other("canonical signature verification failed"))
    };
    assert!(data::applications(&fixture.store, true, &mut failed).is_err());
}

#[test]
fn producer_calls_real_factory_mode_and_publishes_completion_last() {
    let fixture = Fixture::new();
    // Any previous receipt refuses a new qualification.
    assert!(data::applications(&fixture.store, false, &mut fixture.runner()).is_err());
    for name in [APPS, NATIVE, ORIGINAL] {
        std::fs::remove_file(fixture.receipt(name)).unwrap();
    }
    let mut modes = Vec::new();
    let mut observed =
        |command: &[OsString], limit: usize, timeout: Duration| -> io::Result<Vec<u8>> {
            let mode = command.last().unwrap().to_str().unwrap().to_owned();
            if mode == "--applications-consumer-capability" {
                assert_eq!(
                    command,
                    [
                        fixture.store.path(HELPER).unwrap().into_os_string(),
                        mode.clone().into()
                    ]
                );
                assert_eq!((limit, timeout), (8192, Duration::from_secs(1800)));
            }
            if mode == "--reattest-original-source-consumer-capability" {
                // Source receipt first; the app capability is the last marker.
                assert!(fixture.receipt(ORIGINAL).exists());
                assert!(!fixture.receipt(NATIVE).exists() && !fixture.receipt(APPS).exists());
            }
            modes.push(mode);
            Ok(fixture.answer(command))
        };
    assert_eq!(
        data::applications(&fixture.store, false, &mut observed).unwrap(),
        fixture.cap
    );
    assert_eq!(
        modes,
        [
            "--applications-consumer-capability",
            "--reattest-original-source-consumer-capability"
        ]
    );
    let mut cap = json::canonical(&fixture.cap);
    cap.push(b'\n');
    assert_eq!(std::fs::read(fixture.receipt(APPS)).unwrap(), cap);
    assert_eq!(
        std::fs::read(fixture.receipt(NATIVE)).unwrap(),
        json::canonical(&fixture.native)
    );
    for name in [APPS, NATIVE, ORIGINAL] {
        let info = std::fs::symlink_metadata(fixture.receipt(name)).unwrap();
        assert_eq!((info.mode() & 0o777, info.nlink()), (0o644, 1), "{name}");
    }
    // The checked receipts now read back, and nothing is ever replaced.
    assert_eq!(fixture.apps_check().unwrap(), fixture.cap);
    assert!(data::applications(&fixture.store, false, &mut fixture.runner()).is_err());
}

#[test]
fn home_producer_publishes_once() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.receipt(HOME)).unwrap();
    assert_eq!(
        data::home(&fixture.store, false, &mut fixture.runner()).unwrap(),
        fixture.home
    );
    assert_eq!(fixture.home_check().unwrap(), fixture.home);
    assert!(data::home(&fixture.store, false, &mut fixture.runner()).is_err());
}

#[test]
fn catalog_facts_include_applications_only_when_attempted() {
    let fixture = Fixture::new();
    let facts = data::catalog_facts(&fixture.store, &mut fixture.runner()).unwrap();
    assert_eq!(
        facts,
        json!({"home_archive": fixture.home, "applications": fixture.cap,
               "original_source": fixture.original})
    );
    std::fs::remove_file(fixture.receipt(APPS)).unwrap();
    // A partial app publication fails closed instead of downgrading.
    assert!(data::catalog_facts(&fixture.store, &mut fixture.runner()).is_err());
    std::fs::remove_file(fixture.receipt(NATIVE)).unwrap();
    assert_eq!(
        data::catalog_facts(&fixture.store, &mut fixture.runner()).unwrap(),
        json!({"home_archive": fixture.home})
    );
}
