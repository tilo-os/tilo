//! The rendered first-login identity ceremony.
//!
//! Chrome, not an app: the compositor draws this before the desktop
//! exists, since the screen that creates an identity cannot be something
//! an application could imitate.
//!
//! The decisions all live in [`crate::onboarding::Onboarding`]; this
//! file is only the composition, built from shared `ui` widgets (`Card`,
//! `Banner`, `Text`, `Button`, `Checkbox`) so the ceremony inherits the
//! system theme, focus order and accessibility like the rest of the desktop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::theme::Theme;
use ui::widget::{
    Banner, Button, Card, Checkbox, Column, FilePicker, FilePickerMode, ProgressBar, Row,
    Scrollable, Spacer, Text, TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{Modifiers, WindowEvent};
use zeroize::{Zeroize, Zeroizing};

use crate::onboarding::{
    ACKNOWLEDGEMENT, DONE_BODY, DONE_TITLE, INTRO_BODY, INTRO_POINTS, INTRO_TITLE, MintedIdentity,
    NO_RECOVERY_POINTS, NO_RECOVERY_TITLE, Onboarding, Screen,
};

const PANEL_MAX_WIDTH: f32 = 720.0;
const PANEL_MAX_HEIGHT: f32 = 620.0;
const VIEWPORT_GUTTER: f32 = 32.0;

pub const KEY_BEGIN: &str = "onboarding.begin";
pub const KEY_CONNECT: &str = "onboarding.connect";
pub const KEY_BACK: &str = "onboarding.back";
pub const KEY_CONNECT_PATH: &str = "onboarding.connect.path";
pub const KEY_CONNECT_BROWSE: &str = "onboarding.connect.browse";
pub const KEY_IMPORT: &str = "onboarding.import";
pub const KEY_UNDERSTOOD: &str = "onboarding.understood";
pub const KEY_CREATE: &str = "onboarding.create";
pub const KEY_BACKUP_BROWSE: &str = "onboarding.backup.browse";
pub const KEY_FINISH: &str = "onboarding.finish";

type Minter = Box<dyn Fn() -> Result<MintedIdentity, String> + Send>;
type Importer = Box<dyn Fn(&PathBuf, &[u8]) -> Result<MintedIdentity, String> + Send>;
type Exporter = Box<dyn Fn(&PathBuf, &PathBuf, &[u8]) -> Result<(), String> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerPurpose {
    ImportIdentity,
    BackupIdentity,
}

pub struct OnboardingView {
    flow: Onboarding,
    minter: Minter,
    importer: Importer,
    exporter: Exporter,
    /// Set when minting failed, shown in place of the success screen — loud,
    /// since booting on into a desktop that silently has no identity is what this flow replaces.
    error: Option<String>,
    backup_error: Option<String>,
    backup_saved: Option<PathBuf>,
    /// Raised when the user has finished and the desktop may start.
    finished: bool,
    structure_dirty: bool,
    focus_reset: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    transition_started: Instant,
    picker: Option<(PickerPurpose, FilePicker)>,
    connect_passphrase: Arc<Mutex<String>>,
    backup_passphrase: Arc<Mutex<String>>,
    backup_confirmation: Arc<Mutex<String>>,
}

impl OnboardingView {
    /// Build a ceremony that mints into `identity_dir`.
    pub fn new(identity_dir: PathBuf) -> Self {
        let mint_dir = identity_dir.clone();
        let import_dir = identity_dir;
        Self::with_all_handlers(
            Box::new(move || crate::onboarding::mint_into(&mint_dir).map_err(|e| e.to_string())),
            Box::new(move |source, passphrase| {
                let store = crate::identity_registry::IdentityRegistryStore::at(&import_dir);
                let profile = if passphrase.is_empty() {
                    // Explicit compatibility path for schema-1 exports from
                    // older qOS builds. New backups always require a
                    // passphrase and never expose plaintext JSON.
                    store.import(source).map_err(|error| error.to_string())?
                } else {
                    store
                        .import_portable(source, passphrase)
                        .map_err(|error| error.to_string())?
                };
                let runtime_id = profile
                    .record_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| "imported identity has no valid runtime id".to_string())?;
                runtime_identity::set_active_identity(&import_dir, runtime_id)
                    .map_err(|error| error.to_string())?;
                // Restoring on a fresh machine must provision the exact same
                // complete root -> device chain as minting. The narrower
                // identity-keyed certificate helper leaves the legacy system
                // owner/certificate absent until a later login repair.
                crate::firstboot::ensure_collaboration_identity(
                    &import_dir,
                    &crate::firstboot::collaboration_identity_paths(),
                )
                .map_err(|error| error.to_string())?;
                crate::device_fleet::DeviceFleetStore::at(
                    &import_dir,
                    crate::firstboot::collaboration_identity_paths(),
                )
                .register_current_device(&profile.id)
                .map_err(|error| error.to_string())?;
                Ok(MintedIdentity {
                    fingerprint: profile.id,
                    path: profile.record_path,
                })
            }),
            Box::new(move |identity, destination, passphrase| {
                runtime_identity::export_portable_backup(identity, destination, passphrase)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
        )
    }

    /// Build with an injected minter, so tests drive the whole widget without writing a key to disk.
    pub fn with_minter(minter: Minter) -> Self {
        Self::with_handlers(
            minter,
            Box::new(|_, _| Err("identity import is unavailable in this test".to_string())),
        )
    }

    fn with_handlers(minter: Minter, importer: Importer) -> Self {
        Self::with_all_handlers(
            minter,
            importer,
            Box::new(|_, _, _| Err("identity backup is unavailable in this test".to_string())),
        )
    }

    fn with_all_handlers(minter: Minter, importer: Importer, exporter: Exporter) -> Self {
        Self {
            flow: Onboarding::new(),
            minter,
            importer,
            exporter,
            error: None,
            backup_error: None,
            backup_saved: None,
            finished: false,
            structure_dirty: false,
            focus_reset: true,
            composed: None,
            rect: Rect::ZERO,
            transition_started: Instant::now(),
            picker: None,
            connect_passphrase: Arc::new(Mutex::new(String::new())),
            backup_passphrase: Arc::new(Mutex::new(String::new())),
            backup_confirmation: Arc::new(Mutex::new(String::new())),
        }
    }

    pub fn flow(&self) -> &Onboarding {
        &self.flow
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// True once the user has seen the result and dismissed it.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Put the entrance transition in its steady state for deterministic
    /// offscreen review. Production never calls this; the live compositor
    /// advances the exact same transition on animation frames.
    #[doc(hidden)]
    pub fn settle_motion_for_snapshot(&mut self) {
        self.transition_started = Instant::now() - Duration::from_secs(1);
    }

    fn build_composition(&self, theme: &Theme, panel_height: f32) -> Box<dyn Widget> {
        let (step, stage) = match self.flow.screen() {
            Screen::Intro => (1, "Choose"),
            Screen::Connect | Screen::NoRecovery => (2, "Protect"),
            Screen::Done => (3, "Ready"),
        };
        let body: Box<dyn Widget> = match self.flow.screen() {
            Screen::Intro => self.intro(theme),
            Screen::Connect => self.connect(theme),
            Screen::NoRecovery => self.no_recovery(theme),
            Screen::Done => self.done(theme),
        };
        let spacing = &theme.tokens.spacing;
        let progress = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s2)
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new("qOS  •  PRIVATE BY DESIGN")
                            .with_role(ui::tokens::typography::TextRole::Label, theme)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&format!("{stage}  ·  {step} of 3"))
                            .with_role(ui::tokens::typography::TextRole::Label, theme)
                            .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(ProgressBar::new(step as f32 / 3.0).with_label(
                format!("Identity setup, step {step} of 3: {stage}"),
            )));
        let content = Column::new()
            .with_intrinsic_height()
            .with_gap(spacing.s6)
            .push(Box::new(progress))
            .push(body);
        Box::new(
            Scrollable::new_vertical()
                .with_height(panel_height)
                .with_content_gutter(spacing.s3)
                .with_child(Box::new(content)),
        )
    }

    fn intro(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut list = Column::new().with_no_padding().with_gap(spacing.s3);
        for point in INTRO_POINTS {
            list = list.push(bullet(point, theme));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s6)
                .push(heading(INTRO_TITLE, theme))
                .push(paragraph(INTRO_BODY, theme))
                .push(Box::new(Card::titled("What it is for", Box::new(list))))
                .push(actions(vec![
                    Box::new(
                        Button::new(
                            "onboarding.connect.button",
                            "Use existing",
                            Some(KEY_CONNECT),
                        )
                        .with_icon(ui::icons::Icon::ExternalLink),
                    ),
                    Box::new(
                        Button::new(
                            "onboarding.begin.button",
                            "Create identity",
                            Some(KEY_BEGIN),
                        )
                        .with_icon(ui::icons::Icon::ArrowRight)
                        .with_style(ui::widget::ButtonStyle::Primary),
                    ),
                ])),
        )
    }

    fn connect(&self, theme: &Theme) -> Box<dyn Widget> {
        let path = TextInput::new(Vec::new())
            .with_id("onboarding.connect.path.field")
            .with_on_change_path(KEY_CONNECT_PATH)
            .with_placeholder("Choose an encrypted backup or legacy identity JSON")
            .with_font_size(theme.tokens.typography.scale.base);
        let passphrase = secret_input(
            "onboarding.connect.passphrase.field",
            &self.connect_passphrase,
            "Passphrase for encrypted backups",
        )
        .with_font_size(theme.tokens.typography.scale.base);
        let spacing = &theme.tokens.spacing;
        let mut body = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s6)
                .push(heading("Connect an existing identity", theme))
                .push(paragraph(
                    "Bring an encrypted backup from another qOS device. It is authenticated and validated locally before this machine is certified. For a legacy unencrypted JSON export, leave the passphrase empty.",
                    theme,
                ));
        if let Some(error) = &self.error {
            body = body.push(Box::new(
                Banner::error("That identity could not be connected").with_message(error.clone()),
            ));
        }
        let file_row = Row::new().push(Box::new(path)).push(Box::new(
            Button::new(
                "onboarding.connect.browse.button",
                "Browse…",
                Some(KEY_CONNECT_BROWSE),
            )
            .with_icon(ui::icons::Icon::Folder),
        ));
        Box::new(
            body.push(Box::new(Card::titled("Identity file", Box::new(file_row))))
                .push(Box::new(Card::titled(
                    "Backup passphrase",
                    Box::new(passphrase),
                )))
                .push(actions(vec![
                    back_button(),
                    Box::new(
                        Button::new("onboarding.import.button", "Connect", Some(KEY_IMPORT))
                            .with_icon(ui::icons::Icon::ArrowRight)
                            .with_style(ui::widget::ButtonStyle::Primary),
                    ),
                ])),
        )
    }

    fn no_recovery(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut list = Column::new().with_no_padding().with_gap(spacing.s4);
        for point in NO_RECOVERY_POINTS {
            list = list.push(bullet(point, theme));
        }
        // The gate. Disabled until the box is ticked, so the primary action can't be clicked past the consequence.
        let create = Button::new("onboarding.create.button", "Create", Some(KEY_CREATE))
            .with_icon(ui::icons::Icon::Shield)
            .with_style(ui::widget::ButtonStyle::Primary)
            .with_enabled(self.flow.can_create());
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s6)
            .push(heading("Keep the key to your identity", theme))
            .push(Box::new(
                Banner::warning(NO_RECOVERY_TITLE)
                    .with_message(
                        "There is no reset button. Read this once, then choose deliberately.",
                    )
                    .with_icon(ui::icons::Icon::Shield),
            ));
        if let Some(error) = &self.error {
            body = body.push(Box::new(
                Banner::error("Your identity was not created").with_message(error.clone()),
            ));
        }
        Box::new(
            body.push(Box::new(Card::new(Box::new(list))))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Checkbox::new("onboarding.understood.box", KEY_UNDERSTOOD)
                                .with_accessible_name(ACKNOWLEDGEMENT),
                        ))
                        .push(Box::new(
                            Text::new(ACKNOWLEDGEMENT)
                                .with_role(ui::tokens::typography::TextRole::Body, theme),
                        )),
                ))
                .push(actions(vec![back_button(), Box::new(create)])),
        )
    }

    fn done(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(err) = &self.error {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s6)
                    .push(Box::new(
                        Banner::error("Your identity could not be created")
                            .with_message(err.clone()),
                    ))
                    .push(paragraph(
                        "Nothing was written and the desktop is still protected. Go back and try again after checking storage and permissions.",
                        theme,
                    ))
                    .push(actions(vec![back_button()])),
            );
        }
        let minted = self.flow.minted();
        let fingerprint = minted
            .map(super::onboarding::MintedIdentity::grouped_fingerprint)
            .unwrap_or_default();
        let detail = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(
                Text::new(&fingerprint)
                    .with_role(ui::tokens::typography::TextRole::Code, theme)
                    .with_font_family(ui::text_render::FontChoice::Monospace),
            ))
            .push(Box::new(
                Text::new("Others may use this fingerprint to verify that this identity is yours.")
                    .with_role(ui::tokens::typography::TextRole::Caption, theme)
                    .with_color(theme.text_dim),
            ));
        let backup_fields = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(secret_input(
                "onboarding.backup.passphrase",
                &self.backup_passphrase,
                "Long, unique passphrase — 12+ characters",
            )))
            .push(Box::new(secret_input(
                "onboarding.backup.confirmation",
                &self.backup_confirmation,
                "Confirm passphrase",
            )));
        let mut backup = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(paragraph(
                "Save an encrypted copy to another drive. qOS verifies the finished file before calling it a backup; keep its passphrase somewhere separate.",
                theme,
            ))
            .push(Box::new(backup_fields));
        if let Some(error) = &self.backup_error {
            backup = backup.push(Box::new(
                Banner::error("The backup was not saved").with_message(error.clone()),
            ));
        }
        if let Some(path) = &self.backup_saved {
            backup = backup.push(Box::new(
                Banner::success("Encrypted backup verified")
                    .with_message(path.display().to_string())
                    .with_icon(ui::icons::Icon::Check),
            ));
        }
        let mut backup_button = Button::new(
            "onboarding.backup.button",
            if self.backup_saved.is_some() {
                "Save another copy"
            } else {
                "Save backup…"
            },
            Some(KEY_BACKUP_BROWSE),
        );
        if self.backup_saved.is_none() {
            backup_button = backup_button.with_style(ui::widget::ButtonStyle::Primary);
        }
        backup = backup.push(actions(vec![Box::new(backup_button)]));
        let mut finish = Button::new(
            "onboarding.finish.button",
            if self.backup_saved.is_some() {
                "Enter qOS"
            } else {
                "Skip for now"
            },
            Some(KEY_FINISH),
        )
        .with_icon(ui::icons::Icon::ArrowRight);
        if self.backup_saved.is_some() {
            finish = finish.with_style(ui::widget::ButtonStyle::Primary);
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s6)
                .push(heading(DONE_TITLE, theme))
                .push(Box::new(
                    Banner::success("Identity secured on this device")
                        .with_icon(ui::icons::Icon::Check),
                ))
                .push(paragraph(DONE_BODY, theme))
                .push(Box::new(Card::titled("Your fingerprint", Box::new(detail))))
                .push(Box::new(Card::titled(
                    "Make a portable backup",
                    Box::new(backup),
                )))
                // Repeated deliberately: the last moment the user is looking at the
                // subject is the one fact they need to carry away from the whole ceremony.
                .push(paragraph(
                    "There is no recovery service. If this device goes and you have no \
                     encrypted portable backup, this identity goes with it.",
                    theme,
                ))
                .push(actions(vec![Box::new(finish)])),
        )
    }
}

fn heading(text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_role(ui::tokens::typography::TextRole::Display, theme))
}

/// Secondary body copy. Takes the live theme rather than a hard-coded
/// one — a dark-theme dim grey on a light background is barely legible.
fn paragraph(text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Text::new(text)
            .with_role(ui::tokens::typography::TextRole::Body, theme)
            .with_color(theme.text_dim),
    )
}

/// A list item.
///
/// The marker is part of the same `Text` rather than a sibling in a
/// `Row`: these lines wrap, and a marker in its own column detaches from its sentence.
fn bullet(text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Text::new(&format!("•   {text}")).with_role(ui::tokens::typography::TextRole::Body, theme),
    )
}

fn back_button() -> Box<dyn Widget> {
    Box::new(
        Button::new("onboarding.back.button", "Back", Some(KEY_BACK))
            .with_icon(ui::icons::Icon::ArrowLeft),
    )
}

/// A right-aligned action row, so the primary button lands where the eye finishes reading.
fn actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for b in buttons {
        row = row.push(b);
    }
    Box::new(row)
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

fn read_string(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(text) => text.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn secret_input(id: &str, buffer: &Arc<Mutex<String>>, placeholder: &str) -> TextInput {
    let input = TextInput::local()
        .with_id(id)
        .with_external_buffer(Arc::clone(buffer))
        .with_placeholder(placeholder)
        .with_secret(true);
    match buffer.lock() {
        Ok(value) => input.with_initial_value(&value),
        Err(_) => input,
    }
}

fn clear_secret(buffer: &Arc<Mutex<String>>) {
    if let Ok(mut value) = buffer.lock() {
        value.zeroize();
    }
}

impl Drop for OnboardingView {
    fn drop(&mut self) {
        clear_secret(&self.connect_passphrase);
        clear_secret(&self.backup_passphrase);
        clear_secret(&self.backup_confirmation);
    }
}

impl Widget for OnboardingView {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let before = self.flow.screen();

        // Pull widget-local edits (notably passphrases) into their process-local
        // buffers before interpreting action keys from the same input frame.
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }

        let picker_outcome = self
            .picker
            .as_mut()
            .and_then(|(purpose, picker)| picker.take_outcome().map(|outcome| (*purpose, outcome)));
        if let Some((purpose, outcome)) = picker_outcome {
            if let ui::widget::FilePickerOutcome::Selected(paths) = outcome
                && let Some(path) = paths.into_iter().next()
            {
                match purpose {
                    PickerPurpose::ImportIdentity => {
                        let _ = state.put(
                            root,
                            KEY_CONNECT_PATH,
                            ScalarValue::Str(path.to_string_lossy().into_owned().into()),
                        );
                        self.error = None;
                    }
                    PickerPurpose::BackupIdentity => {
                        let passphrase = Zeroizing::new(
                            self.backup_passphrase
                                .lock()
                                .map(|value| value.as_bytes().to_vec())
                                .unwrap_or_default(),
                        );
                        let export = self.flow.minted().map_or_else(
                            || Err("the identity is not ready to back up".to_string()),
                            |identity| (self.exporter)(&identity.path, &path, &passphrase),
                        );
                        match export {
                            Ok(()) => {
                                self.backup_saved = Some(path);
                                self.backup_error = None;
                                clear_secret(&self.backup_passphrase);
                                clear_secret(&self.backup_confirmation);
                            }
                            Err(error) => {
                                self.backup_error = Some(error);
                            }
                        }
                    }
                }
            }
            self.picker = None;
            self.structure_dirty = true;
            self.focus_reset = true;
        }

        if read_bool(state, root, KEY_BEGIN) {
            let _ = state.put(root, KEY_BEGIN, ScalarValue::Boolean(false));
            self.flow.begin();
        }

        if read_bool(state, root, KEY_CONNECT) {
            let _ = state.put(root, KEY_CONNECT, ScalarValue::Boolean(false));
            self.flow.begin_connect();
        }

        if read_bool(state, root, KEY_CONNECT_BROWSE) {
            let _ = state.put(root, KEY_CONNECT_BROWSE, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Connect && self.picker.is_none() {
                let (initial, roots) = onboarding_picker_scope();
                self.picker = Some((
                    PickerPurpose::ImportIdentity,
                    FilePicker::new("Choose a qOS identity", initial, roots).with_filters(vec![
                        ui::widget::file_picker::FileFilter::new(
                            "qOS identity",
                            &["qos-identity", "qos-identity-backup", "json"],
                        ),
                        ui::widget::file_picker::FileFilter::all(),
                    ]),
                ));
                self.focus_reset = true;
            }
        }

        if read_bool(state, root, KEY_BACKUP_BROWSE) {
            let _ = state.put(root, KEY_BACKUP_BROWSE, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Done && self.picker.is_none() {
                self.backup_error = match (
                    self.backup_passphrase.lock(),
                    self.backup_confirmation.lock(),
                ) {
                    (Ok(passphrase), Ok(_)) if passphrase.chars().count() < 12 => Some(
                        "Use at least 12 characters. Make it long and unique; length alone does not make a common passphrase safe."
                            .to_string(),
                    ),
                    (Ok(passphrase), Ok(confirmation)) if *passphrase != *confirmation => {
                        Some("The two passphrases do not match.".to_string())
                    }
                    (Ok(_), Ok(_)) => None,
                    _ => Some("The passphrase fields are temporarily unavailable.".to_string()),
                };
                if self.backup_error.is_none() {
                    let (initial, roots) = onboarding_picker_scope();
                    self.picker = Some((
                        PickerPurpose::BackupIdentity,
                        FilePicker::new("Save encrypted identity backup", initial, roots)
                            .with_mode(FilePickerMode::Save)
                            .with_filters(vec![ui::widget::file_picker::FileFilter::new(
                                "Encrypted qOS identity",
                                &["qos-identity"],
                            )])
                            .with_filename("qos-identity-backup.qos-identity"),
                    ));
                    self.focus_reset = true;
                } else {
                    self.structure_dirty = true;
                }
            }
        }

        if read_bool(state, root, KEY_BACK) {
            let _ = state.put(root, KEY_BACK, ScalarValue::Boolean(false));
            self.error = None;
            clear_secret(&self.connect_passphrase);
            self.flow.back();
        }

        if read_bool(state, root, KEY_IMPORT) {
            let _ = state.put(root, KEY_IMPORT, ScalarValue::Boolean(false));
            let source = PathBuf::from(read_string(state, root, KEY_CONNECT_PATH));
            if self.flow.screen() == Screen::Connect {
                let passphrase = Zeroizing::new(
                    self.connect_passphrase
                        .lock()
                        .map(|value| value.as_bytes().to_vec())
                        .unwrap_or_default(),
                );
                let import = (self.importer)(&source, &passphrase);
                match import {
                    Ok(identity) => {
                        self.error = None;
                        self.flow.complete_connected(identity);
                        if let Ok(mut value) = self.connect_passphrase.lock() {
                            value.clear();
                        }
                    }
                    Err(error) => {
                        tracing::error!(target: "qos::onboarding", %error, "identity import failed");
                        self.error = Some(error);
                        self.structure_dirty = true;
                    }
                }
            }
        }

        // The checkbox drives the gate directly rather than being latched, so unticking really does close it again.
        let understood = read_bool(state, root, KEY_UNDERSTOOD);
        if understood != self.flow.acknowledged() {
            self.flow.set_acknowledged(understood);
            self.structure_dirty = true;
        }

        if read_bool(state, root, KEY_CREATE) {
            let _ = state.put(root, KEY_CREATE, ScalarValue::Boolean(false));
            // `can_create` is checked inside `complete` too — this is the outer guard, that one is the invariant.
            if self.flow.can_create() {
                self.error = None;
                match (self.minter)() {
                    Ok(minted) => {
                        self.flow.complete(minted);
                    }
                    Err(e) => {
                        tracing::error!(target: "qos::onboarding", error = %e, "identity minting failed");
                        self.error = Some(e);
                        // Stay on the protected step. A failed write is not a
                        // completed identity and must never release the desktop.
                        self.structure_dirty = true;
                    }
                }
            }
        }

        if read_bool(state, root, KEY_FINISH) {
            let _ = state.put(root, KEY_FINISH, ScalarValue::Boolean(false));
            self.finished = self.flow.is_done();
            if self.finished {
                clear_secret(&self.connect_passphrase);
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
            }
        }

        if self.flow.screen() != before {
            self.structure_dirty = true;
            self.focus_reset = true;
            self.transition_started = Instant::now();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition(theme, PANEL_MAX_HEIGHT);
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Keep a readable measure on large displays and shrink all the way to
        // a phone-like viewport. The body scrolls vertically when its copy is
        // taller than the bounded setup surface.
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::percent(1.0);
            cs.size.height = taffy::Dimension::percent(1.0);
            cs.max_size.width = taffy::Dimension::length(PANEL_MAX_WIDTH);
            cs.max_size.height = taffy::Dimension::length(PANEL_MAX_HEIGHT);
            cs.flex_grow = 0.0;
            cs.flex_shrink = 1.0;
            let _ = taffy.set_style(child, cs);
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            justify_content: Some(taffy::JustifyContent::Center),
            align_items: Some(taffy::AlignItems::Center),
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                right: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                top: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                bottom: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);

        // Quiet, theme-derived depth: two broad accent blooms make the setup
        // feel like a first-class OS surface without turning security setup
        // into branded illustration. High contrast naturally suppresses them
        // because transparent accent washes resolve near the ground.
        let bloom = colors.accent[5].with_alpha_factor(0.10);
        let radius = self.rect.width().max(self.rect.height()) * 0.34;
        scene.fill(
            Fill::NonZero,
            transform,
            bloom,
            None,
            &Circle::new(Point::new(self.rect.x0, self.rect.y0), radius),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[3].with_alpha_factor(0.07),
            None,
            &Circle::new(Point::new(self.rect.x1, self.rect.y1), radius * 0.82),
        );

        if let Some(c) = &mut self.composed {
            let panel = c.get_rect();
            if panel.area() > 0.0 {
                ui::surface::soft_shadow(
                    scene,
                    transform,
                    panel,
                    colors.neutral[0],
                    42.0,
                    18.0,
                    0.42,
                );
                let shape = RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.xl));
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &shape,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    colors.border_standard,
                    None,
                    &shape,
                );
            }

            let duration = theme.tokens.motion.durations.slow();
            let elapsed = self.transition_started.elapsed();
            let raw = if duration.is_zero() {
                1.0
            } else {
                (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
            };
            let reveal = if theme.reduce_motion {
                1.0
            } else {
                ui::ease_out(f64::from(raw)) as f32
            };
            let mut local = Scene::new();
            c.draw(&mut local, Affine::IDENTITY, text_renderer, theme, presence);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                reveal,
                transform,
                &self.rect,
            );
            scene.append(
                &local,
                Some(transform.pre_translate((0.0, f64::from(1.0 - reveal) * 18.0).into())),
            );
            scene.pop_layer();
        }

        if let Some((_, picker)) = self.picker.as_mut() {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.scrim.with_alpha_factor(0.56),
                None,
                &self.rect,
            );
            picker.set_rect(onboarding_picker_rect(self.rect));
            picker.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some((_, picker)) = self.picker.as_mut() {
            picker.set_rect(onboarding_picker_rect(self.rect));
            let mut picker_taffy = TaffyTree::<()>::new();
            if let Ok(picker_node) = picker_taffy.new_leaf(taffy::Style::default()) {
                picker.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    picker_node,
                    &picker_taffy,
                );
            }
            *handled = true;
            return;
        }
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some((_, picker)) = self.picker.as_ref() {
            return vec![picker];
        }
        self.composed
            .as_ref()
            .map(std::convert::AsRef::as_ref)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some((_, picker)) = self.picker.as_mut() {
            return vec![picker];
        }
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "OnboardingView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("onboarding")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.transition_started.elapsed() < ui::tokens::motion::Motion::default().durations.slow()
            || self
                .picker
                .as_ref()
                .is_some_and(|(_, picker)| picker.needs_animation_frame())
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_animation_frame())
    }
}

/// OOBE is trusted compositor chrome, so its chooser follows the user rather
/// than an application's capability scope. Keep roots narrow: the home tree
/// plus conventional removable-media mount roots, never the whole filesystem.
fn onboarding_picker_scope() -> (PathBuf, Vec<PathBuf>) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir());
    let mut roots = Vec::new();
    if let Some(home) = home.as_ref() {
        roots.push(home.clone());
    }
    let user = std::env::var_os("USER").filter(|value| !value.is_empty());
    let mut media_roots = Vec::new();
    if let Some(user) = user {
        for base in [PathBuf::from("/run/media"), PathBuf::from("/media")] {
            let candidate = base.join(&user);
            if candidate.is_dir() {
                media_roots.push(candidate);
            }
        }
    }
    if media_roots.is_empty() && PathBuf::from("/media").is_dir() {
        media_roots.push(PathBuf::from("/media"));
    }
    let mounted = media_roots
        .iter()
        .find(|root| directory_has_subdirectory(root))
        .cloned();
    roots.extend(media_roots);
    let initial = mounted
        .or_else(|| home.clone())
        .unwrap_or_else(std::env::temp_dir);
    if roots.is_empty() {
        roots.push(initial.clone());
    }
    (initial, roots)
}

fn directory_has_subdirectory(path: &std::path::Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
    })
}

fn onboarding_picker_rect(viewport: Rect) -> Rect {
    let width = (viewport.width() * 0.78)
        .clamp(420.0, 900.0)
        .min(viewport.width());
    let height = (viewport.height() * 0.78)
        .clamp(320.0, 640.0)
        .min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - width) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - height) / 2.0;
    Rect::new(x0, y0, x0 + width, y0 + height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn view() -> (OnboardingView, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let v = OnboardingView::with_minter(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/home/user/.config/qos/identity/primary.json"),
            })
        }));
        (v, calls)
    }

    fn press(doc: &mut AutoCommit, key: &str) {
        let _ = doc.put(&automerge::ROOT, key, ScalarValue::Boolean(true));
    }

    /// The whole point: clicking through without ticking the box must not produce a key.
    #[test]
    fn pressing_create_without_acknowledging_mints_nothing() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.flow().screen(), Screen::NoRecovery);

        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no key was minted");
        assert_eq!(v.flow().screen(), Screen::NoRecovery, "still on the gate");
    }

    #[test]
    fn acknowledging_then_creating_mints_exactly_once() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(v.flow().can_create());
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        // A second sync must not re-fire — the key is consumed.
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(v.flow().screen(), Screen::Done);
        assert!(v.flow().is_done());
    }

    #[test]
    fn unticking_the_box_closes_the_gate_again() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        let _ = doc.put(
            &automerge::ROOT,
            KEY_UNDERSTOOD,
            ScalarValue::Boolean(false),
        );
        v.sync(&mut doc, &automerge::ROOT);
        assert!(!v.flow().can_create());
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A minting failure must be shown, not swallowed into a desktop that silently has no identity.
    #[test]
    fn a_failed_mint_is_surfaced_rather_than_hidden() {
        let mut v = OnboardingView::with_minter(Box::new(|| Err("disk full".to_string())));
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.error(), Some("disk full"));
        assert_eq!(
            v.flow().screen(),
            Screen::NoRecovery,
            "a failed write is not a completed identity"
        );
        assert!(!v.flow().is_done());
        press(&mut doc, KEY_FINISH);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(!v.finished(), "failure must not release the desktop");
    }

    #[test]
    fn finishing_releases_the_desktop() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        assert!(!v.finished());
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_FINISH);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(v.finished());
    }

    #[test]
    fn connect_existing_imports_without_minting() {
        let imported = Arc::new(AtomicUsize::new(0));
        let imported_for_handler = imported.clone();
        let mut view = OnboardingView::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |path, _| {
                imported_for_handler.fetch_add(1, Ordering::SeqCst);
                assert_eq!(path, &PathBuf::from("/media/backup/primary.json"));
                Ok(MintedIdentity {
                    fingerprint: "cd".repeat(32),
                    path: PathBuf::from("/home/user/.config/qos/identity/cd.json"),
                })
            }),
        );
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_CONNECT);
        let _ = doc.put(
            &automerge::ROOT,
            KEY_CONNECT_PATH,
            ScalarValue::Str("/media/backup/primary.json".into()),
        );
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(view.flow().screen(), Screen::Connect);
        press(&mut doc, KEY_IMPORT);
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(imported.load(Ordering::SeqCst), 1);
        assert_eq!(view.flow().screen(), Screen::Done);
        assert!(view.flow().is_done());
    }

    #[test]
    fn failed_connect_stays_on_connect_screen_and_shows_error() {
        let mut view = OnboardingView::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(|_, _| Err("not a qOS identity".to_string())),
        );
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_CONNECT);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_IMPORT);
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(view.flow().screen(), Screen::Connect);
        assert_eq!(view.error(), Some("not a qOS identity"));
    }

    #[test]
    fn connect_browse_opens_a_shell_owned_modal_picker() {
        let (mut view, _) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_CONNECT);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CONNECT_BROWSE);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.picker.is_some());

        view.picker.as_mut().expect("picker").1.click_cancel();
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.picker.is_none());
        assert_eq!(view.flow().screen(), Screen::Connect);
    }

    #[test]
    fn portable_backup_requires_matching_strong_passphrases_and_reports_success() {
        let exports = Arc::new(AtomicUsize::new(0));
        let exports_for_handler = Arc::clone(&exports);
        let mut view = OnboardingView::with_all_handlers(
            Box::new(|| Err("unused".to_string())),
            Box::new(|_, _| Err("unused".to_string())),
            Box::new(move |identity, destination, passphrase| {
                assert_eq!(identity, &PathBuf::from("/identity/primary.json"));
                assert_eq!(
                    destination.file_name().and_then(|name| name.to_str()),
                    Some("qos-identity-backup.qos-identity")
                );
                assert_eq!(passphrase, b"correct horse battery staple");
                exports_for_handler.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );
        view.flow.begin();
        view.flow.set_acknowledged(true);
        assert!(view.flow.complete(MintedIdentity {
            fingerprint: "ef".repeat(32),
            path: PathBuf::from("/identity/primary.json"),
        }));
        let mut doc = AutoCommit::new();

        *view.backup_passphrase.lock().unwrap() = "correct horse battery staple".into();
        *view.backup_confirmation.lock().unwrap() = "does not match".into();
        press(&mut doc, KEY_BACKUP_BROWSE);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.picker.is_none());
        assert!(
            view.backup_error
                .as_deref()
                .unwrap()
                .contains("do not match")
        );

        *view.backup_confirmation.lock().unwrap() = "correct horse battery staple".into();
        press(&mut doc, KEY_BACKUP_BROWSE);
        view.sync(&mut doc, &automerge::ROOT);
        view.picker.as_mut().expect("save picker").1.click_open();
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(exports.load(Ordering::SeqCst), 1);
        assert!(view.backup_saved.is_some());
        assert!(view.backup_error.is_none());
        assert!(view.backup_passphrase.lock().unwrap().is_empty());
        assert!(view.backup_confirmation.lock().unwrap().is_empty());
    }

    /// Every screen must lay out — a ceremony that panics on the screen after the one under test is worse than no ceremony.
    #[test]
    fn every_screen_builds_a_layout() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        for step in [
            None,
            Some(KEY_BEGIN),
            Some(KEY_UNDERSTOOD),
            Some(KEY_CREATE),
        ] {
            if let Some(k) = step {
                press(&mut doc, k);
                v.sync(&mut doc, &automerge::ROOT);
            }
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = v.build_layout(&mut taffy, &theme, &mut node_map);
        }

        let (mut connect, _) = view();
        let mut connect_doc = AutoCommit::new();
        press(&mut connect_doc, KEY_CONNECT);
        connect.sync(&mut connect_doc, &automerge::ROOT);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = connect.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn narrow_layout_keeps_the_setup_surface_inside_the_viewport() {
        let (mut view, _) = view();
        let theme = Theme::dark();
        let mut text = ui::text_render::TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = view.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(420.0),
                    height: taffy::AvailableSpace::Definite(640.0),
                },
                |known, available, node_id, _, _| {
                    node_map
                        .get(&node_id)
                        .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                            (*ptr).measure(known, available, &mut text)
                        })
                },
            )
            .expect("narrow setup layout");
        view.apply_layout(&taffy, node, Point::ZERO);
        let panel = view.composed.as_ref().expect("composition").get_rect();
        assert!(panel.x0 >= 0.0 && panel.x1 <= 420.0, "panel={panel:?}");
        assert!(panel.y0 >= 0.0 && panel.y1 <= 640.0, "panel={panel:?}");
    }
}
