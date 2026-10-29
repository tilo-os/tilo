//! The host's answer to Settings asking for language packs (see
//! [`runtime_portal::runtime_request`]): which ones the repositories offer,
//! and installing one.
//!
//! The connection arrives stamped by the launcher with the calling app's id,
//! and the host answers only Settings. Repository, signatures and
//! destination are the host's (the same pipeline `app install` uses). One
//! install runs per pack however often it is asked, and a failure is
//! reported once, so the next request retries.
//!
//! An app's runtimes are not asked for here: they install and update with
//! the app itself.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use runtime_portal::runtime_request::{
    OfferedLanguage, Phase, Request, Response, read_message, write_message,
};
use tilo_lock::MutexRecover as _;

/// How the host installs a pack: reports progress, then succeeds or says why not.
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

enum Job {
    Running {
        phase: Phase,
        received: u64,
        total: Option<u64>,
    },
    Failed(String),
}

/// What every connection shares: the language-pack host and the installs
/// under way.
pub struct RuntimeRequests {
    packs: LanguagePackHost,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
}

impl RuntimeRequests {
    /// The production host: the enabled repositories and the verified
    /// installer.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_language_packs(LanguagePackHost::enabled_repositories())
    }

    /// A host with the given language packs (tests).
    #[must_use]
    pub fn with_language_packs(packs: LanguagePackHost) -> Arc<Self> {
        Arc::new(Self {
            packs,
            jobs: Arc::default(),
        })
    }

    #[must_use]
    pub fn answer(self: &Arc<Self>, app_id: &str, request: &Request) -> Response {
        let refused = |message: String| Response::Refused { message };
        if app_id != SETTINGS_APP_ID {
            return refused(format!("{app_id} does not choose the person's languages"));
        }
        match request {
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

    /// Settings, and only Settings, may ask which language packs the
    /// repositories offer and install one by its locale; the install runs
    /// once however often it asks, and a locale must read as one.
    #[test]
    fn settings_alone_lists_and_installs_language_packs() {
        let started = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let (counted, finished) = (Arc::clone(&started), Arc::clone(&done));
        let installed = Arc::clone(&done);
        let host = RuntimeRequests::with_language_packs(LanguagePackHost {
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
        });
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
        assert_eq!(started.load(Ordering::SeqCst), 1, "never a second install");
    }

    #[test]
    fn a_failed_install_is_reported_once_and_asking_again_retries() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);
        let host = RuntimeRequests::with_language_packs(LanguagePackHost {
            offers: Box::new(|| Ok(Vec::new())),
            installed: Box::new(|_| false),
            installer: Arc::new(move |_, _| {
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("offline".into())
                } else {
                    Ok(())
                }
            }),
        });
        let install = Request::InstallLanguagePack {
            locale: "de-DE".into(),
        };
        let settled = tilo_test_env::wait_for("the install to settle", || {
            let answer = host.answer(SETTINGS_APP_ID, &install);
            (!matches!(answer, Response::Installing { .. })).then_some(answer)
        });
        assert_eq!(
            settled,
            Response::Failed {
                message: "offline".into()
            }
        );
        let retry = host.answer(SETTINGS_APP_ID, &install);
        assert!(matches!(retry, Response::Installing { .. }), "{retry:?}");
        tilo_test_env::wait_until("the retried install", || {
            attempts.load(Ordering::SeqCst) == 2
        });
    }
}
