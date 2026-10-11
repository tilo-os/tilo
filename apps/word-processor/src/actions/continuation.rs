//! Comparison-only Writer invocation identities; never authority.
use super::{RegionKind, SelectionTarget, Span};
use tilo_actions::{
    ActionSpec, ContinuationArguments, ContinuationEvidence, ContinuationKey, ContinuationQuery,
    EncodedAction,
};

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
        ContinuationArguments::Text(text) => spec.build_text(text)?,
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

#[cfg(test)]
mod tests {
    use super::super::*;
    use tilo_actions::{ContinuationArguments, ContinuationQuery};
    use ui::widget::{ActionDirectoryInvocationDomain, Widget};

    fn attached(doc: &AutoCommit) -> crate::WordProcessorWidget {
        let mut widget = crate::WordProcessorWidget::detached();
        widget.sync(doc, &automerge::ROOT);
        widget
    }

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
            let a = action(&live_presentation_resources(&ctx, &doc, None), id);
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
            let b = action(&live_presentation_resources(&changed, &doc, None), id);
            assert_ne!(
                b.continuation_identity(ContinuationQuery::Draft),
                Some(evidence)
            );
        }
        let a = action(
            &live_presentation_resources(&ctx, &doc, None),
            "set-hyperlink",
        );
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
    fn writer_continuation_blob_domain_stays_unknown() {
        let (doc, _) = document();
        let widget = attached(&doc);
        assert_eq!(
            ui::resources::action_directory_invocation_domain(&widget, &doc),
            ActionDirectoryInvocationDomain::Unsupported
        );
        let ctx = widget.wp_resource_ctx(&doc).unwrap();
        let image = action(
            &live_presentation_resources(&ctx, &doc, None),
            "place-image",
        );
        assert!(
            image
                .continuation_identity(ContinuationQuery::Draft)
                .is_none()
        );
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
            action(
                &live_presentation_resources(context, &doc, None),
                "set-hyperlink",
            )
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
    fn writer_continuation_read_text_stays_dynamic_and_encodes() {
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
