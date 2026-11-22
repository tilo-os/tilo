//! The person's language packs: which the enabled repositories offer, and installing one.
//!
//! Settings is the one app the service answers these calls for; the connection's
//! handoff names the app, and the service decides. Repository, signatures and
//! destination are the service's, through the pipeline `app install` uses. There
//! is one install per locale however often it is asked, and it reports its progress
//! until it lands or says why it did not; a finished install leaves nothing behind,
//! so asking again after a failure starts a fresh attempt.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use crate::wire::{OfferedLanguage, Phase, Progress};

/// How the service installs a pack: reports progress, then succeeds or says why not.
pub type Installer =
    dyn Fn(&str, &mut dyn FnMut(Phase, u64, Option<u64>)) -> Result<(), String> + Send + Sync;

/// The one app the service answers about language packs.
pub use tilo_app_id::apps::SETTINGS as SETTINGS_APP_ID;

/// How the service answers about language packs: which the repositories offer,
/// whether one is installed, and installing one by its locale.
pub struct LanguagePacks {
    /// The packs the enabled repositories offer.
    pub offers: Box<dyn Fn() -> Result<Vec<OfferedLanguage>, String> + Send + Sync>,
    /// Whether the newest offered pack of a locale is installed.
    pub installed: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// Install a locale's pack, reporting progress.
    pub installer: Arc<Installer>,
}

impl LanguagePacks {
    /// The production service: the enabled repositories and the verified
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

/// Where one install stands, as everyone following it sees it.
#[derive(Debug, Default)]
struct State {
    /// Every progress reported, in order.
    progress: Vec<Progress>,
    /// Set once, when the install lands or fails.
    done: Option<Result<(), String>>,
}

struct Job {
    state: Arc<Mutex<State>>,
    /// Wakes everyone following the install when its state changed.
    changed: Arc<tokio::sync::Notify>,
}

/// What every connection shares: the language packs and the installs under way.
pub struct LanguagePackInstalls {
    packs: Arc<LanguagePacks>,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
}

impl LanguagePackInstalls {
    /// The production service: the enabled repositories and the verified
    /// installer.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_packs(LanguagePacks::enabled_repositories())
    }

    #[must_use]
    pub fn with_packs(packs: LanguagePacks) -> Arc<Self> {
        Arc::new(Self {
            packs: Arc::new(packs),
            jobs: Arc::default(),
        })
    }

    /// The packs the enabled repositories offer.
    ///
    /// # Errors
    ///
    /// Why the repositories could not be read.
    pub fn offers(&self) -> Result<Vec<OfferedLanguage>, String> {
        (self.packs.offers)()
    }

    /// Make `locale`'s pack installed, reporting where it stands until it does.
    /// Asking again while one is under way follows the same install.
    ///
    /// # Errors
    ///
    /// Why the pack could not be installed, or that `locale` is not one.
    pub async fn install(
        &self,
        locale: String,
        progress: impl Fn(Progress) + Send + 'static,
    ) -> Result<(), String> {
        if !plausible_locale(&locale) {
            return Err(format!("{locale:?} is not a language"));
        }
        let (state, changed) = self.follow(&locale);
        let mut seen = 0;
        loop {
            // Registered before the state is read, so a change between the two
            // is never missed.
            let notified = changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let now = state.lock_recover();
                for reported in now.progress.iter().skip(seen) {
                    progress(reported.clone());
                }
                seen = now.progress.len();
                if let Some(done) = &now.done {
                    return done.clone();
                }
            }
            notified.await;
        }
    }

    /// Where `locale`'s install stands, starting one when none is under way and
    /// the pack is not installed already.
    fn follow(&self, locale: &str) -> (Arc<Mutex<State>>, Arc<tokio::sync::Notify>) {
        let mut jobs = self.jobs.lock_recover();
        if let Some(job) = jobs.get(locale) {
            return (Arc::clone(&job.state), Arc::clone(&job.changed));
        }
        let state = Arc::new(Mutex::new(State::default()));
        let changed = Arc::new(tokio::sync::Notify::new());
        jobs.insert(
            locale.to_owned(),
            Job {
                state: Arc::clone(&state),
                changed: Arc::clone(&changed),
            },
        );
        let already = (self.packs.installed)(locale);
        if already {
            state.lock_recover().done = Some(Ok(()));
            jobs.remove(locale);
        }
        drop(jobs);
        if already {
            return (state, changed);
        }
        let jobs = Arc::clone(&self.jobs);
        let installer = Arc::clone(&self.packs.installer);
        let pack = locale.to_owned();
        let key = locale.to_owned();
        let installing = Arc::clone(&state);
        let waking = Arc::clone(&changed);
        std::thread::Builder::new()
            .name("tilo-package-service-install".into())
            .spawn(move || {
                let outcome = installer(&pack, &mut |phase, received, total| {
                    installing
                        .lock_recover()
                        .progress
                        .push(Progress::LanguagePack {
                            phase,
                            received,
                            total,
                        });
                    waking.notify_waiters();
                });
                // The install leaves the ones under way as its outcome is set, under one lock:
                // whoever asks once it is known starts a new one, never follows this one.
                {
                    let mut jobs = jobs.lock_recover();
                    installing.lock_recover().done = Some(outcome);
                    jobs.remove(&key);
                }
                waking.notify_waiters();
            })
            .map_or_else(
                |error| {
                    tracing::error!(%error, "cannot start the install");
                },
                drop,
            );
        (state, changed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    fn settle(installs: &Arc<LanguagePackInstalls>, locale: &str) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(installs.install(locale.to_owned(), drop))
    }

    /// Settings, and only Settings, may ask about language packs and install one; the install
    /// happens once however often it asks, and a locale must read as one.
    #[test]
    fn settings_alone_lists_and_installs_language_packs() {
        let started = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let (counted, finished) = (Arc::clone(&started), Arc::clone(&done));
        let installed = Arc::clone(&done);
        let service = LanguagePackInstalls::with_packs(LanguagePacks {
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
                std::thread::sleep(std::time::Duration::from_millis(10));
                finished.store(true, Ordering::SeqCst);
                Ok(())
            }),
        });
        assert_eq!(
            service.offers().unwrap(),
            vec![OfferedLanguage {
                locale: "de-DE".into(),
                installed: false
            }]
        );
        assert_eq!(settle(&service, "de-DE"), Ok(()));
        assert_eq!(started.load(Ordering::SeqCst), 1, "never a second install");
        assert!(
            settle(&service, "../de").is_err(),
            "a locale must read as one"
        );
    }

    #[test]
    fn a_failed_install_is_reported_once_and_asking_again_retries() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);
        let service = LanguagePackInstalls::with_packs(LanguagePacks {
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
        assert_eq!(settle(&service, "de-DE"), Err("offline".into()));
        assert_eq!(settle(&service, "de-DE"), Ok(()));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
