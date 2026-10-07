//! Shared retained widgets and the framework's consented file chooser.

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::i18n::{EmbeddedCatalog, I18nManager};
use ui::portal::{FileFilter, PickRequest};
use ui::tilo_resource::{self, ActionRef, Effect, Invoke};
use ui::tokens::TextRole;
use ui::widget::{Column, Element, EmptyState, NavShell, Property, PropertyList, Scrollable, Text};

use crate::model::{AttentionCache, MAX_REPORT_BYTES, MAX_REPORTS, StorageReport, report_id};
use crate::resource::{ChipProject, import};

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "chip-title",
)
.with_system_locale();

fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "chip_design.effect")]
pub enum ChipEffect {
    /// Import analytical report from the host's consented file chooser.
    #[effect(job, cancellable, device_only)]
    ImportFile,
    /// Inspect one report, or return to all reports.
    #[effect(view, permission = "read")]
    Inspect {
        #[param("Report content identity; omit to show all reports")]
        report: Option<String>,
    },
}

const REMOVE: ActionRef<ChipProject, String> = ActionRef::new("remove-report", &["report"]);

pub struct ChipDesignShell;

impl ResourceApp for ChipDesignShell {
    type Resource = ChipProject;
    const ID: &'static str = "so.tilo.chip-design";
    const TITLE: &'static str = "Chip Design";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<ChipProject>) -> Element {
        let selected = cx.local(None::<String>);
        let opened = selected.clone();
        cx.on_open(move |anchor| opened.set(Some(anchor.to_owned())));
        let inspected = selected.clone();
        cx.effects(move |effect: ChipEffect, cx| match effect {
            ChipEffect::Inspect { report } => {
                if report
                    .as_ref()
                    .is_some_and(|id| !cx.live().with(|state| state.reports.contains_key(id)))
                {
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
                    let id = report_id(&json);
                    let invoke = import(json)?;
                    if cx.live().with(|state| {
                        state.reports.len() >= MAX_REPORTS && !state.reports.contains_key(&id)
                    }) {
                        return Err(tr("chip-report-limit"));
                    }
                    cx.invoke(invoke);
                    Ok(Effected::done())
                },
            )),
        });
        let content = cx.switch(&selected, |cx, selected| {
            let selected = selected.clone();
            let reports = cx.value(|state| state.reports.clone());
            cx.switch(&reports, move |cx, reports| {
                if selected
                    .as_ref()
                    .is_some_and(|id| !reports.contains_key(id))
                {
                    return Box::new(EmptyState::new(tr("chip-report-missing")));
                }
                if reports.is_empty() {
                    return Box::new(
                        EmptyState::new(tr("chip-empty-title"))
                            .with_message(tr("chip-empty-message")),
                    );
                }
                let mut body = Column::new().with_intrinsic_height();
                for (id, report) in reports {
                    if selected.as_ref().is_some_and(|selection| selection != id) {
                        continue;
                    }
                    body = body
                        .push(Box::new(
                            Text::new(&report.model).with_role(TextRole::Heading),
                        ))
                        .push(Box::new(Text::new(&tr("chip-imported-evidence"))))
                        .push(Box::new(report_properties(id, report)))
                        .push(Box::new(
                            cx.button(&tr("chip-remove"), REMOVE.with(id.clone())),
                        ));
                }
                Box::new(Scrollable::new_vertical().with_child(Box::new(body)))
            })
        });
        let import = Box::new(cx.effect_button(&tr("chip-import"), &ChipEffect::ImportFile));
        let all =
            Box::new(cx.effect_button(&tr("chip-show-all"), &ChipEffect::Inspect { report: None }));
        let search = Box::new(cx.search_button(ui::search::SearchRequest::this_app()));
        Box::new(
            NavShell::new()
                .content(content)
                .content_header(tr("chip-title"), vec![search, all, import]),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &["empty", "storage-report"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<ChipProject>> {
        if name == Some("storage-report") {
            let action: ActionRef<ChipProject, String> = ActionRef::new("import-report", &["json"]);
            vec![action.with(include_str!("../fixtures/storage-report.json").to_owned())]
        } else {
            Vec::new()
        }
    }
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
