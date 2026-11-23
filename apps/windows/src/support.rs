//! Windows support (Proton and the Steam Runtime, and FEX on ARM) is a downloaded system
//! extension. A program that needs it says so on its page and installs it there, through the
//! same extension helper Settings uses, and its progress comes from the helper's status.
use crate::app::{Intent, Intents};
use crate::model::Route;
use crate::strings;
use os_actions::windows_compat::{self, Consent, Progress, Readiness, SizeDisclosure};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use ui::widget::{Banner, Button, ButtonRole, Element};

/// Where the install's answer lands once the helper finished.
type Answer = Arc<Mutex<Option<Result<(), String>>>>;

/// Whether a program opened this way needs the downloaded payload: Proton's runners always,
/// and every Wine prefix on a device whose Wine executes through FEX. The Windows appliance
/// does not.
pub(crate) const fn route_needs_support(route: Route) -> bool {
    match route {
        Route::Proton => true,
        Route::Wine => compat_service::wine_is_translated(),
        Route::Vm => false,
    }
}

/// The button that turns an app's route back to the catalogue's recommendation: it submits the
/// reset itself, in the interaction that pressed it, as the route strip submits its choice.
pub(crate) fn use_recommendation(
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::Windows>>,
    intents: Intents,
    app_id: String,
) -> Button {
    let routes = crate::resource::Windows::paths().routes;
    Button::new(
        "windows-use-recommendation",
        &strings::t("windows-use-recommendation"),
    )
    .role(ButtonRole::Plain)
    .on_click(move || {
        if let Some(dispatcher) = &dispatcher {
            dispatcher.submit(routes.remove(app_id.as_str()));
        }
        intents.push(Intent::ResetRoute);
    })
}

/// Windows support on this device, as the manager sees and installs it.
pub(crate) struct WindowsSupport {
    readiness: Readiness,
    live: Option<Live>,
}

struct Live {
    wake: ui::WakeHandle,
    changed: Arc<AtomicBool>,
    /// Keeps the status watch alive.
    _watch: Option<Box<dyn std::any::Any>>,
    job: Option<Answer>,
}

/// How an install from here ended.
pub(crate) enum Finished {
    Ready,
    Failed(String),
}

impl WindowsSupport {
    /// A fixed state, for captures and tests: nothing is read or started.
    pub(crate) const fn offline(readiness: Readiness) -> Self {
        Self {
            readiness,
            live: None,
        }
    }

    /// This device's state, followed as the helper rewrites its status; `wake` has the window
    /// take in each change.
    pub(crate) fn live(wake: ui::WakeHandle) -> Self {
        let changed = Arc::new(AtomicBool::new(false));
        let watch = {
            let (changed, wake) = (Arc::clone(&changed), wake.clone());
            windows_compat::watch(move || {
                changed.store(true, Ordering::Release);
                wake.wake();
            })
            .ok()
            .map(|watch| Box::new(watch) as Box<dyn std::any::Any>)
        };
        Self {
            readiness: Readiness::read(),
            live: Some(Live {
                wake,
                changed,
                _watch: watch,
                job: None,
            }),
        }
    }

    pub(crate) const fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Whether opening a program this way waits for Windows support.
    pub(crate) fn blocks(&self, route: Route) -> bool {
        route_needs_support(route)
            && !matches!(self.readiness, Readiness::Ready | Readiness::NotOffered)
    }

    /// Why opening a program this way cannot start yet, for whoever asked.
    pub(crate) fn refusal(&self, route: Route) -> Option<String> {
        self.blocks(route)
            .then(|| strings::t("windows-support-needed"))
    }

    /// Download and add Windows support, with the size the page showed. The helper is its own
    /// unit; this only asks for it and follows its status.
    pub(crate) fn install(&mut self) -> Result<(), String> {
        let consent = match &self.readiness {
            Readiness::NeedsInstall(disclosure) | Readiness::Failed(_, disclosure) => {
                Consent::shown(disclosure)
            }
            other => return Err(other.message()),
        };
        self.readiness = Readiness::Installing(None);
        let Some(live) = &mut self.live else {
            return Ok(());
        };
        let answer = Answer::default();
        let (landed, wake) = (Arc::clone(&answer), live.wake.clone());
        std::thread::Builder::new()
            .name("tilo-windows-support-install".into())
            .spawn(move || {
                let result = windows_compat::install(&consent);
                *landed.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
                wake.wake();
            })
            .map_err(|error| format!("could not start the install: {error}"))?;
        live.job = Some(answer);
        Ok(())
    }

    /// What a page whose program needs Windows support says about it, with the control that
    /// installs it while it can be installed.
    pub(crate) fn banner(&self, intents: &Intents) -> Option<Element> {
        let install = |label_key: &str| {
            Button::new("windows-support-install", &strings::t(label_key))
                .role(ButtonRole::Primary)
                .on_click(intents.sender(Intent::InstallSupport))
        };
        let banner = match &self.readiness {
            Readiness::NeedsInstall(disclosure) => {
                Banner::warning(strings::t("windows-support-missing-title"))
                    .with_message(missing_message(disclosure))
                    .with_action(install("windows-support-install"))
            }
            Readiness::Failed(error, _) => {
                Banner::error(strings::t("windows-support-failed-title"))
                    .with_message(error.clone())
                    .with_action(install("windows-support-retry"))
            }
            Readiness::Installing(progress) => {
                let mut banner = Banner::info(strings::t("windows-support-installing-title"));
                if let Some(progress) = progress.as_ref().filter(|p| !p.label.is_empty()) {
                    let detail = match progress.megabytes() {
                        Some((done, total)) => strings::format_all(
                            "windows-support-progress-bytes",
                            &[
                                ("label", progress.label.clone()),
                                ("done", done.to_string()),
                                ("total", total.to_string()),
                            ],
                        ),
                        None => strings::format_all(
                            "windows-support-progress",
                            &[
                                ("label", progress.label.clone()),
                                ("step", progress.step.to_string()),
                                ("steps", progress.steps.to_string()),
                            ],
                        ),
                    };
                    banner = banner.with_message(detail);
                }
                banner
            }
            Readiness::WaitingForNetwork => {
                Banner::info(strings::t("windows-support-waiting-title"))
                    .with_message(strings::t("windows-support-waiting"))
            }
            Readiness::Ready | Readiness::NotOffered => return None,
        };
        Some(Box::new(banner))
    }

    /// Take in what changed since the last call; `Some` once support arrived, whichever view
    /// started the install, or once an install from this page failed.
    pub(crate) fn drain(&mut self) -> Option<Finished> {
        let live = self.live.as_mut()?;
        let finished = live
            .job
            .as_ref()
            .and_then(|job| job.lock().unwrap_or_else(PoisonError::into_inner).take());
        if finished.is_some() {
            live.job = None;
        }
        let changed = live.changed.swap(false, Ordering::AcqRel);
        if finished.is_some() || changed {
            let before = std::mem::replace(&mut self.readiness, Readiness::read());
            if finished.is_none() && arrived(&before, &self.readiness) {
                return Some(Finished::Ready);
            }
        }
        Some(match finished? {
            Ok(()) => Finished::Ready,
            Err(error) => Finished::Failed(error),
        })
    }
}

/// Whether support became ready between two reads: an install this page did not start (another
/// view's action, the helper finishing a deferred download) still says on this page that it is
/// done.
fn arrived(before: &Readiness, after: &Readiness) -> bool {
    !matches!(before, Readiness::Ready) && matches!(after, Readiness::Ready)
}

/// The line a program's page shows before support downloads: its size is the extension
/// catalogue's, so the figure is the one that will actually be downloaded.
fn missing_message(disclosure: &SizeDisclosure) -> String {
    strings::format(
        "windows-support-missing",
        "size",
        &SizeDisclosure::gigabytes(disclosure.download_bytes),
    )
}

/// Windows support as a capture of `scenario` shows it: installed, unless the scenario is about
/// installing it.
pub(crate) fn staged(scenario: &str) -> Readiness {
    let disclosure = SizeDisclosure {
        download_bytes: 2_100_000_000,
        installed_bytes: 5_300_000_000,
    };
    match scenario {
        "support-missing" => Readiness::NeedsInstall(disclosure),
        "support-installing" => Readiness::Installing(Some(Progress {
            step: 2,
            steps: 4,
            label: "Downloading".into(),
            bytes: Some((640_000_000, 2_100_000_000)),
        })),
        "support-failed" => Readiness::Failed(
            "the download stopped: the network went away".into(),
            disclosure,
        ),
        _ => Readiness::Ready,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::WindowsApp;

    /// A program opened with Proton before Windows support is here says so on its page and
    /// installs it there; opening it waits until then.
    #[test]
    fn a_proton_program_installs_windows_support_from_its_page() {
        let mut app = WindowsApp::new_offline_scenario("support-missing");
        let selected = app.selected_app().expect("the Proton sample").clone();
        assert_eq!(selected.route, Route::Proton);
        assert!(app.support.blocks(Route::Proton));
        assert!(!app.support.blocks(Route::Vm));
        assert_eq!(
            app.launch_app(&selected.id).unwrap_err(),
            strings::t("windows-support-needed")
        );
        assert!(app.effect_statuses().is_empty(), "installing is offered");
        assert!(app.support.banner(&app.intents).is_some());
        app.intents.push(Intent::InstallSupport);
        app.apply_intents();
        assert!(matches!(
            app.support.readiness(),
            Readiness::Installing(None)
        ));
        assert_eq!(
            app.effect_statuses().len(),
            1,
            "a second install is refused while one works"
        );
        let installed = WindowsApp::new_offline_scenario("detail");
        assert!(installed.support.banner(&installed.intents).is_none());
    }

    /// The page shows the size the extension catalogue publishes, not a figure of its own.
    #[test]
    fn a_program_page_shows_the_size_the_catalogue_publishes() {
        let catalogue = SizeDisclosure {
            download_bytes: 2_518_488_726,
            installed_bytes: 2_518_488_726,
        };
        assert_eq!(
            missing_message(&catalogue),
            "This program needs it to open. It's a download of about 2.6 GB."
        );
        let staged = SizeDisclosure {
            download_bytes: 2_100_000_000,
            installed_bytes: 5_300_000_000,
        };
        assert!(missing_message(&staged).contains("about 2.1 GB"));
    }

    /// Support that arrives while the page is open says so on the page, even when another view
    /// started the install.
    #[test]
    fn support_arriving_from_another_view_is_announced() {
        let disclosure = SizeDisclosure {
            download_bytes: 2_518_488_726,
            installed_bytes: 2_518_488_726,
        };
        assert!(arrived(
            &Readiness::NeedsInstall(disclosure),
            &Readiness::Ready
        ));
        assert!(arrived(&Readiness::Installing(None), &Readiness::Ready));
        assert!(!arrived(&Readiness::Ready, &Readiness::Ready));
        assert!(!arrived(
            &Readiness::Ready,
            &Readiness::NeedsInstall(disclosure)
        ));
        assert!(!arrived(
            &Readiness::Installing(None),
            &Readiness::Failed("the download stopped".into(), disclosure)
        ));
    }
}
