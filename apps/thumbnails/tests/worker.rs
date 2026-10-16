//! The thumbnails package run by a compute worker, end to end.
//!
//! A device with the packed `so.tilo.thumbnails` installed, and the signed
//! runtime build it names, takes an offloaded `thumbnail-batch` job: the
//! production `ComputeWorker` resolves the package through the device's
//! catalogue, snapshots and re-verifies it, stages the inputs, and launches
//! the entrypoint through `LauncherComputeSandbox`, which is bubblewrap with
//! the Landlock shim inside it. The thumbnails it returns are the very bytes
//! the requesting device would have made itself.
//!
//! No part of the sandbox is relaxed for the test. The launcher re-enters
//! the running executable as its in-sandbox shim, as it re-enters the shell
//! in production, so this target has its own `main`: it answers
//! `__sandbox-exec` the way the shell does, and otherwise runs its cases
//! behind the command line `cargo test` and nextest speak to a test binary.
//!
//! Bubblewrap and user namespaces are how a package runs at all. A machine
//! without them cannot observe what these cases check, so the cases fail
//! there rather than skip.

#[cfg(target_os = "linux")]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
mod cases {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;
    use std::process::{Command, ExitCode};
    use std::sync::{Arc, Mutex};

    use launcher::capture::CaptureCancellation;
    use thumbnail::batch::{
        BatchError, ENTRYPOINT, EXIT_UNDECODABLE, PACKAGE, output_name, thumbnail_batch,
    };
    use tilo_app_package::{Manifest, SigningIdentity, Verification, pack, tapp, verify};
    use tilo_relay::compute::COMPUTE_JOB_SCHEMA_VERSION;
    use tilo_relay::{
        ComputeContentStore, ComputeJobLifecycle, ComputeJobPhase, ComputeJobResult,
        ComputeJobSpec, ComputeOutputSource, ComputeResourceLimits, ComputeWorker, DeviceRole,
        InstalledComputeCatalogue, LauncherComputeSandbox, PowerAvailability, ServerAdvertisement,
        ServiceCapabilities,
    };
    use tilo_test_env::TestHome;

    /// The manifest Release / apps packs, before it is staged.
    const MANIFEST: &str = include_str!("../tilo-app.toml");
    /// The publisher placeholder the release pipeline replaces with its key.
    const PLACEHOLDER: &str =
        "\"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"";
    /// The runtime the authored manifest names a release of, which
    /// packaging stamps with the exact build the package was linked against.
    const RUNTIME_NAME: &str = "tilo-runtime";

    const REQUESTER: [u8; 32] = [1; 32];
    const WORKER: [u8; 32] = [2; 32];
    const WORKSPACE: u64 = 7;

    /// The cases, by the names `cargo test` and nextest select them by.
    const CASES: &[(&str, fn())] = &[
        (
            "an_offloaded_batch_runs_in_the_launcher_sandbox_and_returns_the_local_thumbnails",
            an_offloaded_batch_runs_in_the_launcher_sandbox_and_returns_the_local_thumbnails,
        ),
        (
            "a_non_image_fails_the_offloaded_job_with_its_reason",
            a_non_image_fails_the_offloaded_job_with_its_reason,
        ),
    ];

    /// Content held in memory, by digest: the requester's inputs on one
    /// side, the worker's store of outputs on the other.
    #[derive(Clone, Default)]
    struct MemoryContent(Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>);

    impl MemoryContent {
        fn holding(blobs: &[Vec<u8>]) -> Self {
            let content = Self::default();
            for bytes in blobs {
                content.put(*blake3::hash(bytes).as_bytes(), bytes).unwrap();
            }
            content
        }

        fn is_empty(&self) -> bool {
            self.0.lock().unwrap().is_empty()
        }
    }

    impl ComputeContentStore for MemoryContent {
        fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
            Ok(self.0.lock().unwrap().get(&digest).cloned())
        }

        fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
            self.0.lock().unwrap().insert(digest, bytes.to_vec());
            Ok(())
        }
    }

    type ThumbnailWorker =
        ComputeWorker<MemoryContent, InstalledComputeCatalogue, LauncherComputeSandbox>;

    /// A worker device: its own home, holding a trusted signed runtime build
    /// and the packed thumbnails package installed in its package store,
    /// and the production worker over them.
    struct Device {
        worker: ThumbnailWorker,
        /// The worker's store, where it persists the outputs it reports.
        outputs: MemoryContent,
        /// The signed root of the installed package, as its requester's
        /// registry names it.
        package_digest: [u8; 32],
        // Last: the environment the worker and launcher read is restored,
        // and the tree deleted, only once nothing uses them.
        _home: TestHome,
    }

    impl Device {
        fn new() -> Self {
            require_bubblewrap();
            let mut home = TestHome::new().expect("temporary home");
            // Nothing but this home decides which packages and runtimes
            // exist or are trusted.
            home.env()
                .remove("TILO_APPS_DATA_DIR")
                .remove("TILO_APPS_SYSTEM_DIR")
                .remove(runtime_bundle::RUNTIME_DIR_ENV);
            let runtime = install_runtime(&mut home);
            let package_digest = install_package(&runtime, home.root());

            let outputs = MemoryContent::default();
            let worker = ComputeWorker::new(
                advertisement(),
                home.data().to_path_buf(),
                home.root().join("compute-state"),
                outputs.clone(),
                InstalledComputeCatalogue,
                LauncherComputeSandbox::new(None),
            );
            Self {
                worker,
                outputs,
                package_digest,
                _home: home,
            }
        }

        /// Run job `job` over `inputs`, staged from the requester's bytes,
        /// as the worker runs an offloaded job it was handed.
        fn run(&self, job: u8, inputs: &[Vec<u8>]) -> (ComputeJobSpec, ComputeJobResult) {
            let now_ms = u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
            )
            .unwrap();
            // The bounds Photos' Make thumbnails declares for its offload.
            let spec = ComputeJobSpec {
                version: COMPUTE_JOB_SCHEMA_VERSION,
                job_id: [job; 16],
                requester: REQUESTER,
                worker: WORKER,
                workspace_id: WORKSPACE,
                app_id: PACKAGE.into(),
                package_digest: self.package_digest,
                entrypoint: ENTRYPOINT.into(),
                arguments: Vec::new(),
                inputs: inputs
                    .iter()
                    .map(|bytes| *blake3::hash(bytes).as_bytes())
                    .collect(),
                limits: ComputeResourceLimits {
                    cpu_time_ms: 120_000,
                    memory_bytes: 1024 * 1024 * 1024,
                    output_bytes: 16 * 1024 * 1024,
                    gpu: false,
                },
                issued_at_ms: now_ms,
                expires_at_ms: now_ms.saturating_add(270_000),
            };
            let result = self
                .worker
                .execute_with_inputs(
                    &MemoryContent::holding(inputs),
                    (REQUESTER, WORKSPACE),
                    &spec,
                    now_ms,
                    CaptureCancellation::default(),
                )
                .expect("the worker admits, stages and launches the job");
            (spec, result)
        }
    }

    /// Bubblewrap is the package sandbox; without it nothing here can be
    /// observed, which is a broken environment, not a reason to skip.
    fn require_bubblewrap() {
        let probe = Command::new("bwrap").arg("--version").output();
        assert!(
            probe.as_ref().is_ok_and(|output| output.status.success()),
            "bwrap is not runnable ({probe:?}). It is how every Tilo package runs, so a \
             machine without it cannot run these cases."
        );
    }

    /// What the worker offers for the thumbnails package, to its executor.
    fn advertisement() -> ServerAdvertisement {
        ServerAdvertisement {
            device: WORKER,
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities {
                compute: true,
                ..ServiceCapabilities::default()
            },
            workspaces: BTreeSet::from([WORKSPACE]),
            apps: BTreeSet::from([PACKAGE.to_string()]),
            accelerators: BTreeSet::new(),
            priority: 0,
            latency_ms: 0,
            power: PowerAvailability::External,
            energy_saver: false,
            active_assignments: 0,
            assignment_capacity: 1,
            online: true,
        }
    }

    /// The runtime release the authored manifest names: `2026.07` of
    /// `tilo-runtime/2026.07`.
    fn declared_release() -> String {
        let manifest = Manifest::from_toml_str(MANIFEST).expect("the authored manifest parses");
        let runtime = manifest
            .package
            .runtime
            .expect("the manifest names a runtime");
        runtime
            .strip_prefix(RUNTIME_NAME)
            .and_then(|rest| rest.strip_prefix('/'))
            .expect("the manifest names the Tilo runtime")
            .to_string()
    }

    /// Bake the declared runtime release from this machine's loader and C
    /// library, as the release pipeline bakes it from its own, sign it,
    /// install it where the launcher looks for a per-user runtime, and trust
    /// its publisher. Returns the exact build,
    /// `tilo-runtime/<release>+native.<digest>`.
    fn install_runtime(home: &mut TestHome) -> String {
        let identity = runtime_bundle::Identity::generate();
        let identity_file = home.root().join("runtime-publisher.json");
        std::fs::write(&identity_file, identity.to_json_bytes()).unwrap();
        let (bundle, _dir) = runtime_bundle::build_abi_bundle(
            &home.data().join(RUNTIME_NAME),
            &declared_release(),
            &runtime_bundle::BuildOptions {
                from: None,
                arch: None,
                identity: Some(&identity_file),
                extra_libs: &[],
            },
        )
        .expect("the runtime build bakes from this machine's loader and C library");
        let trusted = home.root().join("runtime-publishers.txt");
        std::fs::write(&trusted, format!("{}\n", identity.public_key_hex())).unwrap();
        home.env()
            .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", trusted);
        format!("{RUNTIME_NAME}/{}", bundle.version)
    }

    /// Stage the real manifest and the built `thumbnail-batch` the way
    /// `dist/apps/package-apps.sh` does (its publisher key, then the runtime
    /// build stamped over the authored release), pack and sign it, and
    /// install the `.tapp` into the package store the environment names,
    /// admitting it only once it verifies against its publisher. Returns
    /// the installed package's signed root.
    fn install_package(runtime: &str, scratch: &Path) -> [u8; 32] {
        let staging = scratch.join("staging");
        std::fs::create_dir_all(staging.join("bin")).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_thumbnail-batch"),
            staging.join("bin").join(ENTRYPOINT),
        )
        .unwrap();

        let identity = SigningIdentity::generate();
        let publisher = identity.public_key_bytes();
        assert_eq!(
            MANIFEST.matches(PLACEHOLDER).count(),
            1,
            "the manifest names the publisher placeholder the release pipeline stages"
        );
        let staged = stamp_runtime(
            &MANIFEST.replace(
                PLACEHOLDER,
                &format!("\"ed25519:{}\"", hex::encode(publisher)),
            ),
            runtime,
        );
        let manifest = Manifest::from_toml_str(&staged).expect("the staged manifest parses");
        let packed = pack(&staging, &manifest, &identity).expect("the package packs");
        let tapp = tapp::to_bytes(&packed.meta, Some(&packed)).expect("the tapp writes");

        let store = package_store::PackageStore::from_env().expect("the user's package store");
        let installed = store
            .install(
                tapp.as_slice(),
                |meta| match verify(meta, &[publisher]) {
                    Verification::Trusted { .. } => Ok(package_store::Admission::default()),
                    other => Err(format!("the package does not verify: {other:?}")),
                },
                None,
            )
            .expect("the package installs");
        assert_eq!(installed.package.id, PACKAGE);
        assert_eq!(installed.package.cli_commands, vec![ENTRYPOINT.to_string()]);
        *packed.id().as_bytes()
    }

    /// `os/scripts/stamp-runtime.sh`: replace the one line declaring the
    /// authored release with the exact build.
    fn stamp_runtime(manifest: &str, runtime: &str) -> String {
        let release = format!("\"{RUNTIME_NAME}/{}\"", declared_release());
        let mut stamped = 0_usize;
        let mut staged = String::with_capacity(manifest.len());
        for line in manifest.lines() {
            let is_runtime_line = line
                .split_once('=')
                .is_some_and(|(key, value)| key.trim() == "runtime" && value.trim() == release);
            if is_runtime_line {
                stamped = stamped.saturating_add(1);
                staged.push_str("runtime = \"");
                staged.push_str(runtime);
                staged.push('"');
            } else {
                staged.push_str(line);
            }
            staged.push('\n');
        }
        assert_eq!(stamped, 1, "the manifest declares its runtime release once");
        staged
    }

    /// A small picture with distinct pixels, encoded as `format`.
    fn picture(width: u32, height: u32, seed: u8, format: image::ImageFormat) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap() ^ seed,
                u8::try_from(y % 256).unwrap(),
                seed,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn an_offloaded_batch_runs_in_the_launcher_sandbox_and_returns_the_local_thumbnails() {
        let device = Device::new();
        let inputs = vec![
            picture(640, 480, 3, image::ImageFormat::Jpeg),
            picture(300, 900, 41, image::ImageFormat::Png),
            picture(64, 64, 200, image::ImageFormat::Png),
        ];
        let (spec, result) = device.run(0x51, &inputs);

        let expected = thumbnail_batch(&inputs).expect("the local batch thumbnails every input");
        let expected_digests: Vec<[u8; 32]> = expected
            .iter()
            .map(|thumbnail| *blake3::hash(thumbnail).as_bytes())
            .collect();
        assert_eq!(
            result.lifecycle,
            ComputeJobLifecycle {
                job_id: spec.job_id,
                sequence: 2,
                phase: ComputeJobPhase::Completed {
                    outputs: expected_digests.clone(),
                },
            },
            "the job completes with the local batch's thumbnails ({:?}, in that order); \
             stderr: {}",
            (0..inputs.len()).map(output_name).collect::<Vec<_>>(),
            String::from_utf8_lossy(&result.stderr)
        );
        // What the requester then fetches from the worker, byte for byte.
        for (digest, thumbnail) in expected_digests.iter().zip(&expected) {
            let returned = device
                .worker
                .retained_output(REQUESTER, WORKSPACE, spec.job_id, *digest)
                .expect("the worker reads its retained output");
            assert_eq!(
                returned.as_ref(),
                Some(thumbnail),
                "the requester receives the thumbnail the local batch makes"
            );
        }
    }

    fn a_non_image_fails_the_offloaded_job_with_its_reason() {
        let device = Device::new();
        let inputs = vec![
            picture(32, 32, 9, image::ImageFormat::Png),
            b"not a picture".to_vec(),
        ];
        assert_eq!(
            thumbnail_batch(&inputs),
            Err(BatchError::Undecodable { index: 1 }),
            "the local batch refuses it too"
        );
        let (spec, result) = device.run(0x52, &inputs);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(
            result.lifecycle,
            ComputeJobLifecycle {
                job_id: spec.job_id,
                sequence: 2,
                phase: ComputeJobPhase::Failed {
                    code: format!("exit_{EXIT_UNDECODABLE}"),
                },
            },
            "the batch fails whole, with the entrypoint's exit status; stderr: {stderr}"
        );
        assert!(
            stderr.contains("input 1 is not an image"),
            "the worker returns the reason to the requester: {stderr}"
        );
        assert!(
            device.outputs.is_empty(),
            "a failed batch persists no thumbnail"
        );
    }

    /// The parts of the libtest command line `cargo test` and nextest use
    /// that a custom harness must answer: `--list` (with `--format terse`,
    /// and with `--ignored` for the ignored cases, of which there are
    /// none), name filters with or without `--exact`, and `--skip`.
    struct Selection {
        list: bool,
        ignored_only: bool,
        exact: bool,
        filters: Vec<String>,
        skips: Vec<String>,
    }

    impl Selection {
        fn parse(args: &[String]) -> Self {
            let mut selection = Self {
                list: false,
                ignored_only: false,
                exact: false,
                filters: Vec::new(),
                skips: Vec::new(),
            };
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--list" => selection.list = true,
                    "--ignored" => selection.ignored_only = true,
                    "--exact" => selection.exact = true,
                    "--skip" => selection.skips.extend(args.next().cloned()),
                    // Options with a value this harness has no use for.
                    "--format" | "--test-threads" | "--color" | "--logfile" | "-Z" => {
                        args.next();
                    }
                    other if other.starts_with('-') => {}
                    filter => selection.filters.push(filter.to_string()),
                }
            }
            selection
        }

        fn selects(&self, name: &str) -> bool {
            let matches = |pattern: &String| {
                if self.exact {
                    name == pattern
                } else {
                    name.contains(pattern.as_str())
                }
            };
            !self.ignored_only
                && (self.filters.is_empty() || self.filters.iter().any(matches))
                && !self.skips.iter().any(matches)
        }
    }

    /// Answer the launcher's shim, or list or run the selected cases.
    pub(super) fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        // The launcher runs this executable inside the sandbox to apply
        // Landlock before the entrypoint, as it runs the shell in production.
        if let Some((command, rest)) = args.split_first()
            && command == launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND
        {
            launcher::landlock_shim::run_sandbox_exec(rest);
        }
        let selection = Selection::parse(&args);
        let selected = CASES.iter().filter(|(name, _)| selection.selects(name));
        if selection.list {
            for (name, _) in selected {
                println!("{name}: test");
            }
            return ExitCode::SUCCESS;
        }
        let mut ran = 0_usize;
        let mut failed = Vec::new();
        for &(name, case) in selected {
            ran = ran.saturating_add(1);
            println!("test {name} ...");
            if std::panic::catch_unwind(case).is_ok() {
                println!("test {name} ... ok");
            } else {
                println!("test {name} ... FAILED");
                failed.push(name);
            }
        }
        println!(
            "\ntest result: {}. {} passed; {} failed",
            if failed.is_empty() { "ok" } else { "FAILED" },
            ran.saturating_sub(failed.len()),
            failed.len()
        );
        if failed.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    cases::main()
}

/// The worker's launcher sandbox exists only on Linux; elsewhere this target
/// has no cases.
#[cfg(not(target_os = "linux"))]
fn main() {}
