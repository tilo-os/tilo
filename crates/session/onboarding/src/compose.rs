//! The pages of setup, composed from the toolkit's widgets so setup takes
//! the system theme, focus order and accessibility like the rest of the
//! desktop. Each page is built when it is laid out, from the [`Page`] the
//! shell keeps current; each control asks through an [`OnboardingEffect`].

use std::sync::{Arc, Mutex};

use ui::theme::Theme;
use ui::widget::{
    Banner, ButtonRole, Checkbox, Column, Container, IconWidget, ListRow, ProgressBar, Row,
    Scrollable, Section, Spacer, Spinner, Text, TextInput, Widget,
};
use zeroize::Zeroizing;

use crate::ceremony::{
    ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT, Ceremony, FOOTER_HEIGHT, PROGRESS_HEADER_HEIGHT,
};
use crate::copy::{
    acknowledgement, codecs_background, codecs_note, codecs_title, done_body, done_title,
    host_desktop_intro_body, host_desktop_intro_title, intro_body, intro_points, intro_title,
    no_recovery_points, no_recovery_title,
};
use crate::effect::OnboardingEffect;
use crate::page::{KeyCopy, OnboardingContext, Screen, TourPage, Transfer, WordsCheck};
use crate::seat::SecretBuffer;
use crate::{t, t_args};

// Keep this copy beside the tour rather than duplicating a second conceptual
// keyboard map in prose. These labels mirror `docs/shell-navigation-keyboard-map.md`
// and the gesture contract in `compositor/backend/drm/gestures.rs`; in
// particular, Super+W closes a tab and must never be advertised as Overview.
/// Catalogue keys of each tour card's (title, body).
const NAVIGATE_CARDS: &[(&str, &str)] = &[
    (
        if cfg!(target_os = "macos") {
            "onboarding-tour-search-title-mac"
        } else {
            "onboarding-tour-search-title"
        },
        "onboarding-tour-search-body",
    ),
    (
        "onboarding-tour-overview-title",
        "onboarding-tour-overview-body",
    ),
    ("onboarding-tour-move-title", "onboarding-tour-move-body"),
];

const ORGANISE_CARDS: &[(&str, &str)] = &[
    (
        "onboarding-tour-workspace-title",
        "onboarding-tour-workspace-body",
    ),
    ("onboarding-tour-strip-title", "onboarding-tour-strip-body"),
    (
        "onboarding-tour-window-group-title",
        "onboarding-tour-window-group-body",
    ),
    ("onboarding-tour-tab-title", "onboarding-tour-tab-body"),
];

const COLLABORATE_CARDS: &[(&str, &str)] = &[
    (
        "onboarding-tour-invite-settings-title",
        "onboarding-tour-invite-settings-body",
    ),
    (
        "onboarding-tour-follow-title",
        "onboarding-tour-follow-body",
    ),
    (
        "onboarding-tour-system-features-title",
        "onboarding-tour-system-features-body",
    ),
];

/// A concrete hand-off from trusted setup into the otherwise-empty first
/// workspace.  The earlier tour pages explain the model; this one answers the
/// more important first-use question: "what should I do now?"  Keep these
/// labels aligned with real shell surfaces so the checklist never teaches a
/// product that does not exist.
const START_CARDS: &[(&str, &str)] = &[
    ("onboarding-tour-name-title", "onboarding-tour-name-body"),
    ("onboarding-tour-open-title", "onboarding-tour-open-body"),
    (
        "onboarding-tour-invite-title",
        "onboarding-tour-invite-body",
    ),
    ("onboarding-tour-check-title", "onboarding-tour-check-body"),
];

/// One way forward on a choice page.
struct Choice {
    id: &'static str,
    icon: ui::icons::Icon,
    title: &'static str,
    detail: &'static str,
    effect: OnboardingEffect,
}

impl Ceremony {
    fn progress_notice(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        self.page.progress_error.as_ref().map(|error| {
            spaced_banner(
                Banner::error(t("onboarding-progress-attention")).with_message(error.clone()),
                theme,
            )
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a declarative page composition covering every onboarding screen"
    )]
    pub(crate) fn build_composition(&self, theme: &Theme, panel_height: f32) -> Box<dyn Widget> {
        if self.page.migration.is_some() {
            return self.migration_composition(theme);
        }
        if let Some(page) = self.page.tour_page {
            return self.build_tour_composition(theme, panel_height, page);
        }
        let compact_ready = self.page.screen == Screen::Done
            && self.page.identity_ready_page
            && theme.text_scale <= 1.0
            && !theme.name.eq_ignore_ascii_case("high contrast");
        let total = self.page.total_steps;
        let (step, stage) = if self.page.recovery_only && self.page.screen == Screen::Existing {
            (1, t("onboarding-stage-choose"))
        } else if self.page.recovery_only || self.page.prepared.is_some() {
            (2, t("onboarding-stage-restore"))
        } else {
            match self.page.screen {
                Screen::Intro => (1, t("onboarding-stage-choose")),
                Screen::Existing | Screen::Nearby => (2, t("onboarding-stage-connect")),
                Screen::Restore | Screen::RecoveryKey => (2, t("onboarding-stage-restore")),
                Screen::NoRecovery => (2, t("onboarding-stage-protect")),
                Screen::Done if self.page.identity_ready_page && self.page.codec_page => {
                    (4, t("onboarding-stage-media"))
                }
                Screen::Done if self.page.identity_ready_page => {
                    (total, t("onboarding-stage-ready"))
                }
                Screen::Done => (3, t("onboarding-stage-backup")),
            }
        };
        let region_label = stage.clone(); // names the page's scroll region
        let body: Box<dyn Widget> = if self.page.prepared.is_some() {
            self.prepared_body(theme)
        } else {
            match self.page.screen {
                Screen::Intro => self.intro(theme),
                Screen::Existing => self.existing(theme),
                Screen::Nearby => self.nearby(theme),
                Screen::Restore => self.restore(theme),
                Screen::RecoveryKey => self.recovery_key(theme),
                Screen::NoRecovery => self.no_recovery(theme),
                Screen::Done if self.page.identity_ready_page && self.page.codec_page => {
                    self.codecs(theme)
                }
                Screen::Done if self.page.identity_ready_page => self.done(theme, compact_ready),
                Screen::Done => self.backup(theme),
            }
        };
        let spacing = &theme.tokens.spacing;
        let progress_value = f32::from(step) / f32::from(total);
        let progress = Column::new()
            .with_no_padding()
            // The header clears the panel's top edge by `s3`, and the
            // progress track stays `s3` inside both panel edges (HIG §9.5: a
            // rule inside a Panel never runs full-bleed into the corner
            // radius). The labels add their own `s3` so they sit on the
            // body's `s6` gutter.
            .with_extra_top_padding(spacing.s3)
            .with_horizontal_padding(spacing.s3, spacing.s3)
            .with_intrinsic_height()
            .with_gap(spacing.s3)
            .push(Box::new(
                Row::new()
                    .with_padding(spacing.s3, 0.0)
                    .push(Box::new(
                        Text::new(&t(if self.page.recovery_only {
                            "onboarding-recovery-eyebrow"
                        } else {
                            "onboarding-progress-eyebrow"
                        }))
                        .with_id("onboarding.progress.eyebrow")
                        .with_role(ui::tokens::typography::TextRole::Label)
                        .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&t_args(
                            "onboarding-progress-stage",
                            &[("step", step.into()), ("total", total.into())],
                        ))
                        .with_id("onboarding.progress.stage")
                        .with_role(ui::tokens::typography::TextRole::Label)
                        .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_vertical_padding(spacing.s2)
                    .push(Box::new(ProgressBar::new(progress_value).with_label(
                        t_args(
                            "onboarding-progress-a11y",
                            &[
                                ("step", step.into()),
                                ("total", total.into()),
                                ("stage", stage.into()),
                            ],
                        ),
                    ))),
            ));
        let mut content = Column::new().with_intrinsic_height().with_gap(spacing.s4);
        if let Some(notice) = self.progress_notice(theme) {
            content = content.push(notice);
        }
        content = content.push(body);
        // Keep progression outside the scroll region; the backup explanation
        // and form share one body, with acknowledgment and Continue anchored.
        // Ready is exactly the full safe height on an 800x600 display. Its
        // two column gaps use s2 so the complete hand-off fits without an
        // ornamental 8 px scroll range; the footer still retains the 8 px
        // separation required by the interaction contract.
        let composition_gap = if self.page.identity_ready_page {
            spacing.s2
        } else {
            spacing.s3
        };
        let footer_gap = composition_gap;
        let backup_in_scroll = self.page.screen == Screen::Done && !self.page.identity_ready_page;
        let acknowledgement_footer = matches!(self.page.screen, Screen::NoRecovery);
        let choice_footer = self.choice_footer_height(theme);
        let backup_acknowledgement_visible = backup_in_scroll
            && !self.page.portable_backup_only
            && !self.page.backup_was_verified
            && self.page.recovery_key_copy.is_none();
        let footer_height = if backup_in_scroll {
            FOOTER_HEIGHT
                + if backup_acknowledgement_visible {
                    ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT
                } else {
                    0.0
                }
        } else if let Some(height) = choice_footer {
            height
        } else {
            FOOTER_HEIGHT
                + if acknowledgement_footer {
                    ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT
                } else {
                    0.0
                }
        };
        let scroll_height =
            (panel_height - PROGRESS_HEADER_HEIGHT - footer_height - footer_gap).max(120.0);
        let closing_ready = self.page.screen == Screen::Done
            && self.page.identity_ready_page
            && !self.page.codec_page;
        let fill_panel = !closing_ready
            && (theme.text_scale > 1.0
                || theme.name.eq_ignore_ascii_case("high contrast")
                || matches!(
                    self.page.screen,
                    Screen::Intro
                        | Screen::Nearby
                        | Screen::Restore
                        | Screen::RecoveryKey
                        | Screen::NoRecovery
                )
                || (self.page.recovery_only && self.page.screen == Screen::Existing)
                || self.page.screen == Screen::Done);
        let mut scroll = Scrollable::new_vertical();
        if backup_in_scroll {
            // Backup is a form: Tab should enter its first field/control, and
            // focus-follow scrolling reveals later controls. Explanatory
            // pages retain the viewport stop for Page/Home/End navigation.
            scroll = scroll.without_sequential_focus().with_focus_reveal();
        }
        let mut scroll = scroll;
        if !backup_in_scroll {
            // This is first-run guidance, not an incidental list. Keep the
            // thumb visible whenever content overflows so compact and large
            // text users have an unmistakable cue that more explanation is
            // available below the sticky footer.
            scroll = scroll.always_visible();
        }
        let bottom_content_inset = if compact_ready { 0.0 } else { spacing.s3 };
        let scroll = scroll
            // Keep a clean terminal edge even on Backup's in-flow decision
            // block, so the last control is not painted against the clip.
            // Normal-text Ready already has the parent's `s3` footer gap;
            // repeating it inside the scroller was the last 12 px of a
            // needless 800×600 overflow.
            .with_bottom_content_inset(bottom_content_inset)
            // A 960×540 large-text/high-contrast dialog resolves to a 492 px
            // scroll viewport. Keep its first frame's edge in the gap between
            // the form card and recovery copy; the full-height 672 px dialog
            // retains room for the acknowledgement and Continue action.
            .with_compact_bottom_content_inset(520.0, 0.0)
            .with_label(region_label)
            .with_child(Box::new(content));
        let scroll = if fill_panel {
            scroll.fill_parent_height()
        } else {
            scroll.with_max_height(scroll_height)
        };
        let composition = Column::new()
            .with_no_padding()
            .with_gap(composition_gap)
            .push(Box::new(progress))
            .push(Box::new(scroll));
        if backup_in_scroll {
            Box::new(composition.push(self.backup_decision_footer(theme)))
        } else {
            Box::new(composition.push(self.identity_footer(theme)))
        }
    }

    /// The recovery form lives with its explanation in one scrolling body.
    fn backup_form(&self, theme: &Theme) -> Box<dyn Widget> {
        let explanation = paragraph(&t("onboarding-backup-explanation"), theme);
        let fields = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(labelled_secret_field(
                "onboarding.backup.passphrase",
                &t("onboarding-backup-passphrase"),
                &self.secrets.backup_passphrase,
                &t("onboarding-backup-passphrase"),
                &t("onboarding-backup-passphrase-placeholder"),
            ))
            .push(labelled_secret_field(
                "onboarding.backup.confirmation",
                &t("onboarding-backup-confirmation"),
                &self.secrets.backup_confirmation,
                &t("onboarding-backup-confirmation"),
                &t("onboarding-backup-confirmation-placeholder"),
            ));
        let action = self
            .button(
                "onboarding.backup.button",
                &if self.page.backup_was_verified {
                    t("onboarding-backup-save-another")
                } else {
                    t("onboarding-backup-save")
                },
                OnboardingEffect::Browse,
            )
            .role(if self.page.backup_was_verified {
                // Continue remains the clear completion action after a verified
                // backup; making another copy is an optional follow-up task.
                ui::widget::ButtonRole::Secondary
            } else {
                ui::widget::ButtonRole::Primary
            });
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s2)
                .push(explanation)
                .push(Box::new(fields))
                .push(footer_actions(vec![Box::new(action)])),
        )
    }

    fn backup_decision_footer(&self, theme: &Theme) -> Box<dyn Widget> {
        if self.page.portable_backup_only
            || self.page.backup_was_verified
            || self.page.recovery_key_copy.is_some()
        {
            return self.identity_actions();
        }
        acknowledgement_footer(
            theme,
            self.acknowledgement(
                "onboarding.backup.skip-acknowledged.row",
                t("onboarding-backup-skip-acknowledgement"),
                self.page.backup_skip_acknowledged,
            ),
            self.identity_actions(),
        )
    }

    /// The ways forward from a choice screen, one row per flow.
    fn choices(&self) -> Option<Vec<Choice>> {
        if self.page.prepared.is_some() {
            return None;
        }
        match self.page.screen {
            Screen::Intro => Some(vec![
                Choice {
                    id: "onboarding.existing.card",
                    icon: ui::icons::Icon::User,
                    title: "onboarding-choice-existing",
                    detail: "onboarding-choice-existing-detail",
                    effect: OnboardingEffect::choose("existing"),
                },
                Choice {
                    id: "onboarding.begin.card",
                    icon: ui::icons::Icon::Plus,
                    title: "onboarding-choice-create",
                    detail: "onboarding-choice-create-detail",
                    effect: OnboardingEffect::choose("create"),
                },
            ]),
            Screen::Existing => Some(vec![
                Choice {
                    id: "onboarding.existing.nearby.card",
                    icon: ui::icons::Icon::Monitor,
                    title: "onboarding-choice-nearby",
                    detail: "onboarding-choice-nearby-detail",
                    effect: OnboardingEffect::choose("nearby"),
                },
                Choice {
                    id: "onboarding.existing.recovery-key.card",
                    icon: ui::icons::Icon::ListOrdered,
                    title: "onboarding-choice-recovery-key",
                    detail: "onboarding-choice-recovery-key-detail",
                    effect: OnboardingEffect::choose("recovery-key"),
                },
                Choice {
                    id: "onboarding.existing.restore.card",
                    icon: ui::icons::Icon::Archive,
                    title: "onboarding-choice-restore",
                    detail: "onboarding-choice-restore-detail",
                    effect: OnboardingEffect::choose("backup-file"),
                },
            ]),
            _ => None,
        }
    }

    /// The pinned footer of a choice screen: its rows stay in view at every
    /// size, like the action row of the other screens. Intro has nothing to
    /// go back to; the use-existing choice keeps its Back (or keyring retry)
    /// row below the rows.
    fn choice_footer_height(&self, theme: &Theme) -> Option<f32> {
        let choices = self.choices()?;
        let count = f32::from(u8::try_from(choices.len()).unwrap_or(u8::MAX));
        let stack = count * choice_row_height(theme);
        let spacing = &theme.tokens.spacing;
        Some(if self.page.screen == Screen::Intro {
            stack + spacing.s4
        } else {
            stack + spacing.s2 + FOOTER_HEIGHT
        })
    }

    fn choice_footer(&self, theme: &Theme, choices: Vec<Choice>) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let rows = choices
            .into_iter()
            .map(|choice| {
                let dispatch = self.dispatch.clone();
                Box::new(
                    ListRow::new(choice.id, t(choice.title))
                        .icon(choice.icon)
                        .secondary(t(choice.detail))
                        .disclosure()
                        .on_activate(move || dispatch.effect(&choice.effect)),
                ) as Box<dyn Widget>
            })
            .collect();
        let stack = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_horizontal_padding(spacing.s3, spacing.s3)
            .push(Box::new(Section::new(rows)));
        if self.page.screen == Screen::Intro {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(0.0)
                    .push(Box::new(stack))
                    // The intro's rows have no action row below them; keep
                    // the action row's visible inset from the panel edge.
                    .push(Box::new(Spacer::fixed(spacing.s4))),
            );
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s2)
                .push(Box::new(stack))
                .push(self.identity_actions()),
        )
    }

    fn identity_footer(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(choices) = self.choices() {
            return self.choice_footer(theme, choices);
        }
        let acknowledgement = match self.page.screen {
            // A first session returns from creating its identity straight to
            // the desktop: there is nothing left to acknowledge or accept.
            Screen::Done if self.page.joined_member => None,
            Screen::NoRecovery => Some(self.acknowledgement(
                "onboarding.understood.row",
                acknowledgement(),
                self.page.acknowledged,
            )),
            _ => None,
        };
        let Some(acknowledgement) = acknowledgement else {
            return self.identity_actions();
        };
        acknowledgement_footer(theme, acknowledgement, self.identity_actions())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a declarative footer builder with one arm per onboarding screen"
    )]
    fn identity_actions(&self) -> Box<dyn Widget> {
        if self.page.prepared.is_some() {
            return footer_actions(vec![Box::new(
                self.button(
                    "onboarding.prepared.import.button",
                    &t("onboarding-continue"),
                    OnboardingEffect::Continue,
                )
                .role(ui::widget::ButtonRole::Primary)
                .with_enabled(
                    !self
                        .page
                        .prepared
                        .as_ref()
                        .is_some_and(|prepared| prepared.working),
                ),
            )]);
        }
        match self.page.screen {
            Screen::Intro => footer_actions(Vec::new()),
            Screen::Existing if self.page.recovery_only => footer_actions(vec![Box::new(
                self.button(
                    "onboarding.connect.retry.button",
                    &t("onboarding-connect-retry-keyring"),
                    OnboardingEffect::Retry,
                )
                .with_icon(ui::icons::Icon::Refresh),
            )]),
            Screen::Existing => footer_actions(vec![self.back_button()]),
            Screen::Nearby => {
                let mut actions = vec![self.back_button()];
                if self.page.nearby.active {
                    actions.push(Box::new(self.button(
                        "onboarding.connect.cancel.button",
                        &t("onboarding-cancel"),
                        OnboardingEffect::Cancel,
                    )));
                } else {
                    actions.push(Box::new(
                        self.button(
                            "onboarding.connect.nearby.button",
                            &t("onboarding-nearby-try-again"),
                            OnboardingEffect::Retry,
                        )
                        .with_icon(ui::icons::Icon::Refresh)
                        .role(ui::widget::ButtonRole::Primary),
                    ));
                }
                footer_actions(actions)
            }
            Screen::RecoveryKey => footer_actions(vec![
                self.back_button(),
                Box::new(
                    self.button(
                        "onboarding.recovery-key.restore.button",
                        &t("onboarding-restore-backup"),
                        OnboardingEffect::Continue,
                    )
                    .with_icon(ui::icons::Icon::ArrowRight)
                    .role(ui::widget::ButtonRole::Primary)
                    .with_enabled(
                        matches!(self.page.recovery_check, WordsCheck::Ready { .. })
                            && !self.page.busy,
                    ),
                ),
            ]),
            Screen::Restore => footer_actions(vec![
                self.back_button(),
                Box::new(
                    self.button(
                        "onboarding.import.button",
                        &t("onboarding-restore-backup"),
                        OnboardingEffect::Continue,
                    )
                    .with_icon(ui::icons::Icon::ArrowRight)
                    .role(ui::widget::ButtonRole::Primary)
                    .with_enabled(!self.path.with(|path| path.trim().is_empty())),
                ),
            ]),
            Screen::NoRecovery => footer_actions(vec![
                self.back_button(),
                Box::new(
                    self.button(
                        "onboarding.create.button",
                        &t("onboarding-create-local-identity"),
                        OnboardingEffect::Continue,
                    )
                    .with_icon(ui::icons::Icon::Shield)
                    .role(ui::widget::ButtonRole::Primary)
                    .with_enabled(self.page.can_create),
                ),
            ]),
            Screen::Done if self.page.identity_ready_page && self.page.codec_page => {
                self.codec_actions()
            }
            Screen::Done if self.page.identity_ready_page => self.finish_action(),
            Screen::Done
                if !self.page.identity_ready_page && self.page.recovery_key_copy.is_some() =>
            {
                footer_actions(vec![
                    Box::new(self.button(
                        "onboarding.backup.recovery-key.close.button",
                        &t("onboarding-back"),
                        OnboardingEffect::Back,
                    )),
                    Box::new(
                        self.button(
                            "onboarding.backup.recovery-key.confirm.button",
                            &t("onboarding-recovery-key-confirm-button"),
                            OnboardingEffect::Continue,
                        )
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(self.page.recovery_key_copy_confirmed),
                    ),
                ])
            }
            Screen::Done if self.page.portable_backup_only => footer_actions(vec![
                Box::new(self.button(
                    "onboarding.backup.cancel.button",
                    &t("onboarding-cancel"),
                    OnboardingEffect::Finish,
                )),
                Box::new(
                    self.button(
                        "onboarding.backup.done.button",
                        &t("onboarding-done"),
                        OnboardingEffect::Continue,
                    )
                    .role(ui::widget::ButtonRole::Primary)
                    .with_enabled(self.page.backup_was_verified),
                ),
            ]),
            Screen::Done if self.page.backup_was_verified && self.page.backup_copy_form_open => {
                footer_actions(vec![Box::new(
                    self.button(
                        "onboarding.backup.continue.button",
                        &t("onboarding-continue"),
                        OnboardingEffect::Continue,
                    )
                    .role(ui::widget::ButtonRole::Primary),
                )])
            }
            Screen::Done if self.page.backup_was_verified => footer_actions(vec![
                Box::new(self.button(
                    "onboarding.backup.another.button",
                    &t("onboarding-backup-save-another"),
                    OnboardingEffect::choose("another-copy"),
                )),
                Box::new(
                    self.button(
                        "onboarding.backup.continue.button",
                        &t("onboarding-continue"),
                        OnboardingEffect::Continue,
                    )
                    .role(ui::widget::ButtonRole::Primary),
                ),
            ]),
            Screen::Done => footer_actions(vec![Box::new(
                self.button(
                    "onboarding.backup.continue.button",
                    &t("onboarding-backup-continue-without"),
                    OnboardingEffect::Continue,
                )
                .role(ui::widget::ButtonRole::Secondary)
                .with_enabled(self.page.backup_skip_acknowledged),
            )]),
        }
    }

    fn build_tour_composition(
        &self,
        theme: &Theme,
        panel_height: f32,
        page: TourPage,
    ) -> Box<dyn Widget> {
        let number = page.number();
        let stage = match page {
            TourPage::Navigate => t("onboarding-tour-stage-move"),
            TourPage::Organise => t("onboarding-tour-stage-arrange"),
            TourPage::Collaborate => t("onboarding-tour-stage-share"),
            TourPage::Start => t("onboarding-tour-stage-begin"),
        };
        let progress = Column::new()
            .with_no_padding()
            // Same header rhythm as the identity pages: `s3` off the panel's
            // top edge, the track `s3` inside both edges, labels on the `s6`
            // gutter.
            .with_extra_top_padding(theme.tokens.spacing.s3)
            .with_horizontal_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s3)
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(
                Row::new()
                    .with_padding(theme.tokens.spacing.s3, 0.0)
                    .push(Box::new(
                        Text::new(&t("onboarding-tour-eyebrow"))
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&t_args("onboarding-tour-stage", &[("step", number.into())]))
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_vertical_padding(theme.tokens.spacing.s2)
                    .push(Box::new(
                        ProgressBar::new(f32::from(number) / 4.0).with_label(t_args(
                            "onboarding-tour-progress-a11y",
                            &[("stage", stage.into()), ("step", number.into())],
                        )),
                    )),
            ));
        let mut content = Column::new()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s6);
        if let Some(notice) = self.progress_notice(theme) {
            content = content.push(notice);
        }
        content = content.push(Self::tour_page(theme, page));
        // Keep navigation out of the scrolling lesson. On a compact laptop,
        // portrait display, or large-text setup the cards can be much taller
        // than the panel; making Continue part of that content forced people
        // to discover scrolling before they could finish the introduction.
        // The identity Done screen uses the same sticky-footer contract.
        let footer_gap = theme.tokens.spacing.s3;
        let scroll_height =
            (panel_height - PROGRESS_HEADER_HEIGHT - FOOTER_HEIGHT - footer_gap).max(120.0);
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(footer_gap)
                .push(Box::new(progress))
                .push(Box::new(
                    Scrollable::new_vertical()
                        .with_max_height(scroll_height)
                        // Tour pages intentionally teach several concepts;
                        // expose overflow persistently instead of making a
                        // first-time user wait for a transient wheel hint.
                        .always_visible()
                        .with_child(Box::new(content)),
                ))
                .push(self.tour_actions(page)),
        )
    }

    fn tour_page(theme: &Theme, page: TourPage) -> Box<dyn Widget> {
        let (title, intro, cards): (&str, &str, &[(&str, &str)]) = match page {
            TourPage::Navigate => (
                "onboarding-tour-navigate-title",
                "onboarding-tour-navigate-intro",
                NAVIGATE_CARDS,
            ),
            TourPage::Organise => (
                "onboarding-tour-organise-title",
                "onboarding-tour-organise-intro",
                ORGANISE_CARDS,
            ),
            TourPage::Collaborate => (
                "onboarding-tour-collaborate-title",
                "onboarding-tour-collaborate-intro",
                COLLABORATE_CARDS,
            ),
            TourPage::Start => (
                "onboarding-tour-start-title",
                "onboarding-tour-start-intro",
                START_CARDS,
            ),
        };
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            // The final page is an actionable checklist, not four unrelated
            // ideas. A single grouped checklist preserves the hierarchy and
            // keeps every first-session task above the sticky navigation on a
            // laptop-height display.
            .with_gap(if page == TourPage::Start {
                theme.tokens.spacing.s4
            } else {
                theme.tokens.spacing.s6
            })
            .push(heading(&t(title), theme))
            .push(paragraph(&t(intro), theme));
        if page == TourPage::Start {
            let mut checklist = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s2);
            for (number, (card_title, copy)) in (1_usize..).zip(cards) {
                checklist = checklist.push(Box::new(
                    Row::new()
                        .no_wrap()
                        .align_start()
                        .with_gap(theme.tokens.spacing.s3)
                        .push(Box::new(
                            Text::new(&number.to_string())
                                .with_role(ui::tokens::typography::TextRole::Label)
                                .strong()
                                .numeric()
                                .with_color(theme.text_dim),
                        ))
                        .push(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_gap(theme.tokens.spacing.s1)
                                .push(Box::new(
                                    Text::new(&t(card_title))
                                        .with_role(ui::tokens::typography::TextRole::Label)
                                        .with_color(theme.text_primary),
                                ))
                                .push(paragraph(&t(copy), theme)),
                        )),
                ));
            }
            // The step numbers already provide the grouping and order. A
            // second titled/padded card spent nearly a full row of a laptop
            // viewport on decoration and pushed the last action below the
            // sticky footer.
            body = body.push(Box::new(checklist));
        } else {
            for (card_title, copy) in cards {
                let content: Box<dyn Widget> = match tour_shortcut(card_title) {
                    Some(keys) => Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_gap(theme.tokens.spacing.s1)
                            .push(Box::new(
                                Text::new(&t(keys))
                                    .with_role(ui::tokens::typography::TextRole::Label)
                                    .with_color(theme.text_primary),
                            ))
                            .push(paragraph(&t(copy), theme)),
                    ),
                    None => paragraph(&t(copy), theme),
                };
                body = body.push(Box::new(Section::titled(t(card_title), vec![content])));
            }
        }
        Box::new(body)
    }

    /// Sticky tour navigation, kept outside the scrollable lesson body.
    fn tour_actions(&self, page: TourPage) -> Box<dyn Widget> {
        let (primary_label, primary_key) = if page == TourPage::Start {
            (t("onboarding-tour-enter"), OnboardingEffect::Finish)
        } else {
            (t("onboarding-continue"), OnboardingEffect::Continue)
        };
        footer_actions(vec![
            Box::new(self.button(
                "onboarding.tour.skip.button",
                &t("onboarding-tour-skip"),
                OnboardingEffect::Skip,
            )),
            Box::new(self.button(
                "onboarding.tour.back.button",
                &t("onboarding-back"),
                OnboardingEffect::Back,
            )),
            Box::new(
                self.button("onboarding.tour.next.button", &primary_label, primary_key)
                    .role(ui::widget::ButtonRole::Primary),
            ),
        ])
    }

    fn intro(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let (title, body) = match self.page.context {
            OnboardingContext::FullSystem => (intro_title(), intro_body()),
            OnboardingContext::HostDesktop => {
                (host_desktop_intro_title(), host_desktop_intro_body())
            }
        };
        let icons = [
            ui::icons::Icon::Users,
            ui::icons::Icon::HardDrive,
            ui::icons::Icon::Lock,
        ];
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s4)
                .push(heading(&title, theme))
                .push(paragraph(&body, theme))
                .push(fact_list(icons.into_iter().zip(intro_points()), theme)),
        )
    }

    /// How to bring an identity that already exists to this computer. Each
    /// way opens its own screen from the cards in the footer.
    fn existing(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s4);
        if self.page.recovery_only {
            // Recovery is reached from a computer that already belongs to
            // someone: say what happened and that nothing was replaced,
            // then the same ways back as setting up a new computer.
            use crate::page::IdentityProblem;
            let explanation = match self.page.recovery_problem {
                Some(IdentityProblem::KeyringUnavailable) => "onboarding-recovery-body-keyring",
                Some(IdentityProblem::RecordMissing) => "onboarding-recovery-body-missing",
                Some(IdentityProblem::ChainInvalid) => "onboarding-recovery-body-invalid",
                None => "onboarding-recovery-body",
            };
            body = body
                .push(heading(&t("onboarding-recovery-title"), theme))
                .push(paragraph(&t(explanation), theme));
            if let Some(identity) = &self.page.expected_identity {
                // Which identity to bring back: matches the start of the
                // fingerprint shown on the person's other devices.
                body = body.push(Box::new(
                    Text::new(&t_args(
                        "onboarding-recovery-identity",
                        &[("fingerprint", short_fingerprint(identity).into())],
                    ))
                    .with_id("onboarding.recovery.identity")
                    .with_role(ui::tokens::typography::TextRole::Label)
                    .with_color(theme.text_dim)
                    .numeric(),
                ));
            }
        } else {
            body = body
                .push(heading(&t("onboarding-existing-title"), theme))
                .push(paragraph(&t("onboarding-existing-body"), theme));
        }
        if let Some(error) = &self.page.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-connect-failed")).with_message(error.clone()),
                theme,
            ));
        }
        Box::new(body)
    }

    /// Receive the identity from a trusted device: what to do on the other
    /// device, then the shared verification code once it appears.
    fn nearby(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut steps = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s3);
        for (number, key) in (1_usize..).zip([
            "onboarding-nearby-step-open",
            "onboarding-nearby-step-add",
            "onboarding-nearby-step-compare",
        ]) {
            steps = steps.push(numbered_step(number, &t(key), theme));
        }
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&t("onboarding-nearby-title"), theme))
            .push(paragraph(&t("onboarding-nearby-body"), theme));
        if let Some(error) = &self.page.error {
            // The banner's title is what went wrong; a screen reader speaks
            // it first, so it names the cause rather than only the outcome.
            let banner = match &self.page.nearby.failure {
                Some((title, message)) => {
                    Banner::error(title.clone()).with_message(message.clone())
                }
                None => Banner::error(t("onboarding-connect-failed")).with_message(error.clone()),
            };
            body = body.push(spaced_banner(banner, theme));
        }
        let mut status = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s2);
        if let Some(code) = &self.page.nearby.code {
            status = status
                .push(Box::new(
                    Text::new(&t("onboarding-nearby-code-label"))
                        .with_role(ui::tokens::typography::TextRole::Label)
                        .with_color(theme.text_dim),
                ))
                .push(Box::new(
                    Text::new(&grouped_code(code))
                        .with_id("onboarding.nearby.code")
                        .with_role(ui::tokens::typography::TextRole::Display)
                        .numeric(),
                ));
        }
        if let Some(message) = &self.page.nearby.status {
            let text = Text::new(message)
                // A stable semantic address lets the accessibility host
                // recognise verification-code and completion updates as
                // changes to one status region across composition rebuilds.
                .with_id("onboarding.nearby.status")
                .with_role(ui::tokens::typography::TextRole::Body)
                .with_color(theme.text_dim)
                .with_live(ui::a11y::Politeness::Polite);
            status = if self.page.nearby.active && self.page.nearby.code.is_none() {
                status.push(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_gap(theme.tokens.spacing.s2)
                        .push(Box::new(Spinner::with_size(theme.tokens.metrics.icon_md)))
                        .push(Box::new(text)),
                ))
            } else {
                status.push(Box::new(text))
            };
        }
        Box::new(
            body.push(Box::new(Section::new(vec![Box::new(steps)])))
                .push(Box::new(status)),
        )
    }

    /// Restore the identity from an encrypted backup file.
    fn prepared_body(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s4)
            .push(heading(&t("onboarding-prepared-title"), theme))
            .push(paragraph(&t("onboarding-prepared-body"), theme));
        if let Some(error) = &self.page.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-prepared-failed")).with_message(error.clone()),
                theme,
            ));
        }
        if self
            .page
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.working)
        {
            body = body.push(paragraph(&t("onboarding-prepared-working"), theme));
            return Box::new(body);
        }
        if let Some(peer) = self
            .page
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.peer.as_ref())
        {
            // The package arrives over the peer transport, so there is no
            // password to type. Show the same code the preparing computer
            // shows; a person compares them before approving this computer
            // there. A code appears only once this receiver has bound its own
            // endpoint and key.
            if let Some(code) = peer.clone() {
                body = body.push(paragraph(
                    &t_args("onboarding-prepared-peer-code", &[("code", code.into())]),
                    theme,
                ));
            } else {
                body = body.push(paragraph(&t("onboarding-prepared-peer-waiting"), theme));
            }
            return Box::new(body);
        }
        body = body.push(Box::new(secret_input(
            "onboarding.prepared.password",
            &self.secrets.connect_passphrase,
            &t("onboarding-prepared-password"),
            &t("onboarding-prepared-password"),
        )));
        Box::new(body)
    }

    /// What the migration page is titled: the step in hand.
    fn migration_title_key(&self) -> &'static str {
        match self
            .page
            .migration
            .as_ref()
            .map(|migration| migration.transfer)
        {
            Some(Transfer::PortableHome) => "onboarding-portable-home-title",
            Some(Transfer::PortableApplications) => "onboarding-portable-applications-title",
            Some(Transfer::Browser) => "onboarding-browser-title",
            Some(Transfer::Migration) | None => "onboarding-migration-title",
        }
    }

    fn migration_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s4)
            .push(heading(&t(self.migration_title_key()), theme));
        let status = self.page.migration_status().unwrap_or_default();
        let state = status
            .get("state")
            .and_then(ui::serde_json::Value::as_str)
            .unwrap_or("unavailable");
        let retained = status
            .get("retained_access")
            .and_then(ui::serde_json::Value::as_bool)
            == Some(true);
        let text = migration_body_key(&status, state, retained);
        body = body.push(paragraph(&t(text), theme));
        if self
            .page
            .migration
            .as_ref()
            .is_some_and(|migration| migration.needs_password)
        {
            body = body.push(Box::new(secret_input(
                "onboarding.portable-home.password",
                &self.secrets.connect_passphrase,
                &t("onboarding-prepared-password"),
                &t("onboarding-prepared-password"),
            )));
        }
        if let Some(files) = status
            .get("result")
            .and_then(|result| result.get("verified_files"))
            .and_then(ui::serde_json::Value::as_u64)
        {
            body = body.push(paragraph(
                &t_args(
                    "onboarding-migration-progress",
                    &[("files", files.to_string().into())],
                ),
                theme,
            ));
        }

        if let Some(error) = &self.page.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-migration-failed")).with_message(error.clone()),
                theme,
            ));
        }
        body = body.push(self.migration_actions(state));
        Box::new(
            Scrollable::new_vertical()
                .without_sequential_focus()
                .with_child(Box::new(body)),
        )
    }

    /// What the migration's main button says in `state`.
    fn migration_next_label(&self, state: &str, cleanup_pending: bool) -> &'static str {
        match state {
            _ if cleanup_pending => "onboarding-migration-cleanup-resume",
            "pending" | "unavailable" | "failed" | "cancelled" => "onboarding-migration-review",
            "reviewed"
                if self.page.migration_status().and_then(|value| {
                    value
                        .get("retained_access")
                        .and_then(ui::serde_json::Value::as_bool)
                }) == Some(true) =>
            {
                "onboarding-existing-files-prepare-apps"
            }
            "reviewed"
                if self
                    .page
                    .migration_status()
                    .is_some_and(|value| json_flag(&value, "portable_applications")) =>
            {
                "onboarding-portable-applications-start"
            }
            "reviewed"
                if self
                    .page
                    .migration_status()
                    .is_some_and(|value| json_flag(&value, "browser")) =>
            {
                "onboarding-browser-start"
            }
            "reviewed" => "onboarding-migration-start",
            "ready-to-adopt" => "onboarding-migration-adopt",
            "complete" | "existing-files-available" => "onboarding-continue",
            "incomplete" => "onboarding-migration-retry",
            _ => "onboarding-migration-working",
        }
    }

    /// Whether the migration under way in `state` can still be cancelled.
    fn migration_cancellable(&self) -> bool {
        self.page
            .migration
            .as_ref()
            .is_some_and(|migration| migration.cancellable)
    }

    fn migration_actions(&self, state: &str) -> Box<dyn Widget> {
        let cleanup = self.page.migration_status().unwrap_or_default();
        let cleanup_pending = json_flag(&cleanup, "transfer_complete")
            && json_text(&cleanup, "cleanup_state") == Some("pending");
        let cleanup_blocked = json_flag(&cleanup, "transfer_complete")
            && json_text(&cleanup, "cleanup_state") != Some("finished");
        let busy = !cleanup_pending
            && [
                "copying",
                "verifying",
                "retirement-pending",
                "restoring-applications",
                "adopting",
                "reviewing",
                "cancelling",
                "releasing-applications",
                "activating-browser",
            ]
            .contains(&state);
        let label = self.migration_next_label(state, cleanup_pending);
        let mut actions: Vec<Box<dyn Widget>> = vec![Box::new(
            self.button(
                "onboarding.migration.next.button",
                &t(label),
                OnboardingEffect::Continue,
            )
            .role(ui::widget::ButtonRole::Primary)
            .with_enabled(!busy && (!cleanup_blocked || cleanup_pending)),
        )];
        if busy {
            actions.push(Box::new(
                self.button(
                    "onboarding.migration.cancel.button",
                    &t("onboarding-migration-cancel"),
                    OnboardingEffect::Cancel,
                )
                .with_enabled(self.migration_cancellable()),
            ));
        } else if !cleanup_blocked {
            actions.push(Box::new(
                self.button(
                    "onboarding.migration.later.button",
                    &t("onboarding-migration-later"),
                    OnboardingEffect::Skip,
                )
                .with_enabled(state != "unavailable"),
            ));
            if state == "incomplete" {
                actions.push(Box::new(self.button(
                    "onboarding.migration.continue.button",
                    &t("onboarding-continue"),
                    OnboardingEffect::choose("accept-incomplete"),
                )));
            }
        }
        footer_actions(actions)
    }

    fn restore(&self, theme: &Theme) -> Box<dyn Widget> {
        let compact = self.page.compact;
        let spacing = &theme.tokens.spacing;
        let path = self
            .path_input()
            .with_id("onboarding.connect.path.field")
            .with_accessible_name(t("onboarding-connect-file"))
            .with_placeholder(t("onboarding-connect-file-placeholder"));
        let passphrase = secret_input(
            "onboarding.connect.passphrase.field",
            &self.secrets.connect_passphrase,
            &t("onboarding-connect-passphrase"),
            &t("onboarding-connect-passphrase-placeholder"),
        );
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact { spacing.s3 } else { spacing.s4 })
            .push(heading(&t("onboarding-restore-title"), theme))
            .push(paragraph(&t("onboarding-restore-body"), theme));
        if let Some(error) = &self.page.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-connect-failed")).with_message(error.clone()),
                theme,
            ));
        }
        let file = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s1)
            .push(Box::new(
                Text::new(&t("onboarding-restore-file-label"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(
                Row::new().no_wrap().push(Box::new(path)).push(Box::new(
                    self.button(
                        "onboarding.connect.browse.button",
                        &t("onboarding-connect-browse"),
                        OnboardingEffect::Browse,
                    )
                    .with_icon(ui::icons::Icon::FolderOpen),
                )),
            ));
        let secret = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s1)
            .push(Box::new(
                Text::new(&t("onboarding-connect-passphrase"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(passphrase));
        Box::new(
            body.push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(if compact { spacing.s3 } else { spacing.s4 })
                    .push(Box::new(file))
                    .push(Box::new(secret)),
            )),
        )
    }

    fn no_recovery(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let icons = [ui::icons::Icon::Ban, ui::icons::Icon::Archive];
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(spacing.s3)
                    .push(heading(&t("onboarding-protect-title"), theme))
                    .push(spaced_banner(
                        Banner::warning(no_recovery_title())
                            .with_message(t("onboarding-protect-message"))
                            .with_icon(ui::icons::Icon::Shield),
                        theme,
                    )),
            ));
        if let Some(error) = &self.page.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-create-failed")).with_message(error.clone()),
                theme,
            ));
        }
        Box::new(body.push(fact_list(
            icons.into_iter().zip(no_recovery_points()),
            theme,
        )))
    }

    fn backup(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(err) = &self.page.error {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s6)
                    .push(spaced_banner(
                        Banner::error(t("onboarding-create-failed-title"))
                            .with_message(err.clone()),
                        theme,
                    ))
                    .push(paragraph(&t("onboarding-create-failed-message"), theme))
                    .push(actions(vec![self.back_button()])),
            );
        }
        if !self.page.backup_export_available {
            return self.done(theme, false);
        }
        if let Some(copy) = &self.page.recovery_key_copy {
            return recovery_key_copy_page(theme, copy, &self.secrets.key_answers);
        }
        let compact = self.page.compact;
        let mut backup = Column::new().with_no_padding().with_intrinsic_height();
        if let Some(error) = &self.page.backup_error {
            backup = backup.push(spaced_banner(
                Banner::error(t("onboarding-backup-failed")).with_message(error.clone()),
                theme,
            ));
        }
        if self.page.recovery_key_written && self.page.backup_saved.is_none() {
            backup = backup.push(spaced_banner(
                Banner::success(t("onboarding-recovery-key-written"))
                    .with_message(t("onboarding-recovery-key-written-detail"))
                    .with_icon(ui::icons::Icon::Check),
                theme,
            ));
        } else if let Some(path) = &self.page.backup_saved {
            backup = backup.push(spaced_banner(
                Banner::success(t("onboarding-backup-verified"))
                    .with_message(path.clone())
                    .with_icon(ui::icons::Icon::Check),
                theme,
            ));
        } else if self.page.backup_was_verified {
            backup = backup.push(spaced_banner(
                Banner::success(t("onboarding-backup-verified"))
                    .with_message(t("onboarding-backup-verified-earlier"))
                    .with_icon(ui::icons::Icon::Check),
                theme,
            ));
        }
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact {
                theme.tokens.spacing.s3
            } else {
                theme.tokens.spacing.s4
            })
            .push(Box::new(
                Text::new(&t("onboarding-backup-title"))
                    .with_id("onboarding.backup.heading")
                    .with_role(ui::tokens::typography::TextRole::LargeTitle),
            ));
        if self.page.backup_error.is_some()
            || self.page.backup_saved.is_some()
            || self.page.backup_was_verified
        {
            page = page.push(Box::new(backup));
        }
        if !self.page.recovery_key_written
            && (!self.page.backup_was_verified || self.page.backup_copy_form_open)
        {
            // The quicker way to keep the identity: 24 words on paper. Offered
            // first and beside the file, never instead of it, because the two
            // suit different people.
            let dispatch = self.dispatch.clone();
            page = page.push(Box::new(Section::new(vec![Box::new(
                ListRow::new(
                    "onboarding.backup.recovery-key.row",
                    t("onboarding-backup-recovery-key"),
                )
                .icon(ui::icons::Icon::ListOrdered)
                .secondary(t("onboarding-backup-recovery-key-detail"))
                .disclosure()
                .on_activate(move || {
                    dispatch.effect(&OnboardingEffect::choose("write-down-recovery-key"));
                }),
            )])));
        }
        if !self.page.backup_was_verified || self.page.backup_copy_form_open {
            page = page.push(self.backup_form(theme));
        }
        Box::new(page)
    }

    fn done(&self, theme: &Theme, compact: bool) -> Box<dyn Widget> {
        if !self.page.joined_member {
            // The hand-off is the footer action, exactly like every other
            // setup screen's: one control at the bottom edge, never a second
            // way in floating inside the page.
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(if compact {
                        theme.tokens.spacing.s3
                    } else {
                        theme.tokens.spacing.s4
                    })
                    .push(Box::new(
                        Text::new(&t("onboarding-ready-welcome"))
                            .with_id("onboarding.done.heading")
                            .with_role(ui::tokens::typography::TextRole::LargeTitle),
                    ))
                    .push(paragraph_with_id(
                        &t("onboarding-ready-next"),
                        theme,
                        "onboarding.done.body",
                    )),
            );
        }
        let (title, body, ready) = match self.page.context {
            OnboardingContext::FullSystem => {
                (done_title(), done_body(), t("onboarding-done-ready"))
            }
            OnboardingContext::HostDesktop => (
                t("onboarding-host-done-title"),
                t("onboarding-host-done-body"),
                t("onboarding-host-done-ready"),
            ),
        };
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            // Keep every hand-off block, with a full token between them. The
            // ordinary-text surface uses `s3` so its complete hand-off
            // decision fits a safe 800×600 panel without scrolling; large
            // text and high contrast retain the roomier scrollable rhythm.
            .with_gap(if compact {
                theme.tokens.spacing.s3
            } else {
                theme.tokens.spacing.s4
            })
            .push(heading(&title, theme))
            .push(spaced_banner(
                Banner::success(ready).with_icon(ui::icons::Icon::Check),
                theme,
            ))
            .push(paragraph(&body, theme))
            .push(Box::new(Section::titled(
                t("onboarding-done-next-title"),
                vec![paragraph(&t("onboarding-done-next-body"), theme)],
            )))
            .push(Box::new(Section::titled(
                t(done_backup_title_key(
                    self.page.joined_member,
                    self.page.backup_was_verified,
                )),
                vec![paragraph_with_id(
                    &t(done_backup_body_key(
                        self.page.joined_member,
                        self.page.backup_was_verified,
                    )),
                    theme,
                    "onboarding.done.backup-guidance",
                )],
            )));

        if self.page.context == OnboardingContext::FullSystem {
            page = page.push(Box::new(Section::titled(
                t("onboarding-done-system-care-title"),
                vec![paragraph(&t("onboarding-done-system-care-body"), theme)],
            )));
        }
        Box::new(page)
    }

    /// The offer to play more formats: a centred title and one short line
    /// over a single large Install button, with a quiet way past it. Nothing
    /// is chosen until the person presses Install; once they did, the live
    /// result takes the button's place.
    fn codecs(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let centred = |widget: Box<dyn Widget>| -> Box<dyn Widget> {
            Box::new(
                Row::new()
                    .with_padding(0.0, 0.0)
                    .no_wrap()
                    .push(Box::new(Spacer::new()))
                    .push(widget)
                    .push(Box::new(Spacer::new())),
            )
        };
        let outcome = self.codec_outcome();
        // The bare offer sits in the middle of the page; a result needs the
        // room for its banner.
        let top = if outcome.is_some() {
            spacing.s2
        } else if theme.text_scale > 1.0 {
            spacing.s6
        } else {
            spacing.s8 * 2.0
        };
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_extra_top_padding(top)
            .with_gap(spacing.s4)
            .push(centred(Box::new(
                ui::widget::IconWidget::new(ui::icons::Icon::Film)
                    .sized(ui::widget::IconSize::Large)
                    .accent(),
            )))
            .push(Box::new(
                Text::new(&codecs_title())
                    .with_role(ui::tokens::typography::TextRole::Display)
                    .with_align(ui::ParagraphAlign::Center),
            ))
            .push(Box::new(
                Text::new(&codecs_note())
                    .with_id("onboarding.codecs.note")
                    .with_role(ui::tokens::typography::TextRole::Body)
                    .with_color(theme.text_dim)
                    .with_align(ui::ParagraphAlign::Center),
            ));
        if let Some((title, message)) = outcome {
            let failed = self.page.codecs.error.is_some()
                || self.page.codecs.state == crate::page::PackState::Failed;
            let banner = match self.page.codecs.state {
                _ if failed => Banner::error(title),
                crate::page::PackState::Installed => {
                    Banner::success(title).with_icon(ui::icons::Icon::Check)
                }
                crate::page::PackState::WaitingForNetwork => Banner::warning(title),
                _ => Banner::info(title),
            };
            let banner = match message {
                Some(message) => banner.with_message(message),
                None => banner,
            };
            page = page.push(spaced_banner(banner, theme));
        }
        if let Some(fraction) = self.codec_progress() {
            page = page.push(Box::new(
                ProgressBar::new(fraction).with_label(t("onboarding-codecs-installing-title")),
            ));
        }
        if let Some([(id, label, key), (quiet_id, quiet_label, quiet_key)]) = self.codec_choice() {
            // The one way in is this button. It holds keyboard focus so
            // Enter is the same deliberate press; it starts the download and
            // never moves past the page by itself.
            let install = self
                .button(id, &label, key)
                .control_size(ui::widget::ControlSize::Large)
                .with_icon(ui::icons::Icon::Download)
                .role(ButtonRole::Primary)
                .with_default_focus();
            let skip = self
                .button(quiet_id, &quiet_label, quiet_key)
                .role(ButtonRole::Plain);
            if self.codec_outcome().is_none() {
                page = page.push(Box::new(Spacer::fixed(spacing.s2)));
            }
            page = page
                .push(centred(Box::new(install)))
                .push(centred(Box::new(skip)));
        }
        Box::new(page)
    }

    fn codec_actions(&self) -> Box<dyn Widget> {
        if self.codec_choice().is_some() {
            return footer_actions(Vec::new());
        }
        footer_actions(vec![Box::new(
            self.button(
                "onboarding.codecs.continue.button",
                &t("onboarding-continue"),
                OnboardingEffect::Continue,
            )
            .control_size(ui::widget::ControlSize::Large)
            .with_icon(ui::icons::Icon::ArrowRight)
            .role(ButtonRole::Primary)
            .with_default_focus(),
        )])
    }

    /// The one action that ends the ceremony, in the same sticky footer as
    /// every other setup screen's.
    fn finish_action(&self) -> Box<dyn Widget> {
        // Entering the desktop is the hand-off and the only way out of the
        // ceremony. A first session has no tour to accept or decline: the
        // desktop teaches itself by being used, and its own overlay hint
        // and search carry what a page of prose would have said.
        let enter = self
            .button(
                "onboarding.finish.button",
                &t("onboarding-finish"),
                OnboardingEffect::Finish,
            )
            .with_icon(ui::icons::Icon::ArrowRight)
            .role(ui::widget::ButtonRole::Primary);
        footer_actions(vec![Box::new(enter)])
    }

    /// Restore the identity from the 24 words of its recovery key, checking
    /// them while they are typed so a mistake shows where it is.
    fn recovery_key(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&t("onboarding-recovery-key-title"), theme))
            .push(paragraph(&t("onboarding-recovery-key-body"), theme));
        if let Some(error) = &self.page.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-recovery-key-failed")).with_message(error.clone()),
                theme,
            ));
        }
        let mut words = TextInput::local()
            .with_id("onboarding.recovery-key.field")
            .with_external_buffer(Arc::clone(&self.secrets.recovery_words))
            .with_accessible_name(t("onboarding-recovery-key-field"))
            .with_placeholder(t("onboarding-recovery-key-placeholder"))
            .with_mono()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .multiline(true)
            .with_min_lines(4)
            // Each mistyped word is marked where it is, not only named.
            .with_mistakes(self.mistakes.unwrap_or(|_| Vec::new()))
            // The words stay put while they are being restored.
            .with_read_only(self.page.busy);
        if let Ok(value) = self.secrets.recovery_words.lock() {
            words = words.with_initial_value(&value);
        }
        let field = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s1)
            .push(Box::new(
                Text::new(&t("onboarding-recovery-key-field"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(words));
        body = body.push(Box::new(field));
        if let Some(status) = self.recovery_key_status(theme) {
            body = body.push(status);
        }
        if self.page.busy {
            body = body.push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_gap(spacing.s2)
                    .push(Box::new(Spinner::with_size(theme.tokens.metrics.icon_md)))
                    .push(Box::new(
                        Text::new(&t("onboarding-recovery-key-restoring"))
                            .with_role(ui::tokens::typography::TextRole::Body)
                            .with_color(theme.text_dim)
                            .with_live(ui::a11y::Politeness::Polite),
                    )),
            ));
        }
        Box::new(body)
    }

    /// What the words typed so far amount to, under the field.
    fn recovery_key_status(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        match &self.page.recovery_check {
            WordsCheck::Empty => None,
            WordsCheck::Typing { words } => Some(Box::new(
                Text::new(&t_args(
                    "onboarding-recovery-key-progress",
                    &[
                        ("count", (*words).into()),
                        ("total", self.page.recovery_words_total.into()),
                    ],
                ))
                .with_id("onboarding.recovery-key.status")
                .with_role(ui::tokens::typography::TextRole::Body)
                .with_color(theme.text_dim)
                .numeric(),
            )),
            WordsCheck::Problem(problem) => Some(spaced_banner(
                Banner::warning(problem.clone()).with_icon(ui::icons::Icon::Pencil),
                theme,
            )),
            WordsCheck::Ready { fingerprint } => {
                let message = if self.page.expected_identity.is_some() {
                    t("onboarding-recovery-key-matches")
                } else {
                    t("onboarding-recovery-key-ready-detail")
                };
                Some(spaced_banner(
                    Banner::success(t_args(
                        "onboarding-recovery-key-ready",
                        &[("fingerprint", short_fingerprint(fingerprint).into())],
                    ))
                    .with_message(message),
                    theme,
                ))
            }
            WordsCheck::Different { fingerprint } => Some(spaced_banner(
                Banner::error(t("onboarding-recovery-key-different")).with_message(t_args(
                    "onboarding-recovery-key-different-detail",
                    &[
                        ("found", short_fingerprint(fingerprint).into()),
                        (
                            "expected",
                            short_fingerprint(
                                self.page.expected_identity.as_deref().unwrap_or_default(),
                            )
                            .into(),
                        ),
                    ],
                )),
                theme,
            )),
        }
    }

    fn codec_install_requested(&self) -> bool {
        self.page.codecs.running
            || self.page.codecs.error.is_some()
            || self.page.codecs.state != crate::page::PackState::NotInstalled
    }

    fn codec_progress(&self) -> Option<f32> {
        match self.page.codecs.state {
            crate::page::PackState::Installing => Some(self.page.codecs.fraction.unwrap_or(0.0)),
            crate::page::PackState::NotInstalled if self.page.codecs.running => Some(0.0),
            _ => None,
        }
    }

    /// Banner title and message for what happened to the request, if any.
    fn codec_outcome(&self) -> Option<(String, Option<String>)> {
        if let Some(error) = &self.page.codecs.error {
            return Some((t("onboarding-codecs-failed-title"), Some(error.clone())));
        }
        match self.page.codecs.state {
            crate::page::PackState::NotInstalled if self.page.codecs.running => {
                Some((t("onboarding-codecs-starting"), Some(codecs_background())))
            }
            crate::page::PackState::NotInstalled | crate::page::PackState::Removing => None,
            crate::page::PackState::Installing => Some((
                t("onboarding-codecs-installing-title"),
                Some(t_args(
                    "onboarding-codecs-installing-message",
                    &[("status", self.page.codecs.summary.clone().into())],
                )),
            )),
            crate::page::PackState::WaitingForNetwork => Some((
                t("onboarding-codecs-offline-title"),
                Some(t("onboarding-codecs-offline-message")),
            )),
            crate::page::PackState::Installed => Some((
                t("onboarding-codecs-installed-title"),
                self.page.codecs.when_it_applies.clone(),
            )),
            crate::page::PackState::Failed => Some((
                t("onboarding-codecs-failed-title"),
                Some(if self.page.codecs.last_error.is_empty() {
                    t("onboarding-codecs-failed-message")
                } else {
                    t_args(
                        "onboarding-codecs-failed-message-detail",
                        &[("error", self.page.codecs.last_error.clone().into())],
                    )
                }),
            )),
        }
    }

    /// The page's own choice while nothing runs: (id, label, key) of the
    /// one large Install (or Try again) button, then the quiet way past it.
    /// `None` once a request is underway or done; the footer then offers
    /// Continue.
    fn codec_choice(&self) -> Option<[(&'static str, String, OnboardingEffect); 2]> {
        let failed = self.page.codecs.error.is_some()
            || self.page.codecs.state == crate::page::PackState::Failed;
        if failed && !self.page.codecs.running {
            return Some([
                (
                    "onboarding.codecs.install.button",
                    t("onboarding-codecs-retry"),
                    OnboardingEffect::choose("install-codecs"),
                ),
                (
                    "onboarding.codecs.skip.button",
                    t("onboarding-codecs-skip"),
                    OnboardingEffect::Skip,
                ),
            ]);
        }
        if self.codec_install_requested() {
            return None;
        }
        Some([
            (
                "onboarding.codecs.install.button",
                t("onboarding-codecs-install"),
                OnboardingEffect::choose("install-codecs"),
            ),
            (
                "onboarding.codecs.skip.button",
                t("onboarding-codecs-not-now"),
                OnboardingEffect::Skip,
            ),
        ])
    }
}
/// The ready screen's backup heading. A device that joined from another one
/// holds no root key, so nothing on it needs a backup.
fn done_backup_title_key(joined_member: bool, backup_verified: bool) -> &'static str {
    if joined_member {
        "onboarding-done-joined-key-elsewhere"
    } else if backup_verified {
        "onboarding-done-backup-verified"
    } else {
        "onboarding-done-backup-recommended"
    }
}

/// The ready screen's backup guidance under [`done_backup_title_key`].
fn done_backup_body_key(joined_member: bool, backup_verified: bool) -> &'static str {
    if joined_member {
        "onboarding-done-joined-key-elsewhere-body"
    } else if backup_verified {
        "onboarding-done-backup-verified-body"
    } else {
        "onboarding-done-backup-recommended-body"
    }
}

/// The title at the top of an onboarding page.
fn heading(text: &str, _theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_role(ui::tokens::typography::TextRole::LargeTitle))
}

/// Secondary body copy. Takes the live theme rather than a hard-coded
/// one — a dark-theme dim grey on a light background is barely legible.
fn paragraph_with_id(text: &str, theme: &Theme, id: &str) -> Box<dyn Widget> {
    Box::new(
        Text::new(text)
            .with_id(id)
            .with_role(ui::tokens::typography::TextRole::Body)
            .with_color(theme.text_dim),
    )
}

fn paragraph(text: &str, theme: &Theme) -> Box<dyn Widget> {
    paragraph_with_id(text, theme, "onboarding.paragraph")
}

/// Give status surfaces deliberate breathing room independent of the parent
/// composition's gap calculation. This also keeps consecutive warning/error
/// banners from visually merging into one block.
fn spaced_banner(banner: Banner, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_vertical_padding(theme.tokens.spacing.s2)
            .push(Box::new(banner)),
    )
}

/// Short statements, each led by an icon that names its subject. The icon
/// sits in its own column, top-aligned with the first line, so wrapped lines
/// keep a clean left edge.
fn fact_list(
    facts: impl IntoIterator<Item = (ui::icons::Icon, String)>,
    theme: &Theme,
) -> Box<dyn Widget> {
    let mut list = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.s3);
    for (icon, text) in facts {
        list = list.push(Box::new(
            Row::new()
                .no_wrap()
                .align_start()
                .with_gap(theme.tokens.spacing.s3)
                .push(Box::new(IconWidget::new(icon).dim()))
                .push(Box::new(
                    Text::new(&text).with_role(ui::tokens::typography::TextRole::Body),
                )),
        ));
    }
    Box::new(list)
}

/// One step of a real sequence: its number in quiet ink, then the
/// instruction.
fn numbered_step(number: usize, text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .no_wrap()
            .align_start()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(
                Text::new(&number.to_string())
                    .with_role(ui::tokens::typography::TextRole::Body)
                    .strong()
                    .numeric()
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(
                Text::new(text).with_role(ui::tokens::typography::TextRole::Body),
            )),
    )
}

/// A six-digit verification code in two groups of three, the way people
/// read a code aloud and compare it between two screens.
fn grouped_code(code: &str) -> String {
    let digits: Vec<char> = code.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() == 6 {
        let (head, tail) = digits.split_at(3);
        format!(
            "{} {}",
            head.iter().collect::<String>(),
            tail.iter().collect::<String>()
        )
    } else {
        digits.into_iter().collect()
    }
}

/// A right-aligned action row, so the primary button lands where the eye finishes reading.
fn actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for b in buttons {
        row = row.push(b);
    }
    Box::new(row.no_wrap())
}

/// Sticky page navigation with a deliberate 20 px side and 16 px bottom
/// inset, independent of the scrollable page body.
/// An acknowledgement above the action row. Only the checkbox takes the
/// content gutter, so a long label wraps inside the panel; the actions keep
/// every other footer's inset, so the buttons do not jump between steps.
fn acknowledgement_footer(
    theme: &Theme,
    acknowledgement: Checkbox,
    actions: Box<dyn Widget>,
) -> Box<dyn Widget> {
    let spacing = &theme.tokens.spacing;
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_vertical_padding(spacing.s2)
            .with_gap(spacing.s3)
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_horizontal_padding(spacing.s3, spacing.s3)
                    .push(Box::new(acknowledgement)),
            ))
            .push(actions),
    )
}

fn footer_actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for button in buttons {
        row = row.push(button);
    }
    // Taffy rounds logical positions at the output edge; 17 keeps the
    // promised visible 16 px bottom inset after that rounding.
    Box::new(row.no_wrap().with_padding(20.0, 17.0))
}

fn secret_input(
    id: &str,
    buffer: &Arc<Mutex<String>>,
    accessible_name: &str,
    placeholder: &str,
) -> TextInput {
    let input = TextInput::local()
        .with_id(id)
        .with_external_buffer(Arc::clone(buffer))
        .with_accessible_name(accessible_name)
        .with_placeholder(placeholder)
        .with_secret(true);
    match buffer.lock() {
        Ok(value) => input.with_initial_value(&value),
        Err(_) => input,
    }
}

fn labelled_secret_field(
    id: &str,
    label: &str,
    buffer: &Arc<Mutex<String>>,
    accessible_name: &str,
    placeholder: &str,
) -> Box<dyn Widget> {
    let label_id = format!("{id}.label");
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(4.0)
            .push(Box::new(
                Text::new(label)
                    .with_id(label_id)
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(secret_input(
                id,
                buffer,
                accessible_name,
                placeholder,
            ))),
    )
}

fn choice_row_height(theme: &Theme) -> f32 {
    let spacing = &theme.tokens.spacing;
    let text = theme
        .text(ui::tokens::typography::TextRole::Body)
        .line_height_px()
        + theme
            .text(ui::tokens::typography::TextRole::Caption)
            .line_height_px();
    (text + 2.0 * spacing.s2)
        .max(theme.tokens.metrics.min_pointer_target + spacing.s2)
        .ceil()
}

/// What the migration page says for its `status` in `state`; `retained`
/// when this device keeps the old computer's files where they are.
fn migration_body_key(status: &ui::serde_json::Value, state: &str, retained: bool) -> &'static str {
    let apps = status
        .get("applications_only")
        .and_then(ui::serde_json::Value::as_bool)
        == Some(true)
        || retained && matches!(state, "reviewed" | "ready-to-adopt");
    if json_flag(status, "browser") {
        "onboarding-browser-body"
    } else if status
        .get("portable_applications")
        .and_then(ui::serde_json::Value::as_bool)
        == Some(true)
        && matches!(state, "pending" | "failed" | "unavailable" | "cancelled")
    {
        "onboarding-portable-applications-body"
    } else if retained && state == "pending" {
        "onboarding-existing-files-body"
    } else if apps && state == "reviewed" {
        "onboarding-existing-files-apps-reviewed"
    } else if apps && state == "ready-to-adopt" {
        "onboarding-existing-files-apps-adopt"
    } else if status
        .get("portable_home")
        .and_then(ui::serde_json::Value::as_bool)
        == Some(true)
        && matches!(state, "pending" | "failed" | "unavailable")
    {
        "onboarding-portable-home-body"
    } else {
        match state {
            "pending" => "onboarding-migration-body",
            "reviewed" => "onboarding-migration-reviewed",
            "ready-to-adopt" => "onboarding-migration-adopt-body",
            _ if json_flag(status, "transfer_complete")
                && json_text(status, "cleanup_state") == Some("running") =>
            {
                "onboarding-migration-cleanup-running"
            }
            _ if json_flag(status, "transfer_complete")
                && json_text(status, "cleanup_state") == Some("pending") =>
            {
                "onboarding-migration-cleanup-pending"
            }
            _ if json_flag(status, "transfer_complete")
                && json_text(status, "cleanup_state") == Some("unavailable") =>
            {
                "onboarding-migration-cleanup-unavailable"
            }
            "complete" => "onboarding-migration-complete",
            "existing-files-available" => "onboarding-existing-files-available",
            "incomplete" => "onboarding-migration-incomplete",
            "failed" | "unavailable" => "onboarding-migration-failed",
            "cancelled" => "onboarding-migration-cancelled",
            _ => "onboarding-migration-working",
        }
    }
}

/// Whether `value`'s `key` is `true`.
fn json_flag(value: &ui::serde_json::Value, key: &str) -> bool {
    value.get(key).and_then(ui::serde_json::Value::as_bool) == Some(true)
}

/// `value`'s `key` as text.
fn json_text<'a>(value: &'a ui::serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(ui::serde_json::Value::as_str)
}

/// The keyboard and gesture route a navigation card teaches, shown under
/// its title.
fn tour_shortcut(card_title: &str) -> Option<&'static str> {
    match card_title {
        "onboarding-tour-search-title" => Some("onboarding-tour-search-keys"),
        "onboarding-tour-search-title-mac" => Some("onboarding-tour-search-keys-mac"),
        "onboarding-tour-overview-title" => Some("onboarding-tour-overview-keys"),
        "onboarding-tour-move-title" => Some("onboarding-tour-move-keys"),
        _ => None,
    }
}

/// The 24 words, numbered in reading order, and three of them to type
/// back once they are written down.
fn recovery_key_copy_page(
    theme: &Theme,
    copy: &KeyCopy,
    answers: &[SecretBuffer; 3],
) -> Box<dyn Widget> {
    const COLUMNS: usize = 4;
    let spacing = &theme.tokens.spacing;
    let words: Vec<&str> = copy.words.expose().split(' ').collect();
    let rows = words.len().div_ceil(COLUMNS);
    let mut grid = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing.s2);
    // Numbered down each column, as people write a list on paper.
    for row in 0..rows {
        let mut line = Row::new().no_wrap().with_gap(spacing.s3);
        for column in 0..COLUMNS {
            let at = column.saturating_mul(rows).saturating_add(row);
            let Some(word) = words.get(at) else { continue };
            line = line.push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(
                        Row::new()
                            .no_wrap()
                            .with_gap(spacing.s2)
                            .push(Box::new(
                                // A fixed column keeps the words aligned past
                                // the single-digit numbers.
                                Container::new()
                                    .with_no_padding()
                                    .with_width(spacing.s6)
                                    .with_child(Box::new(
                                        Text::new(&at.saturating_add(1).to_string())
                                            .with_role(ui::tokens::typography::TextRole::Label)
                                            .with_color(theme.text_dim)
                                            .numeric(),
                                    )),
                            ))
                            .push(Box::new(
                                Text::new(word).with_role(ui::tokens::typography::TextRole::Body),
                            )),
                    )),
            ));
        }
        grid = grid.push(Box::new(line));
    }
    let answers = recovery_key_answers(theme, copy, answers);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&t("onboarding-recovery-key-copy-title"), theme))
            .push(paragraph(&t("onboarding-recovery-key-copy-body"), theme))
            .push(Box::new(Section::new(vec![Box::new(grid)])))
            .push(Box::new(answers)),
    )
}

/// The words to type back, each in its own field under its number.
fn recovery_key_answers(theme: &Theme, copy: &KeyCopy, buffers: &[SecretBuffer; 3]) -> Column {
    let spacing = &theme.tokens.spacing;
    let mut answers = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing.s2)
        .push(paragraph(
            &t_args(
                "onboarding-recovery-key-confirm",
                &[
                    ("first", copy.checks[0].into()),
                    ("second", copy.checks[1].into()),
                    ("third", copy.checks[2].into()),
                ],
            ),
            theme,
        ));
    let mut fields = Row::new().no_wrap().with_gap(spacing.s3);
    for (position, answer) in copy.checks.iter().zip(buffers) {
        let label = t_args(
            "onboarding-recovery-key-word",
            &[("position", (*position).into())],
        );
        let mut input = TextInput::local()
            .with_id(&format!("onboarding.backup.recovery-key.word-{position}"))
            .with_external_buffer(Arc::clone(answer))
            .with_accessible_name(label.clone())
            .with_mono()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_mistakes(wrong_answer(Zeroizing::new(
                copy.word(*position).to_owned(),
            )));
        if let Ok(value) = answer.lock() {
            input = input.with_initial_value(&value);
        }
        fields = fields.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(spacing.s1)
                        .push(Box::new(
                            Text::new(&label).with_role(ui::tokens::typography::TextRole::Label),
                        ))
                        .push(Box::new(input)),
                )),
        ));
    }
    answers = answers.push(Box::new(fields));
    answers
}

/// Marks a typed-back word once it can no longer become `expected`, so a
/// slip shows while the key is still on screen to compare against.
fn wrong_answer(expected: Zeroizing<String>) -> impl Fn(&str) -> Vec<std::ops::Range<usize>> {
    move |typed| {
        let answer = typed.trim();
        let start = typed.len().saturating_sub(typed.trim_start().len());
        let fits = expected
            .get(..answer.len())
            .is_some_and(|begun| begun.eq_ignore_ascii_case(answer));
        if fits {
            Vec::new()
        } else {
            std::iter::once(start..start.saturating_add(answer.len())).collect()
        }
    }
}

/// The first groups of a fingerprint: enough to tell identities apart by eye.
fn short_fingerprint(fingerprint: &str) -> String {
    let groups: Vec<&str> = fingerprint
        .as_bytes()
        .chunks(4)
        .take(4)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect();
    format!("{} …", groups.join(" "))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each labelled phase keeps one statement of the copy contract"
    )]
    fn setup_copy_speaks_the_real_contract() {
        // Phase `desktop_tour_teaches_the_real_keyboard_and_gesture_contract`.
        {
            let navigation = NAVIGATE_CARDS
                .iter()
                .flat_map(|(title, body)| {
                    [
                        t(title),
                        tour_shortcut(title).map(t).unwrap_or_default(),
                        t(body),
                    ]
                })
                .collect::<Vec<_>>()
                .join(" ");

            if cfg!(target_os = "macos") {
                assert!(navigation.contains("Command+K"));
                assert!(navigation.contains("Option+Space"));
            } else {
                assert!(navigation.contains("Super+Space"));
            }
            // The palette opened empty shows tabs, apps and actions; the tour
            // names those, never categories the palette doesn't have.
            assert!(navigation.contains("see your tabs, apps and actions"));
            assert!(!navigation.contains("Everyday"));
            assert!(navigation.contains("Super+Tab"));
            assert!(navigation.contains("Super+Tab or pinch"));
            assert!(navigation.contains("Super+arrow keys"));
            assert!(navigation.contains("three-finger swipe"));
            assert!(navigation.contains("up maximizes the window and down restores it"));
            assert!(navigation.contains("Ctrl+Super+Up and Down"));
            assert!(navigation.contains("Super+1 to 9 straight to a strip"));
            assert!(navigation.contains("a four-finger swipe, switch workspaces"));
            assert!(navigation.contains("Left and right step between tiled windows"));
            assert!(
                !navigation.contains("Super+W"),
                "Super+W closes the focused tab; teaching it as Overview would be destructive"
            );

            assert_eq!(
                ORGANISE_CARDS
                    .iter()
                    .map(|(title, _)| t(title))
                    .collect::<Vec<_>>(),
                ["Workspace", "Strip", "Window group", "Tab"],
                "the tour must name every level of the desktop hierarchy"
            );
        }
        // Phase `desktop_tour_ends_with_a_concrete_collaboration_handoff`.
        {
            let handoff = START_CARDS
                .iter()
                .flat_map(|(title, body)| [t(title), t(body)])
                .collect::<Vec<_>>()
                .join(" ");

            for real_surface in [
                "Workspace Settings",
                "Notes",
                "Calendar",
                "Chat",
                "Invite people",
                "Activity",
            ] {
                assert!(
                    handoff.contains(real_surface),
                    "first-use handoff must name the real {real_surface} surface"
                );
            }
            assert!(handoff.contains("undo only the change"));
        }
        // Phase `onboarding_secret_fields_have_stable_spoken_names`.
        {
            let buffer = Arc::new(Mutex::new(String::new()));
            let input = secret_input(
                "onboarding.test.secret",
                &buffer,
                "Backup passphrase",
                "Twelve or more characters",
            );
            let node = Widget::accessibility(&input).expect("text input accessibility node");

            assert_eq!(node.name.as_deref(), Some("Backup passphrase"));
            assert!(
                node.state.protected,
                "passphrases must be exposed as protected"
            );
        }
    }

    #[test]
    fn codes_are_grouped_for_comparison() {
        assert_eq!(grouped_code("123456"), "123 456");
        assert_eq!(grouped_code("12 34 56"), "123 456");
        assert_eq!(grouped_code("1234"), "1234");
    }

    #[test]
    fn a_typed_back_word_is_marked_once_it_cannot_match() {
        let mark = wrong_answer(Zeroizing::new("ability".to_owned()));
        assert!(mark("").is_empty());
        assert!(mark(" Abil").is_empty(), "still on its way to the word");
        assert!(mark("ability ").is_empty());
        assert_eq!(mark(" abel"), vec![1..5]);
        assert_eq!(mark("abilityx"), vec![0..8]);
    }
}
