//! The workspace governance actions whose change the asking device signs, and the change each
//! asks for with its arguments: the host that runs the action and the device that signs what it
//! answers read them here, the same, so a device signs only the change it asked for
//! ([`crate::asked_signing`]).

use serde_json::{Map, Value};
use tilo_action_policy::roles::governance;
use tilo_action_policy::{Change, EntryPolicy, ListingPolicy};

use crate::workspace_membership::{
    AppPayload, AppRelease, MemberPlace, SettingsChange, WorkspaceMembershipCommand, flatpak_app_id,
};

/// Holding a person in the workspace at a place: a role, an administrator's or an owner's.
pub const ADD_MEMBER: &str = "member.add";
/// Keeping the version of an app the workspace runs: an update is refused until it is unpinned.
pub const PIN_APP: &str = "app.pin";
/// Letting the version of an app the workspace runs be updated again.
pub const UNPIN_APP: &str = "app.unpin";
/// No longer choosing which version of an app the workspace runs (`install-app`, the governance
/// action of that name, chooses one).
pub const UNINSTALL_APP: &str = "uninstall-app";
/// Add or replace an attestor the workspace trusts.
pub const ADD_ATTESTOR: &str = "trust.add";
/// Remove an attestor.
pub const REMOVE_ATTESTOR: &str = "trust.remove";
/// Refuse every package publisher key from now on.
pub const REVOKE_PUBLISHER: &str = "trust.revoke";
/// Add a registry.
pub const ADD_REGISTRY: &str = "registry.add";
/// Remove a registry.
pub const REMOVE_REGISTRY: &str = "registry.remove";
/// Read a registry again.
pub const ENABLE_REGISTRY: &str = "registry.enable";
/// Stop reading a registry.
pub const DISABLE_REGISTRY: &str = "registry.disable";
/// Choose the channel a registry offers.
pub const REGISTRY_CHANNEL: &str = "registry.channel";
/// Listing the workspace as a public workspace, with its description and the role everyone who
/// joins gets.
pub const PUBLISH: &str = "publish";
/// Taking the workspace out of the directory.
pub const UNPUBLISH: &str = "unpublish";
/// Settling the workspace's content policy: its owners'.
pub const SET_CONTENT_POLICY: &str = "content.set-policy";

impl WorkspaceMembershipCommand {
    /// The change the workspace governance action `verb` asks for with `arguments`. `None` for
    /// an action that makes no change a device signs, or reads only (`show`).
    ///
    /// # Errors
    /// The arguments do not name what `verb` needs.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per governance action: the one place a verb becomes its change"
    )]
    pub fn of_action(verb: &str, arguments: &Map<String, Value>) -> Result<Option<Self>, String> {
        let args = Arguments(arguments);
        let person = || args.key("person").map(hex::encode);
        Ok(Some(match verb {
            ADD_MEMBER | governance::ADD_MEMBER => Self::Place {
                person: args.key("person")?,
                place: {
                    let text = args.text("role")?;
                    MemberPlace::parse(text).ok_or_else(|| format!("no role `{text}`"))?
                },
                name: args
                    .optional_text("name")?
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
            },
            governance::REMOVE_MEMBER => Self::Member {
                change: Change::RemoveMember { person: person()? },
            },
            governance::BAN => Self::Member {
                change: Change::Ban { person: person()? },
            },
            governance::UNBAN => Self::Member {
                change: Change::Unban { person: person()? },
            },
            governance::INSTALL_APP => {
                let flatpak = args.optional_text("flatpak")?;
                // A Flatpak names neither a package nor a publisher: its reference is its
                // payload. A version that publishes no surface names none.
                let (package, publisher) = if flatpak.is_some() {
                    (String::new(), String::new())
                } else {
                    (
                        args.text("package")?.to_owned(),
                        args.text("publisher")?.to_owned(),
                    )
                };
                Self::InstallApp(AppRelease {
                    app: named_app(args.optional_text("app")?, flatpak)?,
                    version: args.text("version")?.to_owned(),
                    surface: args
                        .optional_text("surface")?
                        .unwrap_or_default()
                        .to_owned(),
                    package,
                    publisher,
                    file: args.optional_text("file")?.map(str::to_owned),
                    dev: args.flag_or("dev", false)?,
                    pinned: false,
                    payload: match flatpak {
                        Some(reference) => AppPayload::Flatpak {
                            reference: reference.to_owned(),
                        },
                        None => AppPayload::Tapp,
                    },
                })
            }
            UNINSTALL_APP => Self::UninstallApp {
                app: named_app(args.optional_text("app")?, args.optional_text("flatpak")?)?,
            },
            ADD_ATTESTOR => Self::AddAttestor(crate::TrustAttestor {
                id: args.text("id")?.to_owned(),
                pubkey_hex: args.text("pubkey")?.to_ascii_lowercase(),
                scope: args.optional_text("scope")?.unwrap_or("all").to_owned(),
            }),
            REMOVE_ATTESTOR => Self::RemoveAttestor {
                id: args.text("id")?.to_owned(),
            },
            REVOKE_PUBLISHER => Self::RevokePublisher {
                pubkey: args.text("pubkey")?.to_ascii_lowercase(),
                reason: args.optional_text("reason")?.unwrap_or_default().to_owned(),
            },
            ADD_REGISTRY => Self::AddRegistry(crate::PackageRegistry {
                name: args.text("name")?.to_owned(),
                base_url: args.text("url")?.trim_end_matches('/').to_owned(),
                owner_pubkey_hex: args.text("owner_key")?.to_ascii_lowercase(),
                enabled: true,
                channel: args
                    .optional_text("channel")?
                    .unwrap_or("stable")
                    .to_owned(),
            }),
            REMOVE_REGISTRY => Self::RemoveRegistry {
                name: args.text("name")?.to_owned(),
            },
            ENABLE_REGISTRY | DISABLE_REGISTRY => Self::SetRegistryEnabled {
                name: args.text("name")?.to_owned(),
                enabled: verb == ENABLE_REGISTRY,
            },
            REGISTRY_CHANNEL => Self::SetRegistryChannel {
                name: args.text("name")?.to_owned(),
                channel: args.text("channel")?.to_owned(),
            },
            tilo_actions::remote::WORKSPACE_GRANT_REVOKE => Self::Revoke {
                grant: tilo_action_policy::GrantId(args.text("grant")?.to_owned()),
            },
            tilo_actions::remote::WORKSPACE_READ_GRANT
            | tilo_actions::remote::WORKSPACE_READ_REVOKE => {
                let grantee = match (arguments.get("member"), args.optional_text("role")?) {
                    (Some(member), None) => tilo_action_policy::Grantee::member(
                        serde_json::from_value(member.clone())
                            .map_err(|error| error.to_string())?,
                    ),
                    (None, Some(role)) => tilo_action_policy::Grantee::role(
                        tilo_action_policy::Role::parse(role)
                            .ok_or_else(|| format!("`{role}` is no role"))?,
                    ),
                    _ => return Err("name a member or a role, not both".to_owned()),
                };
                Self::Reading {
                    grantee,
                    resource: args.text("resource")?.to_owned(),
                    reads: verb == tilo_actions::remote::WORKSPACE_READ_GRANT,
                }
            }
            governance::RENAME => Self::Settings(SettingsChange::Rename {
                name: args.text("name")?.to_owned(),
            }),
            PUBLISH => Self::Settings(SettingsChange::Entry(published_entry(arguments)?)),
            UNPUBLISH => Self::Settings(SettingsChange::Unlist),
            // Without a limit named it only reads the policy.
            SET_CONTENT_POLICY => {
                let limits = (
                    args.integer("file_bytes")?,
                    args.integer("member_bytes")?,
                    args.integer("hourly_bytes")?,
                );
                if limits == (None, None, None) {
                    return Ok(None);
                }
                Self::Settings(SettingsChange::Limits {
                    file_bytes: limits.0,
                    member_bytes: limits.1,
                    hourly_bytes: limits.2,
                })
            }
            governance::CALL => {
                let channel = args.text("channel")?.to_owned();
                collab_types::calls::validate_channel(&channel)?;
                let Some(change) = call_change(&args)? else {
                    return Ok(None);
                };
                Self::Call { channel, change }
            }
            _ => return Ok(None),
        }))
    }

    /// Whether this change is the one the workspace governance action `verb` asks for with
    /// `arguments` ([`Self::of_action`]): what a device checks before it signs a change its host
    /// answered, so it signs only what it asked for.
    ///
    /// # Errors
    /// The arguments do not name what `verb` needs.
    pub fn asked_by(&self, verb: &str, arguments: &Map<String, Value>) -> Result<bool, String> {
        Ok(Self::of_action(verb, arguments)?.as_ref() == Some(self))
    }

    /// The change a page's `/workspaces` request asks for, as the page's device signs it. `None`
    /// for a request that makes no change a device signs.
    #[must_use]
    pub fn of_page_request(
        request: &tilo_server_protocol::workspaces::WorkspacesRequest,
    ) -> Option<Self> {
        use tilo_server_protocol::workspaces::WorkspacesRequest;
        Some(match request {
            WorkspacesRequest::EditRoles { change, .. } => Self::Member {
                change: change.clone(),
            },
            WorkspacesRequest::SetDocumentAccess {
                app,
                document,
                recipient,
                access,
                ..
            } => Self::DocumentAccess {
                recipient: recipient.clone(),
                app: app.clone(),
                document: document.clone(),
                access: access.clone(),
            },
            _ => return None,
        })
    }
}

/// The app a change names: `app`, or the one the Flatpak `flatpak` installs, which must agree.
///
/// # Errors
/// Neither is named, the reference does not read, or the two disagree.
pub fn named_app(app: Option<&str>, flatpak: Option<&str>) -> Result<String, String> {
    let unreadable = || "`flatpak` names `app/<application id>/<arch>/<branch>`".to_owned();
    match (app, flatpak) {
        (Some(app), None) => Ok(app.to_owned()),
        (Some(app), Some(reference)) => {
            let named = flatpak_app_id(reference).ok_or_else(unreadable)?;
            if named != app {
                return Err(format!(
                    "the Flatpak `{reference}` is {named} in this workspace, not `{app}`"
                ));
            }
            Ok(named)
        }
        (None, Some(reference)) => flatpak_app_id(reference).ok_or_else(unreadable),
        (None, None) => Err("`app` is required".to_owned()),
    }
}

/// The change to a call the arguments of `call` ask for: `None` for `show`.
fn call_change(args: &Arguments<'_>) -> Result<Option<collab_types::calls::CallChange>, String> {
    use collab_types::calls::{CallChange, Directions};
    let directions = || -> Result<Directions, String> {
        Ok(Directions {
            publish: args.flag_or("publish", true)?,
            subscribe: args.flag_or("subscribe", true)?,
        })
    };
    Ok(Some(match args.text("operation")? {
        "start" => CallChange::Start {
            allowed: directions()?,
        },
        "end" => CallChange::End,
        "restart" => CallChange::Restart,
        "set-directions" => CallChange::SetDirections {
            allowed: directions()?,
        },
        "deny" => CallChange::Deny {
            person: args.key("person")?,
            denied: directions()?,
        },
        "allow" => CallChange::Allow {
            person: args.key("person")?,
        },
        "show" => return Ok(None),
        other => return Err(format!("no call operation `{other}`")),
    }))
}

/// The entry `publish` asks for, as its arguments name it: the whole entry, so the device signs
/// exactly what it asked for.
///
/// # Errors
/// The arguments name a role a public workspace does not hand out, an entry that is neither
/// `open` nor `invite`, or a number too large.
pub fn published_entry(arguments: &Map<String, Value>) -> Result<EntryPolicy, String> {
    let args = Arguments(arguments);
    let text = args.text("default_role")?;
    let default_role = crate::join::Role::parse(text)
        .filter(|role| {
            matches!(
                role,
                crate::join::Role::Viewer
                    | crate::join::Role::Commenter
                    | crate::join::Role::Editor
            )
        })
        .ok_or_else(|| {
            format!("a public workspace hands out viewer, commenter or editor, not `{text}`")
        })?;
    let invitation_only = match args.optional_text("admission")?.unwrap_or("open").trim() {
        "open" => false,
        "invite" => true,
        other => return Err(format!("an entry is `open` or `invite`, not `{other}`")),
    };
    let wait_list_max = args
        .integer("wait_list_max")?
        .map(usize::try_from)
        .transpose()
        .map_err(|_| "`wait_list_max` is too large".to_owned())?
        .unwrap_or(0);
    let trimmed = |name| -> Result<Option<String>, String> {
        Ok(args
            .optional_text(name)?
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned))
    };
    let mail = trimmed("mail_from")?.map(|from| {
        Ok::<_, String>(tilo_action_policy::EntryMail {
            from,
            templates: trimmed("mail_templates")?,
        })
    });
    EntryPolicy {
        listing: Some(ListingPolicy {
            description: args
                .optional_text("description")?
                .unwrap_or_default()
                .to_owned(),
            default_role,
        }),
        invitation_only,
        wait_list_max,
        contact: trimmed("contact")?,
        mail: mail.transpose()?,
    }
    .checked()
    .map_err(|error| error.to_string())
}

/// A governance action's named arguments, read as every reader of them reads them: each
/// refusal names the argument and what it must be.
pub struct Arguments<'a>(pub &'a Map<String, Value>);

impl<'a> Arguments<'a> {
    /// The text `name`, where it is given.
    pub fn optional_text(&self, name: &str) -> Result<Option<&'a str>, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(format!("`{name}` must be text")),
        }
    }

    /// The text `name`, which must be given.
    pub fn text(&self, name: &str) -> Result<&'a str, String> {
        self.optional_text(name)?
            .ok_or_else(|| format!("`{name}` is required"))
    }

    /// The whole number `name`, where it is given.
    pub fn integer(&self, name: &str) -> Result<Option<u64>, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| format!("`{name}` must be a whole number")),
        }
    }

    /// The flag `name`, or `otherwise` where it is not given.
    pub fn flag_or(&self, name: &str, otherwise: bool) -> Result<bool, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(otherwise),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(format!("`{name}` must be true or false")),
        }
    }

    /// [`Self::key`], where `name` is given.
    pub fn optional_key(&self, name: &str) -> Result<Option<[u8; 32]>, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(_) => self.key(name).map(Some),
        }
    }

    /// The key `name`: 64 hex characters, or a person as a program gives one
    /// (`{"person": "<hex>"}`).
    pub fn key(&self, name: &str) -> Result<[u8; 32], String> {
        let text = match self.0.get(name) {
            Some(Value::Object(person)) => person
                .get("person")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("`{name}` must be a person"))?,
            _ => self.text(name)?,
        };
        let mut key = [0; 32];
        hex::decode_to_slice(text, &mut key)
            .map_err(|_| format!("`{name}` must be 64 hex characters"))?;
        Ok(key)
    }
}
