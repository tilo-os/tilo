//! Compatibility for an OS prepared on another computer: its architecture, its
//! report, and what the person said it is.

use std::path::Path;
use std::sync::mpsc::TryRecvError;

use hardware_report::{MAX_REPORT_BYTES, ReportSource, SupportReport};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{tokens::Rhythm, tokens::TextRole};
use tilo_ui_base::{widget::LabelPlacement, widget::Spacer, widget::Widget};
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, OpenedFile, PickRequest};
use ui::{
    widget::Button, widget::ButtonRole, widget::Column, widget::Field, widget::ListRow,
    widget::Row, widget::Section, widget::SectionHeader, widget::Text, widget::TextInput,
    widget::TextInputKind,
};

use super::{Commands, InstallerPreviewEffect};
use crate::layout;
use crate::setup::strings::{t, t_args};

const FIELD_IDS: [(&str, &str); 5] = [
    ("manufacturer", "installer.target.manufacturer"),
    ("model", "installer.target.model"),
    ("variant", "installer.target.variant"),
    ("board", "installer.target.board"),
    ("sku", "installer.target.sku"),
];

/// The manufacturer and model a report names, when it names both.
fn model_label(report: &SupportReport) -> Option<String> {
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
                Some(variant) => Some(t_args(
                    "installer-hardware-model-with-variant",
                    &[("model", label.into()), ("variant", variant.into())],
                )),
                None => Some(label),
            }
        }
        (Some(value), None) | (None, Some(value)) => Some(value.to_string()),
        (None, None) => None,
    }
}

pub(super) struct Target {
    manufacturer: tilo_ui_base::LocalState<String>,
    model: tilo_ui_base::LocalState<String>,
    variant: tilo_ui_base::LocalState<String>,
    board: tilo_ui_base::LocalState<String>,
    sku: tilo_ui_base::LocalState<String>,
    architecture: String,
    report: Option<SupportReport>,
    picker: Option<OpenFileChannel>,
    error: Option<String>,
    submitted: bool,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            manufacturer: tilo_ui_base::LocalState::new(String::new()),
            model: tilo_ui_base::LocalState::new(String::new()),
            variant: tilo_ui_base::LocalState::new(String::new()),
            board: tilo_ui_base::LocalState::new(String::new()),
            sku: tilo_ui_base::LocalState::new(String::new()),
            architecture: host_architecture(),
            report: None,
            picker: None,
            error: None,
            submitted: false,
        }
    }
}

impl Target {
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

    pub(super) fn architecture(&self) -> &str {
        self.report
            .as_ref()
            .and_then(|report| report.architecture.as_deref())
            .unwrap_or(&self.architecture)
    }

    pub(super) fn set_architecture(&mut self, architecture: String) -> Result<(), &'static str> {
        if let Some(boundary) = crate::release::unqualified_architecture(&architecture) {
            self.error = Some(t(boundary));
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

    pub(super) fn payload_available(&self) -> Result<bool, &'static str> {
        let architecture = self.architecture();
        let media = crate::release::bundled_releases_for_arch(architecture)
            .map_err(|_| "installer-preview-target-report-unsupported-architecture")?;
        let internal = crate::release::bundled_internal_releases_for_arch(architecture)
            .map_err(|_| "installer-preview-target-report-unsupported-architecture")?;
        Ok(!media.is_empty() || !internal.is_empty())
    }

    pub(super) fn effective_report(&self) -> Option<SupportReport> {
        self.report.clone().or_else(|| self.declared())
    }

    pub(super) fn blocking(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(SupportReport::blocks_install)
    }

    pub(super) fn manufacturer(&self) -> String {
        self.manufacturer.get()
    }

    pub(super) fn model(&self) -> String {
        self.model.get()
    }

    pub(super) fn variant(&self) -> String {
        self.variant.get()
    }

    pub(super) fn board(&self) -> String {
        self.board.get()
    }

    pub(super) fn sku(&self) -> String {
        self.sku.get()
    }

    /// The report the person imported, not the one their named model stands for.
    pub(super) fn report(&self) -> Option<&SupportReport> {
        self.report.as_ref()
    }

    pub(super) fn set_fields(
        &mut self,
        manufacturer: &str,
        model: &str,
        variant: &str,
        board: &str,
        sku: &str,
    ) {
        self.manufacturer.set(manufacturer.to_owned());
        self.model.set(model.to_owned());
        self.variant.set(variant.to_owned());
        self.board.set(board.to_owned());
        self.sku.set(sku.to_owned());
    }

    pub(super) fn set_report(&mut self, report: Option<SupportReport>) {
        self.report = report;
    }

    pub(super) fn set_field(&mut self, field: &str, value: String) -> Result<(), &'static str> {
        match field {
            "manufacturer" => self.manufacturer.set(value),
            "model" => self.model.set(value),
            "variant" => self.variant.set(value),
            "board" => self.board.set(value),
            "sku" => self.sku.set(value),
            _ => return Err("installer-preview-invalid-choice"),
        }
        self.error = None;
        Ok(())
    }

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
        self.picker = Some(request.send_open_handle());
        self.error = None;
        Ok(())
    }

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
        let Some(architecture) = report.architecture.as_deref() else {
            self.error = Some(t("installer-preview-target-report-missing-architecture"));
            return Err("installer-preview-target-report-missing-architecture");
        };
        if let Some(boundary) = crate::release::unqualified_architecture(architecture) {
            self.error = Some(t(boundary));
            return Err("installer-preview-target-report-unsupported-architecture");
        }
        self.architecture = architecture.to_string();
        self.report = Some(report);
        Ok(())
    }

    pub(super) fn clear_report(&mut self) {
        self.report = None;
        self.error = None;
    }

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

    pub(super) fn poll(&mut self) -> bool {
        let Some(picker) = &self.picker else {
            return false;
        };
        match picker.try_recv() {
            Ok(OpenFileOutcome::Opened(opened)) => {
                self.picker = None;
                let name = opened.display_name().to_owned();
                match read_report(opened) {
                    Ok(bytes) => {
                        let _ = self.import_bytes(Some(name), &bytes);
                    }
                    Err(()) => {
                        self.error = Some(t("installer-preview-target-report-unreadable"));
                    }
                }
                true
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.picker = None;
                true
            }
            Ok(OpenFileOutcome::Failed(_)) | Err(TryRecvError::Disconnected) => {
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

    pub(super) fn mark_submitted(&mut self) {
        self.submitted = true;
    }

    fn field_missing(&self, field: &str) -> bool {
        match field {
            "manufacturer" => self.manufacturer.get().trim().is_empty(),
            "model" => self.model.get().trim().is_empty(),
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

    /// The name a plan suggests for that computer: the model the person named,
    /// or nothing when they named none, so the plan leaves the name to the
    /// computer.
    pub(super) fn suggested_name(&self) -> String {
        let report = self.effective_report();
        let label = report.as_ref().and_then(model_label);
        label.map_or_else(String::new, |label| {
            label.chars().take(64).collect::<String>().trim().to_owned()
        })
    }

    /// What the plan says about that computer's hardware: what the person
    /// named or reported, and that the computer checks it when it installs.
    pub(super) fn review_plan_summary(&self) -> String {
        if let Some(report) = &self.report {
            if report.blocks_install() {
                return t("installer-preview-target-review-blocking");
            }
            let summary = report.summary();
            let counts = [
                ("supported", summary.supported.into()),
                ("limited", summary.limited.into()),
                ("unsupported", summary.unsupported.into()),
                ("unverified", summary.unverified.into()),
            ];
            return match model_label(report) {
                Some(model) => {
                    let mut arguments = vec![("model", model.into())];
                    arguments.extend(counts);
                    t_args("installer-preview-target-review-imported", &arguments)
                }
                None => t_args("installer-preview-target-review-imported-unnamed", &counts),
            };
        }
        match self.declared().as_ref().and_then(model_label) {
            Some(model) => t_args(
                "installer-preview-target-review-named",
                &[("model", model.into())],
            ),
            None => t("installer-preview-target-review-nameless"),
        }
    }

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
            "destination_check_required": true,
            "error": self.error.as_deref(),
        })
    }

    pub(super) fn page(&self, commands: &Commands) -> Column {
        let mut body = layout::stack().with_rhythm(Rhythm::Sibling);
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
                    .with_live(tilo_ui_base::a11y::Politeness::Assertive),
            ));
        }
        body.push(Box::new(self.destination_check_section()))
    }

    fn identity_section(&self, commands: &Commands) -> Section {
        let mut rows: Vec<Box<dyn Widget>> = Vec::new();
        for (field, id) in FIELD_IDS {
            let field_name = field;
            let input = super::submits(
                commands,
                super::changes(
                    commands,
                    TextInput::from_state(self.field_state(field))
                        .with_id(id)
                        .with_kind(TextInputKind::Name),
                    move |value| InstallerPreviewEffect::SetTargetIdentityField {
                        field: field.to_owned(),
                        value,
                    },
                ),
            );
            let field = Field::new(self.field_label(field), input)
                .with_label_placement(Some(LabelPlacement::Above));
            rows.push(Box::new(
                if self.submitted && self.field_missing(field_name) {
                    field.with_error_live(
                        t("installer-preview-target-required"),
                        tilo_ui_base::a11y::Politeness::Assertive,
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
                    .on_click(move || {
                        select
                            .borrow_mut()
                            .push(InstallerPreviewEffect::SetTargetArchitecture {
                                architecture: architecture.into(),
                            });
                    }),
            ));
        }
        rows.push(Box::new(architectures));
        if let Some(boundary) = crate::release::unqualified_architecture(self.architecture()) {
            rows.push(Box::new(
                Text::new(&t(boundary))
                    .with_id("installer.target.architecture-boundary")
                    .with_live(tilo_ui_base::a11y::Politeness::Polite),
            ));
        }
        rows.push(Box::new(
            Text::new(&t("installer-preview-target-scan-hint"))
                .with_role(TextRole::Caption)
                .dimmed()
                .with_id("installer.target.scan-hint"),
        ));
        Section::titled(t("installer-preview-target-identity-heading"), rows)
    }

    fn field_state(&self, field: &str) -> &tilo_ui_base::LocalState<String> {
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
            .on_click(move || {
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
                .on_click(move || {
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
            .on_click(move || {
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

pub(super) fn host_architecture() -> String {
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

/// The report a chooser answered, read through its descriptor within the report bound.
fn read_report(opened: OpenedFile) -> Result<Vec<u8>, ()> {
    let mut file = opened.into_file();
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.len() <= MAX_REPORT_BYTES as u64)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(&mut file, MAX_REPORT_BYTES as u64 + 1);
    std::io::Read::read_to_end(&mut limited, &mut bytes).map_err(|_| ())?;
    (bytes.len() <= MAX_REPORT_BYTES).then_some(bytes).ok_or(())
}
