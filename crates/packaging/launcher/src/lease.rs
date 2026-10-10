//! What a launch holds from choosing its trees until its sandbox exits.
//!
//! An update of the app being opened retires the payload tree the launch
//! chose, and the collection after it removes the runtime build that tree
//! ran on. The mount table protects both once bwrap has bound them, but not
//! between the choice and the bind. So before a launch looks at either it
//! takes a shared lease on each: its payload tree
//! ([`package_store::lease`]) and every per-user runtime build its manifest
//! names ([`runtime_bundle::gc::BundleRoot::hold_launch`]). It keeps them
//! until the sandbox exits; the kernel drops them with the launcher's
//! descriptors however it ends, and they are close-on-exec, so the sandbox
//! never sees them.
//!
//! A remover takes the same lease exclusively without waiting and keeps
//! what is held. A launch that arrives while a remover holds a tree waits
//! for that one removal and then finds the tree whole or gone: gone is a
//! payload refused here as changed, or a runtime the lookup after this
//! reports missing. Neither is a half-deleted tree in a sandbox.
//!
//! Cost: one `open` and one `flock` per tree, with nothing to wait for
//! unless a removal of that very tree is under way.

use std::path::Path;

use advisory_lock::FileLock;
use runtime_bundle::gc::BundleRoot;

use crate::manifest::ManifestView;
use crate::run::{RunError, RunOptions};

/// Where [`LaunchLeases::take_observed`] has got to, for a test that
/// interleaves an update with a launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The payload tree is held and still there; no runtime is held yet.
    PayloadHeld,
    /// Every lease is held; the runtime lookup comes next.
    RuntimesHeld,
}

/// The leases of one launch. Dropping it releases them.
#[derive(Debug)]
#[must_use = "a launch holds its trees until its sandbox exits"]
pub struct LaunchLeases {
    _held: Vec<FileLock>,
}

impl LaunchLeases {
    /// Hold the payload tree at `payload_root` and every per-user runtime
    /// build `manifest` (or an explicit `opts.runtime_root`) names. Call it
    /// before the runtime is looked up and keep it until the sandbox exits.
    ///
    /// # Errors
    ///
    /// [`RunError::PackageChanged`] when an update removed the payload tree
    /// before it could be held, [`RunError::LaunchLease`] when a lease file
    /// cannot be created or locked.
    pub fn take(
        manifest: &ManifestView,
        payload_root: &Path,
        opts: &RunOptions,
    ) -> Result<Self, RunError> {
        Self::take_observed(manifest, payload_root, opts, &mut |_| {})
    }

    /// [`Self::take`], calling `observe` at each [`Stage`].
    pub(crate) fn take_observed(
        manifest: &ManifestView,
        payload_root: &Path,
        opts: &RunOptions,
        observe: &mut dyn FnMut(Stage),
    ) -> Result<Self, RunError> {
        let mut held = Vec::new();
        if let Some(lease) = hold_payload(payload_root)? {
            held.push(lease);
        }
        if !payload_root.is_dir() {
            return Err(RunError::PackageChanged {
                package: manifest.package_id.clone(),
                path: payload_root.to_path_buf(),
            });
        }
        observe(Stage::PayloadHeld);
        if let Some(bundles) = per_user_bundles() {
            for version in runtime_versions(manifest, opts, bundles.path()) {
                let lease =
                    bundles
                        .hold_launch(&version)
                        .map_err(|source| RunError::LaunchLease {
                            path: bundles.bundle(&version),
                            source,
                        })?;
                held.push(lease);
            }
        }
        observe(Stage::RuntimesHeld);
        Ok(Self { _held: held })
    }
}

/// Hold the payload tree at `payload_root` when it is one of the user's
/// store; see [`package_store::lease::hold_payload`].
///
/// # Errors
///
/// [`RunError::LaunchLease`] when the lease cannot be created or locked.
pub fn hold_payload(payload_root: &Path) -> Result<Option<FileLock>, RunError> {
    package_store::lease::hold_payload(payload_root).map_err(|source| RunError::LaunchLease {
        path: payload_root.to_path_buf(),
        source,
    })
}

/// How many times [`find_installed_held`] looks again after an update
/// removed the tree it found.
const RESELECTIONS: usize = 3;

/// Look `package` up in the installed set and hold its payload tree. An
/// update that removed the tree between the lookup and the hold makes it
/// look again, so the launch opens the build that replaced it.
///
/// # Errors
///
/// Returns the catalogue's error, or one from taking the lease.
pub fn find_installed_held(
    package: &str,
) -> Result<Option<(package_store::InstalledPackage, Option<FileLock>)>, String> {
    let mut found = None;
    for _ in 0..=RESELECTIONS {
        let Some(row) = package_store::find_installed(package).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let lease = hold_payload(&row.payload_root).map_err(|e| e.to_string())?;
        if row.payload_root.is_dir() && row.manifest_path.is_file() {
            return Ok(Some((row, lease)));
        }
        found = Some((row, lease));
    }
    // Still changing: the launch reports the build gone.
    Ok(found)
}

/// The per-user runtime root, when it exists: builds elsewhere (the
/// image's) are never removed, and a root that does not exist holds none.
fn per_user_bundles() -> Option<BundleRoot> {
    runtime_bundle::bundle_root_from(package_store::dev_env::dev_override(
        runtime_bundle::RUNTIME_DIR_ENV,
    ))
    .ok()
    .filter(|root| root.is_dir())
    .map(BundleRoot::new)
}

/// The runtime versions a launch of `manifest` may mount from `root`: an
/// explicit runtime directory there, else the manifest's runtime, and its
/// optional runtimes. Malformed references are left to the lookup to
/// refuse.
fn runtime_versions(manifest: &ManifestView, opts: &RunOptions, root: &Path) -> Vec<String> {
    let version_of = |raw: &str| {
        raw.split_once('/')
            .map(|(_, version)| version.to_string())
            .filter(|version| crate::run::valid_runtime_version(version))
    };
    let mut versions: Vec<String> = match &opts.runtime_root {
        Some(explicit) => explicit
            .parent()
            .filter(|parent| *parent == root)
            .and_then(|_| explicit.file_name()?.to_str().map(str::to_owned))
            .filter(|version| crate::run::valid_runtime_version(version))
            .into_iter()
            .collect(),
        None => manifest
            .runtime
            .as_deref()
            .and_then(version_of)
            .into_iter()
            .collect(),
    };
    versions.extend(
        manifest
            .optional_runtimes
            .iter()
            .filter_map(|raw| version_of(raw)),
    );
    versions.sort();
    versions.dedup();
    versions
}

#[cfg(test)]
mod tests {
    //! An app opened while it is being updated, interleaved deterministically:
    //! the update (a store install of v2 on another runtime build, then the
    //! collection `tilo-install` runs after every change, with no grace) runs
    //! at each point of the launch, and whatever the launch goes on to mount
    //! must be whole.

    use std::path::{Path, PathBuf};

    use super::*;
    use crate::manifest::read_manifest;

    const APP: &str = "so.tilo.race";
    const OLD_RUNTIME: &str = "2026.10.1";
    const NEW_RUNTIME: &str = "2026.10.2";

    /// Where the update lands in the launch.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum At {
        /// After the launch read the installed row, before any lease.
        Selected,
        /// At a stage of [`LaunchLeases::take_observed`].
        Lease(Stage),
        /// After the runtime lookup verified the build, before bwrap binds.
        Binding,
    }

    struct Fixture {
        dir: tempfile::TempDir,
        _env: tilo_test_env::ScopedEnv,
        store: package_store::PackageStore,
        v2: Vec<u8>,
        opts: RunOptions,
        publisher: String,
    }

    impl Fixture {
        fn runtimes(&self) -> PathBuf {
            self.dir.path().join("runtimes")
        }

        /// `tilo-install update`: v2 replaces v1, then the collection after
        /// the change.
        fn update(&self) {
            self.store
                .install(
                    self.v2.as_slice(),
                    |_| Ok(package_store::Admission::default()),
                    None,
                )
                .unwrap();
            self.collect();
        }

        fn collect(&self) {
            let catalog = package_store::Catalog::at(self.store.layout().root(), None);
            let request = install::gc::Request {
                grace_secs: 0,
                ..install::gc::Request::after_change(0)
            };
            let runtimes = BundleRoot::new(self.runtimes());
            install::gc::collect(&self.store, &catalog, &runtimes, request).unwrap();
        }
    }

    fn signed_runtime(work: &Path, out: &Path, version: &str, identity: &Path, seed: u8) {
        let host = work.join(format!("host-{version}"));
        let loader = runtime_bundle::host_minimal_paths_for(std::env::consts::ARCH).unwrap()[0]
            .trim_start_matches('/');
        std::fs::create_dir_all(host.join(loader).parent().unwrap()).unwrap();
        std::fs::write(host.join(loader), [seed; 64]).unwrap();
        let toolkit = work.join(format!("toolkit-{version}"));
        std::fs::create_dir_all(&toolkit).unwrap();
        let dylib = toolkit.join("libruntime_dylib.so");
        std::fs::write(&dylib, vec![seed; 3 * 4096]).unwrap();
        runtime_bundle::build_bundle_with(
            out,
            version,
            &runtime_bundle::BuildOptions {
                from: Some(&host),
                arch: Some(std::env::consts::ARCH),
                identity: Some(identity),
                extra_libs: &[dylib],
            },
        )
        .unwrap();
    }

    fn app(work: &Path, version: &str, runtime: &str, seed: u8) -> Vec<u8> {
        let identity = tilo_app_package::SigningIdentity::from_signing_key(
            ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        );
        let payload = work.join(format!("payload-{version}"));
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::create_dir_all(payload.join("share")).unwrap();
        std::fs::write(payload.join("bin/app"), vec![seed; 20_000]).unwrap();
        std::fs::write(payload.join("share/assets.dat"), vec![!seed; 50_000]).unwrap();
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "{APP}"
version = "{version}"
kind = "app"
runtime = "tilo-runtime/{runtime}"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
        tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
    }

    /// v1 on the old runtime installed, v2 on the new runtime ready to
    /// replace it; both runtime builds installed.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = work.join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        for (version, seed) in [(OLD_RUNTIME, 1u8), (NEW_RUNTIME, 2u8)] {
            let out = root.join("runtimes").join(version);
            signed_runtime(&work, &out, version, &identity_path, seed);
        }
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", root.join("store"));
        env.set(
            package_store::layout::SYSTEM_DIR_ENV,
            root.join("no-system-store"),
        );
        env.set(runtime_bundle::RUNTIME_DIR_ENV, root.join("runtimes"));
        env.set(
            crate::dev_env::TRUSTED_PUBLISHERS_FILE_ENV,
            root.join("no-trusted-publishers"),
        );
        let store = package_store::PackageStore::at(root.join("store"));
        store
            .install(
                app(&work, "1.0.0", OLD_RUNTIME, 10).as_slice(),
                |_| Ok(package_store::Admission::default()),
                None,
            )
            .unwrap();
        let v2 = app(&work, "2.0.0", NEW_RUNTIME, 20);
        let opts = RunOptions {
            require_runtime_publisher: Some(identity.public_key_hex()),
            ..Default::default()
        };
        Fixture {
            dir,
            _env: env,
            store,
            v2,
            opts,
            publisher: identity.public_key_hex(),
        }
    }

    /// What a launch did.
    #[derive(Debug, PartialEq, Eq)]
    enum Opened {
        /// It bound this version's payload on its runtime, both whole.
        Mounted(String),
        /// The update removed the build it chose before it could hold it.
        Changed,
    }

    /// One launch as the launcher runs it: look the app up, hold its trees,
    /// look the runtime up and verify it, then check what bwrap would bind
    /// is whole. `update` runs once, at `at`.
    fn open(fx: &Fixture, at: Option<At>) -> Opened {
        let update_at = |point: At| {
            if at == Some(point) {
                fx.update();
            }
        };
        let row = package_store::find_installed(APP).unwrap().unwrap();
        let manifest = read_manifest(&row.manifest_path).unwrap();
        update_at(At::Selected);
        let leases = match LaunchLeases::take_observed(
            &manifest,
            &row.payload_root,
            &fx.opts,
            &mut |stage| update_at(At::Lease(stage)),
        ) {
            Err(RunError::PackageChanged { .. }) => return Opened::Changed,
            other => other.unwrap(),
        };
        let (bundle, version) = crate::run::resolve_runtime(&manifest, &fx.opts)
            .unwrap()
            .expect("the runtime the payload names is found");
        assert_eq!(
            Some(version.as_str()),
            manifest.runtime_ref().map(|(_, v)| v)
        );
        update_at(At::Binding);
        // What bwrap binds: the payload tree and the runtime bundle, whole.
        assert!(
            package_store::integrity::verify(&row, &row.payload_root)
                .unwrap()
                .is_intact(),
            "the payload {} is whole",
            row.version
        );
        runtime_bundle::verify_bundle_with(
            &bundle,
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(fx.publisher.clone()),
                require_signature: true,
                require_arch: None,
            },
        )
        .unwrap();
        drop(leases);
        Opened::Mounted(row.version)
    }

    #[test]
    fn launch_and_update_interleave_deterministically() {
        for at in [
            At::Selected,
            At::Lease(Stage::PayloadHeld),
            At::Lease(Stage::RuntimesHeld),
            At::Binding,
        ] {
            let fx = fixture();
            let opened = open(&fx, Some(at));
            if at == At::Selected {
                // Nothing held the old build yet, so the update removed it
                // whole; the launch refuses it rather than bind a gap, and
                // the next look finds the new build.
                assert_eq!(opened, Opened::Changed, "{at:?}");
                let (row, held) = find_installed_held(APP).unwrap().unwrap();
                assert_eq!(row.version, "2.0.0");
                drop(held);
                assert_eq!(open(&fx, None), Opened::Mounted("2.0.0".into()));
            } else {
                // Once the payload is held, the launch runs the build it
                // chose, on that build's runtime, both whole.
                assert_eq!(opened, Opened::Mounted("1.0.0".into()), "{at:?}");
            }
            // With the launch gone, the next collection takes what it held.
            fx.collect();
            assert!(
                !fx.runtimes().join(OLD_RUNTIME).exists(),
                "{at:?}: the old runtime goes once nothing holds it"
            );
            assert!(fx.runtimes().join(NEW_RUNTIME).exists());
            let trees = std::fs::read_dir(fx.store.layout().packages())
                .unwrap()
                .count();
            assert_eq!(trees, 2, "{at:?}: only v2's tree and record are left");
        }
    }

    #[test]
    fn a_launch_holds_only_what_can_be_removed_and_refuses_a_vanished_payload() {
        let fx = fixture();
        let row = package_store::find_installed(APP).unwrap().unwrap();
        let manifest = read_manifest(&row.manifest_path).unwrap();
        // A tree outside the user's store (the image's) is never removed, so
        // only the runtime is held.
        let elsewhere = fx.dir.path().join("work/payload-1.0.0");
        let leases = LaunchLeases::take(&manifest, &elsewhere, &fx.opts).unwrap();
        assert!(!fx.store.layout().root().join("leases").exists());
        let launches = fx.runtimes().join(runtime_bundle::gc::LAUNCHES_DIR);
        assert!(launches.join(OLD_RUNTIME).exists());
        drop(leases);
        let gone = fx.store.layout().packages().join("0".repeat(64));
        assert!(matches!(
            LaunchLeases::take(&manifest, &gone, &fx.opts),
            Err(RunError::PackageChanged { .. })
        ));
    }
}
