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
    },
}

impl KeyScope {
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
            } => {
                let mut hash = blake3::Hasher::new_derive_key("tilo document key group v1");
                hash.update(workspace.as_uuid().as_bytes());
                hash.update(&u64::try_from(app.len()).unwrap_or(u64::MAX).to_le_bytes());
                hash.update(app.as_bytes());
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
        let (app, document) = route.split_once('/')?;
        let workspace = WorkspaceId::parse(workspace)?;
        let app = String::from_utf8(hex::decode(app).ok()?).ok()?;
        let document = String::from_utf8(hex::decode(document).ok()?).ok()?;
        if app.is_empty() {
            return None;
        }
        Some(Self::Document {
            workspace,
            app,
            document,
        })
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
            } => {
                write!(
                    f,
                    "{workspace}/{}/{}",
                    hex::encode(app),
                    hex::encode(document)
                )
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
        let request = |document: &str| {
            let scope = KeyScope::Document {
                workspace,
                app: "tilo.notes".to_owned(),
                document: document.to_owned(),
            };
            crate::NeighbourMsg::MlsAccessRequest {
                workspace: scope.to_string(),
                request: Box::new(collab_types::MlsAccessRequestV1::unsigned(
                    scope.group(),
                    device,
                    vec![1, 2],
                    100,
                )),
                delivery: 0,
            }
        };
        let first = request("first");
        let second = request("second");
        assert_eq!(
            first.governance_workspace(),
            Some(workspace.to_string().as_str())
        );
        assert_ne!(
            crate::retention(&first, false).unwrap(),
            crate::retention(&second, false).unwrap()
        );
        let crate::NeighbourMsg::MlsAccessRequest { request, .. } = first else {
            unreachable!("the fixture is an access request");
        };
        let crate::NeighbourMsg::MlsAccessRequest { workspace, .. } = second else {
            unreachable!("the fixture is an access request");
        };
        let wrong_scope = crate::NeighbourMsg::MlsAccessRequest {
            workspace,
            request,
            delivery: 0,
        };
        assert_eq!(crate::retention(&wrong_scope, false), None);
    }
}
