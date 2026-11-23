//! Reading a site and its directory, and joining with an invitation: the
//! requests a directory makes, on `tilo_net` so the desktop's connection pool
//! and a page's `fetch` run the same ones.
//!
//! A site's `entry.json` names its home workspace and its directory of public
//! ones; the directory then lists every public workspace it serves with what
//! it is, who is in it now and how lively it is
//! ([`tilo_server_protocol::directory`]). A workspace is joined with an
//! invitation, never by entering through a site: no site's entry is read for a
//! host to dial, and a device joins only where an invitation it holds admits it.
//!
//! What a join became is logged at `info` under
//! [`tilo_startup_metrics::JOIN_FLOW_TARGET`], which the session's log keeps
//! whatever `RUST_LOG` says: whether an invitation verified, and what the host
//! it admits into answered in its own words.

use std::time::Duration;

use serde::Deserialize;
use tilo_net::Client;

use crate::SiteError;

/// How long establishing a connection to a site may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a site's entry, its directory or its entry may take to answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest `entry.json` read, and the largest an entry may say.
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The largest directory read: a few hundred listings with their faces.
pub const MAX_DIRECTORY_BYTES: usize = 1024 * 1024;

/// The client the reads and joins are made over: a page's `fetch`, or the
/// desktop's pool of connections, which every read shares.
///
/// # Errors
/// The TLS and HTTP stack could not be initialised.
pub fn client() -> Result<Client, SiteError> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| {
            said(
                "an HTTP client for a site could not be built",
                error,
                "directory-error-unreachable",
            )
        })
}

/// An origin a site or host is reached at: HTTPS, or plain HTTP on this
/// machine's loopback only (a host under development). A `*.localhost` name
/// is loopback too, as browsers and resolvers take it: the local tilo.sh
/// serves its site as `tilo.localhost`.
pub fn web_origin(text: &str) -> Result<url::Url, SiteError> {
    let url = url::Url::parse(text).map_err(|_| "that is not a web address")?;
    let loopback = url.host_str().is_some_and(|host| {
        matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost")
    });
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("that is not a web address".into()),
    }
    if url.cannot_be_a_base() || url.host_str().is_none() {
        return Err("that is not a web address".into());
    }
    Ok(url)
}

/// The entry a person typed: a site's address, with or without its scheme
/// and path. The route of a link is not part of it.
pub fn site_entry(text: &str) -> Result<url::Url, SiteError> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err("that is not a web address".into());
    }
    let with_scheme = if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    let mut entry = web_origin(&with_scheme)?;
    entry.set_fragment(None);
    entry.set_query(None);
    // `entry.json` sits beside the entry page: a path names a directory.
    if !entry.path().ends_with('/') {
        let path = format!("{}/", entry.path());
        entry.set_path(&path);
    }
    Ok(entry)
}

/// The person a join is made as: the key an entry admits, and the name a
/// workspace learns of them. The caller supplies both: the desktop's own
/// identity here, the browser's there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// The person's root key, which this device is certified as.
    pub user: [u8; 32],
    /// The name they go by, never their device's.
    pub name: String,
}

/// A workspace a join chose, and the way this device comes into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    /// The workspace joined.
    pub workspace: String,
    /// How this device comes into it.
    pub way: Way,
}

/// How a device comes into the workspace a join chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Way {
    /// An invitation for this person, in its text (pasted, carried in a link,
    /// or fetched from the host a short link names), redeemed at the host it
    /// names.
    Invitation(String),
    /// A native link whose signed invitation still has to be fetched by the device.
    Native(tilo_workspace_join::native::InvitationLink),
}

impl Granted {
    /// The invitation this device redeems, when it holds one.
    #[must_use]
    pub fn invitation(&self) -> Option<&str> {
        match &self.way {
            Way::Invitation(text) => Some(text),
            Way::Native(_) => None,
        }
    }
}

#[derive(Deserialize)]
struct EntryJson {
    home: Option<HomeJson>,
    /// The site's directory of public workspaces.
    directory: Option<String>,
}

#[derive(Deserialize)]
struct HomeJson {
    #[serde(default)]
    name: String,
    host: HostJson,
    admission: Option<String>,
}

#[derive(Deserialize)]
struct HostJson {
    origin: String,
}

/// A site's home workspace, as its `entry.json` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteWorkspace {
    /// The site's entry: where its links point.
    pub entry: url::Url,
    /// What the site calls it.
    pub name: String,
    /// The host's admission entry, when it admits people without an
    /// invitation.
    pub admission: Option<url::Url>,
}

/// What a site's `entry.json` says: its home workspace and its directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteEntry {
    pub home: Option<SiteWorkspace>,
    pub directory: Option<url::Url>,
}

/// Read what the site at `entry` names: its home workspace and its directory
/// of public workspaces.
pub async fn read_site(client: &Client, entry: &url::Url) -> Result<SiteEntry, SiteError> {
    let config: EntryJson = get_json(
        client,
        &entry
            .join("entry.json")
            .map_err(|_| "that is not a web address")
            .map_err(str::to_owned)?,
    )
    .await?;
    let directory = config
        .directory
        .as_deref()
        .and_then(|directory| entry.join(directory).ok())
        .and_then(|directory| web_origin(directory.as_str()).ok());
    let home = match config.home {
        Some(home) => Some(home_workspace(entry, &home)?),
        None => None,
    };
    Ok(SiteEntry { home, directory })
}

/// The home workspace a site's `entry.json` names.
fn home_workspace(entry: &url::Url, home: &HomeJson) -> Result<SiteWorkspace, SiteError> {
    let origin = web_origin(&home.host.origin)?;
    let admission = home
        .admission
        .as_deref()
        .map(|path| {
            entry
                .join(path)
                .map_err(|_| "that is not a web address".to_owned())
        })
        .transpose()?;
    let name = if home.name.trim().is_empty() {
        origin.host_str().unwrap_or_default().to_owned()
    } else {
        home.name.trim().to_owned()
    };
    Ok(SiteWorkspace {
        entry: entry.clone(),
        name,
        admission,
    })
}

/// The public workspaces the directory at `directory` lists. Every directory
/// route is a `POST`, as a server serves nothing else.
pub async fn fetch_directory(
    client: &Client,
    directory: &url::Url,
) -> Result<tilo_server_protocol::directory::DirectoryPage, SiteError> {
    let response = client
        .post(directory.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|error| {
            said(
                "a directory could not be reached",
                error,
                "directory-error-unreachable",
            )
        })?;
    if !response.status().is_success() {
        return Err(said(
            "a directory answered with a status that is not success",
            response.status().as_u16(),
            "directory-error-status",
        ));
    }
    let bytes = response
        .bytes_limited(MAX_DIRECTORY_BYTES)
        .await
        .map_err(|error| {
            said(
                "a directory could not be read",
                error,
                "directory-error-unreadable",
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        said(
            "a directory is not readable",
            error,
            "directory-error-unreadable",
        )
    })
}

/// An invitation verified for `person`: the workspace it grants, and the text
/// the workspace's host admits this device by.
pub fn verify_invitation(text: &str, person: &Person) -> Result<Granted, SiteError> {
    let text = text.trim();
    let text = tilo_workspace_join::native_invitation_text(text).unwrap_or(text);
    let workspace = verify_workspace(text, person.user)?;
    Ok(Granted {
        workspace,
        way: Way::Invitation(text.to_owned()),
    })
}

/// The invitation `workspace` keeps at its host, the web origin `host`, under
/// `secret`, the `invite` a short link carries: its text, as every way of
/// joining takes one. A `secret` that is an invitation's own text is answered
/// as it came, so links that carry their invitation join the same way.
///
/// # Errors
/// Why there is none: the host could not be reached, or keeps no invitation
/// under that secret any more (spent, lapsed or taken back).
pub async fn kept_invitation(
    host: &str,
    workspace: &str,
    secret: &str,
) -> Result<String, SiteError> {
    if tilo_workspace_join::native::parse_bearer(secret).is_none() {
        return Ok(secret.to_owned());
    }
    let host = web_origin(&format!("{}/", host.trim_end_matches('/')))?;
    open_kept(&client()?, &host, workspace, secret).await
}

/// Ask the host at `host` for the invitation `workspace` keeps under
/// `secret` ([`tilo_server_protocol::web::OPEN_INVITATION_PATH`]): its text.
async fn open_kept(
    client: &Client,
    host: &url::Url,
    workspace: &str,
    secret: &str,
) -> Result<String, SiteError> {
    use tilo_server_protocol::web::{
        API_PREFIX, INVITATION_GONE, KeptInvitation, OPEN_INVITATION_PATH, OpenedInvitation,
    };
    let url = host
        .join(&format!("{API_PREFIX}{OPEN_INVITATION_PATH}"))
        .map_err(|_| "that is not a web address".to_owned())?;
    let response = client
        .post(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&KeptInvitation {
            workspace: workspace.to_owned(),
            secret: secret.to_owned(),
        })
        .send()
        .await
        .map_err(|error| {
            said(
                "the workspace's host could not be reached for its invitation",
                error,
                "directory-error-unreachable",
            )
        })?;
    let status = response.status().as_u16();
    if status == INVITATION_GONE {
        return Err(said(
            "the invitation a link names is gone",
            status,
            "directory-error-invitation-gone",
        ));
    }
    if !(200..300).contains(&status) {
        return Err(said(
            "the workspace's host did not answer with the invitation",
            status,
            "directory-error-status",
        ));
    }
    let bytes = response
        .bytes_limited(MAX_ANSWER_BYTES)
        .await
        .map_err(|error| {
            said(
                "the host's answer could not be read",
                error,
                "directory-error-unreadable",
            )
        })?;
    let opened: OpenedInvitation = serde_json::from_slice(&bytes).map_err(|error| {
        said(
            "the host answered with something that is not an invitation",
            error,
            "directory-error-unreadable",
        )
    })?;
    Ok(opened.invitation)
}

/// The workspace an invitation for this person joins, without redeeming it:
/// what a session the host set up for a workspace knows before the join
/// settles.
pub fn invited_workspace(text: &str, person: &Person) -> Result<String, SiteError> {
    // What a person is handed is a link, with the invitation in it; its own text is read alike.
    match classify(text)? {
        Pasted::Invitation { text, .. } => verify_workspace(&text, person.user),
        Pasted::Native(link) => Ok(link.reference.workspace.to_string()),
        // The invitation waits in the workspace the link names, at its host.
        Pasted::Kept { workspace, .. } => Ok(workspace.to_string()),
    }
}

/// The workspace an invitation grants `user`, verified for them.
fn verify_workspace(text: &str, user: [u8; 32]) -> Result<String, SiteError> {
    let verified = tilo_workspace_join::decode_and_verify_invitation_text(
        text.trim(),
        Some(user),
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| SiteError::Plain(format!("that invitation is not for you: {error}")))?;
    Ok(verified.grant().workspace_id.to_string())
}

/// What joining the pasted `text` got: the name the workspace goes by, when
/// what was pasted says it, and what it was granted.
pub struct Taken {
    pub name: String,
    pub granted: Result<Granted, SiteError>,
}

/// Join what the person pasted, as [`classify`] reads it: an invitation's
/// text is redeemed where it says, or the one a short link names is asked of its host.
/// Nothing is entered through a site: a link to a site or a workspace that
/// carries no invitation is no join.
pub async fn take(client: &Result<Client, SiteError>, text: &str, person: &Person) -> Taken {
    let pasted = match classify(text) {
        Ok(pasted) => pasted,
        Err(error) => {
            return Taken {
                name: String::new(),
                granted: Err(error),
            };
        }
    };
    match pasted {
        Pasted::Native(link) => Taken {
            name: String::new(),
            granted: Ok(Granted {
                workspace: link.reference.workspace.to_string(),
                way: Way::Native(link),
            }),
        },
        Pasted::Invitation { text, entry } => Taken {
            name: match (&entry, client) {
                (Some(entry), Ok(client)) => site_name(client, entry).await,
                _ => String::new(),
            },
            granted: verify_invitation(&text, person),
        },
        // The link names the invitation in its workspace, at its host, which
        // answers with it for the secret the link carries.
        Pasted::Kept {
            entry,
            host,
            workspace,
            secret,
        } => match client {
            Err(error) => Taken {
                name: host_of(&entry),
                granted: Err(error.clone()),
            },
            Ok(client) => Taken {
                name: site_name(client, &entry).await,
                granted: open_kept(client, &host, &workspace.to_string(), &secret)
                    .await
                    .and_then(|invitation| verify_invitation(&invitation, person)),
            },
        },
    }
}

/// What a site calls itself, for a workspace joined by its link: its own
/// name, else the host's.
async fn site_name(client: &Client, entry: &url::Url) -> String {
    read_site(client, entry)
        .await
        .ok()
        .and_then(|read| read.home)
        .map_or_else(|| host_of(entry), |home| home.name)
}

/// The host `entry` is reached at, for when a site does not say what it
/// calls itself.
fn host_of(entry: &url::Url) -> String {
    entry.host_str().unwrap_or_default().to_owned()
}

/// What the person pasted, as the join field reads it.
enum Pasted {
    Native(tilo_workspace_join::native::InvitationLink),
    /// An invitation's text, redeemed at the host it names; with the site the
    /// link it came in is under, when it came in a link.
    Invitation {
        text: String,
        entry: Option<url::Url>,
    },
    /// A short link: the invitation `workspace` keeps at `host`, named by the
    /// secret only its holder can derive, on the page of the site `entry`.
    Kept {
        entry: url::Url,
        host: url::Url,
        workspace: tilo_desktop_model::WorkspaceId,
        secret: String,
    },
}

/// Read what the person pasted: an invitation's text, which a link may carry,
/// or a short link naming the one its workspace keeps. A site or a workspace
/// link without one is no join.
fn classify(text: &str) -> Result<Pasted, SiteError> {
    let text = text.trim();
    if text.is_empty() {
        return Err("that is not a link or a code".into());
    }
    if text.starts_with(tilo_workspace_join::native::PREFIX) {
        return tilo_workspace_join::native::InvitationLink::parse(text)
            .map(Pasted::Native)
            .ok_or_else(|| "that native invitation link is malformed".into());
    }
    if let Some(text) = tilo_workspace_join::native_invitation_text(text) {
        return Ok(Pasted::Invitation {
            text: text.to_owned(),
            entry: None,
        });
    }
    if is_invitation(text) {
        return Ok(Pasted::Invitation {
            text: text.to_owned(),
            entry: None,
        });
    }
    if !text.contains('.') && !text.contains(':') && !text.contains('/') {
        return Err("that is not a link or a code".into());
    }
    if text.contains('#') {
        let entry = link_entry(text);
        if let Some(entry) = &entry
            && let Ok(web_entry) = tilo_reference::link::WebEntry::new(entry)
            && let Ok(link) = tilo_reference::link::Link::parse(
                text,
                &tilo_reference::link::WebEntries::new(vec![web_entry]),
            )
        {
            let entry = site_entry(entry)?;
            if link.secrets.pair.is_some() {
                return Err("that link pairs a device with a host, not a workspace".into());
            }
            let tilo_reference::link::Scope::Workspace { id: room, host, .. } = link.target.scope
            else {
                return Err(
                    "that link is for a site, not an invitation to one of its workspaces: ask \
                     someone in the workspace you want to join for an invite link"
                        .into(),
                );
            };
            if let Some(invitation) = &link.secrets.invite
                && tilo_workspace_join::native::parse_bearer(invitation.expose()).is_some()
            {
                return Ok(Pasted::Kept {
                    entry,
                    host: web_origin(&format!("{}/", host.as_str()))?,
                    workspace: tilo_desktop_model::WorkspaceId::from(room),
                    secret: invitation.expose().to_owned(),
                });
            }
            if let Some(invitation) = link.secrets.invite {
                return Ok(Pasted::Invitation {
                    text: invitation.expose().to_owned(),
                    entry: Some(entry),
                });
            }
            return Err(
                "that link names a workspace but carries no invitation: ask someone in it for \
                 an invite link"
                    .into(),
            );
        }
    }
    site_entry(text)?;
    Err(
        "that is a site, not an invitation: a workspace is joined with an invite link someone \
         in it sends you"
            .into(),
    )
}

/// Whether `text` is an invitation's text itself.
fn is_invitation(text: &str) -> bool {
    tilo_workspace_join::decode_and_verify_invitation_text(
        text,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .is_ok()
}

/// The entry of a web link: everything before its route, without the
/// full-tab `app/` segment.
fn link_entry(url: &str) -> Option<String> {
    let before = url.split_once('#').map_or(url, |(before, _)| before);
    let before = before.split_once('?').map_or(before, |(before, _)| before);
    let before = before.strip_suffix("app/").unwrap_or(before);
    before.ends_with('/').then(|| before.to_owned())
}

/// What a site answered when its entry or its directory was read, in the
/// sheet's own words: the protocol's detail (an error, a status code) goes to
/// the log for whoever reads it, never into what the person sees.
fn said(logged: &'static str, error: impl std::fmt::Display, shown: &'static str) -> SiteError {
    tracing::warn!(%error, "{logged}");
    SiteError::Said(shown)
}

/// Read `url` as JSON, at most [`MAX_ANSWER_BYTES`] of it.
async fn get_json<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &url::Url,
) -> Result<T, SiteError> {
    let response = client
        .get(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            said(
                "a site's entry could not be reached",
                error,
                "directory-error-unreachable",
            )
        })?;
    if !response.status().is_success() {
        return Err(said(
            "a site's entry was not there",
            response.status().as_u16(),
            "directory-error-no-workspace",
        ));
    }
    let bytes = response
        .bytes_limited(MAX_ANSWER_BYTES)
        .await
        .map_err(|error| {
            said(
                "a site's entry could not be read",
                error,
                "directory-error-unreadable",
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        said(
            "a site's entry is not readable",
            error,
            "directory-error-no-workspace",
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        reason = "a fixture that cannot mint its invitation or read its own reply has no fallback"
    )]

    use super::*;
    use test_http::{Reply, Responder};

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    /// Plain HTTP is a site on this machine only: its loopback, by address or
    /// by a `localhost` name; anywhere else a site is HTTPS.
    #[test]
    fn plain_http_is_a_site_on_this_machine_only() {
        for local in [
            "http://localhost:8080/",
            "http://127.0.0.1:8080/",
            "http://[::1]:8080/",
            "http://tilo.localhost:25120/",
        ] {
            assert!(web_origin(local).is_ok(), "{local}");
        }
        for elsewhere in [
            "http://tilo.sh/",
            "http://localhost.evil.example/",
            "ftp://localhost/",
        ] {
            assert!(web_origin(elsewhere).is_err(), "{elsewhere}");
        }
        assert!(web_origin("https://tilo.sh/").is_ok());
    }

    /// An invitation for `person` to [`WORKSPACE`], minted as a host mints
    /// one: a real signed invitation, kilobyte and all.
    fn invitation_for(person: [u8; 32]) -> String {
        let host = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
        let host_device = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
        let now = tilo_clock::unix_now_ns();
        let mut issuer = tilo_user_identity::DeviceCert {
            user_public_key: host.verifying_key().to_bytes(),
            device_public_key: host_device.verifying_key().to_bytes(),
            device_label: "The book club's host".to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: now,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut issuer, &host).expect("a certified host");
        let grant = tilo_workspace_join::InviteGrant {
            workspace_id: tilo_workspace_join::WorkspaceId::parse(WORKSPACE)
                .expect("a workspace's id"),
            issuer,
            role: tilo_workspace_join::Role::Viewer,
            nonce: [0x33; 16],
            recipient_user_public_key: Some(person),
            invitee_name: None,
            expires_at_ns: now.saturating_add(3_600_000_000_000),
            transport: Vec::new(),
        };
        let signed = tilo_workspace_join::issue_invitation(grant, &host_device)
            .expect("a signed invitation");
        tilo_workspace_join::encode_invitation_text(&signed)
    }

    #[test]
    fn a_native_link_uses_the_signed_invitation_without_a_site() {
        let person = Person {
            user: [0x21; 32],
            name: "Ada".to_owned(),
        };
        let text = invitation_for(person.user);
        let signed = tilo_workspace_join::invitation_bytes(&text).expect("the invitation's bytes");
        let link = tilo_workspace_join::native_invitation_link(&signed);
        let no_http = Err("no web client".into());
        let granted = tilo_rt::block_on(take(&no_http, &link, &person))
            .expect("the join task answers")
            .granted
            .expect("a join");
        assert_eq!(granted.workspace, WORKSPACE);
        assert_eq!(granted.invitation(), Some(text.as_str()));
        let other = Person {
            user: [0x42; 32],
            name: "Grace".to_owned(),
        };
        assert!(
            tilo_rt::block_on(take(&no_http, &link, &other))
                .expect("the join task answers")
                .granted
                .is_err()
        );
    }

    /// The secret a short link carries for `text`'s invitation.
    fn secret_of(text: &str) -> String {
        tilo_workspace_join::native::bearer_text(
            &tilo_workspace_join::invitation_bytes(text).expect("the invitation's bytes"),
        )
    }

    #[test]
    fn a_pasted_short_link_joins_with_the_invitation_its_workspace_keeps() {
        let person = Person {
            user: [0x24; 32],
            name: "Grace".to_owned(),
        };
        let text = invitation_for(person.user);
        let secret = secret_of(&text);
        let host = Responder::start(vec![
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(
                    serde_json::to_string(&tilo_server_protocol::web::OpenedInvitation {
                        invitation: text.clone(),
                    })
                    .expect("the host's answer"),
                ),
        ]);
        // The site whose page the link opens names itself by its host here.
        let site = Responder::start(vec![Reply::status(404)]);
        let link = format!(
            "http://127.0.0.1:{}/#/@127.0.0.1:{}/w/{WORKSPACE}?invite={secret}",
            site.port(),
            host.port()
        );
        assert_eq!(
            invited_workspace(&link, &person).expect("the link's workspace"),
            WORKSPACE,
            "the link names the workspace that keeps its invitation"
        );
        let taken = tilo_rt::block_on(take(&client(), &link, &person)).expect("the app runtime");
        assert_eq!(
            taken.name, "127.0.0.1",
            "the site the link opens at stands in for the workspace until it joins"
        );
        let granted = taken.granted.expect("a granted join");
        assert_eq!(granted.workspace, WORKSPACE);
        assert_eq!(
            granted.invitation(),
            Some(text.as_str()),
            "what the device joins with is the invitation the workspace keeps"
        );

        // The host was asked for exactly that invitation, by the secret alone.
        let asked = host.next_request();
        assert_eq!(asked.method, "POST");
        assert_eq!(asked.target, "/peer/v1/invitation/open");
        let body: tilo_server_protocol::web::KeptInvitation =
            serde_json::from_str(&asked.body_text()).expect("what the host was asked");
        assert_eq!(body.workspace, WORKSPACE);
        assert_eq!(body.secret, secret);

        // Someone else's device holding the link is not let in by it.
        let stranger = Person {
            user: [0x26; 32],
            name: "Bo".to_owned(),
        };
        let again = Responder::start(vec![
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(
                    serde_json::to_string(&tilo_server_protocol::web::OpenedInvitation {
                        invitation: text,
                    })
                    .expect("the host's answer"),
                ),
        ]);
        let site = Responder::start(vec![Reply::status(404)]);
        let link = format!(
            "http://127.0.0.1:{}/#/@127.0.0.1:{}/w/{WORKSPACE}?invite={secret}",
            site.port(),
            again.port()
        );
        assert!(
            tilo_rt::block_on(take(&client(), &link, &stranger))
                .expect("the app runtime")
                .granted
                .is_err(),
            "an invitation for someone else stays theirs"
        );
    }

    #[test]
    fn a_link_whose_invitation_was_taken_back_says_so() {
        let host = Responder::start(vec![Reply::status(
            tilo_server_protocol::web::INVITATION_GONE,
        )]);
        let secret = secret_of(&invitation_for([0x27; 32]));
        let refused = tilo_rt::block_on(kept_invitation(
            &format!("http://127.0.0.1:{}", host.port()),
            WORKSPACE,
            &secret,
        ))
        .expect("the app runtime")
        .expect_err("an invitation taken back");
        assert_eq!(
            refused,
            SiteError::Said("directory-error-invitation-gone"),
            "the person is told it lapsed or was taken back, not a status code"
        );
        // An invitation's own text asks no host at all.
        let text = invitation_for([0x27; 32]);
        assert_eq!(
            tilo_rt::block_on(kept_invitation("https://nowhere.example", WORKSPACE, &text))
                .expect("the app runtime")
                .expect("its own text"),
            text
        );
    }

    #[test]
    fn an_invitation_in_a_link_names_its_workspace_as_its_own_text_does() {
        let person = Person {
            user: [0x25; 32],
            name: "Ada".to_owned(),
        };
        let text = invitation_for(person.user);
        let link = format!("http://tilo.localhost:26320/#/w/{WORKSPACE}?invite={text}");
        assert_eq!(
            invited_workspace(&text, &person).expect("its text"),
            WORKSPACE
        );
        assert_eq!(
            invited_workspace(&link, &person).expect("its link"),
            WORKSPACE
        );
        let stranger = Person {
            user: [0x26; 32],
            name: "Bo".to_owned(),
        };
        assert!(
            invited_workspace(&link, &stranger).is_err(),
            "an invitation for someone else stays theirs, in a link too"
        );
        assert!(invited_workspace("http://tilo.localhost:26320/", &person).is_err());
    }
}
