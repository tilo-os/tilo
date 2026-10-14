//! `tilo-install gc` and the collection after every change, through the
//! real binary: runtime builds no installed package runs on go, pinned and
//! freshly installed ones stay, and a collection a crash stopped halfway is
//! finished by the next change without a half-deleted build ever being
//! selectable.

use std::path::Path;

use super::{Env, identity_file};

/// Bytes of the regular files under `dir`.
fn disk_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&next) else {
            continue;
        };
        if meta.is_dir() {
            stack.extend(std::fs::read_dir(&next).unwrap().map(|e| e.unwrap().path()));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[test]
fn runtime_updates_leave_only_the_build_the_app_runs_on() {
    // Each update moves the app to a new runtime build; the builds it left
    // stay through the install's grace period and go at the next collection
    // after it.
    const UPDATES: usize = 4;
    const TOOLKIT_BYTES: usize = 256 * 1024;
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    let mut versions = Vec::new();
    for update in 0..UPDATES {
        let name = format!("build{update}");
        let mut toolkit = vec![0u8; TOOLKIT_BYTES];
        toolkit[0] = u8::try_from(update).unwrap();
        let (version, bundle) = env.bundle(&name, &toolkit, &runtime_identity);
        let runtime = format!("tilo-runtime/{version}");
        let package = env.runtime_tapp(&name, &bundle, &runtime);
        let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
        assert!(ok, "{out}");
        let app = env.app_tapp(&runtime);
        let (ok, out) = env.cmd(&[
            "install",
            app.to_str().unwrap(),
            "--allow-self-signed",
            "--force",
        ]);
        assert!(ok, "{out}");
        versions.push(version);
    }
    let root = env.path("data/tilo-runtime");
    let before = disk_bytes(&root);
    for version in &versions {
        assert!(
            env.installed_bundle(version).exists(),
            "every build is still in its grace period: {version}"
        );
    }

    let (current, retired) = versions.split_last().unwrap();

    // Phase `a_dry_run_names_the_builds_and_removes_none`.
    let (ok, out) = env.cmd(&["gc", "--dry-run", "--grace-secs", "0"]);
    assert!(ok, "{out}");
    for version in retired {
        assert!(
            out.contains(&format!("would go : runtime {version}")),
            "{out}"
        );
    }
    assert_eq!(disk_bytes(&root), before);

    // Phase `collection_keeps_only_the_build_an_installed_package_names`.
    let (ok, out) = env.cmd(&["gc", "--grace-secs", "0"]);
    assert!(ok, "{out}");
    let after = disk_bytes(&root);
    println!(
        "RUNTIME-GC updates={UPDATES} toolkit_bytes={TOOLKIT_BYTES} before={before} after={after}"
    );
    for version in retired {
        assert!(!env.installed_bundle(version).exists(), "{version}: {out}");
    }
    assert!(
        out.contains(&format!(
            "kept     : runtime {current} (an installed package runs on it)"
        )),
        "{out}"
    );
    runtime_bundle::verify_bundle(&env.installed_bundle(current))
        .expect("the build in use is untouched");
    assert!(after.saturating_mul(3) < before, "{after} of {before}");
    let (ok, out) = env.cmd(&["runtime", "path", "--version", current.as_str()]);
    assert!(
        ok && out.trim() == env.installed_bundle(current).display().to_string(),
        "{out}"
    );

    // Phase `uninstalling_the_last_app_frees_its_runtime_after_the_grace_period`.
    let (ok, out) = env.cmd(&["uninstall", "so.tilo.linked", "--keep-state"]);
    assert!(ok, "{out}");
    assert!(
        env.installed_bundle(current).exists(),
        "installed within the grace period: {out}"
    );
    let (ok, out) = env.cmd(&["gc", "--grace-secs", "0"]);
    assert!(ok, "{out}");
    assert!(!env.installed_bundle(current).exists(), "{out}");
}

#[test]
fn pinned_and_freshly_installed_runtimes_survive_collection() {
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    let (kept, kept_bundle) = env.bundle("kept", b"toolkit kept", &runtime_identity);
    let (fresh, fresh_bundle) = env.bundle("fresh", b"toolkit fresh", &runtime_identity);
    let pinned = env.runtime_tapp("kept", &kept_bundle, &format!("tilo-runtime/{kept}"));
    let (ok, out) = env.cmd(&[
        "install",
        pinned.to_str().unwrap(),
        "--allow-self-signed",
        "--pinned",
    ]);
    assert!(ok, "{out}");
    let package = env.runtime_tapp("fresh", &fresh_bundle, &format!("tilo-runtime/{fresh}"));
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");

    // Phase `nothing_names_them_and_both_stay`: one is pinned, the other was
    // installed moments ago (an app's first open installs its runtime first).
    let (ok, out) = env.cmd(&["gc"]);
    assert!(ok, "{out}");
    assert!(out.contains(&format!("runtime {kept} (pinned)")), "{out}");
    assert!(
        out.contains(&format!(
            "runtime {fresh} (installed within the grace period)"
        )),
        "{out}"
    );
    assert!(env.installed_bundle(&kept).exists() && env.installed_bundle(&fresh).exists());

    // Phase `past_the_grace_period_only_the_pin_keeps_one`.
    let (ok, out) = env.cmd(&["gc", "--grace-secs", "0"]);
    assert!(ok, "{out}");
    assert!(env.installed_bundle(&kept).exists(), "{out}");
    assert!(!env.installed_bundle(&fresh).exists(), "{out}");

    // Phase `an_unpinned_runtime_goes`.
    let (ok, out) = env.cmd(&["runtime", "unpin", kept.as_str()]);
    assert!(ok, "{out}");
    let (ok, out) = env.cmd(&["gc", "--grace-secs", "0"]);
    assert!(ok, "{out}");
    assert!(!env.installed_bundle(&kept).exists(), "{out}");
}

#[test]
fn an_install_finishes_the_collection_a_crash_left_halfway() {
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    let (version, bundle) = env.bundle("one", b"toolkit build one", &runtime_identity);
    let package = env.runtime_tapp("one", &bundle, &format!("tilo-runtime/{version}"));
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");
    // A collection was killed after taking a build out of use and deleting
    // part of it, and an install died mid-download.
    let root = env.path("data/tilo-runtime");
    let half = root.join(".trash/2026.07+native.dead.1.2");
    std::fs::create_dir_all(half.join("lib")).unwrap();
    std::fs::write(
        half.join("bundle.toml"),
        b"version = \"2026.07+native.dead\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".install-999999/blobs")).unwrap();
    // `runtime path` falls back to where the build would be installed:
    // nothing resolves to the half-deleted copy.
    let (ok, out) = env.cmd(&["runtime", "path", "--version", "2026.07+native.dead"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim(),
        root.join("2026.07+native.dead").display().to_string(),
        "the half-deleted build is never what a lookup finds"
    );
    assert!(!root.join("2026.07+native.dead").exists());

    // Phase `the_next_change_sweeps_the_leftovers`.
    let app = env.app_tapp(&format!("tilo-runtime/{version}"));
    let (ok, out) = env.cmd(&["install", app.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");
    assert!(!half.exists(), "{out}");
    assert!(!root.join(".install-999999").exists(), "{out}");
    assert!(env.installed_bundle(&version).exists(), "{out}");
}
