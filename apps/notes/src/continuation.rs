//! Comparison-only identities for Notes' actual captured builder domains.
//! The current owner/document and invocation scope remain SDK responsibilities.
use crate::model::{PageId, PageMeta, TagId, TagMeta};
use tilo_actions::{
    ContinuationArguments, ContinuationEvidence, ContinuationKey, ContinuationQuery,
};

#[derive(Clone)]
pub(crate) enum BuilderDomain {
    File(Option<PageId>),
    ImportContent,
    RenamePage(PageMeta),
    CreatePage {
        parent: Option<PageId>,
        order: u32,
    },
    ReplaceBlock {
        page: PageId,
        block: String,
        original: String,
    },
    CreateTag,
    CreateAssignedTag(PageId),
    RenameTag(TagMeta),
    TagMembership {
        tag: TagId,
        assigned: bool,
    },
    Duplicate {
        source: PageId,
        pages: Vec<PageId>,
    },
    DailyCreate {
        title: String,
        order: u32,
    },
    /// Generic published doors (`apply` / `group` / rich-text catalogue
    /// builders) that mint from arguments and therefore need a Draft domain.
    PublishedDoor {
        action: String,
        scope: Vec<u8>,
    },
}

pub(crate) fn optional_page(page: Option<PageId>) -> Vec<u8> {
    let mut bytes = vec![u8::from(page.is_some())];
    if let Some(page) = page {
        bytes.extend_from_slice(page.as_bytes());
    }
    bytes
}

impl BuilderDomain {
    fn identity(&self) -> (&'static str, Vec<Vec<u8>>) {
        match self {
            Self::File(page) => (
                "file-picker-import-or-export/v1",
                vec![optional_page(*page)],
            ),
            Self::ImportContent => ("markdown-content-import/v1", vec![]),
            Self::RenamePage(page) => (
                "rename-page-trim-upsert/v1",
                vec![
                    page.id.as_bytes().to_vec(),
                    page.title.as_bytes().to_vec(),
                    optional_page(page.parent),
                    page.order.to_be_bytes().to_vec(),
                ],
            ),
            Self::CreatePage { parent, order } => (
                "create-page-v4-uuid-trim-default/v1",
                vec![optional_page(*parent), order.to_be_bytes().to_vec()],
            ),
            Self::ReplaceBlock {
                page,
                block,
                original,
            } => (
                "replace-block-byte-range/v1",
                vec![
                    page.as_bytes().to_vec(),
                    block.as_bytes().to_vec(),
                    original.as_bytes().to_vec(),
                ],
            ),
            Self::CreateTag => ("create-tag-v4-uuid-trim-default/v1", vec![]),
            Self::CreateAssignedTag(page) => {
                ("create-and-assign-tag/v1", vec![page.as_bytes().to_vec()])
            }
            Self::RenameTag(tag) => (
                "rename-tag-trim/v1",
                vec![tag.id.as_bytes().to_vec(), tag.name.as_bytes().to_vec()],
            ),
            Self::TagMembership { tag, assigned } => (
                "tag-membership-uuid-or-nil/v1",
                vec![tag.as_bytes().to_vec(), vec![u8::from(*assigned)]],
            ),
            Self::Duplicate { source, pages } => {
                let mut parts = vec![
                    source.as_bytes().to_vec(),
                    (pages.len() as u64).to_be_bytes().to_vec(),
                ];
                for page in pages {
                    parts.push(page.as_bytes().to_vec());
                }
                ("duplicate-exact-source-map-fresh-ids/v2", parts)
            }
            Self::DailyCreate { title, order } => (
                "daily-create-v4-uuid/v1",
                vec![title.as_bytes().to_vec(), order.to_be_bytes().to_vec()],
            ),
            Self::PublishedDoor { action, scope } => (
                "published-dynamic-door/v1",
                vec![scope.clone(), action.as_bytes().to_vec()],
            ),
        }
    }
    fn exact_parts(&self, arguments: &ContinuationArguments<'_>) -> Option<Vec<Vec<u8>>> {
        let text = match *arguments {
            ContinuationArguments::Text(value) => Some(value),
            ContinuationArguments::Json(values) => values
                .get(tilo_actions::resource::PROMPTED_PARAM)
                .and_then(serde_json::Value::as_str),
            ContinuationArguments::Immediate => None,
        }?;
        match self {
            Self::RenamePage(_) | Self::ReplaceBlock { .. } | Self::RenameTag(_) => {
                Some(self.identity().1)
            }
            Self::TagMembership { .. } => {
                let page = text.trim().parse::<PageId>().ok()?;
                let mut parts = self.identity().1;
                parts.push(page.as_bytes().to_vec());
                Some(parts)
            }
            // New identities / file chooser targets do not yet exist.
            _ => None,
        }
    }
}

pub(crate) fn builder(
    domain: BuilderDomain,
) -> impl for<'a> Fn(ContinuationQuery<'a>) -> Option<ContinuationEvidence> + Send + Sync {
    move |query| {
        let (version, mut parts) = domain.identity();
        parts.insert(0, version.as_bytes().to_vec());
        let handler = ContinuationKey::new("notes.continuation.handler.v1", [version.as_bytes()]);
        match query {
            ContinuationQuery::Draft => Some(ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new("notes.continuation.domain.v1", parts),
                handler,
            }),
            ContinuationQuery::Exact(arguments) => {
                domain.exact_parts(&arguments).map(|mut target| {
                    target.insert(0, version.as_bytes().to_vec());
                    ContinuationEvidence::Exact {
                        target: ContinuationKey::new("notes.continuation.target.v1", target),
                        handler,
                    }
                })
            }
        }
    }
}

pub(crate) fn fixed_target(
    version: &'static str,
    parts: Vec<Vec<u8>>,
) -> impl for<'a> Fn(ContinuationQuery<'a>) -> Option<ContinuationEvidence> + Send + Sync {
    move |query| {
        let handler = ContinuationKey::new("notes.continuation.handler.v1", [version.as_bytes()]);
        let key = || ContinuationKey::new("notes.continuation.target.v1", parts.clone());
        match query {
            ContinuationQuery::Draft => Some(ContinuationEvidence::Draft {
                resolution_domain: key(),
                handler,
            }),
            ContinuationQuery::Exact(ContinuationArguments::Immediate) => {
                Some(ContinuationEvidence::Exact {
                    target: key(),
                    handler,
                })
            }
            ContinuationQuery::Exact(ContinuationArguments::Json(args)) if args.is_empty() => {
                Some(ContinuationEvidence::Exact {
                    target: key(),
                    handler,
                })
            }
            ContinuationQuery::Exact(_) => None,
        }
    }
}

pub(crate) fn provider(
    version: &'static str,
    parts: Vec<Vec<u8>>,
) -> ui::widget::ActionDirectoryInvocationDomain {
    ui::widget::ActionDirectoryInvocationDomain::versioned(&ContinuationEvidence::Draft {
        resolution_domain: ContinuationKey::new("notes.lazy-domain.v1", parts),
        handler: ContinuationKey::new("notes.lazy-handler.v1", [version.as_bytes()]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn draft(domain: BuilderDomain) -> ContinuationEvidence {
        builder(domain)(ContinuationQuery::Draft).unwrap()
    }

    #[test]
    fn continuation_domains_bind_hidden_captures_beyond_labels() {
        let page = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let meta = PageMeta {
            id: page,
            title: "Same label".into(),
            parent: None,
            order: 0,
        };
        let mut reparented = meta.clone();
        reparented.parent = Some(other);
        let mut reordered = meta.clone();
        reordered.order = 1;
        for changed in [reparented, reordered] {
            assert_ne!(
                draft(BuilderDomain::RenamePage(meta.clone())),
                draft(BuilderDomain::RenamePage(changed))
            );
        }
        assert_ne!(
            draft(BuilderDomain::CreatePage {
                parent: None,
                order: 0
            }),
            draft(BuilderDomain::CreatePage {
                parent: Some(page),
                order: 0
            })
        );
        assert_ne!(
            draft(BuilderDomain::CreatePage {
                parent: Some(page),
                order: 0
            }),
            draft(BuilderDomain::CreatePage {
                parent: Some(page),
                order: 1
            })
        );
        assert_ne!(
            draft(BuilderDomain::ReplaceBlock {
                page,
                block: "block-a".into(),
                original: "a b".into()
            }),
            draft(BuilderDomain::ReplaceBlock {
                page,
                block: "block-a".into(),
                original: "a  b".into()
            })
        );
        assert_ne!(
            draft(BuilderDomain::TagMembership {
                tag: page,
                assigned: false
            }),
            draft(BuilderDomain::TagMembership {
                tag: page,
                assigned: true
            })
        );
        assert_ne!(
            draft(BuilderDomain::File(None)),
            draft(BuilderDomain::File(Some(page)))
        );
        assert_ne!(
            draft(BuilderDomain::Duplicate {
                source: page,
                pages: vec![page]
            }),
            draft(BuilderDomain::Duplicate {
                source: page,
                pages: vec![page, Uuid::from_u128(2)]
            })
        );
        assert_ne!(
            draft(BuilderDomain::DailyCreate {
                title: "2026-09-22".into(),
                order: 0
            }),
            draft(BuilderDomain::DailyCreate {
                title: "2026-09-23".into(),
                order: 0
            })
        );
    }

    #[test]
    fn continuation_creation_and_chooser_domains_do_not_invent_exact_targets() {
        let page = Uuid::from_u128(1);
        for domain in [
            BuilderDomain::File(None),
            BuilderDomain::File(Some(page)),
            BuilderDomain::ImportContent,
            BuilderDomain::CreatePage {
                parent: Some(page),
                order: 1,
            },
            BuilderDomain::CreateTag,
            BuilderDomain::CreateAssignedTag(page),
            BuilderDomain::Duplicate {
                source: page,
                pages: vec![],
            },
            BuilderDomain::DailyCreate {
                title: "2026-09-22".into(),
                order: 0,
            },
        ] {
            let resolver = builder(domain);
            assert!(matches!(
                resolver(ContinuationQuery::Draft),
                Some(ContinuationEvidence::Draft { .. })
            ));
            assert!(resolver(ContinuationQuery::Exact(ContinuationArguments::Immediate)).is_none());
            assert!(
                resolver(ContinuationQuery::Exact(ContinuationArguments::Text(
                    "New name"
                )))
                .is_none()
            );
        }
    }

    #[test]
    fn continuation_exact_tag_target_uses_same_prompted_uuid_parser() {
        let resolver = builder(BuilderDomain::TagMembership {
            tag: Uuid::from_u128(1),
            assigned: true,
        });
        let page = Uuid::from_u128(2).to_string();
        let args = serde_json::json!({"value": format!(" {page} ")})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            resolver(ContinuationQuery::Exact(ContinuationArguments::Text(&page))),
            resolver(ContinuationQuery::Exact(ContinuationArguments::Json(&args)))
        );
        assert!(
            resolver(ContinuationQuery::Exact(ContinuationArguments::Text(
                "not a UUID"
            )))
            .is_none()
        );
    }
}
