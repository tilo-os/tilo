//! Comparison-only Writer invocation identities; never authority.
use super::{
    RegionKind, SelectionTarget, Span, WORD_PROCESSOR_OPERATION_TYPE, WordProcessorOperation,
};
use crate::WpFormat;
use std::path::{Path, PathBuf};
use tilo_actions::{
    ActionInput, ActionSpec, ContinuationArguments, ContinuationEvidence, ContinuationKey,
    ContinuationQuery, EncodedAction,
};
use ui::widget::ActionDirectoryInvocationDomain;

type Parts = Vec<Vec<u8>>;
fn key(kind: &str, parts: Parts) -> ContinuationKey {
    ContinuationKey::new(format!("writer.continuation.{kind}.v1"), parts)
}
fn evidence(handler: &'static str, parts: Parts, exact: bool) -> ContinuationEvidence {
    let handler = key("handler", vec![handler.as_bytes().to_vec()]);
    if exact {
        ContinuationEvidence::Exact {
            target: key("target", parts),
            handler,
        }
    } else {
        ContinuationEvidence::Draft {
            resolution_domain: key("domain", parts),
            handler,
        }
    }
}
fn number(value: usize) -> Vec<u8> {
    (value as u128).to_be_bytes().to_vec()
}
fn region(value: RegionKind) -> Vec<u8> {
    vec![match value {
        RegionKind::Body => 0,
        RegionKind::Header => 1,
        RegionKind::Footer => 2,
    }]
}
pub(super) fn spans(value: RegionKind, spans: &[Span]) -> Parts {
    let mut parts = vec![region(value), number(spans.len())];
    for span in spans {
        parts.extend([
            span.block.as_bytes().to_vec(),
            number(span.start),
            number(span.end),
        ]);
    }
    parts
}
pub(super) fn selection(value: RegionKind, selected: &SelectionTarget) -> Parts {
    vec![
        region(value),
        selected.start_block.as_bytes().to_vec(),
        number(selected.start),
        selected.end_block.as_bytes().to_vec(),
        number(selected.end),
    ]
}
/// A participant's caret and ordered selection: the resolution domain of the
/// editing primitives, whose envelopes are built from exactly this view.
pub(super) fn view(
    value: RegionKind,
    caret: &super::participant::ViewPoint,
    selected: Option<(
        &super::participant::ViewPoint,
        &super::participant::ViewPoint,
    )>,
) -> Parts {
    let mut parts = vec![
        region(value),
        caret.block.as_bytes().to_vec(),
        number(caret.offset),
    ];
    match selected {
        Some((start, end)) => parts.extend([
            vec![1],
            start.block.as_bytes().to_vec(),
            number(start.offset),
            end.block.as_bytes().to_vec(),
            number(end.offset),
        ]),
        None => parts.push(vec![0]),
    }
    parts
}

/// Only the audited pure Writer argument encoders use this helper. In
/// particular place-image is excluded: evaluating its builder can fetch data.
/// Both real invocation and evidence reuse the original normalization closure.
fn pure_encoding(spec: &ActionSpec, args: &ContinuationArguments<'_>) -> Option<EncodedAction> {
    let encoded = match *args {
        ContinuationArguments::Json(args) => spec.build_json(args).ok()?,
        ContinuationArguments::Immediate => match spec.fixed_invocation_encoding() {
            Some(encoded) => encoded.clone(),
            None => spec.build_json(&serde_json::Map::new()).ok()?,
        },
        ContinuationArguments::Text(text) => match &spec.input {
            ActionInput::Prompted { build, .. } => build(text),
            _ => return None,
        },
    };
    (encoded.type_id != tilo_actions::resource::INVALID_ARGUMENTS_TYPE).then_some(encoded)
}
pub(crate) fn pure(spec: ActionSpec, handler: &'static str, domain: Parts) -> ActionSpec {
    let builder = spec.clone();
    spec.with_continuation_identity(move |query| match query {
        ContinuationQuery::Draft => Some(evidence(handler, domain.clone(), false)),
        ContinuationQuery::Exact(args) => {
            let encoded = pure_encoding(&builder, &args)?;
            let mut target = domain.clone();
            target.extend([encoded.type_id.into_bytes(), encoded.payload]);
            Some(evidence(handler, target, true))
        }
    })
}

#[derive(Clone)]
pub(crate) struct FileContext {
    pub(crate) path: Option<PathBuf>,
    pub(crate) format: Option<WpFormat>,
    pub(crate) canonical: bool,
}
fn format_tag(format: WpFormat) -> u8 {
    match format {
        WpFormat::Odt => 0,
        WpFormat::Docx => 1,
        WpFormat::Markdown => 2,
        WpFormat::Pdf => 3,
        WpFormat::Html => 4,
        WpFormat::Rtf => 5,
        WpFormat::Epub => 6,
        WpFormat::Text => 7,
    }
}
impl FileContext {
    fn parts(&self, variant: u8) -> Parts {
        let mut parts = vec![vec![variant], vec![u8::from(self.canonical)]];
        match &self.path {
            Some(path) => parts.extend([vec![1], path.as_os_str().as_encoded_bytes().to_vec()]),
            None => parts.push(vec![0]),
        }
        match self.format {
            Some(format) => parts.push(vec![1, format_tag(format)]),
            None => parts.push(vec![0]),
        }
        // The current filename controls the chooser's suggested name. Filters,
        // path interpretation and post-save adoption are versioned by handler.
        parts
    }
}
pub(super) fn file(spec: ActionSpec, variant: u8, context: Option<&FileContext>) -> ActionSpec {
    let Some(context) = context.cloned() else {
        return spec;
    };
    let builder = spec.clone();
    spec.with_continuation_identity(move |query| {
        let domain = context.parts(variant);
        match query {
            ContinuationQuery::Draft => {
                Some(evidence("file-operation-and-chooser.v1", domain, false))
            }
            ContinuationQuery::Exact(args) => {
                let encoded = pure_encoding(&builder, &args)?;
                let operation =
                    super::decode_operation(&encoded.type_id, &encoded.payload).ok()??;
                let (path, format) = match operation {
                    WordProcessorOperation::Save { path: Some(path) }
                    | WordProcessorOperation::SaveAs { path: Some(path) } => {
                        let path = PathBuf::from(path);
                        let format = WpFormat::from_path(&path);
                        (path, format)
                    }
                    WordProcessorOperation::Save { path: None } => {
                        (context.path.clone()?, context.format?)
                    }
                    // Open: native destination mount unresolved. Anything else: no
                    // invented destination for a future picker.
                    _ => return None,
                };
                let mut target = domain;
                target.extend([
                    path.as_os_str().as_encoded_bytes().to_vec(),
                    vec![format_tag(format)],
                    WORD_PROCESSOR_OPERATION_TYPE.as_bytes().to_vec(),
                    encoded.payload,
                ]);
                Some(evidence("file-operation-and-chooser.v1", target, true))
            }
        }
    })
}

pub(crate) fn bind_file_choice(
    resource: &mut tilo_actions::Resource,
    generation: u64,
    path: &Path,
    bytes: &[u8],
    format: WpFormat,
) -> ActionDirectoryInvocationDomain {
    let parts = vec![
        generation.to_be_bytes().to_vec(),
        path.as_os_str().as_encoded_bytes().to_vec(),
        vec![format_tag(format)],
        tilo_content::hash_bytes(bytes).into_bytes(),
    ];
    for spec in &mut resource.actions {
        *spec = pure(spec.clone(), "retained-file-choice.v1", parts.clone());
    }
    ActionDirectoryInvocationDomain::versioned(&evidence("retained-file-choice.v1", parts, false))
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use tilo_actions::{ContinuationArguments, ContinuationQuery};
    use ui::widget::{ActionDirectoryInvocationDomain, Widget};

    fn document() -> (AutoCommit, WpResourceCtx) {
        let mut doc = AutoCommit::new();
        crate::initialize_empty_document(&mut doc).unwrap();
        let c = tilo_doc::model::open(&doc, &automerge::ROOT).unwrap();
        let block = tilo_doc::model::blocks_in_order(&doc, &c)[0].clone();
        let ctx = WpResourceCtx {
            caret_block: block.clone(),
            caret_offset: 0,
            target_blocks: vec![block.clone()],
            format_spans: vec![Span {
                block: block.clone(),
                start: 0,
                end: 1,
            }],
            selection: Some(SelectionTarget {
                start_block: block.clone(),
                start: 0,
                end_block: block,
                end: 1,
            }),
            region: RegionKind::Body,
            search_hits: vec![],
            content: tilo_content::ContentStore::memory(),
        };
        (doc, ctx)
    }
    fn action(resources: &[Resource], id: &str) -> ActionSpec {
        resources
            .iter()
            .flat_map(|r| r.actions.iter())
            .find(|a| a.id == id)
            .unwrap()
            .clone()
    }
    #[test]
    fn writer_continuation_captured_selection_and_span_domains_are_exact() {
        let (doc, ctx) = document();
        for id in ["set-hyperlink", "replace-selection", "add-comment"] {
            let a = action(&provide_resources(&ctx, &doc), id);
            let evidence = a
                .continuation_identity(ContinuationQuery::Draft)
                .expect("real Writer dynamic action needs captured domain");
            assert_eq!(
                a.continuation_identity(ContinuationQuery::Draft),
                Some(evidence.clone())
            );
            let mut changed = ctx.clone();
            changed.format_spans[0].end += 1;
            changed.selection.as_mut().unwrap().end += 1;
            let b = action(&provide_resources(&changed, &doc), id);
            assert_ne!(
                b.continuation_identity(ContinuationQuery::Draft),
                Some(evidence)
            );
        }
        let a = action(&provide_resources(&ctx, &doc), "set-hyperlink");
        let first = serde_json::json!({"target":" https://example.test/path "});
        let normalized = serde_json::json!({"target":"https://example.test/path"});
        assert_eq!(
            a.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                first.as_object().unwrap()
            ))),
            a.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                normalized.as_object().unwrap()
            )))
        );
        assert!(
            a.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                normalized.as_object().unwrap()
            )))
            .is_some()
        );
    }
    #[test]
    fn writer_continuation_file_context_changes_but_chooser_destination_is_unknown() {
        let (doc, _) = document();
        let mut widget = crate::WordProcessorWidget::from_document(&doc).unwrap();
        widget.set_file(
            std::path::PathBuf::from("first.md"),
            crate::WpFormat::Markdown,
        );
        let first = action(&widget.publish_resources(&doc), "save-as");
        let draft = first
            .continuation_identity(ContinuationQuery::Draft)
            .expect("actual file context");
        widget.set_file(
            std::path::PathBuf::from("second.md"),
            crate::WpFormat::Markdown,
        );
        let next = action(&widget.publish_resources(&doc), "save-as");
        assert_ne!(
            next.continuation_identity(ContinuationQuery::Draft),
            Some(draft)
        );
        assert!(
            next.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                &serde_json::Map::new()
            )))
            .is_none()
        );
        let path = serde_json::json!({"path":"chosen.md"});
        assert!(
            next.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                path.as_object().unwrap()
            )))
            .is_some()
        );
    }
    #[test]
    fn writer_continuation_file_offer_noop_stays_exact_and_blob_domain_stays_unknown() {
        let (mut doc, _) = document();
        let mut widget = crate::WordProcessorWidget::from_document(&doc).unwrap();
        assert_eq!(
            ui::resources::action_directory_invocation_domain(&widget, &doc),
            ActionDirectoryInvocationDomain::Unsupported
        );
        let image = action(&widget.publish_resources(&doc), "place-image");
        assert!(
            image
                .continuation_identity(ContinuationQuery::Draft)
                .is_none()
        );
        widget
            .offer_file_choice(
                "selected.md".into(),
                b"selected history".to_vec(),
                crate::WpFormat::Markdown,
            )
            .unwrap();
        let before = ui::resources::action_directory_invocation_domain(&widget, &doc);
        assert!(matches!(
            before,
            ActionDirectoryInvocationDomain::Versioned(_)
        ));
        widget.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            ui::resources::action_directory_invocation_domain(&widget, &doc),
            before
        );
        let a = action(&ui::resources::resources(&widget, &doc), "use-selected");
        assert!(
            a.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
                .is_some()
        );
        for (path, bytes, format) in [
            (
                "other.md",
                b"selected history".as_slice(),
                crate::WpFormat::Markdown,
            ),
            (
                "selected.md",
                b"different history".as_slice(),
                crate::WpFormat::Markdown,
            ),
            (
                "selected.md",
                b"selected history".as_slice(),
                crate::WpFormat::Html,
            ),
        ] {
            let mut changed = crate::WordProcessorWidget::from_document(&doc).unwrap();
            changed
                .offer_file_choice(path.into(), bytes.to_vec(), format)
                .unwrap();
            assert_ne!(
                ui::resources::action_directory_invocation_domain(&changed, &doc),
                before
            );
            let other = action(&ui::resources::resources(&changed, &doc), "use-selected");
            assert_ne!(
                other.continuation_identity(ContinuationQuery::Exact(
                    ContinuationArguments::Immediate
                )),
                a.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
            );
        }
    }
    #[test]
    fn writer_continuation_region_span_order_and_reply_parent_cannot_alias() {
        let (doc, mut ctx) = document();
        ctx.format_spans.push(Span {
            block: "second-block".into(),
            start: 2,
            end: 4,
        });
        let identity = |context: &WpResourceCtx| {
            action(&provide_resources(context, &doc), "set-hyperlink")
                .continuation_identity(ContinuationQuery::Draft)
                .unwrap()
        };
        let before = identity(&ctx);
        let mut changed = ctx.clone();
        changed.region = RegionKind::Header;
        assert_ne!(identity(&changed), before);
        changed = ctx.clone();
        changed.format_spans.reverse();
        assert_ne!(identity(&changed), before);
        let mut comment = tilo_doc::snapshot::CommentView {
            id: "first-parent".into(),
            author: "Author".into(),
            date: String::new(),
            blocks: vec![],
            replies: vec![],
            range: None,
        };
        let first = action(&[comment_resource(&comment)], "reply");
        comment.id = "second-parent".into();
        let second = action(&[comment_resource(&comment)], "reply");
        assert_ne!(
            first.continuation_identity(ContinuationQuery::Draft),
            second.continuation_identity(ContinuationQuery::Draft)
        );
        let args = serde_json::json!({"author":" Author ","text":"reply"});
        let canonical = serde_json::json!({"author":"Author","text":"reply","date":""});
        let exact = |spec: &ActionSpec, args: &serde_json::Value| {
            spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                args.as_object().unwrap(),
            )))
        };
        assert!(exact(&first, &args).is_some());
        assert_eq!(exact(&first, &args), exact(&first, &canonical));
        assert_ne!(exact(&first, &args), exact(&second, &args));
        let invalid = serde_json::json!({"author":"  ","text":"reply"});
        assert!(exact(&first, &invalid).is_none());
    }

    #[test]
    fn writer_continuation_save_uses_actual_default_format_and_read_encodes() {
        let (doc, _) = document();
        let mut widget = crate::WordProcessorWidget::from_document(&doc).unwrap();
        widget.set_file("same-name.md".into(), crate::WpFormat::Html);
        let first = action(&widget.publish_resources(&doc), "save");
        let args = serde_json::Map::new();
        let before = first
            .continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(&args)));
        assert!(before.is_some());
        widget.set_file("same-name.md".into(), crate::WpFormat::Markdown);
        let second = action(&widget.publish_resources(&doc), "save");
        assert_ne!(
            second.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                &args
            ))),
            before
        );
        let read = crate::semantic_read::action::spec();
        assert!(
            read.continuation_identity(ContinuationQuery::Draft)
                .is_some()
        );
        assert!(
            read.fixed_invocation_encoding().is_none(),
            "typed actions stay dynamic"
        );
        let request = serde_json::json!({"pageSize":1});
        assert!(
            read.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                request.as_object().unwrap()
            )))
            .is_some()
        );
        let resource = Resource::new(
            "word-processor.document",
            "word_processor.document",
            "Document",
        )
        .with_action(read);
        let encoded = tilo_actions::door::encode_json(
            &[resource],
            &tilo_actions::door::ActionAddress::new("word-processor.document", "read-text"),
            request.as_object().unwrap(),
        )
        .unwrap();
        assert_eq!(
            encoded.type_id,
            crate::semantic_read::action::OPERATION_TYPE
        );
    }
}
