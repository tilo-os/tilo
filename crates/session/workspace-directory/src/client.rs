//! Reading a site, and asking its door: the requests a directory makes, on
//! `tilo_net` so the desktop's connection pool and a page's `fetch` run the
//! same ones.
//!
//! A site's `entry.json` names its home workspace and its directory of public
//! ones; the directory then lists every public workspace it serves with what
//! it is, who is in it now and how lively it is
//! ([`tilo_relay_protocol::directory`]). Joining one asks its door to admit
//! this person ([`tilo_relay_protocol::directory::DoorAnswer`]) and verifies
//! the invitation the door answers with for them: the workspace that
//! invitation grants, and nothing the directory claims, is the workspace
//! joined ([`join`]).
//!
//! Every step of a join is logged at `info` under
//! [`tilo_startup_metrics::JOIN_FLOW_TARGET`], which the session's log keeps
//! whatever `RUST_LOG` says: the directory read, the door asked, the person
//! the invitation is presented for, the host's answer in its own words, the
//! workspace slot made for it, and that slot taken away with why — so a join
//! that lands nowhere says where it stopped, in the log the device keeps.

use std::time::Duration;

use serde::Deserialize;
use tilo_net::{Client, NetError};
use tilo_startup_metrics::JOIN_FLOW_TARGET;

/// How long establishing a connection to a site may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a site's entry, its directory or its door may take to answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest `entry.json` read, and the largest a door may say.
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The largest directory read: a few hundred listings with their faces.
pub const MAX_DIRECTORY_BYTES: usize = 1024 * 1024;

/// The client the reads and joins are made over: a page's `fetch`, or the
/// desktop's pool of connections, which every read shares.
///
/// # Errors
/// The TLS and HTTP stack could not be initialised.
pub fn client() -> Result<Client, String> {
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
/// machine's loopback only (a host under development).
pub fn web_origin(text: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(text).map_err(|_| "that is not a web address")?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
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
pub fn site_entry(text: &str) -> Result<url::Url, String> {
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

/// The person a join is made as: the key a door admits, and the name a
/// workspace learns of them. The caller supplies both: the desktop's own
/// identity here, the browser's there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// The person's root key, which this device is certified as.
    pub user: [u8; 32],
    /// The name they go by, never their device's.
    pub name: String,
}

/// What a door granted this person: the workspace it let them into, and the
/// invitation that admits this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    /// The workspace the door's invitation grants: the one joined.
    pub workspace: String,
    /// The invitation itself, which this device enrolls into the workspace's
    /// state with.
    pub invitation: String,
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
    /// The host's admission door, when it admits people without an
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
pub async fn read_site(client: &Client, entry: &url::Url) -> Result<SiteEntry, String> {
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
fn home_workspace(entry: &url::Url, home: &HomeJson) -> Result<SiteWorkspace, String> {
    let origin = web_origin(&home.host.origin)?;
    let admission = home
        .admission
        .as_deref()
        .map(|door| {
            entry
                .join(door)
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
/// route is a `POST`, as a relay serves nothing else.
pub async fn fetch_directory(
    client: &Client,
    directory: &url::Url,
) -> Result<tilo_relay_protocol::directory::DirectoryPage, String> {
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

/// Ask `door` to admit this person, and verify the invitation it answers
/// with for them. The workspace that invitation grants is the workspace
/// joined: what a directory said about it is not what makes it so.
pub async fn join(client: &Client, door: &url::Url, person: &Person) -> Result<Granted, String> {
    let url = door
        .join("join")
        .map_err(|_| "that is not a web address".to_owned())?;
    let asking = Asking::of(&url, person);
    let (status, bytes) = asking.ask(client).await?;
    if !(200..300).contains(&status) {
        let reason = door_refusal(status, &bytes);
        asking.refused(status, &reason);
        return Err(reason);
    }
    let answer: tilo_relay_protocol::directory::DoorAnswer = serde_json::from_slice(&bytes)
        .map_err(|error| {
            let reason = format!("the workspace did not let you in: {error}");
            asking.refused(status, &reason);
            reason
        })?;
    asking.granted(&answer)
}

/// The home workspace of the community `site`, joined as `person`: the name
/// the site calls it, and what its door granted. `None` when the site serves
/// no home workspace of its own, which is nothing to join.
///
/// This is the door a listing's Join asks and the same verified invitation:
/// a community is joined as a public workspace is.
pub(crate) async fn join_home(
    client: &Client,
    site: &url::Url,
    person: &Person,
) -> Result<Option<(String, Granted)>, String> {
    let Some(home) = read_site(client, site).await?.home else {
        return Ok(None);
    };
    let Some(door) = home.admission else {
        return Ok(None);
    };
    let granted = join(client, &door, person).await?;
    Ok(Some((home.name, granted)))
}

/// An invitation verified for `person`: the workspace it grants, and the text
/// the workspace's host admits this device by.
pub fn verify_invitation(text: &str, person: &Person) -> Result<Granted, String> {
    let workspace = verify_workspace(text, person.user)?;
    Ok(Granted {
        workspace,
        invitation: text.trim().to_owned(),
    })
}

/// The workspace an invitation for this person joins, without redeeming it:
/// what a session the host set up for a workspace knows before the join
/// settles.
pub fn invited_workspace(text: &str, person: &Person) -> Result<String, String> {
    verify_workspace(text, person.user)
}

/// The workspace an invitation grants `user`, verified for them.
fn verify_workspace(text: &str, user: [u8; 32]) -> Result<String, String> {
    let verified = tilo_workspace_join::decode_and_verify_invitation_text(
        text.trim(),
        Some(user),
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| format!("that invitation is not for you: {error}"))?;
    let workspace = verified.grant().room.clone();
    tilo_desktop_model::WorkspaceId::parse(&workspace)
        .ok_or_else(|| "that invitation names an invalid workspace".to_owned())?;
    Ok(workspace)
}

/// One join as the log keeps it: the door asked and the person it asks as.
struct Asking<'a> {
    url: &'a url::Url,
    person: &'a Person,
    person_hex: String,
}

impl<'a> Asking<'a> {
    fn of(url: &'a url::Url, person: &'a Person) -> Self {
        let asking = Self {
            url,
            person,
            person_hex: hex::encode(person.user),
        };
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            door = %asking.url,
            person = %asking.person_hex,
            name = %person.name,
            "join: asking the door to admit this person"
        );
        asking
    }

    /// Ask the door, and read what it answered.
    async fn ask(&self, client: &Client) -> Result<(u16, Vec<u8>), String> {
        let response = client
            .post(self.url.as_str())
            .timeout(ANSWER_TIMEOUT)
            .json(&serde_json::json!({
                "userPublicKey": hex::encode(self.person.user),
                "displayName": self.person.name,
            }))
            .send()
            .await
            .map_err(|error| {
                let reason = said(
                    "a door could not be reached",
                    error,
                    "directory-error-unreachable",
                );
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    door = %self.url,
                    person = %self.person_hex,
                    %reason,
                    outcome = "door unreachable",
                    "join: the door could not be reached"
                );
                reason
            })?;
        let status = response.status();
        let bytes = response
            .bytes_limited(MAX_ANSWER_BYTES)
            .await
            .map_err(|error| {
                let reason = match error {
                    NetError::TooLarge { .. } => said(
                        "a door answered more than a door says",
                        error,
                        "directory-error-unreadable",
                    ),
                    _ => said(
                        "a door could not be read",
                        error,
                        "directory-error-unreachable",
                    ),
                };
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    door = %self.url,
                    person = %self.person_hex,
                    status = status.as_u16(),
                    %reason,
                    outcome = "refused",
                    "join: the door's answer could not be read"
                );
                reason
            })?;
        Ok((status.as_u16(), bytes.to_vec()))
    }

    /// The door refused: keep its reason, in its own words, in the log.
    fn refused(&self, status: u16, reason: &str) {
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            door = %self.url,
            person = %self.person_hex,
            status,
            %reason,
            outcome = "refused",
            "join: the door refused"
        );
    }

    /// The door let this person in: verify its invitation for them, and
    /// answer what it granted.
    fn granted(
        &self,
        answer: &tilo_relay_protocol::directory::DoorAnswer,
    ) -> Result<Granted, String> {
        let workspace =
            verify_workspace(&answer.invitation, self.person.user).map_err(|error| {
                let reason = format!("the workspace did not let you in: {error}");
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    door = %self.url,
                    person = %self.person_hex,
                    %reason,
                    outcome = "invitation refused",
                    "join: the invitation the door answered with is not for this person"
                );
                reason
            })?;
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            door = %self.url,
            workspace = %workspace,
            person = %self.person_hex,
            outcome = "granted",
            "join: the door granted this person an invitation to its workspace"
        );
        Ok(Granted {
            workspace,
            invitation: answer.invitation.clone(),
        })
    }
}

/// What a door that refused said, in the person's words: its own reason when
/// it gave one, a sentence of the sheet's when it did not, and the site
/// being unreachable when the host failed rather than refused.
fn door_refusal(status: u16, bytes: &[u8]) -> String {
    let refusal = serde_json::from_slice::<DoorRefusal>(bytes)
        .inspect_err(|error| {
            tracing::warn!(%error, status, "a door's refusal does not read back");
        })
        .ok()
        .and_then(|refusal| refusal.error)
        .unwrap_or_else(|| tilo_sheets::word("directory-error-status"));
    // A 5xx is a host that is full or failing: worth trying again.
    if (500..600).contains(&status) {
        said(
            "a door's site could not be reached",
            &refusal,
            "directory-error-unreachable",
        )
    } else {
        // The door said why in its own words: those are the ones a person
        // reads.
        refusal
    }
}

/// A door's refusal, when it says why.
#[derive(Deserialize)]
struct DoorRefusal {
    error: Option<String>,
}

/// What joining the pasted `text` got: the name the workspace goes by, when
/// what was pasted says it, and what it was granted.
pub(crate) struct Taken {
    pub(crate) name: String,
    pub(crate) granted: Result<Granted, String>,
}

/// Join what the person pasted, as [`classify`] reads it: an invitation's
/// text is redeemed where it says; a site or a workspace link is joined
/// through the door of the workspace it names, on the site it names.
pub(crate) async fn take(client: &Result<Client, String>, text: &str, person: &Person) -> Taken {
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
        Pasted::Invitation { text, entry } => Taken {
            name: match (&entry, client) {
                (Some(entry), Ok(client)) => site_name(client, entry).await,
                _ => String::new(),
            },
            granted: verify_invitation(&text, person),
        },
        Pasted::Site { entry, workspace } => match client {
            Err(error) => Taken {
                name: host_of(&entry),
                granted: Err(format!("no network to join over: {error}")),
            },
            Ok(client) => match read_site(client, &entry).await {
                Ok(read) => {
                    let name = read
                        .home
                        .as_ref()
                        .map_or_else(|| host_of(&entry), |home| home.name.clone());
                    let granted = match read.home.and_then(|home| home.admission) {
                        Some(door) => join(client, &door, person)
                            .await
                            .and_then(|granted| match workspace {
                                Some(workspace) if workspace.to_string() != granted.workspace => {
                                    Err(
                                        "that link is for another workspace on this site: ask whoever shared it for an invitation"
                                            .to_owned(),
                                    )
                                }
                                _ => Ok(granted),
                            }),
                        None => Err(said(
                            "a site's home workspace has no door",
                            &entry,
                            "directory-error-no-workspace",
                        )),
                    };
                    Taken { name, granted }
                }
                Err(error) => Taken {
                    name: host_of(&entry),
                    granted: Err(error),
                },
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
    /// An invitation's text, redeemed at the host it names; with the site the
    /// link it came in is under, when it came in a link.
    Invitation {
        text: String,
        entry: Option<url::Url>,
    },
    /// A site, whose home workspace its door admits people to; with the workspace
    /// a link named, which must be that workspace.
    Site {
        entry: url::Url,
        workspace: Option<tilo_desktop_model::WorkspaceId>,
    },
}

/// Read what the person pasted: an invitation's text, a workspace link, or a
/// site.
fn classify(text: &str) -> Result<Pasted, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("that is not a link or a code".to_owned());
    }
    if is_invitation(text) {
        return Ok(Pasted::Invitation {
            text: text.to_owned(),
            entry: None,
        });
    }
    if !text.contains('.') && !text.contains(':') && !text.contains('/') {
        return Err("that is not a link or a code".to_owned());
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
                return Err("that link pairs a device with a host, not a workspace".to_owned());
            }
            let tilo_reference::link::Scope::Room { id: room, .. } = link.target.scope else {
                return Ok(Pasted::Site {
                    entry,
                    workspace: None,
                });
            };
            let workspace = tilo_desktop_model::WorkspaceId::from(room);
            if let Some(invitation) = link.secrets.invite {
                return Ok(Pasted::Invitation {
                    text: invitation.expose().to_owned(),
                    entry: Some(entry),
                });
            }
            return Ok(Pasted::Site {
                entry,
                workspace: Some(workspace),
            });
        }
    }
    Ok(Pasted::Site {
        entry: site_entry(text)?,
        workspace: None,
    })
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
fn said(logged: &'static str, error: impl std::fmt::Display, shown: &str) -> String {
    tracing::warn!(%error, "{logged}");
    tilo_sheets::word(shown)
}

/// Read `url` as JSON, at most [`MAX_ANSWER_BYTES`] of it.
async fn get_json<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &url::Url,
) -> Result<T, String> {
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
