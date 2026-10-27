//! Compatibility for an OS prepared on another computer.
//!
//! The preparation computer can read its own hardware. It cannot inspect the
//! destination's disks, firmware or protection, and the repository publishes
//! no per-model qualification catalogue that could stand in for that check.
//! This module therefore records only what the person can supply — a specific
//! model, or a hardware report produced on the destination — and states what
//! still has to be checked there. A model name is never component evidence,
//! and neither a model nor a report authorizes a boot, partition or
//! protection change.

use std::path::Path;
use std::sync::mpsc::TryRecvError;

use imager_backend::support_report::{MAX_REPORT_BYTES, ReportSource, SupportReport};
use ui::icons::Icon;
use ui::portal::{ContentsChannel, ContentsOutcome, FileFilter, PickRequest};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    Button, ButtonRole, Column, Field, LabelPlacement, ListRow, Row, Section, SectionHeader,
    Spacer, Text, TextInput, TextInputKind, Widget,
};

use super::{Commands, InstallerPreviewEffect, queue_continue};
use crate::page;
use crate::setup::strings::{t, t_args};

/// The private restart record has a 32 KiB bound; leave room for the rest of
/// the selection instead of making a large report fail the whole save.
const MAX_DRAFT_REPORT_BYTES: usize = 16 * 1024;

const FIELD_IDS: [(&str, &str); 5] = [
    ("manufacturer", "installer.target.manufacturer"),
    ("model", "installer.target.model"),
    ("variant", "installer.target.variant"),
    ("board", "installer.target.board"),
    ("sku", "installer.target.sku"),
];

/// What the person supplied about the destination.
pub(super) struct Target {
    manufacturer: ui::LocalState<String>,
    model: ui::LocalState<String>,
    variant: ui::LocalState<String>,
    board: ui::LocalState<String>,
    sku: ui::LocalState<String>,
    /// The architecture the medium must start on. An imported report supplies
    /// it; a named model starts from this computer's architecture and may
    /// select the other one when the verified catalog offers it.
    architecture: String,
    report: Option<SupportReport>,
    picker: Option<ContentsChannel>,
    error: Option<String>,
    submitted: bool,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            manufacturer: ui::LocalState::new(String::new()),
            model: ui::LocalState::new(String::new()),
            variant: ui::LocalState::new(String::new()),
            board: ui::LocalState::new(String::new()),
            sku: ui::LocalState::new(String::new()),
            architecture: host_architecture(),
            report: None,
            picker: None,
            error: None,
            submitted: false,
        }
    }
}

impl Target {
    /// A specific model or an accepted report. An empty, architecture-only or
    /// otherwise incomplete description is not ready.
    pub(super) fn ready(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(|report| !report.blocks_install())
            || (self.report.is_none() && self.declared().is_some())
    }

    fn declared(&self) -> Option<SupportReport> {
        let manufacturer = self.manufacturer.get();
        let model = self.model.get();
        let variant = self.variant.get();
        let board = self.board.get();
        let sku = self.sku.get();
        SupportReport::declared_model_with(
            &manufacturer,
            &model,
            &variant,
            optional(&board),
            optional(&sku),
        )
        .ok()
    }

    /// The architecture the prepared medium must start on.
    pub(super) fn architecture(&self) -> &str {
        self.report
            .as_ref()
            .and_then(|report| report.architecture.as_deref())
            .unwrap_or(&self.architecture)
    }

    /// Select the other supported architecture for a named model. An imported
    /// report keeps its own observation architecture; replacing the evidence
    /// is an explicit choice, not an edit behind it.
    pub(super) fn set_architecture(&mut self, architecture: String) -> Result<(), &'static str> {
        if !crate::release::SUPPORTED_ARCHITECTURES.contains(&architecture.as_str()) {
            return Err("installer-preview-target-report-unsupported-architecture");
        }
        if let Some(report) = &self.report {
            if report.architecture.as_deref() != Some(architecture.as_str()) {
                return Err("installer-preview-target-report-architecture-fixed");
            }
            return Ok(());
        }
        self.architecture = architecture;
        self.error = None;
        Ok(())
    }

    /// Whether the verified catalog offers a target payload for the selected
    /// architecture. An empty selection is not replaced by this computer's
    /// own architecture.
    pub(super) fn payload_available(&self) -> Result<bool, &'static str> {
        let architecture = self.architecture();
        let media = crate::release::bundled_releases_for_arch(architecture)
            .map_err(|_| "installer-preview-target-report-unsupported-architecture")?;
        let internal = crate::release::bundled_internal_releases_for_arch(architecture)
            .map_err(|_| "installer-preview-target-report-unsupported-architecture")?;
        Ok(!media.is_empty() || !internal.is_empty())
    }

    /// The report whose facts are shown: the imported one, or the specific
    /// model the person named. A declared report has no components.
    pub(super) fn effective_report(&self) -> Option<SupportReport> {
        self.report.clone().or_else(|| self.declared())
    }

    pub(super) fn blocking(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(SupportReport::blocks_install)
    }

    /// Record one identity field from the page or a semantic caller.
    pub(super) fn set_field(&mut self, field: &str, value: String) -> Result<(), &'static str> {
        match field {
            "manufacturer" => self.manufacturer.set(value),
            "model" => self.model.set(value),
            "variant" => self.variant.set(value),
            "board" => self.board.set(value),
            "sku" => self.sku.set(value),
            _ => return Err("installer-preview-invalid-choice"),
        }
        // Editing the identity starts a new declared description; an imported
        // report remains the evidence until the person explicitly replaces it.
        self.error = None;
        Ok(())
    }

    /// Start the system file chooser for a report produced elsewhere.
    pub(super) fn choose_report(&mut self) -> Result<(), &'static str> {
        if self.picker.is_some() {
            return Err("installer-preview-target-busy");
        }
        let request = PickRequest::open(
            &t("installer-preview-target-file-title"),
            &t("installer-preview-target-file-accept"),
        )
        .filter(
            FileFilter::new(&t("installer-preview-target-file-filter"))
                .glob("*.json")
                .mimetype("application/json"),
        )
        .all_files()
        .max_contents_bytes(MAX_REPORT_BYTES);
        self.picker = Some(request.send_contents());
        self.error = None;
        Ok(())
    }

    /// Read a report the person named by path through the semantic door.
    pub(super) fn import_file(&mut self, path: &Path) -> Result<(), &'static str> {
        if !path.is_file() {
            self.error = Some(t("installer-preview-target-report-unreadable"));
            return Err("installer-preview-target-report-unreadable");
        }
        let bytes = match std::fs::metadata(path) {
            Ok(metadata) if metadata.len() <= MAX_REPORT_BYTES as u64 => std::fs::read(path),
            _ => Err(std::io::Error::other("report is too large")),
        };
        if let Ok(bytes) = bytes {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            self.import_bytes(name, &bytes)
        } else {
            self.error = Some(t("installer-preview-target-report-unreadable"));
            Err("installer-preview-target-report-unreadable")
        }
    }

    /// Validate an imported report. It must be produced by this probe on the
    /// other computer, name a specific model and say what architecture it ran
    /// on; the last is necessary, but not sufficient, for a usable medium.
    pub(super) fn import_bytes(
        &mut self,
        _name: Option<String>,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        let report = match SupportReport::import_json(bytes) {
            Ok(report) => report,
            Err(error) => {
                self.error = Some(error);
                return Err("installer-preview-target-report-invalid");
            }
        };
        self.accept(report)?;
        self.error = None;
        Ok(())
    }

    fn accept(&mut self, report: SupportReport) -> Result<(), &'static str> {
        if report.identity.source != ReportSource::Imported {
            self.error = Some(t("installer-preview-target-report-invalid"));
            return Err("installer-preview-target-report-invalid");
        }
        match report.architecture.as_deref() {
            Some(architecture)
                if crate::release::SUPPORTED_ARCHITECTURES.contains(&architecture) =>
            {
                self.architecture = architecture.to_string();
            }
            Some(_) => {
                self.error = Some(t(
                    "installer-preview-target-report-unsupported-architecture",
                ));
                return Err("installer-preview-target-report-unsupported-architecture");
            }
            None => {
                self.error = Some(t("installer-preview-target-report-missing-architecture"));
                return Err("installer-preview-target-report-missing-architecture");
            }
        }
        self.report = Some(report);
        Ok(())
    }

    pub(super) fn clear_report(&mut self) {
        self.report = None;
        self.error = None;
    }

    /// The other computer's description, kept in the same private restart
    /// record as the rest of the choices. A report too large for that bounded
    /// record is marked omitted, so a resume asks for it again instead of
    /// looking as if it still had the evidence.
    pub(super) fn draft_choices(&self) -> super::draft::TargetDraft {
        let mut draft = super::draft::TargetDraft {
            manufacturer: self.manufacturer.get(),
            model: self.model.get(),
            variant: self.variant.get(),
            board: self.board.get(),
            sku: self.sku.get(),
            architecture: self.architecture().to_string(),
            report_omitted: false,
            report: self.report.clone(),
        };
        if let Some(report) = &draft.report
            && !serde_json::to_vec(report).is_ok_and(|bytes| bytes.len() <= MAX_DRAFT_REPORT_BYTES)
        {
            draft.report = None;
            draft.report_omitted = true;
        }
        draft
    }

    /// Put a saved description back. It is remembered evidence, not a new
    /// grant: it still goes through the same review, and one whose report did
    /// not fit the record asks to be imported again.
    pub(super) fn restore(&mut self, draft: Option<&super::draft::TargetDraft>) {
        let Some(draft) = draft else {
            return;
        };
        self.manufacturer.set(draft.manufacturer.clone());
        self.model.set(draft.model.clone());
        self.variant.set(draft.variant.clone());
        self.board.set(draft.board.clone());
        self.sku.set(draft.sku.clone());
        self.architecture.clone_from(&draft.architecture);
        self.report.clone_from(&draft.report);
        self.error = draft
            .report_omitted
            .then(|| t("installer-preview-target-report-too-large-to-resume"));
    }

    /// Check the current description again. This never probes this computer,
    /// never inspects the destination's disks and grants nothing.
    pub(super) fn recheck(&mut self) -> Result<(), &'static str> {
        let Some(report) = self.report.clone() else {
            if self.declared().is_some() {
                self.error = None;
                return Ok(());
            }
            self.error = Some(t("installer-preview-target-incomplete-detail"));
            return Err("installer-preview-target-incomplete");
        };
        if let Err(error) = report.validate() {
            self.error = Some(error);
            return Err("installer-preview-target-report-invalid");
        }
        self.accept(report)
    }

    /// Drain the chooser without blocking the UI thread.
    pub(super) fn poll(&mut self) -> bool {
        let Some(picker) = &self.picker else {
            return false;
        };
        match picker.try_recv() {
            Ok(ContentsOutcome::Picked(files)) => {
                self.picker = None;
                if let Some(file) = files.into_iter().next() {
                    let _ = self.import_bytes(Some(file.name), &file.bytes);
                }
                true
            }
            Ok(ContentsOutcome::Cancelled) => {
                self.picker = None;
                true
            }
            Ok(ContentsOutcome::Failed(_)) | Err(TryRecvError::Disconnected) => {
                self.picker = None;
                self.error = Some(t("installer-preview-target-report-unreadable"));
                true
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.picker.is_some()
    }

    /// A refusal reveals the first field that needs a value.
    pub(super) fn mark_submitted(&mut self) {
        self.submitted = true;
    }

    fn field_missing(&self, field: &str) -> bool {
        match field {
            "manufacturer" => self.manufacturer.get().trim().is_empty(),
            "model" => self.model.get().trim().is_empty(),
            // A board or SKU distinguishes an otherwise identical model just
            // as a variant name does, so none of the three is individually
            // mandatory.
            "variant" => {
                self.variant.get().trim().is_empty()
                    && self.board.get().trim().is_empty()
                    && self.sku.get().trim().is_empty()
            }
            _ => false,
        }
    }

    pub(super) fn first_invalid_focus(&self) -> Option<String> {
        if self.report.is_some() {
            return None;
        }
        FIELD_IDS
            .iter()
            .find(|(field, _)| self.field_missing(field))
            .map(|(_, id)| (*id).to_string())
    }

    pub(super) fn review_title(&self) -> String {
        let Some(report) = self.effective_report() else {
            return t("installer-preview-target-unidentified");
        };
        let manufacturer = report
            .identity
            .manufacturer
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let model = report
            .identity
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (manufacturer, model) {
            (Some(manufacturer), Some(model)) => {
                let label = format!("{manufacturer} {model}");
                match report
                    .identity
                    .variant
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(variant) => t_args(
                        "installer-hardware-model-with-variant",
                        &[("model", label.into()), ("variant", variant.into())],
                    ),
                    None => label,
                }
            }
            (Some(value), None) | (None, Some(value)) => value.to_string(),
            (None, None) => t("installer-preview-target-unidentified"),
        }
    }

    pub(super) fn review_summary(&self) -> String {
        if let Some(report) = &self.report {
            return if report.blocks_install() {
                t("installer-preview-target-review-blocking")
            } else {
                let summary = report.summary();
                t_args(
                    "installer-preview-target-review-imported",
                    &[
                        ("supported", summary.supported.into()),
                        ("limited", summary.limited.into()),
                        ("unsupported", summary.unsupported.into()),
                        ("unverified", summary.unverified.into()),
                    ],
                )
            };
        }
        t("installer-preview-target-review-declared")
    }

    /// The semantic projection. It always says the destination has to be
    /// checked again, because this computer holds no authority over it.
    pub(super) fn projection(&self) -> serde_json::Value {
        let report = self.effective_report();
        serde_json::json!({
            "ready": self.ready(),
            "source": report.as_ref().map(|report| report.identity.source).map(|source| match source {
                ReportSource::LocalScan => "local-scan",
                ReportSource::Imported => "imported",
                ReportSource::DeclaredModel => "declared-model",
            }),
            "report": report.as_ref().map(crate::setup::actions::HardwareProjection::from_report),
            "architecture": self.architecture(),
            "blocks_preparation": self.blocking(),
            // A model name and a report are preparation-time evidence only.
            // Booting, partitioning and data protection are always checked on
            // the actual destination before anything changes.
            "destination_check_required": true,
            "error": self.error.as_deref(),
        })
    }

    /// The destination-compatibility page. Important limitations are expanded
    /// by the shared report sections; working components stay compact.
    pub(super) fn page(&self, commands: &Commands) -> Column {
        let mut body = page::stack().with_rhythm(Rhythm::Sibling);
        if self.report.is_none() {
            body = body.push(Box::new(self.identity_section(commands)));
        }
        if let Some(report) = self.effective_report() {
            for section in crate::setup::view::another_computer_report_sections(&report) {
                body = body.push(section);
            }
        }
        body = body.push(Box::new(self.actions(commands)));
        if let Some(error) = &self.error {
            body = body.push(Box::new(
                Text::new(error)
                    .with_role(TextRole::Caption)
                    .with_id("installer.target.error")
                    .with_live(ui::a11y::Politeness::Assertive),
            ));
        }
        body.push(Box::new(self.destination_check_section()))
    }

    fn identity_section(&self, commands: &Commands) -> Section {
        let mut rows: Vec<Box<dyn Widget>> = Vec::new();
        for (field, id) in FIELD_IDS {
            let changes = commands.clone();
            let submits = commands.clone();
            let field_name = field;
            let name = field.to_string();
            let input = TextInput::from_state(self.field_state(field))
                .with_id(id)
                .with_kind(TextInputKind::Name)
                .on_change(move |value| {
                    changes
                        .borrow_mut()
                        .push(InstallerPreviewEffect::SetTargetIdentityField {
                            field: name.clone(),
                            value: value.to_string(),
                        });
                })
                .on_submit(move |_| queue_continue(&submits));
            let field = Field::new(self.field_label(field), input)
                .with_label_placement(Some(LabelPlacement::Above));
            rows.push(Box::new(
                if self.submitted && self.field_missing(field_name) {
                    field.with_error_live(
                        t("installer-preview-target-required"),
                        ui::a11y::Politeness::Assertive,
                    )
                } else {
                    field
                },
            ));
        }
        rows.push(Box::new(
            Text::new(&t("installer-preview-target-architecture-label"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
        let mut architectures = Row::new().no_wrap();
        for architecture in crate::release::SUPPORTED_ARCHITECTURES {
            let select = commands.clone();
            let selected = self.architecture() == architecture;
            let id = format!("installer.target.architecture.{architecture}");
            architectures = architectures.push(Box::new(
                Button::new(&id, architecture)
                    .role(if selected {
                        ButtonRole::Primary
                    } else {
                        ButtonRole::Plain
                    })
                    .on_effect(move || {
                        select
                            .borrow_mut()
                            .push(InstallerPreviewEffect::SetTargetArchitecture {
                                architecture: architecture.into(),
                            });
                    }),
            ));
        }
        rows.push(Box::new(architectures));
        rows.push(Box::new(
            Text::new(&t("installer-preview-target-scan-hint"))
                .with_role(TextRole::Caption)
                .dimmed()
                .with_id("installer.target.scan-hint"),
        ));
        Section::titled(t("installer-preview-target-identity-heading"), rows)
    }

    fn field_state(&self, field: &str) -> &ui::LocalState<String> {
        match field {
            "manufacturer" => &self.manufacturer,
            "model" => &self.model,
            "variant" => &self.variant,
            "board" => &self.board,
            _ => &self.sku,
        }
    }

    fn field_label(&self, field: &str) -> String {
        let key = match field {
            "manufacturer" => "installer-preview-target-manufacturer-label",
            "model" => "installer-preview-target-model-label",
            "variant" => "installer-preview-target-variant-label",
            "board" => "installer-preview-target-board-label",
            _ => "installer-preview-target-sku-label",
        };
        t(key)
    }

    fn actions(&self, commands: &Commands) -> Row {
        let choose = commands.clone();
        let mut row = Row::new().no_wrap().push(Box::new(
            Button::new(
                "installer.target.import",
                &t("installer-preview-target-import"),
            )
            .with_enabled(!self.busy())
            .on_effect(move || {
                choose
                    .borrow_mut()
                    .push(InstallerPreviewEffect::ChooseTargetHardwareReport);
            }),
        ));
        if self.report.is_some() {
            let clear = commands.clone();
            row = row.push(Box::new(
                Button::new(
                    "installer.target.use-model",
                    &t("installer-preview-target-use-model"),
                )
                .role(ButtonRole::Plain)
                .on_effect(move || {
                    clear
                        .borrow_mut()
                        .push(InstallerPreviewEffect::ClearTargetHardwareReport);
                }),
            ));
        }
        let recheck = commands.clone();
        row = row.push(Box::new(
            Button::new(
                "installer.target.recheck",
                &t("installer-preview-target-recheck"),
            )
            .role(ButtonRole::Plain)
            .on_effect(move || {
                recheck
                    .borrow_mut()
                    .push(InstallerPreviewEffect::RecheckTargetHardware);
            }),
        ));
        row.push(Box::new(Spacer::new()))
    }

    fn destination_check_section(&self) -> Section {
        let rows = [
            (
                "installer.target.check.boot",
                "installer-preview-target-check-boot",
                "installer-preview-target-check-boot-body",
                Icon::Shield,
            ),
            (
                "installer.target.check.storage",
                "installer-preview-target-check-storage",
                "installer-preview-target-check-storage-body",
                Icon::HardDrive,
            ),
            (
                "installer.target.check.protection",
                "installer-preview-target-check-protection",
                "installer-preview-target-check-protection-body",
                Icon::Lock,
            ),
        ]
        .into_iter()
        .map(|(id, title, body, icon)| {
            Box::new(
                ListRow::new(id, t(title))
                    .secondary(t(body))
                    .two_line_secondary()
                    .icon(icon),
            ) as Box<dyn Widget>
        })
        .chain(std::iter::once(Box::new(
            Text::new(&t("installer-preview-target-check-note"))
                .with_role(TextRole::Caption)
                .dimmed()
                .with_id("installer.target.check.note"),
        ) as Box<dyn Widget>))
        .collect::<Vec<_>>();
        Section::new(rows).with_header(SectionHeader::heading(t(
            "installer-preview-target-check-heading",
        )))
    }
}

fn host_architecture() -> String {
    let architecture = std::env::consts::ARCH;
    if crate::release::SUPPORTED_ARCHITECTURES.contains(&architecture) {
        architecture.to_string()
    } else {
        crate::release::SUPPORTED_ARCHITECTURES[0].to_string()
    }
}

fn optional(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
