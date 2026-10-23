//! The host's answer to an app that asks for a runtime it declared (see
//! [`runtime_portal::runtime_request`]): the Browser's engine, when its
//! per-user copy is gone.
//!
//! The connection arrives stamped by the launcher with the calling app's id.
//! The request names only a runtime, and the host answers only for one the
//! caller's *installed manifest* lists in `optional-runtimes`: an app can
//! cause the install of what installing it would have brought anyway, never
//! of anything else. Repository, signatures and destination are the host's
//! (the same pipeline `app install` uses). One install runs per runtime
//! however many windows ask, and a failure is reported once, so the next
//! request retries.
//!
//! Settings asks one more thing, for the person's languages: which language
//! packs the repositories offer, and to install one. The host answers it
//! that and nothing else, and answers no other app either request.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use package_store::{Catalog, InstalledPackage};
use runtime_portal::runtime_request::{
    OfferedLanguage, Phase, Request, Response, read_message, write_message,
};
use tilo_lock::MutexRecover as _;

/// How the host installs a runtime: reports progress, then succeeds or says why not.
pub type Installer =
    dyn Fn(&str, &mut dyn FnMut(Phase, u64, Option<u64>)) -> Result<(), String> + Send + Sync;

/// The one app the host answers about language packs.
pub const SETTINGS_APP_ID: &str = "so.tilo.settings";

/// How the host answers about language packs: which the repositories offer,
/// whether one is installed, and installing one by its locale.
pub struct LanguagePackHost {
    /// The packs the enabled repositories offer.
    pub offers: Box<dyn Fn() -> Result<Vec<OfferedLanguage>, String> + Send + Sync>,
    /// Whether the newest offered pack of a locale is installed.
    pub installed: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// Install a locale's pack, reporting progress.
    pub installer: Arc<Installer>,
}

impl LanguagePackHost {
    /// The production host: the enabled repositories and the verified
    /// installer, into the person's language packs.
    #[must_use]
    pub fn enabled_repositories() -> Self {
        Self {
            offers: Box::new(|| {
                app_cli::language_pack_offers().map(|offers| {
                    offers
                        .into_iter()
                        .map(|offer| OfferedLanguage {
                            installed: app_cli::installed_language_pack(&offer.locale).as_deref()
                                == Some(offer.offer.version.as_str()),
                            locale: offer.locale,
                        })
                        .collect()
                })
            }),
            installed: Box::new(|locale| app_cli::installed_language_pack(locale).is_some()),
            installer: Arc::new(|locale, report| {
                app_cli::install::install_language_pack_from_enabled(locale, &mut |progress| {
                    report(phase_of(&progress), progress.received, progress.total);
                })
                .map_err(|error| error.user_message())
            }),
        }
    }
}

fn phase_of(progress: &app_cli::install::DownloadProgress) -> Phase {
    match progress.phase {
        app_cli::install::InstallPhase::Downloading => Phase::Downloading,
        app_cli::install::InstallPhase::Installing { .. } => Phase::Installing,
    }
}

/// Whether `locale` reads as a BCP 47 tag a pack could be named by.
fn plausible_locale(locale: &str) -> bool {
    (2..=35).contains(&locale.len())
        && locale
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !locale.starts_with('-')
        && !locale.ends_with('-')
}

/// How the host starts a package: its row and the address to open.
pub type Launcher = dyn Fn(&InstalledPackage, Option<&str>) -> Result<(), String> + Send + Sync;

enum Job {
    Running {
        phase: Phase,
        received: u64,
        total: Option<u64>,
    },
    Failed(String),
}

/// What every connection shares: the catalogue that says what an app
/// declares, the installer, and the installs under way.
pub struct RuntimeRequests {
    catalog: Arc<Catalog>,
    installed: Box<dyn Fn(&str) -> bool + Send + Sync>,
    installer: Arc<Installer>,
    launcher: Arc<Launcher>,
    packs: LanguagePackHost,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
}

impl RuntimeRequests {
    /// The production host: the shared installer over the enabled repositories.
    #[must_use]
    pub fn new(catalog: Arc<Catalog>) -> Arc<Self> {
        Self::with_language_packs(
            catalog,
            Box::new(app_cli::runtime_installed),
            Arc::new(|runtime, report| {
                app_cli::install::install_runtime_from_enabled(runtime, &mut |progress| {
                    let (phase, received, total) = match progress.phase {
                        app_cli::install::InstallPhase::Downloading => {
                            (Phase::Downloading, progress.received, progress.total)
                        }
                        app_cli::install::InstallPhase::Installing { .. } => {
                            (Phase::Installing, progress.received, progress.total)
                        }
                    };
                    report(phase, received, total);
                })
                .map_err(|error| error.user_message())
            }),
            Arc::new(crate::service::launch_package),
            LanguagePackHost::enabled_repositories(),
        )
    }

    /// A host with the given probe and installer (tests).
    #[must_use]
    pub fn with(
        catalog: Arc<Catalog>,
        probe: Box<dyn Fn(&str) -> bool + Send + Sync>,
        installer: Arc<Installer>,
        launcher: Arc<Launcher>,
    ) -> Arc<Self> {
        Self::with_language_packs(
            catalog,
            probe,
            installer,
            launcher,
            LanguagePackHost {
                offers: Box::new(|| Err("no repository offers language packs".into())),
                installed: Box::new(|_| false),
                installer: Arc::new(|_, _| Err("no repository offers language packs".into())),
            },
        )
    }

    /// A host with the given probe, installers and language packs (tests).
    #[must_use]
    pub fn with_language_packs(
        catalog: Arc<Catalog>,
        probe: Box<dyn Fn(&str) -> bool + Send + Sync>,
        installer: Arc<Installer>,
        launcher: Arc<Launcher>,
        packs: LanguagePackHost,
    ) -> Arc<Self> {
        Arc::new(Self {
            catalog,
            installed: probe,
            installer,
            launcher,
            packs,
            jobs: Arc::default(),
        })
    }

    /// Whether `app_id`'s installed manifest declares `runtime` as optional.
    fn declares(&self, app_id: &str, runtime: &str) -> bool {
        let Ok(rows) = self.catalog.list() else {
            return false;
        };
        rows.iter()
            .filter(|row| row.id == app_id)
            .filter_map(|row| std::fs::read_to_string(&row.manifest_path).ok())
            .filter_map(|text| tilo_app_package::Manifest::from_toml_str(&text).ok())
            .any(|manifest| {
                tilo_app_package::desktop::optional_runtime_dependencies(&manifest)
                    .iter()
                    .any(|declared| declared == runtime)
            })
    }

    /// Start a fresh instance of `app_id`'s own package, when it declares a
    /// runtime to be mounted at start.
    fn relaunch(&self, app_id: &str, uri: Option<&str>) -> Response {
        let refused = |message: String| Response::Refused { message };
        if let Some(uri) = uri
            && !(uri.len() <= 2048
                && (uri.starts_with("https://") || uri.starts_with("http://"))
                && !uri.chars().any(char::is_control))
        {
            return refused("only an http(s) address can be opened".into());
        }
        let Ok(rows) = self.catalog.list() else {
            return refused("the installed apps cannot be read".into());
        };
        let Some(row) = rows.into_iter().find(|row| row.id == app_id) else {
            return refused(format!("{app_id} is not installed"));
        };
        let declares_runtime = std::fs::read_to_string(&row.manifest_path)
            .ok()
            .and_then(|text| tilo_app_package::Manifest::from_toml_str(&text).ok())
            .is_some_and(|manifest| {
                !tilo_app_package::desktop::optional_runtime_dependencies(&manifest).is_empty()
            });
        if !declares_runtime {
            return refused(format!("{app_id} declares no runtime to start with"));
        }
        match (self.launcher)(&row, uri) {
            Ok(()) => Response::Relaunched,
            Err(message) => Response::Failed { message },
        }
    }

    #[must_use]
    pub fn answer(self: &Arc<Self>, app_id: &str, request: &Request) -> Response {
        let refused = |message: String| Response::Refused { message };
        match request {
            Request::Ensure { runtime } => {
                if !self.declares(app_id, runtime) {
                    return refused(format!("{app_id} does not declare {runtime}"));
                }
                self.ensure(
                    runtime.clone(),
                    (self.installed)(runtime),
                    Arc::clone(&self.installer),
                    runtime.clone(),
                )
            }
            Request::Relaunch { uri } => self.relaunch(app_id, uri.as_deref()),
            Request::LanguagePacks | Request::InstallLanguagePack { .. }
                if app_id != SETTINGS_APP_ID =>
            {
                refused(format!("{app_id} does not choose the person's languages"))
            }
            Request::LanguagePacks => match (self.packs.offers)() {
                Ok(offered) => Response::LanguagePacks { offered },
                Err(message) => Response::Failed { message },
            },
            Request::InstallLanguagePack { locale } => {
                if !plausible_locale(locale) {
                    return refused(format!("{locale:?} is not a language"));
                }
                self.ensure(
                    format!("language-pack:{locale}"),
                    (self.packs.installed)(locale),
                    Arc::clone(&self.packs.installer),
                    locale.clone(),
                )
            }
        }
    }

    /// Where the install `key` of `subject` stands, starting it unless it
    /// runs or `done` says there is nothing to do.
    fn ensure(
        self: &Arc<Self>,
        key: String,
        done: bool,
        installer: Arc<Installer>,
        subject: String,
    ) -> Response {
        let mut jobs = self.jobs.lock_recover();
        if done && !matches!(jobs.get(&key), Some(Job::Running { .. })) {
            jobs.remove(&key);
            return Response::Installed;
        }
        match jobs.get(&key) {
            Some(Job::Running {
                phase,
                received,
                total,
            }) => Response::Installing {
                phase: *phase,
                received: *received,
                total: *total,
            },
            Some(Job::Failed(message)) => {
                let message = message.clone();
                jobs.remove(&key);
                Response::Failed { message }
            }
            None => {
                jobs.insert(
                    key.clone(),
                    Job::Running {
                        phase: Phase::Preparing,
                        received: 0,
                        total: None,
                    },
                );
                drop(jobs);
                self.spawn_install(key, installer, subject);
                Response::Installing {
                    phase: Phase::Preparing,
                    received: 0,
                    total: None,
                }
            }
        }
    }

    fn spawn_install(self: &Arc<Self>, key: String, installer: Arc<Installer>, subject: String) {
        let jobs = Arc::clone(&self.jobs);
        std::thread::Builder::new()
            .name("tilo-app-host-install".into())
            .spawn(move || {
                let outcome = installer(&subject, &mut |phase, received, total| {
                    jobs.lock_recover().insert(
                        key.clone(),
                        Job::Running {
                            phase,
                            received,
                            total,
                        },
                    );
                });
                let mut jobs = jobs.lock_recover();
                match outcome {
                    Ok(()) => {
                        jobs.remove(&key);
                    }
                    Err(message) => {
                        tracing::warn!(%key, %message, "an install an app asked for failed");
                        jobs.insert(key, Job::Failed(message));
                    }
                }
            })
            .map_or_else(
                |error| tracing::error!(%error, "cannot start the install"),
                drop,
            );
    }

    /// Serve one connection until the app hangs up.
    ///
    /// # Errors
    ///
    /// Returns a transport error; a clean hang-up is `Ok`.
    pub fn serve(
        self: &Arc<Self>,
        app_id: &str,
        mut stream: std::os::unix::net::UnixStream,
    ) -> io::Result<()> {
        while let Some(request) = read_message::<_, Request>(&mut stream)? {
            write_message(&mut stream, &self.answer(app_id, &request))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    const ENGINE: &str = "tilo-runtime/2026.10.9-chromium";

    /// A catalogue whose system store ships `so.tilo.browser` declaring ENGINE.
    fn catalog(dir: &std::path::Path) -> Arc<Catalog> {
        let system = dir.join("system");
        let payload = system.join("payloads/so.tilo.browser");
        std::fs::create_dir_all(&payload).unwrap();
        let manifest = payload.join("manifest.toml");
        std::fs::write(
            &manifest,
            format!(
                "[package]\nid = \"so.tilo.browser\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
                 optional-runtimes = [\"{ENGINE}\"]\n\
                 [publisher]\nidentity = \"ed25519:{key}\"\nsignature = \"\"\n\
                 [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
                 [sandbox.net]\nmode = \"off\"\n[entrypoint]\nbin = \"bin/app\"\n",
                key = "00".repeat(32)
            ),
        )
        .unwrap();
        std::fs::write(
            system.join("registry.toml"),
            format!(
                "[[entry]]\nid = 'so.tilo.browser'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            ),
        )
        .unwrap();
        Arc::new(Catalog::at(
            dir.join("user"),
            Some(package_store::SystemStore {
                data_dir: system.clone(),
                payload_dir: system.join("payloads"),
                interfaces: None,
            }),
        ))
    }

    fn ensure(runtime: &str) -> Request {
        Request::Ensure {
            runtime: runtime.into(),
        }
    }

    fn settle(host: &Arc<RuntimeRequests>, request: &Request) -> Response {
        tilo_test_env::wait_for("the install to settle", || {
            let answer = host.answer("so.tilo.browser", request);
            (!matches!(answer, Response::Installing { .. })).then_some(answer)
        })
    }

    #[test]
    fn an_app_can_only_cause_the_install_of_a_runtime_it_declares() {
        let dir = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&started);
        let host = RuntimeRequests::with(
            catalog(dir.path()),
            Box::new(|_| false),
            Arc::new(move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Arc::new(|_, _| Ok(())),
        );
        let refused = host.answer("so.tilo.browser", &ensure("tilo-runtime/2026.07+native.ab"));
        assert!(matches!(refused, Response::Refused { .. }), "{refused:?}");
        let other = host.answer("so.tilo.notes", &ensure(ENGINE));
        assert!(matches!(other, Response::Refused { .. }), "{other:?}");
        assert_eq!(started.load(Ordering::SeqCst), 0, "nothing was installed");
    }

    /// Settings, and only Settings, may ask which language packs the
    /// repositories offer and install one by its locale; the install runs
    /// once however often it asks, and a locale must read as one.
    #[test]
    fn settings_alone_lists_and_installs_language_packs() {
        let dir = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let (counted, finished) = (Arc::clone(&started), Arc::clone(&done));
        let installed = Arc::clone(&done);
        let host = RuntimeRequests::with_language_packs(
            catalog(dir.path()),
            Box::new(|_| false),
            Arc::new(|_, _| Ok(())),
            Arc::new(|_, _| Ok(())),
            LanguagePackHost {
                offers: Box::new(|| {
                    Ok(vec![OfferedLanguage {
                        locale: "de-DE".into(),
                        installed: false,
                    }])
                }),
                installed: Box::new(move |_| installed.load(Ordering::SeqCst)),
                installer: Arc::new(move |locale, report| {
                    assert_eq!(locale, "de-DE");
                    counted.fetch_add(1, Ordering::SeqCst);
                    report(Phase::Downloading, 10, Some(20));
                    finished.store(true, Ordering::SeqCst);
                    Ok(())
                }),
            },
        );
        let install = Request::InstallLanguagePack {
            locale: "de-DE".into(),
        };
        for request in [&Request::LanguagePacks, &install] {
            let answer = host.answer("so.tilo.browser", request);
            assert!(matches!(answer, Response::Refused { .. }), "{answer:?}");
        }
        assert_eq!(
            host.answer(SETTINGS_APP_ID, &Request::LanguagePacks),
            Response::LanguagePacks {
                offered: vec![OfferedLanguage {
                    locale: "de-DE".into(),
                    installed: false
                }]
            }
        );
        let odd = Request::InstallLanguagePack {
            locale: "../de".into(),
        };
        assert!(matches!(
            host.answer(SETTINGS_APP_ID, &odd),
            Response::Refused { .. }
        ));
        let answer = tilo_test_env::wait_for("the install to settle", || {
            let answer = host.answer(SETTINGS_APP_ID, &install);
            (!matches!(answer, Response::Installing { .. })).then_some(answer)
        });
        assert_eq!(answer, Response::Installed);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        // Settings asks for no runtime it does not declare.
        let runtime = host.answer(SETTINGS_APP_ID, &ensure(ENGINE));
        assert!(matches!(runtime, Response::Refused { .. }), "{runtime:?}");
    }

    #[test]
    fn one_install_serves_every_asker_and_progress_reaches_them() {
        let dir = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (counted, gate, finished) = (
            Arc::clone(&started),
            Arc::clone(&release),
            Arc::clone(&done),
        );
        let probe = Arc::clone(&done);
        let host = RuntimeRequests::with(
            catalog(dir.path()),
            Box::new(move |_| probe.load(Ordering::SeqCst)),
            Arc::new(move |_, report| {
                counted.fetch_add(1, Ordering::SeqCst);
                report(Phase::Downloading, 40, Some(100));
                while !gate.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                finished.store(true, Ordering::SeqCst);
                Ok(())
            }),
            Arc::new(|_, _| Ok(())),
        );
        let first = host.answer("so.tilo.browser", &ensure(ENGINE));
        assert!(matches!(first, Response::Installing { .. }), "{first:?}");
        tilo_test_env::wait_until("the install's progress to reach the asker", || {
            host.answer("so.tilo.browser", &ensure(ENGINE))
                == (Response::Installing {
                    phase: Phase::Downloading,
                    received: 40,
                    total: Some(100),
                })
        });
        let progress = host.answer("so.tilo.browser", &ensure(ENGINE));
        assert_eq!(
            progress,
            Response::Installing {
                phase: Phase::Downloading,
                received: 40,
                total: Some(100)
            }
        );
        release.store(true, Ordering::SeqCst);
        assert_eq!(settle(&host, &ensure(ENGINE)), Response::Installed);
        assert_eq!(started.load(Ordering::SeqCst), 1, "never a second install");
    }

    #[test]
    fn a_failed_install_is_reported_once_and_asking_again_retries() {
        let dir = tempfile::tempdir().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);
        let host = RuntimeRequests::with(
            catalog(dir.path()),
            Box::new(|_| false),
            Arc::new(move |_, _| {
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("offline".into())
                } else {
                    Ok(())
                }
            }),
            Arc::new(|_, _| Ok(())),
        );
        assert_eq!(
            settle(&host, &ensure(ENGINE)),
            Response::Failed {
                message: "offline".into()
            }
        );
        let retry = host.answer("so.tilo.browser", &ensure(ENGINE));
        assert!(matches!(retry, Response::Installing { .. }), "{retry:?}");
        tilo_test_env::wait_until("the retried install", || {
            attempts.load(Ordering::SeqCst) == 2
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    type Launched = Arc<Mutex<Vec<(String, Option<String>)>>>;

    #[test]
    fn a_declaring_app_can_start_a_fresh_instance_of_itself_only_on_a_web_address() {
        let dir = tempfile::tempdir().unwrap();
        let launched: Launched = Arc::default();
        let record = Arc::clone(&launched);
        let host = RuntimeRequests::with(
            catalog(dir.path()),
            Box::new(|_| true),
            Arc::new(|_, _| Ok(())),
            Arc::new(move |row, uri| {
                record
                    .lock()
                    .unwrap()
                    .push((row.id.clone(), uri.map(str::to_owned)));
                Ok(())
            }),
        );
        let relaunch = |app: &str, uri: Option<&str>| {
            host.answer(
                app,
                &Request::Relaunch {
                    uri: uri.map(str::to_owned),
                },
            )
        };
        assert_eq!(
            relaunch("so.tilo.browser", Some("https://tilo.so/")),
            Response::Relaunched
        );
        assert_eq!(relaunch("so.tilo.browser", None), Response::Relaunched);
        assert!(matches!(
            relaunch("so.tilo.browser", Some("file:///etc/passwd")),
            Response::Refused { .. }
        ));
        assert!(matches!(
            relaunch("so.tilo.notes", None),
            Response::Refused { .. }
        ));
        assert_eq!(
            *launched.lock().unwrap(),
            [
                (
                    "so.tilo.browser".to_owned(),
                    Some("https://tilo.so/".to_owned())
                ),
                ("so.tilo.browser".to_owned(), None)
            ]
        );
    }
}
