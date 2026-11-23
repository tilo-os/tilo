//! Setup as an app: its resource, its actions and the scenarios that render
//! each step headless.

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::Element;
use zeroize::Zeroizing;

use crate::ceremony::Ceremony;
use crate::effect::OnboardingEffect;
use crate::page::{
    Codecs, IdentityProblem, KeyCopy, Nearby, OnboardingContext, PackState, Page, Screen,
    SecretText, TourPage, WordsCheck,
};
use crate::state::OnboardingState;

/// The app the shell mounts setup as.
pub const APP_ID: &str = "so.tilo.onboarding";

/// Setup's document: what the person acknowledged while setting this computer
/// up. The ceremony itself is the shell's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Onboarding {
    /// The person acknowledged how their identity is protected here.
    pub understood: bool,
    /// What the backup step was told.
    pub backup: BackupAcknowledgement,
}

/// What the backup step was told.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct BackupAcknowledgement {
    /// The person chose to skip the backup.
    pub skip_acknowledged: bool,
}

#[resource(kind = "onboarding.setup")]
impl Onboarding {
    /// Acknowledge how this computer's identity is protected.
    #[action]
    pub fn set_understood(&mut self, checked: bool) {
        self.understood = checked;
    }

    /// Acknowledge skipping the backup.
    #[action]
    pub fn set_backup_skip_acknowledged(&mut self, checked: bool) {
        self.backup.skip_acknowledged = checked;
    }
}

/// Setup as an app.
pub struct OnboardingApp;

impl ResourceApp for OnboardingApp {
    type Resource = Onboarding;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Setup";
    const ICON: Option<&'static str> = Some("user");
    // The tour shows the desktop under it; the identity steps paint their own
    // ground. A step composes its page again when what it shows changes (an
    // acknowledgement ticked enables what follows): the keyboard stays on
    // the control the person was on, and a new step puts it on that step's
    // first control.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        autofocus_on_structure_change: true,
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: setup's record is the shell's journal.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<Onboarding>) -> Element {
        let state = cx.take_lent::<OnboardingState>();
        let page = match &state {
            Some(state) => state.page(),
            None => cx
                .fixture::<Page>()
                .map(|page| (*page).clone())
                .unwrap_or_default(),
        };
        let wake = cx.waker();
        if let Some(state) = &state {
            state.wake_with(wake.clone());
        }
        let asking = state.clone();
        cx.effects(move |mut effect: OnboardingEffect, _cx| {
            let Some(state) = &asking else {
                effect.forget_secret();
                return Err("setup is not running here".to_owned());
            };
            if let OnboardingEffect::EnterSecret { field, value } = &effect {
                // Straight into the shell's buffer; the secret never joins
                // the queue the shell reads, and nothing echoes it back.
                let entered = state.secrets().enter(field, value);
                effect.forget_secret();
                return if entered {
                    Ok(Effected::done())
                } else {
                    Err("no such secret field".to_owned())
                };
            }
            state.ask(effect);
            Ok(Effected::done())
        });
        Box::new(Ceremony::new(state, page, cx.dispatcher(), Some(wake)))
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Onboarding>) {
        fixture.provide(scenario(fixture.scenario().unwrap_or("intro")));
    }
}

/// One scenario per step, and per state of a step a person meets.
const SCENARIOS: &[&str] = &[
    "intro",
    "intro-host",
    "existing",
    "recovery",
    "nearby",
    "nearby-joining",
    "nearby-failed",
    "restore",
    "restore-error",
    "recovery-key-typing",
    "recovery-key-typo",
    "recovery-key-ready",
    "recovery-key-different",
    "protect",
    "protect-acknowledged",
    "create-error",
    "backup",
    "backup-skip-acknowledged",
    "backup-verified",
    "backup-recovery-key",
    "backup-recovery-key-written",
    "portable-backup",
    "ready",
    "ready-joined",
    "ready-host",
    "codecs",
    "codecs-installing",
    "codecs-waiting-for-network",
    "codecs-installed",
    "codecs-failed",
    "tour-navigate",
    "tour-organise",
    "tour-collaborate",
    "tour-start",
];

const FINGERPRINT: &str = "3f8a21c94b07de5512aa90c3e4f61d28b97c0e3a5f4d12b6c8e09a7d3b1f5c24";

/// The catalogue message holding a sample recovery key for review renders:
/// real BIP39 words, not anyone's key. A key's words are the key in every
/// language, so the catalogue carries the same words on both sides and only
/// the painter decides when they are shown.
const WORDS: &str = "onboarding-sample-recovery-words";

/// The page `name` shows.
#[allow(
    clippy::too_many_lines,
    reason = "one plain page per review scenario, side by side"
)]
fn scenario(name: &str) -> Page {
    let identity = || Page {
        screen: Screen::Done,
        acknowledged: true,
        total_steps: 4,
        backup_export_available: true,
        recovery_words_total: 24,
        ..Page::default()
    };
    let base = Page {
        total_steps: 4,
        recovery_words_total: 24,
        ..Page::default()
    };
    let recovery = Page {
        recovery_only: true,
        total_steps: 2,
        expected_identity: Some(FINGERPRINT.to_owned()),
        recovery_problem: Some(IdentityProblem::ChainInvalid),
        ..base.clone()
    };
    let tour = |page: TourPage| Page {
        identity_ready_page: true,
        backup_was_verified: true,
        tour_page: Some(page),
        ..identity()
    };
    let codecs = |state: PackState| Page {
        identity_ready_page: true,
        backup_was_verified: true,
        codec_page: true,
        total_steps: 5,
        codecs: Codecs {
            state,
            fraction: (state == PackState::Installing).then_some(0.37),
            summary: "Downloading (3 of 5)".to_owned(),
            last_error: if state == PackState::Failed {
                "Could not download the manifest (connection reset).".to_owned()
            } else {
                String::new()
            },
            ..Codecs::default()
        },
        ..identity()
    };
    let words = |check: WordsCheck| Page {
        screen: Screen::RecoveryKey,
        recovery_check: check,
        ..recovery.clone()
    };
    match name {
        "intro-host" => Page {
            context: OnboardingContext::HostDesktop,
            ..base
        },
        "existing" => Page {
            screen: Screen::Existing,
            ..base
        },
        "recovery" => Page {
            screen: Screen::Existing,
            ..recovery
        },
        "nearby" | "nearby-joining" | "nearby-failed" => Page {
            screen: Screen::Nearby,
            nearby: Nearby {
                active: name == "nearby-joining",
                failure: (name == "nearby-failed").then(|| {
                    (
                        crate::t("onboarding-nearby-used-title"),
                        crate::t("onboarding-nearby-used"),
                    )
                }),
            },
            error: (name == "nearby-failed").then(|| crate::t("onboarding-nearby-used")),
            ..base
        },
        "restore" | "restore-error" => Page {
            screen: Screen::Restore,
            connect_path: "/home/person/tilo-identity-backup.tilo-identity".to_owned(),
            error: (name == "restore-error").then(|| crate::t("onboarding-connect-failed")),
            ..base
        },
        "recovery-key-typing" => words(WordsCheck::Typing { words: 9 }),
        "recovery-key-typo" => words(WordsCheck::Problem(
            "Word 7, “abandn”, is not a recovery key word.".to_owned(),
        )),
        "recovery-key-ready" => words(WordsCheck::Ready {
            fingerprint: FINGERPRINT.to_owned(),
        }),
        "recovery-key-different" => words(WordsCheck::Different {
            fingerprint: "5a17c0de".repeat(8),
        }),
        "protect" | "protect-acknowledged" | "create-error" => Page {
            screen: Screen::NoRecovery,
            acknowledged: name == "protect-acknowledged",
            can_create: name == "protect-acknowledged",
            error: (name == "create-error").then(|| crate::t("onboarding-create-failed")),
            ..base
        },
        "backup" => identity(),
        "backup-skip-acknowledged" => Page {
            backup_skip_acknowledged: true,
            ..identity()
        },
        "backup-verified" => Page {
            backup_was_verified: true,
            backup_saved: Some(
                "/home/person/Documents/tilo-identity-backup.tilo-identity".to_owned(),
            ),
            ..identity()
        },
        "backup-recovery-key" => Page {
            recovery_key_copy: Some(KeyCopy {
                words: SecretText::new(Zeroizing::new(
                    crate::t(WORDS)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                )),
                checks: [3, 11, 20],
            }),
            ..identity()
        },
        "backup-recovery-key-written" => Page {
            recovery_key_written: true,
            backup_was_verified: true,
            ..identity()
        },
        "portable-backup" => Page {
            portable_backup_only: true,
            ..identity()
        },
        "ready" => Page {
            identity_ready_page: true,
            backup_was_verified: true,
            ..identity()
        },
        "ready-joined" => Page {
            identity_ready_page: true,
            joined_device: Some(crate::JoinedDevice {
                label: Some("Kitchen laptop".to_owned()),
                person: Some("Ada".to_owned()),
            }),
            ..identity()
        },
        "ready-host" => Page {
            identity_ready_page: true,
            joined_device: Some(crate::JoinedDevice {
                label: Some("Kitchen laptop".to_owned()),
                person: Some("Ada".to_owned()),
            }),
            context: OnboardingContext::HostDesktop,
            ..identity()
        },
        "codecs" => codecs(PackState::NotInstalled),
        "codecs-installing" => codecs(PackState::Installing),
        "codecs-waiting-for-network" => codecs(PackState::WaitingForNetwork),
        "codecs-installed" => codecs(PackState::Installed),
        "codecs-failed" => codecs(PackState::Failed),
        "tour-navigate" => tour(TourPage::Navigate),
        "tour-organise" => tour(TourPage::Organise),
        "tour-collaborate" => tour(TourPage::Collaborate),
        "tour-start" => tour(TourPage::Start),
        _ => base,
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(OnboardingApp)
}
