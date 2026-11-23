//! The signed group name and transport workspace of a key exchange.

use collab_types::WorkspaceId;

/// A workspace group or the independent group for one of its documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyScope {
    Workspace(WorkspaceId),
    Document {
        workspace: WorkspaceId,
        app: String,
        document: String,
        readers: Option<String>,
    },
}

impl KeyScope {
    /// The document's group, bound to its declared reader set when it names one.
    #[must_use]
    pub fn document(
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        readers: Option<&str>,
    ) -> Option<Self> {
        let named = collab_types::store_document_readers(document)
            .filter(|name| *name != collab_types::EVERY_MEMBER);
        match (named, readers) {
            (None, None) => {}
            (Some(name), Some(readers))
                if matches!(
                    collab_types::Readers::parse(readers),
                    Some(collab_types::Readers::People(_))
                ) && collab_types::reader_set_name(readers) == name => {}
            _ => return None,
        }
        (!app.is_empty()).then(|| Self::Document {
            workspace,
            app: app.to_owned(),
            document: document.to_owned(),
            readers: readers.map(str::to_owned),
        })
    }

    /// Whether the document id binds a list of named readers.
    #[must_use]
    pub fn has_named_readers(document: &str) -> bool {
        collab_types::store_document_readers(document)
            .is_some_and(|name| name != collab_types::EVERY_MEMBER)
    }

    /// The workspace that routes this group's messages.
    #[must_use]
    pub const fn workspace(&self) -> WorkspaceId {
        match self {
            Self::Workspace(workspace) | Self::Document { workspace, .. } => *workspace,
        }
    }

    /// The identity signed in an MLS commit or access request.
    #[must_use]
    pub fn group(&self) -> WorkspaceId {
        match self {
            Self::Workspace(workspace) => *workspace,
            Self::Document {
                workspace,
                app,
                document,
                readers,
            } => {
                let Some(readers) = readers else {
                    return workspace.document_key_group(app, document);
                };
                let mut hash =
                    blake3::Hasher::new_derive_key("tilo named-reader document key group v1");
                hash.update(workspace.as_uuid().as_bytes());
                hash.update(&u64::try_from(app.len()).unwrap_or(u64::MAX).to_le_bytes());
                hash.update(app.as_bytes());
                hash.update(
                    &u64::try_from(readers.len())
                        .unwrap_or(u64::MAX)
                        .to_le_bytes(),
                );
                hash.update(readers.as_bytes());
                hash.update(document.as_bytes());
                let mut id = [0; 16];
                hash.finalize_xof().fill(&mut id);
                // Group ids share the MLS wire field with workspace UUIDs.
                // Reserve a non-RFC variant so this cannot alias a workspace
                // or the derived UUID of a personal store.
                id[8] |= 0xe0;
                WorkspaceId::from_u128(u128::from_be_bytes(id))
            }
        }
    }

    /// Whether this signed group id requires an exact document route.
    #[must_use]
    pub fn requires_document(group: WorkspaceId) -> bool {
        group.is_document_key_group()
    }

    #[must_use]
    pub fn parse(wire: &str) -> Option<Self> {
        let Some((workspace, route)) = wire.split_once('/') else {
            return WorkspaceId::parse(wire).map(Self::Workspace);
        };
        let (app, route) = route.split_once('/')?;
        let (document, readers) = route
            .split_once('/')
            .map_or((route, None), |(document, readers)| {
                (document, Some(readers))
            });
        let workspace = WorkspaceId::parse(workspace)?;
        let decode = |value| String::from_utf8(hex::decode(value).ok()?).ok();
        let app = decode(app)?;
        let document = decode(document)?;
        let readers = match readers {
            Some(readers) => Some(decode(readers)?),
            None => None,
        };
        Self::document(workspace, &app, &document, readers.as_deref())
    }
}

impl std::fmt::Display for KeyScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(workspace) => workspace.fmt(f),
            Self::Document {
                workspace,
                app,
                document,
                readers,
            } => {
                write!(
                    f,
                    "{workspace}/{}/{}",
                    hex::encode(app),
                    hex::encode(document)
                )?;
                if let Some(readers) = readers {
                    write!(f, "/{}", hex::encode(readers))?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_has_a_stable_group_distinct_from_the_workspace_and_other_documents() {
        let workspace = WorkspaceId::from_u128(1);
        let scope = |workspace, app: &str, document: &str| KeyScope::Document {
            workspace,
            app: app.to_owned(),
            document: document.to_owned(),
            readers: None,
        };
        let document = scope(workspace, "tilo.notes", "notes/é");
        assert_eq!(
            KeyScope::parse(&document.to_string()),
            Some(document.clone())
        );
        assert_eq!(document.workspace(), workspace);
        assert_ne!(document.group(), workspace);
        for different in [
            scope(workspace, "tilo.notes", ""),
            scope(workspace, "tilo.other", "notes/é"),
            scope(WorkspaceId::from_u128(2), "tilo.notes", "notes/é"),
        ] {
            assert_ne!(document.group(), different.group());
        }
        assert_eq!(
            KeyScope::parse(&workspace.to_string()),
            Some(KeyScope::Workspace(workspace))
        );
    }

    #[test]
    fn document_requests_route_to_the_workspace_and_keep_separate_checkpoints() {
        let workspace = WorkspaceId::from_u128(1);
        let device = collab_types::Ed25519PublicKey::from_bytes([42; 32]);
        let fields = |document: &str| {
            let scope = KeyScope::Document {
                workspace,
                app: "tilo.notes".to_owned(),
                document: document.to_owned(),
                readers: None,
            };
            (
                scope.to_string(),
                Box::new(collab_types::MlsAccessRequestV1::unsigned(
                    scope.group(),
                    device,
                    vec![1, 2],
                    100,
                )),
            )
        };
        let (first_scope, first_request) = fields("first");
        let (second_scope, second_request) = fields("second");
        let first = crate::NeighbourMsg::MlsAccessRequest {
            workspace: first_scope,
            request: first_request.clone(),
            delivery: 0,
        };
        let second = crate::NeighbourMsg::MlsAccessRequest {
            workspace: second_scope.clone(),
            request: second_request,
            delivery: 0,
        };
        assert_eq!(
            first.governance_workspace(),
            Some(workspace.to_string().as_str())
        );
        assert_ne!(
            crate::retention(&first, false).unwrap(),
            crate::retention(&second, false).unwrap()
        );
        let wrong_scope = crate::NeighbourMsg::MlsAccessRequest {
            workspace: second_scope,
            request: first_request,
            delivery: 0,
        };
        assert_eq!(crate::retention(&wrong_scope, false), None);
    }
}
