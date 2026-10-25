//! Shared retained widgets and the framework's consented file chooser.

use ui::framework::{Effected, LaunchInput, ResourceApp, ViewCx};
use ui::i18n::{EmbeddedCatalog, I18nManager};
use ui::portal::{FileFilter, PickRequest};
use ui::tilo_resource::{self, Effect, Invoke};
use ui::tokens::TextRole;
use ui::widget::{
    Button, Column, Element, EmptyState, NavShell, Property, PropertyList, Scrollable, Text,
};

use crate::model::{AttentionCache, MAX_REPORT_BYTES, MAX_REPORTS, StorageReport, report_id};
use crate::resource::{Project as ChipProject, ProjectState, import};
use crate::verification::{CheckStatus, ReceiptStatus, VerificationReceipt, VerificationRecord};

ui::embedded_catalog!(static CATALOG, ["en-US"]);

fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "chip_design.effect")]
pub enum ChipEffect {
    /// Import analytical report from the host's consented file chooser.
    #[effect(attended, job, cancellable)]
    ImportFile,
    /// Open the structural operator graph.
    #[effect(view, kind = "read")]
    OperatorGraph,
    /// Add an operator to the graph, placed after the ones already there.
    AddOperator {
        #[param("The operator's name, as the add menu lists it (e.g. Input, Multiply)")]
        kind: String,
    },
    /// Inspect a structural operator; omit node to clear local selection.
    #[effect(view, kind = "read")]
    InspectOperator { node: Option<String> },
    /// Choose an output before invoking the typed connect action.
    #[effect(view, kind = "read")]
    SelectOutput { node: String, port: String },
    /// Inspect a reported evidence stage, artifact, or subflow reference.
    #[effect(view, kind = "read")]
    InspectEvidence { report: String, item: String },
    /// Open an exactly matched imported subflow; refuses unloaded references.
    #[effect(view, kind = "read")]
    OpenEvidenceSubflow { report: String, id: String },
    /// Inspect one report, or return to all reports.
    #[effect(view, kind = "read")]
    Inspect {
        #[param("Report content identity; omit to show all reports")]
        report: Option<String>,
    },
}

pub struct ChipDesignShell;

/// What a search hit or an agent's `open` names, as the selection the view
/// shows: a report or receipt by its identity, an evidence item by its
/// anchor, or the operator graph.
fn opened_place(anchor: &str) -> String {
    let place = anchor
        .rsplit_once("/derived/")
        .map_or(anchor, |(_, place)| place);
    if place == "operator-graph" {
        return crate::operator_graph::RESOURCE_ID.to_owned();
    }
    ["report:", "verification:", "flow:"]
        .iter()
        .find_map(|prefix| place.strip_prefix(prefix))
        .unwrap_or(place)
        .to_owned()
}

fn capture_notice(cx: &ViewCx<ChipProject>, selected: &ui::LocalState<Option<String>>) -> Element {
    let invalid_file = crate::launch::invalid(&cx.launch());
    let synthetic = cx.local(false);
    let notice = synthetic.clone();
    let staged = selected.clone();
    cx.on_stage(move |scenario| {
        notice.set(scenario.is_some_and(|name| name != "empty" && name != "operator-graph"));
        if scenario == Some("evidence-flow")
            && let Ok((id, _)) =
                VerificationRecord::import(include_str!("../fixtures/evidence-flow.json"))
        {
            staged.set(Some(format!("{id}/stage/physical")));
        }
        if scenario == Some("operator-graph") {
            staged.set(Some(crate::operator_graph::RESOURCE_ID.into()));
        }
    });
    cx.switch(&synthetic, move |_, synthetic| {
        let label = if *synthetic {
            tr("chip-synthetic-fixture")
        } else if invalid_file {
            tr("chip-open-invalid")
        } else {
            String::new()
        };
        Box::new(Text::new(&label))
    })
}

impl ResourceApp for ChipDesignShell {
    type Resource = ChipProject;
    const ID: &'static str = "so.tilo.chip-design";
    // Each window shows the document it opened, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Chip Design";
    // Content collection currently stages non-media transfers with a .bin name.
    // Both routes validate the complete bounded report before any action is sent.
    const OPENS: &'static [&'static str] = &["application/json", "application/octet-stream"];
    const ARGUMENTS: usize = 1;

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<ChipProject>> {
        crate::launch::actions(input)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<ChipProject>) -> Element {
        let selected = cx.local(None::<String>);
        let graph_selection = cx.local(crate::operator_graph_view::Selection::default());
        let notice = capture_notice(cx, &selected);
        let opened = selected.clone();
        cx.on_open(move |anchor| opened.set(Some(opened_place(anchor))));
        let inspected = selected.clone();
        let graph_opened = selected.clone();
        let graph_inspected = graph_selection.clone();
        cx.effects(move |effect: ChipEffect, cx| match effect {
            effect @ (ChipEffect::OperatorGraph
            | ChipEffect::AddOperator { .. }
            | ChipEffect::InspectOperator { .. }
            | ChipEffect::SelectOutput { .. }) => {
                crate::operator_graph_view::present(effect, cx, &graph_opened, &graph_inspected)
            }
            effect @ (ChipEffect::OpenEvidenceSubflow { .. }
            | ChipEffect::InspectEvidence { .. }) => {
                crate::evidence_flow_view::present(effect, cx, &inspected)
            }
            ChipEffect::Inspect { report } => {
                if report.as_ref().is_some_and(|id| {
                    !cx.live().with(|state| {
                        state.reports.contains_key(id) || state.verifications.contains_key(id)
                    })
                }) {
                    return Err(tr("chip-report-missing"));
                }
                inspected.set(report);
                Ok(Effected::done())
            }
            ChipEffect::ImportFile => Ok(cx.choose_file(
                PickRequest::open(&tr("chip-import-title"), &tr("chip-import"))
                    .filter(FileFilter::new(&tr("chip-report-filter")).glob("*.json"))
                    .max_contents_bytes(MAX_REPORT_BYTES),
                |(), chosen, cx| {
                    let Some(chosen) = chosen else {
                        return Err(tr("chip-import-cancelled"));
                    };
                    if chosen.bytes.len() > MAX_REPORT_BYTES {
                        return Err(tr("chip-import-too-large"));
                    }
                    let json =
                        String::from_utf8(chosen.bytes).map_err(|error| error.to_string())?;
                    let is_storage = StorageReport::parse(&json).is_ok();
                    let id = if is_storage {
                        report_id(&json)
                    } else {
                        VerificationRecord::import(&json)?.0
                    };
                    let invoke = import(json)?;
                    if cx.live().with(|state| {
                        if is_storage {
                            state.reports.len() >= MAX_REPORTS && !state.reports.contains_key(&id)
                        } else {
                            state.verifications.len() >= MAX_REPORTS
                                && !state.verifications.contains_key(&id)
                        }
                    }) {
                        return Err(tr("chip-report-limit"));
                    }
                    cx.submit(invoke);
                    Ok(Effected::done())
                },
            )),
        });
        let content = cx.switch(&graph_selection, move |cx, graph_selection| {
            let graph_selection = graph_selection.clone();
            cx.switch(&selected, move |cx, selected| {
                let selected = selected.clone();
                let graph_selection = graph_selection.clone();
                let evidence = cx.value(ProjectState::of);
                cx.switch(&evidence, move |cx, state| {
                    if selected.as_deref() == Some(crate::operator_graph::RESOURCE_ID) {
                        crate::operator_graph_view::body(
                            cx,
                            &state.operator_graph,
                            &graph_selection,
                        )
                    } else {
                        evidence_body(cx, state, selected.as_deref())
                    }
                })
            })
        });
        let import = Box::new(cx.effect_button(tr("chip-import"), &ChipEffect::ImportFile));
        let all =
            Box::new(cx.effect_button(tr("chip-show-all"), &ChipEffect::Inspect { report: None }));
        let graph = Box::new(cx.effect_button(tr("chip-graph-title"), &ChipEffect::OperatorGraph));
        Box::new(
            NavShell::new()
                .content_kind(ui::widget::ContentKind::Document)
                .content(Box::new(Column::new().push(notice).push(content)))
                // The window bar already names the app; the header holds only
                // its commands, which the bar shows as its actions.
                .content_header("", vec![graph, all, import]),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "empty",
            "operator-graph",
            "storage-report",
            "checkpoint-verification",
            "q4k-superblock",
            "research-mapping",
            "evidence-flow",
        ]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<ChipProject>> {
        let verification = |json: &str| vec![ChipProject::IMPORT_VERIFICATION.with(json)];
        match name {
            Some("operator-graph") => crate::operator_graph_view::scenario(),
            Some("evidence-flow") => verification(include_str!("../fixtures/evidence-flow.json")),
            Some("storage-report") => vec![
                ChipProject::IMPORT_REPORT.with(include_str!("../fixtures/storage-report.json")),
            ],
            Some("research-mapping") => {
                verification(include_str!("../fixtures/research-mapping.json"))
            }
            Some("q4k-superblock") => verification(include_str!("../fixtures/q4k-superblock.json")),
            Some("checkpoint-verification") => {
                verification(include_str!("../fixtures/checkpoint-verification.json"))
            }
            _ => Vec::new(),
        }
    }
}

pub(super) fn remove_verification_button(cx: &ViewCx<ChipProject>, id: &str) -> Button {
    cx.actions().bind_invocation(
        Button::new(
            &format!("chip-remove-verification-{id}"),
            &tr("chip-remove"),
        ),
        ChipProject::REMOVE_VERIFICATION.with(id),
    )
}

fn evidence_body(
    cx: &ViewCx<ChipProject>,
    state: &ProjectState,
    selected: Option<&str>,
) -> Element {
    if let Some(value) = selected {
        let (report, selection) = crate::evidence_flow_view::split(value)
            .map_or((value, None), |(report, kind, id)| {
                (report, Some((kind, id)))
            });
        if state
            .verifications
            .get(report)
            .is_some_and(|record| record.receipt.evidence_flow.is_some())
        {
            return crate::evidence_flow_view::body(cx, state, report, selection);
        }
    }
    let reports = &state.reports;
    let verifications = &state.verifications;
    if selected.is_some_and(|id| !reports.contains_key(id) && !verifications.contains_key(id)) {
        return Box::new(EmptyState::new(tr("chip-report-missing")));
    }
    if reports.is_empty() && verifications.is_empty() {
        // A first visit says what the app is for and offers its one way in
        // that needs nothing from outside: the operator graph.
        return Box::new(
            cx.actions().bind_effect_invocation(
                EmptyState::new(tr("chip-empty-title"))
                    .with_message(tr("chip-empty-message"))
                    .with_action(tr("chip-empty-open-graph"))
                    .with_action_id("chip-empty-open-graph"),
                &ChipEffect::OperatorGraph,
            ),
        );
    }
    let mut body = Column::new().with_intrinsic_height();
    for (id, report) in reports {
        if selected.is_some_and(|selection| selection != id.as_str()) {
            continue;
        }
        body = body
            .push(Box::new(
                Text::new(&report.model).with_role(TextRole::Heading),
            ))
            .push(Box::new(Text::new(&tr("chip-imported-evidence"))))
            .push(Box::new(report_properties(id, report)))
            .push(Box::new(cx.button(
                tr("chip-remove"),
                ChipProject::REMOVE_REPORT.with(id.as_str()),
            )));
    }
    for (id, record) in verifications {
        if selected.is_some_and(|selection| selection != id.as_str()) {
            continue;
        }
        if let Some(flow) = &record.receipt.evidence_flow {
            body = body
                .push(Box::new(cx.effect_button(
                    &flow.title,
                    &ChipEffect::Inspect {
                        report: Some(id.clone()),
                    },
                )))
                .push(Box::new(remove_verification_button(cx, id)));
            continue;
        }
        body = body
            .push(Box::new(
                Text::new(&record.receipt.evidence).with_role(TextRole::Heading),
            ))
            .push(Box::new(Text::new(&tr(
                if record.receipt.architecture_study.is_some() {
                    "chip-study-disclaimer"
                } else if record.receipt.operator_summary.is_some() {
                    "chip-operator-disclaimer"
                } else {
                    "chip-verification-disclaimer"
                },
            ))))
            .push(Box::new(verification_properties(id, record)))
            .push(Box::new(remove_verification_button(cx, id)));
    }
    Box::new(Scrollable::new_vertical().with_child(Box::new(body)))
}

fn property(key: &str, value: &(impl ToString + ?Sized)) -> Property {
    Property::new(tr(key), value.to_string())
}

fn report_properties(id: &str, report: &StorageReport) -> PropertyList {
    let source = vec![
        property("chip-model", &report.model),
        property("chip-revision", &report.revision),
        property("chip-config-digest", &report.config_sha256),
        property("chip-report-digest", id),
    ];
    let assumptions = &report.assumptions;
    let precision = vec![
        property("chip-parameters", &assumptions.logical_parameters),
        property("chip-weight-bits", &assumptions.weight_bits),
        property("chip-kv-bits", &assumptions.kv_bits),
        property("chip-context", &assumptions.context_tokens),
        property("chip-batch", &report.batch_size),
    ];
    let mut storage = vec![property("chip-weight-bytes", &report.packed_weight_bytes)];
    let mut excluded = Vec::new();
    match &report.full_attention_cache {
        AttentionCache::PartialBound {
            full_attention_layers,
            kv_elements,
            packed_bytes,
            excludes,
        } => {
            storage.extend([
                property("chip-attention-layers", full_attention_layers),
                property("chip-kv-elements", kv_elements),
                property("chip-kv-bytes", packed_bytes),
            ]);
            excluded.extend(
                excludes
                    .iter()
                    .map(|value| property("chip-excluded", value)),
            );
        }
        AttentionCache::Unsupported { reason } => {
            storage.push(property("chip-cache-unsupported", reason));
        }
    }
    excluded.extend(
        report
            .not_estimated
            .iter()
            .map(|value| property("chip-unestimated", value)),
    );
    excluded.extend(
        report
            .limitations
            .iter()
            .map(|value| property("chip-limitation", value)),
    );
    excluded.push(property("chip-assessment", &tr("chip-assessment-value")));
    PropertyList::sections(vec![
        (tr("chip-provenance"), source),
        (tr("chip-assumptions"), precision),
        (tr("chip-storage"), storage),
        (tr("chip-scope"), excluded),
    ])
    .with_id(format!("chip-report-{id}"))
}

fn check_status(status: &CheckStatus) -> String {
    tr(match status {
        CheckStatus::Passed => "chip-check-passed",
        CheckStatus::Failed => "chip-check-failed",
    })
}

fn mapping_scope(receipt: &VerificationReceipt) -> Vec<Property> {
    let mut scope = Vec::new();
    if let Some(count) = receipt.mapped_cell_count {
        scope.push(property("chip-mapped-cell-count", &count));
    }
    if let Some(area) = &receipt.reported_library_area {
        scope.push(property("chip-library-area", area));
    }
    if let Some(target) = receipt.abc_delay_target_ps {
        scope.push(property("chip-abc-target", &target));
    }
    if receipt.manufacturable == Some(false) {
        scope.push(property("chip-process-scope", &tr("chip-research-only")));
    }
    scope
}

fn superblock_scope(receipt: &VerificationReceipt) -> Vec<Property> {
    let mut properties = Vec::new();
    if let Some(count) = receipt.generic_cell_count {
        properties.push(property("chip-generic-cell-count", &count));
    }
    if let Some(count) = receipt.external_scalar_c_comparisons {
        properties.push(property("chip-external-scalar-comparisons", &count));
    }
    if let Some(protocol) = &receipt.real_protocol {
        properties.push(property("chip-protocol-observation", protocol));
    }
    if let Some(instrumentation) = &receipt.c_instrumentation {
        properties.push(property("chip-c-instrumentation", instrumentation));
    }
    if let Some(flags) = &receipt.compiler_flags {
        properties.push(property("chip-scalar-compiler-flags", &flags.join(" ")));
    }
    properties
}

fn superblock_identities(receipt: &VerificationReceipt) -> Vec<Property> {
    let mut properties = Vec::new();
    for (key, value) in [
        ("chip-sample-digest", &receipt.sample_sha256),
        ("chip-sample-receipt-digest", &receipt.sample_receipt_sha256),
        ("chip-rtl-source-digest", &receipt.rtl_source_sha256),
    ] {
        if let Some(value) = value {
            properties.push(property(key, value));
        }
    }
    for values in [&receipt.primary_source_sha256, &receipt.source_sha256]
        .into_iter()
        .flatten()
    {
        properties.extend(
            values
                .iter()
                .map(|(name, digest)| Property::new(name, digest.clone())),
        );
    }
    properties
}

fn verification_checks_label(receipt: &VerificationReceipt) -> &'static str {
    if receipt.architecture_study.is_some() {
        "chip-study-checks"
    } else if receipt.operator_summary.is_some() {
        "chip-operator-checks"
    } else {
        "chip-receipt-checks"
    }
}

fn verification_properties(id: &str, record: &VerificationRecord) -> PropertyList {
    let receipt = &record.receipt;
    let scope = verification_scope(receipt);
    let identities = verification_identities(id, record);
    let checks = receipt
        .checks
        .iter()
        .map(|(name, status)| Property::new(name, check_status(status)))
        .collect();
    let artifacts = receipt
        .artifact_sha256
        .iter()
        .map(|(name, digest)| Property::new(name, digest.clone()))
        .collect();
    let exclusions = receipt
        .excludes
        .iter()
        .map(|name| property("chip-verification-exclusion", name))
        .collect();
    let mut sections = vec![
        (tr("chip-receipt-scope"), scope),
        (tr(verification_checks_label(receipt)), checks),
        (tr("chip-provenance"), identities),
        (tr("chip-receipt-artifacts"), artifacts),
        (tr("chip-scope"), exclusions),
    ];
    if let Some(study) = &receipt.architecture_study {
        for variant in &study.variants {
            sections.push((variant.identity.clone(), study_variant_properties(variant)));
            if let Some(synthesis) = &variant.synthesis {
                for memory in &synthesis.memories {
                    sections.push((
                        format!("{} / {}", variant.identity, memory.name),
                        study_memory_properties(&variant.identity, memory),
                    ));
                }
            }
        }
    }
    PropertyList::sections(sections).with_id(format!("chip-verification-{id}"))
}

fn verification_scope(receipt: &VerificationReceipt) -> Vec<Property> {
    let status = tr(match receipt.status {
        ReceiptStatus::Passed => "chip-check-passed",
        ReceiptStatus::Failed => "chip-check-failed",
        ReceiptStatus::Cancelled => "chip-check-cancelled",
    });
    let mut scope = vec![
        property("chip-reported-status", &status),
        property("chip-evidence-class", &receipt.evidence),
    ];
    if let Some(failure) = &receipt.failure {
        scope.push(property(
            "chip-failure",
            if failure.is_empty() { &status } else { failure },
        ));
    }
    if let Some(stage) = &receipt.failure_stage {
        scope.push(property("chip-failure-stage", stage));
    }
    if let Some(validation) = &receipt.input_validation {
        scope.push(property("chip-input-validation", &check_status(validation)));
    }
    if let Some(lanes) = receipt.lanes {
        scope.push(property("chip-lanes", &lanes));
    }
    if let Some(size) = receipt.group_size {
        scope.push(property("chip-group-size", &size));
    }
    if let Some(operator) = &receipt.operator_summary {
        identities_for_operator_scope(&mut scope, operator);
    }
    scope.extend(superblock_scope(receipt));
    scope.extend(mapping_scope(receipt));
    if let Some(study) = &receipt.architecture_study {
        scope.push(property(
            "chip-study-stage",
            &tr(&format!("chip-study-stage-{}", study.stage.key())),
        ));
        scope.push(property(
            "chip-study-words",
            &study.expected_comparison_words,
        ));
    }
    scope
}

fn verification_identities(id: &str, record: &VerificationRecord) -> Vec<Property> {
    let receipt = &record.receipt;
    let mut identities = vec![
        property("chip-import-digest", &record.imported_bytes_blake3),
        property("chip-shared-receipt-digest", id),
    ];
    identities.push(property(
        "chip-toolchain",
        &receipt
            .declared_toolchain_image
            .clone()
            .unwrap_or_else(|| tr("chip-unreported")),
    ));
    if let Some(digest) = &receipt.sample_manifest_sha256 {
        identities.push(property("chip-sample-digest", digest));
    }
    if let Some(digest) = &receipt.verifier_sha256 {
        identities.push(property("chip-verifier-digest", digest));
    }
    if let Some(digest) = &receipt.artifact_validator_sha256 {
        identities.push(property("chip-artifact-validator-digest", digest));
    }
    if let Some(digest) = &receipt.mapper_sha256 {
        identities.push(property("chip-mapper-digest", digest));
    }
    if let Some(revision) = &receipt.library_revision {
        identities.push(property("chip-library-revision", revision));
    }
    if let Some(inputs) = &receipt.input_sha256 {
        identities.extend(
            inputs
                .iter()
                .map(|(name, digest)| Property::new(name, digest.clone())),
        );
    }
    if let Some(operator) = &receipt.operator_summary {
        identities.extend(operator_identities(operator));
    }
    identities.extend(superblock_identities(receipt));
    identities.extend(reported_tools(receipt));
    if let Some(study) = &receipt.architecture_study {
        identities.push(property(
            "chip-study-original",
            &study.original_receipt_sha256,
        ));
        identities.push(property("chip-study-producer", &study.producer_sha256));
        identities.push(property("chip-study-input", &study.input_identity_sha256));
        for (stage, link) in &study.links {
            identities.push(Property::new(
                tr(&format!("chip-study-stage-{stage}")),
                link.receipt.clone(),
            ));
            identities.push(property("chip-study-producer", &link.producer));
        }
    }
    identities
}

fn study_variant_properties(variant: &crate::verification::study::Variant) -> Vec<Property> {
    let mut rows = Vec::new();
    if let Some(value) = &variant.numerical {
        rows.extend([
            property("chip-study-cold-cycles", &value.cold_program_cycles),
            property("chip-study-vector-cycles", &value.vector_load_cycles),
            property("chip-study-warm-cycles", &value.warm_cycles),
            property("chip-study-weight-bytes", &value.accepted_weight_bytes),
            property(
                "chip-study-descriptor-bytes",
                &value.accepted_descriptor_bytes,
            ),
            property("chip-study-cache-reads", &value.cache_reads),
            property("chip-study-quant-packets", &value.quantizer_packets),
            property("chip-study-compared-words", &value.comparison_words),
            property("chip-study-output", &value.output_sha256),
        ]);
    }
    if let Some(value) = &variant.synthesis {
        rows.extend([
            property("chip-study-generic-cells", &value.generic_logic_cells),
            property("chip-study-cache-bits", &value.activation_cache_bits),
            property("chip-study-programmed-bits", &value.programmed_weight_bits),
            property("chip-study-fixed-bits", &value.fixed_initialized_bits),
        ]);
    }
    rows
}

fn study_memory_properties(
    variant: &str,
    memory: &crate::verification::study::LogicalMemory,
) -> Vec<Property> {
    vec![
        property(
            "chip-study-memory-name",
            &format!("{variant} / {}", memory.name),
        ),
        property("chip-study-memory-width", &memory.width_bits),
        property("chip-study-memory-depth", &memory.depth_words),
        property("chip-study-memory-bits", &memory.bits),
        property("chip-study-memory-read-ports", &memory.read_ports),
        property(
            "chip-study-memory-clocked-reads",
            &memory.clocked_read_ports,
        ),
        property("chip-study-memory-write-ports", &memory.write_ports),
        property(
            "chip-study-memory-initialized",
            &tr(if memory.fully_initialized {
                "chip-value-yes"
            } else {
                "chip-value-no"
            }),
        ),
    ]
}

fn identities_for_operator_scope(
    scope: &mut Vec<Property>,
    operator: &crate::verification::OperatorSummary,
) {
    use crate::verification::Observations;
    let observations = match &operator.observations {
        Observations::RmsNorm(cases) => cases
            .iter()
            .map(|case| {
                CATALOG.t_args(
                    "chip-operator-rms",
                    &[
                        ("profile", case.profile.as_str().into()),
                        ("words", case.fp32_comparisons.into()),
                    ],
                )
            })
            .collect::<Vec<_>>(),
        Observations::Silu(case) => vec![CATALOG.t_args(
            "chip-operator-silu",
            &[
                ("inputs", case.total_inputs.into()),
                ("exp_differences", case.exp_differences.into()),
                ("exp_ulp", case.max_exp_ulp.into()),
            ],
        )],
        Observations::GatedDelta(cases) => cases
            .iter()
            .map(|case| {
                CATALOG.t_args(
                    "chip-operator-delta",
                    &[
                        ("heads", case.config.heads.into()),
                        ("tokens", case.config.tokens.into()),
                        ("width", case.config.width.into()),
                        ("attention", case.attention_words.into()),
                        ("state", case.state_words.into()),
                    ],
                )
            })
            .collect(),
        Observations::Softmax(cases) => cases
            .iter()
            .map(|case| {
                CATALOG.t_args(
                    "chip-operator-softmax",
                    &[
                        ("name", case.name.as_str().into()),
                        ("words", case.width.into()),
                        ("masked", case.masked.into()),
                        ("differences", case.differing_words.into()),
                        ("ulp", case.max_ulp.into()),
                    ],
                )
            })
            .collect(),
    };
    scope.extend(
        observations
            .iter()
            .map(|value| property("chip-operator-observation", value)),
    );
}

fn operator_identities(operator: &crate::verification::OperatorSummary) -> Vec<Property> {
    let mut identities = vec![
        property(
            "chip-original-receipt-sha",
            &operator.original_receipt_sha256,
        ),
        property("chip-primary-revision", &operator.primary_revision),
        property("chip-compiler-version", &operator.compiler_version),
        property("chip-tool-digest", &operator.compiler_executable_sha256),
        property("chip-compiler-flags", &operator.compiler_flags.join(" ")),
    ];
    for values in [
        &operator.primary_source_sha256,
        &operator.local_source_sha256,
        &operator.linked_library_sha256,
    ] {
        identities.extend(
            values
                .iter()
                .map(|(name, digest)| Property::new(name, digest.clone())),
        );
    }
    identities
}

fn reported_tools(receipt: &VerificationReceipt) -> Vec<Property> {
    let mut properties = Vec::new();
    for (name, tool) in &receipt.tools {
        properties.push(Property::new(name, tool.version.clone()));
        properties.push(property("chip-tool-digest", &tool.executable_sha256));
    }
    if receipt.tools.is_empty() && receipt.operator_summary.is_none() {
        properties.push(property("chip-tools", &tr("chip-unreported")));
    }
    properties
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::ChipDesignShell::scenarios() {
            let mut app = TestApp::<super::ChipDesignShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
