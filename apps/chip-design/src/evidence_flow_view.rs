//! Read-only reported dependency graph, using the shared retained canvas.
use crate::resource::{Project as ChipProject, ProjectState};
use crate::verification::flow::{
    Comparison, DependencyKind, EvidenceFlow, Observation, SimulationOutcome,
};
use crate::verification::{CheckStatus, ReceiptStatus};
use crate::view::ChipEffect;
use ui::framework::{EffectCx, Effected, ViewCx};
use ui::i18n::I18nManager;
use ui::widget::{
    Button, Card, Column, Container, Element, GraphCanvas, GraphCanvasConnection, Property,
    PropertyList, ScrollAxis, Scrollable, Text,
};

fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}
pub fn anchor(report: &str, kind: &str, id: &str) -> String {
    format!("{report}/{kind}/{id}")
}
/// The id the evidence item is published under.
pub fn resource_id(report: &str, kind: &str, id: &str) -> String {
    format!(
        "chip_design.project/derived/flow:{}",
        anchor(report, kind, id)
    )
}
pub fn split(value: &str) -> Option<(&str, &str, &str)> {
    let mut parts = value.splitn(3, '/');
    let result = (parts.next()?, parts.next()?, parts.next()?);
    parts.next().is_none().then_some(result)
}
pub fn item(state: &ProjectState, value: &str) -> Option<serde_json::Value> {
    let (report, kind, id) = split(value)?;
    let record = state.verifications.get(report)?;
    let flow = record.receipt.evidence_flow.as_ref()?;
    let value = match kind {
        "stage" => serde_json::to_value(flow.stages.iter().find(|stage| stage.id == id)?).ok()?,
        "subflow" => {
            let link = flow.subflows.iter().find(|link| link.id == id)?;
            serde_json::json!({"reference":link, "resolved_resource":subflow_target(state,report,id).map(crate::resource::verification_resource_id)})
        }
        "artifact" => {
            serde_json::json!({"relative_path":id,"sha256":record.receipt.artifact_sha256.get(id)?})
        }
        "placement" => crate::placement_view::item(state, report, id)?,
        _ => return None,
    };
    Some(serde_json::json!({"custody":"reported_unverified","content":value}))
}
fn button(cx: &ViewCx<ChipProject>, report: &str, kind: &str, id: &str, label: &str) -> Button {
    cx.actions().bind_effect_invocation(
        Button::new(&format!("flow-{kind}-{id}"), label),
        &ChipEffect::InspectEvidence {
            report: report.into(),
            item: format!("{kind}/{id}"),
        },
    )
}
fn property(key: &str, value: &(impl ToString + ?Sized)) -> Property {
    Property::new(tr(key), value.to_string())
}
pub fn body(
    cx: &ViewCx<ChipProject>,
    state: &ProjectState,
    report: &str,
    selected: Option<(&str, &str)>,
) -> Element {
    let Some(record) = state.verifications.get(report) else {
        return Box::new(Text::new(&tr("chip-report-missing")));
    };
    let Some(flow) = record.receipt.evidence_flow.as_ref() else {
        return Box::new(Text::new(&tr("chip-report-missing")));
    };
    let mut page = Column::new()
        .push(Box::new(Text::new(&flow.title)))
        .push(Box::new(Text::new(&tr("chip-flow-disclaimer"))))
        .push(Box::new(crate::view::remove_verification_button(
            cx, report,
        )));
    let mut provenance = vec![property("chip-flow-input", &flow.input_identity_sha256)];
    for (tool, identity) in &record.receipt.tools {
        provenance.push(Property::new(
            tool,
            format!("{} — {}", identity.version, identity.executable_sha256),
        ));
    }
    page = page
        .push(Box::new(PropertyList::new(provenance)))
        .push(canvas(cx, report, flow, selected));
    if let Some((kind, id)) = selected {
        page = page.push(inspector(cx, state, report, kind, id));
        if let Some(placement) = crate::placement_view::body(cx, state, report, kind, id) {
            page = page.push(placement);
        }
    }
    let artifacts = record
        .receipt
        .artifact_sha256
        .keys()
        .map(|name| Box::new(button(cx, report, "artifact", name, name)) as Element)
        .collect::<Vec<_>>();
    let mut links = Column::new().push(Box::new(Text::new(&tr("chip-flow-artifacts"))));
    for link in artifacts {
        links = links.push(link);
    }
    Box::new(Scrollable::new_vertical().with_child(Box::new(page.push(Box::new(links)))))
}
fn canvas(
    cx: &ViewCx<ChipProject>,
    report: &str,
    flow: &EvidenceFlow,
    selected: Option<(&str, &str)>,
) -> Element {
    let mut nodes = Vec::new();
    for stage in &flow.stages {
        let scope = stage
            .observations
            .as_ref()
            .map_or("unobserved", Observation::key);
        nodes.push(Box::new(
            Card::new(Box::new(
                Column::new()
                    .push(Box::new(button(
                        cx,
                        report,
                        "stage",
                        &stage.id,
                        &stage.title,
                    )))
                    .push(Box::new(Text::new(&tr(&format!("chip-flow-{scope}")))))
                    .push(Box::new(Text::new(&status(&stage.status)))),
            ))
            .selected(selected == Some(("stage", stage.id.as_str()))),
        ) as Element);
    }
    for link in &flow.subflows {
        nodes.push(Box::new(
            Column::new()
                .push(Box::new(button(
                    cx,
                    report,
                    "subflow",
                    &link.id,
                    &link.title,
                )))
                .push(Box::new(Text::new(&tr("chip-flow-reference")))),
        ) as Element);
    }
    let endpoint = |id: &str| {
        let kind = if flow.stages.iter().any(|stage| stage.id == id) {
            "stage"
        } else {
            "subflow"
        };
        format!("flow-{kind}-{id}")
    };
    let edges = flow
        .edges
        .iter()
        .map(|edge| GraphCanvasConnection {
            from: endpoint(&edge.from),
            to: endpoint(&edge.to),
        })
        .collect();
    Box::new(
        Container::new().with_height(420.0).with_child(Box::new(
            Scrollable::new(ScrollAxis::Both)
                .with_height(420.0)
                .with_child(Box::new(GraphCanvas::grid(nodes, edges, 280.0, 4))),
        )),
    )
}
fn inspector(
    cx: &ViewCx<ChipProject>,
    state: &ProjectState,
    report: &str,
    kind: &str,
    id: &str,
) -> Element {
    let Some(record) = state.verifications.get(report) else {
        return Box::new(Text::new(&tr("chip-report-missing")));
    };
    let Some(flow) = record.receipt.evidence_flow.as_ref() else {
        return Box::new(Text::new(&tr("chip-report-missing")));
    };
    let mut properties = vec![property("chip-flow-custody", &tr("chip-flow-unverified"))];
    if let Some(stage) = flow
        .stages
        .iter()
        .find(|stage| kind == "stage" && stage.id == id)
    {
        properties.extend([
            property("chip-flow-receipt", &stage.original_receipt_artifact),
            property("chip-flow-producer", &stage.producer_sha256),
            property("chip-flow-input", &stage.input_identity_sha256),
        ]);
        if let Some(hash) = record
            .receipt
            .artifact_sha256
            .get(&stage.original_receipt_artifact)
        {
            properties.push(property("chip-flow-sha", hash));
        }
        for edge in &flow.edges {
            if edge.from == stage.id || edge.to == stage.id {
                properties.push(property(
                    "chip-flow-dependency",
                    &format!(
                        "{} → {} / {} / {}",
                        edge.from,
                        edge.to,
                        tr(match edge.kind {
                            DependencyKind::Data => "chip-flow-data",
                            DependencyKind::Build => "chip-flow-build",
                            DependencyKind::Verification => "chip-flow-verification",
                        }),
                        edge.artifact
                    ),
                ));
            }
        }
        for (check, status) in &stage.checks {
            properties.push(Property::new(
                check,
                tr(match status {
                    CheckStatus::Passed => "chip-flow-passed",
                    CheckStatus::Failed => "chip-flow-failed",
                }),
            ));
        }
        for limitation in &stage.limitations {
            properties.push(property("chip-flow-limit", limitation));
        }
        if let Some(observation) = &stage.observations {
            observations(&mut properties, observation);
        }
    } else if let Some(link) = flow
        .subflows
        .iter()
        .find(|link| kind == "subflow" && link.id == id)
    {
        let Some(hash) = record.receipt.artifact_sha256.get(&link.summary_artifact) else {
            return Box::new(Text::new(&tr("chip-report-missing")));
        };
        properties.extend([
            property("chip-flow-artifact", &link.summary_artifact),
            property("chip-flow-sha", hash),
        ]);
        let target = subflow_target(state, report, id);
        let mut body = Column::new().push(Box::new(PropertyList::new(properties)));
        if target.is_some() {
            body = body.push(Box::new(cx.effect_button(
                tr("chip-flow-open-subflow"),
                &ChipEffect::OpenEvidenceSubflow {
                    report: report.into(),
                    id: id.into(),
                },
            )));
        } else {
            body = body.push(Box::new(Text::new(&tr("chip-flow-not-imported"))));
        }
        return Box::new(body);
    } else if kind == "artifact"
        && let Some(hash) = record.receipt.artifact_sha256.get(id)
    {
        properties.extend([
            property("chip-flow-artifact", id),
            property("chip-flow-sha", hash),
        ]);
    }
    Box::new(PropertyList::new(properties))
}
fn observations(properties: &mut Vec<Property>, observation: &Observation) {
    match observation {
        Observation::CpuReference {
            comparison,
            compared_words,
            differing_words,
            output_sha256,
        } => properties.extend([
            property(
                "chip-flow-comparison",
                &tr(match comparison {
                    Comparison::Exact => "chip-flow-exact",
                    Comparison::Characterization => "chip-flow-characterization",
                }),
            ),
            property("chip-flow-compared", compared_words),
            property("chip-flow-differing", differing_words),
            property("chip-flow-output", output_sha256),
        ]),
        Observation::RtlSimulation {
            outcome,
            refused_inputs,
            cycles,
            accepted_inputs,
            accepted_outputs,
            output_sha256,
        } => properties.extend([
            property(
                "chip-flow-outcome",
                &tr(match outcome {
                    SimulationOutcome::Completed => "chip-flow-completed",
                    SimulationOutcome::ExpectedRefusal => "chip-flow-expected-refusal",
                }),
            ),
            property("chip-flow-refusals", refused_inputs),
            property("chip-flow-cycles", cycles),
            property("chip-flow-input-count", accepted_inputs),
            property("chip-flow-output-count", accepted_outputs),
            property("chip-flow-output", output_sha256),
        ]),
        Observation::GenericSynthesis {
            logic_cells,
            logical_memory_bits,
        } => properties.extend([
            property("chip-flow-cells", logic_cells),
            property("chip-flow-memory", logical_memory_bits),
        ]),
        Observation::LibraryPhysical {
            library,
            clock_constraint_ns,
            setup_slack_ns,
            hold_slack_ns,
            routed_signal_nets,
            router_violations,
            library_area_um2,
        } => properties.extend([
            property("chip-flow-library", library),
            property("chip-flow-clock-constraint", clock_constraint_ns),
            property("chip-flow-setup", setup_slack_ns),
            property("chip-flow-hold", hold_slack_ns),
            property("chip-flow-nets", routed_signal_nets),
            property("chip-flow-router", router_violations),
            property("chip-flow-area", library_area_um2),
        ]),
    }
}

fn status(status: &ReceiptStatus) -> String {
    tr(match status {
        ReceiptStatus::Passed => "chip-flow-passed",
        ReceiptStatus::Failed => "chip-flow-failed",
        ReceiptStatus::Cancelled => "chip-flow-cancelled",
    })
}

pub fn subflow_target<'a>(state: &'a ProjectState, report: &str, id: &str) -> Option<&'a str> {
    let record = state.verifications.get(report)?;
    let link = record
        .receipt
        .evidence_flow
        .as_ref()?
        .subflows
        .iter()
        .find(|link| link.id == id)?;
    let hash = record.receipt.artifact_sha256.get(&link.summary_artifact)?;
    state
        .verifications
        .iter()
        .find(|(_, record)| {
            record.receipt.evidence_flow.is_some()
                && record.imported_bytes_sha256.as_ref() == Some(hash)
        })
        .map(|(id, _)| id.as_str())
}

pub fn present(
    effect: ChipEffect,
    cx: &EffectCx<ChipProject>,
    selected: &ui::LocalState<Option<String>>,
) -> Result<Effected, String> {
    let target = match effect {
        ChipEffect::OpenEvidenceSubflow { report, id } => cx
            .live()
            .with(|state| subflow_target(&ProjectState::of(state), &report, &id).map(str::to_owned))
            .ok_or_else(|| tr("chip-flow-not-imported"))?,
        ChipEffect::InspectEvidence { report, item: part } => {
            let target = format!("{report}/{part}");
            if !cx
                .live()
                .with(|state| item(&ProjectState::of(state), &target).is_some())
            {
                return Err(tr("chip-report-missing"));
            }
            target
        }
        _ => return Err(tr("chip-report-missing")),
    };
    selected.set(Some(target));
    Ok(Effected::done())
}
