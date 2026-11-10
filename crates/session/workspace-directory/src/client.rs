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
    /// The one-time token a page redeems the invitation with at the door's
    /// host; empty when the invitation came without a door (a pasted link).
    pub admission_token: String,
    /// Where a page redeems it; the door's own origin when absent.
    pub origin: Option<String>,
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
        admission_token: String::new(),
        origin: None,
    })
}

/// Seal `invitation`, the signed bytes of one, under a fresh key and leave it
/// on `site`'s shelf until `expires_at_ns`, naming `host`, the origin it
/// admits into: the short link a person hands over
/// (`https://<site>/i/<handle>#<key>`) and what takes it back. The desk's
/// sheet ([`crate::Store::shelve`]) and a door that invites for a person
/// leave it the same way.
///
/// # Errors
/// No randomness to seal with, or why the site kept nothing.
pub async fn shelve(
    client: &Client,
    site: &url::Url,
    host: Option<&str>,
    invitation: &[u8],
    expires_at_ns: u64,
) -> Result<crate::Shelved, String> {
    let sealed = tilo_workspace_join::sealed::seal(invitation)?;
    let link = leave(client, site, host, &sealed, expires_at_ns).await?;
    Ok(crate::Shelved {
        link,
        site: site.clone(),
        handle: sealed.handle,
        delete: sealed.delete,
    })
}

/// Leave `sealed` on `site`'s shelf, for the link that names it and carries
/// its key: what a person hands over instead of the invitation's kilobytes.
///
/// The site is given the ciphertext, when the invitation lapses, and the
/// secret that takes it back — never the key, which is in the link alone.
async fn leave(
    client: &Client,
    site: &url::Url,
    host: Option<&str>,
    sealed: &tilo_workspace_join::sealed::Sealed,
    expires_at_ns: u64,
) -> Result<String, String> {
    let url = site
        .join(&format!("i/{}", sealed.handle))
        .map_err(|_| "that is not a web address".to_owned())?;
    let stored = tilo_relay_protocol::sealed_invitations::Stored {
        sealed: sealed.sealed_text(),
        expires_at_ns,
        delete: sealed.delete.clone(),
        host: host.map(str::to_owned),
    };
    let response = client
        .post(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&stored)
        .send()
        .await
        .map_err(|error| {
            said(
                "a site's shelf could not be reached",
                error,
                "directory-error-unreachable",
            )
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(said(
            "a site did not keep the invitation",
            status,
            "directory-error-status",
        ));
    }
    sealed.link(site.as_str())
}

/// Take `sealed` back off `site`'s shelf, with the secret it was left with:
/// the link admits nobody from now on.
///
/// # Errors
/// Why the site did not take it back: it could not be reached, refused, or
/// holds nothing there any more.
pub(crate) async fn take_back(
    client: &Client,
    site: &url::Url,
    handle: &str,
    delete: &str,
) -> Result<(), String> {
    let url = site
        .join(&format!("i/{handle}"))
        .map_err(|_| "that is not a web address".to_owned())?;
    let response = client
        .delete(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&tilo_relay_protocol::sealed_invitations::TakeBack {
            delete: delete.to_owned(),
        })
        .send()
        .await
        .map_err(|error| {
            said(
                "a site's shelf could not be reached",
                error,
                "directory-error-unreachable",
            )
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(said(
            "a site did not take the invitation back",
            status,
            "directory-error-status",
        ));
    }
    Ok(())
}

/// The invitation a sealed `link` names, fetched from the site that keeps it
/// and opened with the key the link carries: its text, as every build hands
/// one over, and the origin of the host it admits into where the inviter
/// named one.
///
/// # Errors
/// Why there is none: the site could not be reached, holds nothing under the
/// handle any more, or the key does not open what it answered with.
pub(crate) async fn redeem(
    client: &Client,
    link: &tilo_workspace_join::sealed::SealedLink,
) -> Result<SealedInvitation, String> {
    let site = web_origin(&format!("{}/", link.site))?;
    let url = site
        .join(&format!("i/{}", link.handle))
        .map_err(|_| "that is not a web address".to_owned())?;
    let response = client
        .get(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            said(
                "a site's shelf could not be reached",
                error,
                "directory-error-unreachable",
            )
        })?;
    let status = response.status().as_u16();
    if status == tilo_relay_protocol::sealed_invitations::GONE {
        return Err(said(
            "a sealed invitation is gone",
            status,
            "directory-error-invitation-gone",
        ));
    }
    if !(200..300).contains(&status) {
        return Err(said(
            "a site did not answer with the invitation",
            status,
            "directory-error-status",
        ));
    }
    let bytes = response
        .bytes_limited(MAX_ANSWER_BYTES)
        .await
        .map_err(|error| {
            said(
                "a site's answer could not be read",
                error,
                "directory-error-unreadable",
            )
        })?;
    let fetched: tilo_relay_protocol::sealed_invitations::Fetched = serde_json::from_slice(&bytes)
        .map_err(|error| {
            said(
                "a site answered with something that is not an invitation",
                error,
                "directory-error-unreadable",
            )
        })?;
    let invitation =
        tilo_workspace_join::sealed::open_sealed(&fetched.sealed, &link.handle, &link.key)
            .map_err(|error| {
                said(
                    "a sealed invitation did not open",
                    error,
                    "directory-error-invitation-broken",
                )
            })?;
    Ok(SealedInvitation {
        invitation: tilo_workspace_join::encode_invitation_text(&invitation),
        host: fetched.host,
    })
}

/// An invitation read off a site's shelf ([`redeem`]): its own text, and the
/// host it admits into.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedInvitation {
    /// The invitation's text, as every build hands one over.
    pub invitation: String,
    /// The origin of the host serving the workspace it admits into, where the
    /// device that left it named one: a link names the site that keeps the
    /// invitation, not the host it admits into.
    pub host: Option<String>,
}

/// The invitation the link's `secret` names, fetched from `site` and opened:
/// its text, as every build hands one over, and where it admits. A `secret`
/// that names no sealed invitation — an invitation's own text — is answered
/// as it came, with no host.
///
/// # Errors
/// Why there is none: the site could not be reached, holds nothing under the
/// handle any more, or the key does not open what it answered with.
pub async fn sealed_invitation(site: &str, secret: &str) -> Result<SealedInvitation, String> {
    let Some((handle, key)) = tilo_workspace_join::sealed::parse_ref(secret) else {
        return Ok(SealedInvitation {
            invitation: secret.to_owned(),
            host: None,
        });
    };
    let link = tilo_workspace_join::sealed::SealedLink {
        site: site.to_owned(),
        handle: handle.to_owned(),
        key: key.to_owned(),
    };
    redeem(&client()?, &link).await
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
            admission_token: answer.admission_token.clone(),
            origin: answer.origin.clone(),
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
        // The link carries a key and a place; the invitation itself waits at
        // the site, sealed under that key. The site's address names the join
        // until the workspace it brings is known.
        Pasted::Sealed { link } => Taken {
            name: url::Url::parse(&format!("{}/", link.site))
                .map(|site| host_of(&site))
                .unwrap_or_default(),
            granted: match client {
                Err(error) => Err(format!("no network to join over: {error}")),
                Ok(client) => redeem(client, &link)
                    .await
                    .and_then(|sealed| verify_invitation(&sealed.invitation, person)),
            },
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
    /// A sealed invitation's link: the site that keeps it, the handle it is
    /// under, and the key that opens it — which the site was never given.
    Sealed {
        link: tilo_workspace_join::sealed::SealedLink,
    },
    /// A site, whose home workspace its door admits people to; with the workspace
    /// a link named, which must be that workspace.
    Site {
        entry: url::Url,
        workspace: Option<tilo_desktop_model::WorkspaceId>,
    },
}

/// Read what the person pasted: a sealed link, an invitation's text, a
/// workspace link, or a site.
fn classify(text: &str) -> Result<Pasted, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("that is not a link or a code".to_owned());
    }
    if let Some(link) = tilo_workspace_join::sealed::parse_link(text) {
        return Ok(Pasted::Sealed { link });
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
            workspace_id: tilo_workspace_join::room_workspace_id(WORKSPACE)
                .expect("a workspace's id"),
            room: WORKSPACE.to_owned(),
            issuer,
            role: tilo_workspace_join::MemberRole::Viewer,
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

    /// `responder` as a site's origin, trailing slash and all.
    fn site(responder: &Responder) -> url::Url {
        url::Url::parse(&format!("{}/", responder.url())).expect("the site's address")
    }

    /// The host an invitation admits into.
    const HOST: &str = "https://host.example";
    /// The key a link's fragment carries.
    fn key_of(link: &str) -> String {
        link.rsplit_once('#')
            .expect("a link carries a key")
            .1
            .to_owned()
    }

    #[test]
    fn a_short_link_comes_back_as_the_invitation_it_was_sealed_from() {
        let person = Person {
            user: [0x21; 32],
            name: "Ada Lovelace".to_owned(),
        };
        let text = invitation_for(person.user);
        let bytes = tilo_workspace_join::invitation_bytes(&text).expect("the invitation's bytes");
        let sealed = tilo_workspace_join::sealed::seal(&bytes).expect("a sealed invitation");
        let responder = Responder::start(vec![
            Reply::status(204),
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(
                    serde_json::to_string(&tilo_relay_protocol::sealed_invitations::Fetched {
                        sealed: sealed.sealed_text(),
                        host: Some(HOST.to_owned()),
                    })
                    .expect("a shelf's answer"),
                ),
            Reply::status(204),
        ]);
        let site = site(&responder);
        let client = client().expect("a client");
        let now = tilo_clock::unix_now_ns();
        let (link, redeemed) = tilo_rt::block_on(async {
            let link = leave(
                &client,
                &site,
                Some(HOST),
                &sealed,
                now.saturating_add(3_600_000_000_000),
            )
            .await?;
            let read = tilo_workspace_join::sealed::parse_link(&link)
                .ok_or("the link the site answered with does not read back")?;
            let redeemed = redeem(&client, &read).await?;
            Ok::<_, String>((link, redeemed))
        })
        .expect("the app runtime")
        .expect("a shelf round trip");

        // What comes back is the signed invitation itself, so it verifies as
        // one: the same workspace, for the person it was granted to. The
        // host it admits into comes back with it, since the link names the
        // site that keeps it and not the host it joins.
        assert_eq!(
            redeemed.invitation, text,
            "the link carries the invitation byte for byte"
        );
        assert_eq!(redeemed.host.as_deref(), Some(HOST), "and where it admits");
        let granted =
            verify_invitation(&redeemed.invitation, &person).expect("the invitation verifies");
        assert_eq!(granted.workspace, WORKSPACE);
        assert_eq!(
            verify_invitation(&text, &person).expect("the invitation verifies"),
            granted
        );

        // The site was given ciphertext, when it lapses and the secret that
        // takes it back: never the key.
        let key = key_of(&link);
        let post = responder.next_request();
        assert_eq!(post.method, "POST");
        assert_eq!(post.target, format!("/i/{}", sealed.handle));
        let body = post.body_text();
        assert!(body.contains(&sealed.sealed_text()), "{body}");
        assert!(body.contains(&sealed.delete), "{body}");
        assert!(body.contains(&format!("\"{HOST}\"")), "{body}");
        assert!(!body.contains(&key), "the key stays in the link: {body}");
        let fetch = responder.next_request();
        assert_eq!(fetch.method, "GET");
        assert_eq!(fetch.target, format!("/i/{}", sealed.handle));
        assert!(
            !fetch.target.contains(&key),
            "a fragment is never sent: {}",
            fetch.target
        );
        assert!(
            !body.contains(&text),
            "the site never holds the invitation itself"
        );

        // Taking it back is the delete secret's alone: the link names nothing
        // from then on, and the key is not what asks for that.
        tilo_rt::block_on(take_back(&client, &site, &sealed.handle, &sealed.delete))
            .expect("the app runtime")
            .expect("the shelf takes it back");
        let taken = responder.next_request();
        assert_eq!(taken.method, "DELETE");
        assert_eq!(taken.target, format!("/i/{}", sealed.handle));
        let body = taken.body_text();
        assert!(body.contains(&sealed.delete), "{body}");
        assert!(!body.contains(&key), "the key takes nothing back: {body}");
    }

    #[test]
    fn a_link_whose_invitation_was_taken_back_says_so() {
        let responder = Responder::start(vec![Reply::status(
            tilo_relay_protocol::sealed_invitations::GONE,
        )]);
        let site = site(&responder);
        let sealed =
            tilo_workspace_join::sealed::seal(b"an invitation").expect("a sealed invitation");
        let link = sealed.link(site.as_str()).expect("the link");
        let read = tilo_workspace_join::sealed::parse_link(&link).expect("the link reads back");
        let client = client().expect("a client");
        let refused = tilo_rt::block_on(redeem(&client, &read))
            .expect("the app runtime")
            .expect_err("an invitation taken back");
        assert_eq!(
            refused,
            tilo_sheets::word("directory-error-invitation-gone"),
            "the person is told it lapsed or was taken back, not a status code"
        );
    }

    #[test]
    fn a_pasted_short_link_joins_through_its_site() {
        let person = Person {
            user: [0x24; 32],
            name: "Grace".to_owned(),
        };
        let text = invitation_for(person.user);
        let sealed = tilo_workspace_join::sealed::seal(
            &tilo_workspace_join::invitation_bytes(&text).expect("the invitation's bytes"),
        )
        .expect("a sealed invitation");
        let responder = Responder::start(vec![
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(
                    serde_json::to_string(&tilo_relay_protocol::sealed_invitations::Fetched {
                        sealed: sealed.sealed_text(),
                        host: None,
                    })
                    .expect("a shelf's answer"),
                ),
        ]);
        let link = sealed.link(site(&responder).as_str()).expect("the link");
        let client = client().expect("a client");
        let taken = tilo_rt::block_on(take(&Ok(client), &link, &person)).expect("the app runtime");
        assert_eq!(
            taken.name, "127.0.0.1",
            "the site the link names stands in for the workspace until it joins"
        );
        let granted = taken.granted.expect("a granted join");
        assert_eq!(granted.workspace, WORKSPACE);
        assert_eq!(
            granted.invitation, text,
            "what the sheet joins with is the invitation the site never read"
        );
    }
}
