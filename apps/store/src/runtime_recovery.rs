//! Optional runtime repair through the existing authenticated package custodian.
//! Inspection resolves only metadata; acquisition requires the exact review.

use app_host::runtime_transaction::RuntimeReview;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeTarget {
    pub app: String,
    pub runtime: String,
}

impl RuntimeTarget {
    /// Validate an exact consuming app and runtime reference.
    ///
    /// # Errors
    /// Rejects path, URL, escaping and argument-like references.
    pub fn new(app: String, runtime: String) -> Result<Self, String> {
        let valid_part = |part: &str| {
            !part.is_empty()
                && part.len() <= 128
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        };
        let mut parts = runtime.split('/');
        if !crate::reports::valid_app_id(&app)
            || !app.starts_with(|ch: char| ch.is_ascii_alphanumeric())
            || !app.contains('.')
            || !app.split('.').all(valid_part)
            || !parts.next().is_some_and(valid_part)
            || !parts.next().is_some_and(valid_part)
            || parts.next().is_some()
        {
            return Err(ui::i18n!("store-runtime-invalid"));
        }
        Ok(Self { app, runtime })
    }

    /// Parse the exact Store runtime handoff without percent-decoding.
    ///
    /// # Errors
    /// Rejects unsupported or malformed handoffs.
    pub fn from_uri(uri: &str) -> Result<Self, String> {
        let rest = uri
            .strip_prefix("tilo-store:runtime/")
            .ok_or_else(|| ui::i18n!("store-runtime-invalid"))?;
        let (app, runtime) = rest
            .split_once('/')
            .ok_or_else(|| ui::i18n!("store-runtime-invalid"))?;
        Self::new(app.into(), runtime.into())
    }
}

/// Runs exclusively on the Store worker, never by reading host install paths.
///
/// # Errors
/// Returns host admission failures, unavailable service, or missing installed root.
pub fn inspect(
    host: Option<&app_host::client::BlockingClient>,
    target: &RuntimeTarget,
) -> Result<RuntimeReview, String> {
    let host = host.ok_or_else(|| ui::i18n!("store-runtime-repair-unavailable"))?;
    let root = host
        .list()?
        .into_iter()
        .find(|package| package.package.id == target.app)
        .and_then(|package| package.package.package_id())
        .ok_or_else(|| ui::i18n!("store-runtime-not-installed"))?;
    host.inspect_optional_runtime(&target.app, root, &target.runtime)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Inspecting,
    Review(RuntimeReview),
    Acquiring(RuntimeReview),
    Complete(RuntimeReview),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Recovery {
    pub target: RuntimeTarget,
    pub phase: Phase,
    pub visible: bool,
}

impl Recovery {
    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Inspecting | Phase::Acquiring(_))
    }

    pub fn reviewed(&mut self, result: Result<RuntimeReview, String>) {
        self.phase = match result {
            Ok(review)
                if review.consumer_app != self.target.app
                    || review.runtime != self.target.runtime =>
            {
                Phase::Failed(ui::i18n!("store-runtime-review-required"))
            }
            Ok(review) if review.offer.is_none() => Phase::Complete(review),
            Ok(review) => Phase::Review(review),
            Err(error) => Phase::Failed(error),
        };
    }

    /// The exact metadata displayed to the person is also required from agents.
    ///
    /// # Errors
    /// Rejects missing, altered, oversized or already-consumed reviews.
    pub fn consent(&mut self, encoded: &str) -> Result<RuntimeReview, String> {
        if encoded.len() > 16 * 1024 {
            return Err(ui::i18n!("store-runtime-review-required"));
        }
        let candidate: RuntimeReview = serde_json::from_str(encoded)
            .map_err(|_| ui::i18n!("store-runtime-review-required"))?;
        let Phase::Review(review) = &self.phase else {
            return Err(ui::i18n!("store-runtime-review-required"));
        };
        if candidate != *review || candidate.offer.is_none() {
            return Err(ui::i18n!("store-runtime-review-required"));
        }
        self.phase = Phase::Acquiring(candidate.clone());
        Ok(candidate)
    }

    pub fn acquired(&mut self, result: Result<(), String>) {
        self.phase = match (&self.phase, result) {
            (Phase::Acquiring(review), Ok(())) => Phase::Complete(review.clone()),
            (_, Err(error)) => Phase::Failed(error),
            _ => return,
        };
    }

    pub fn inspection(&self) -> serde_json::Value {
        let (phase, review, error) = match &self.phase {
            Phase::Inspecting => ("inspecting", None, None),
            Phase::Review(review) => ("review", Some(review), None),
            Phase::Acquiring(review) => ("acquiring", Some(review), None),
            Phase::Complete(review) => ("complete", Some(review), None),
            Phase::Failed(error) => ("failed", None, Some(error)),
        };
        serde_json::json!({"app": self.target.app, "runtime": self.target.runtime,
            "phase": phase, "review": review, "error": error, "visible": self.visible,
            "can_cancel": false})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Signed fixtures exercise the worker resolver and privileged declaration
    // admission together, rather than injecting an already-reviewed UI state.
    fn signed_bundle(root: &std::path::Path) -> String {
        let identity = runtime_bundle::Identity::generate();
        let identity_path = root.join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        std::fs::write(
            root.join("trusted.txt"),
            format!("{}\n", identity.public_key_hex()),
        )
        .unwrap();
        let host = root.join("host");
        let loader = runtime_bundle::host_minimal_paths_for(runtime_bundle::host_arch())
            .unwrap()
            .first()
            .unwrap()
            .trim_start_matches('/');
        std::fs::create_dir_all(host.join(loader).parent().unwrap()).unwrap();
        std::fs::write(host.join(loader), b"fixture loader").unwrap();
        let dylib = root.join("libruntime_dylib.so");
        std::fs::write(&dylib, b"fixture native toolkit").unwrap();
        let (manifest, _) = runtime_bundle::build_abi_bundle(
            &root.join("runtimes"),
            "2026.07",
            &runtime_bundle::BuildOptions {
                from: Some(&host),
                arch: None,
                identity: Some(&identity_path),
                extra_libs: &[dylib],
                extra_bins: &[],
            },
        )
        .unwrap();
        format!("tilo-runtime/{}", manifest.version)
    }

    fn signed_consumer(root: &std::path::Path, runtime: &str) -> Vec<u8> {
        let publisher = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
        let isa = match std::env::consts::ARCH {
            "aarch64" => "armv8-a",
            _ => "baseline",
        };
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.runtime-fixture"
version = "1.0.0"
kind = "app"
name = "Runtime fixture"
optional-runtimes = ["{runtime}"]
[package.isa]
{arch} = "{isa}"
[publisher]
identity = "ed25519:{publisher}"
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
            arch = std::env::consts::ARCH,
            publisher = hex::encode(publisher.verifying_key().to_bytes()),
        ))
        .unwrap();
        let payload = root.join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), b"#!/bin/true\n").unwrap();
        let packed = tilo_app_package::pack::pack(
            &payload,
            &manifest,
            &tilo_app_package::pack::SigningIdentity::from_signing_key(publisher),
        )
        .unwrap();
        tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
    }

    fn offline_signed_env(root: &std::path::Path) -> tilo_test_env::ScopedEnv {
        std::fs::write(root.join("registries.toml"), "registry = []\n").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", root)
            .set("TILO_STORE_CACHE_DIR", root.join("cache"))
            .set("TILO_APPS_DATA_DIR", root.join("apps"))
            .set("TILO_RUNTIME_BUNDLE_DIR", root.join("runtimes"))
            .set(
                "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
                root.join("trusted.txt"),
            )
            .set("XDG_DATA_HOME", root.join("data"))
            .set("FLATHUB_API_BASE", "http://127.0.0.1:9")
            .set("TILO_STORE_HTTP_TIMEOUT", "1");
        assert!(app_cli::config::load_registries().registries.is_empty());
        env
    }

    #[test]
    fn runtime_signed_fixture_resolves_through_store_actions_offline() {
        use crate::{StoreApp, StoreEffect, StoreShell};
        use package_store::{Catalog, PackageStore, SystemStore};
        use std::sync::Arc;
        use ui::framework::testing::TestApp;

        let root = tempfile::tempdir().unwrap();
        let runtime = signed_bundle(root.path());
        let _env = offline_signed_env(root.path());
        let store = Arc::new(PackageStore::at(root.path().join("apps")));
        let installed = app_host::install::install_tapp(
            &store,
            &signed_consumer(root.path(), &runtime),
            package_store::Pinning::Pinned,
            None,
        )
        .unwrap();
        let image = root.path().join("empty-image");
        std::fs::create_dir(&image).unwrap();
        let catalog = Arc::new(Catalog::at(
            root.path().join("apps"),
            Some(SystemStore {
                data_dir: image.clone(),
                payload_dir: image,
                interfaces: None,
            }),
        ));
        let server = app_host::service::AppHostServer::new(store, catalog, 0);
        let (service, connection) = app_host::testing::ConnectedService::start(server).unwrap();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        app.with_surface::<StoreApp, _>(|store| store.connect_test_host(connection))
            .unwrap()
            .unwrap();
        app.settle();
        app.effect(&StoreEffect::ReviewRuntime {
            app: installed.id.clone(),
            runtime: runtime.clone(),
        })
        .unwrap();
        app.settle();
        let snapshot = app
            .effect(&StoreEffect::InspectRuntime)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(snapshot["phase"], "complete", "{snapshot}");
        assert_eq!(snapshot["review"]["consumer_app"], installed.id);
        assert_eq!(
            snapshot["review"]["consumer_root"],
            installed.package_id().unwrap().to_hex()
        );
        assert_eq!(snapshot["review"]["runtime"], runtime);
        assert!(snapshot["review"]["offer"].is_null());
        assert!(
            app.effect(&StoreEffect::AcquireRuntime {
                review: serde_json::to_string(&snapshot["review"]).unwrap(),
            })
            .is_err()
        );
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("Download and verify")
        );
        assert!(
            app.effect(&StoreEffect::ReviewRuntime {
                app: installed.id,
                runtime: "tilo-runtime/not-declared".into(),
            })
            .is_err(),
            "the production worker must propagate signed declaration refusal"
        );
        app.settle();
        let failed = app
            .effect(&StoreEffect::InspectRuntime)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(failed["phase"], "failed", "{failed}");
        assert!(
            failed["error"]
                .as_str()
                .unwrap()
                .contains("does not declare"),
            "{failed}"
        );
        assert!(
            RuntimeTarget::from_uri(
                "tilo-store:runtime/so.tilo.runtime-fixture/tilo-runtime/v?url=https://evil.test"
            )
            .is_err()
        );
        drop(app);
        service.shutdown().unwrap();
    }

    struct EmptyHost(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl app_host::app_host_capnp::app_host::Server for EmptyHost {
        fn list(
            self: capnp::capability::Rc<Self>,
            _params: app_host::app_host_capnp::app_host::ListParams,
            mut results: app_host::app_host_capnp::app_host::ListResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            results.get().init_entries(0);
            std::future::ready(Ok(()))
        }
    }

    #[test]
    fn runtime_resolver_uses_the_connected_host_and_rejects_absent_consumer() {
        let lists = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (service, connection) =
            app_host::testing::ConnectedService::start(EmptyHost(lists.clone())).unwrap();
        let client = app_host::client::BlockingClient::from_connected_fd(connection).unwrap();
        let target = RuntimeTarget::from_uri(
            "tilo-store:runtime/so.tilo.browser/tilo-runtime/2026.10.9-chromium",
        )
        .unwrap();
        assert!(inspect(Some(&client), &target).is_err());
        assert_eq!(lists.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(inspect(None, &target).is_err());
        assert_eq!(lists.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(client);
        service.shutdown().unwrap();
    }

    #[test]
    fn runtime_repair_and_repository_failures_have_distinct_localized_copy() {
        let catalogs = [
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ];
        let target = RuntimeTarget::new("so.tilo.mail".into(), "engine/mail".into()).unwrap();
        for (locale, service_error, missing_offer) in [
            (
                "en-US",
                "The app service is unavailable. No download was started.",
                "No enabled repository offers the required runtime for this device.",
            ),
            (
                "de-DE",
                "Der App-Dienst ist nicht verfügbar. Es wurde kein Download gestartet.",
                "Keine aktivierte Quelle bietet die benötigte Laufzeit für dieses Gerät an.",
            ),
        ] {
            ui::i18n::I18nManager::install("en-US", locale, &catalogs).unwrap();
            assert_eq!(inspect(None, &target).unwrap_err(), service_error);
            assert_eq!(ui::i18n!("store-runtime-unavailable"), missing_offer);
            let reviewed = ui::i18n!("store-runtime-reviewed", "name" => "Example");
            assert!(reviewed.contains("Example"));
            assert_ne!(reviewed, ui::i18n!("store-runtime-repair-reviewed"));
        }
    }

    #[test]
    fn exact_runtime_handoff_has_no_url_or_argument_escape() {
        let target = RuntimeTarget::from_uri(
            "tilo-store:runtime/so.tilo.browser/tilo-runtime/2026.10.9-chromium",
        )
        .unwrap();
        assert_eq!(target.runtime, "tilo-runtime/2026.10.9-chromium");
        for bad in [
            "tilo-store:runtime/so.tilo.browser/tilo-runtime/v/extra",
            "tilo-store:runtime/so.tilo.browser/../v",
            "tilo-store:runtime/so.tilo.browser/engine/v?url=https://evil.test",
            "tilo-store:runtime/so.tilo.browser/engine/%76",
            "tilo-store:runtime/--app/engine/v",
        ] {
            assert!(RuntimeTarget::from_uri(bad).is_err(), "{bad}");
        }
    }
}
