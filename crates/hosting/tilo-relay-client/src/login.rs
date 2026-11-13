//! Signing a computer in as one of a person's devices, and the two ways into
//! the workspace with it.
//!
//! A page opened a one-time pairing invite and copied a link
//! ([`PairingLink`]). This computer makes its own device key, asks the
//! workspace's host to have the person's root certify it ([`sign_in`]), and is
//! then a device of theirs: a member for thirty days, the host pinned, no root
//! anywhere here.
//!
//! Reading the code and signing in are a computer's work: a page writes the
//! code and never reads one. The ways into the workspace a signed-in device
//! takes — its site's door ([`Door`]) and an invitation, both entered on the
//! device's session ([`tilo_relay_protocol::entering`]) — are a page's as much
//! as a computer's, so only the first part is native.

#[cfg(not(target_arch = "wasm32"))]
use tilo_reference::link::{Link, Mode, Scope, WebEntries, WebEntry};
#[cfg(not(target_arch = "wasm32"))]
use tilo_user_identity::DeviceCert;
#[cfg(not(target_arch = "wasm32"))]
use uuid::Uuid;

#[cfg(not(target_arch = "wasm32"))]
use crate::error::CertifyError;
#[cfg(not(target_arch = "wasm32"))]
use crate::host::Host;
#[cfg(not(target_arch = "wasm32"))]
use crate::https;
#[cfg(not(target_arch = "wasm32"))]
use crate::https::HttpClient;
#[cfg(not(target_arch = "wasm32"))]
use crate::signer::DeviceSigner;

/// A person's user root, hex, in `?pair=`.
#[cfg(not(target_arch = "wasm32"))]
const PAIR_USER_KEY_HEX: usize = 64;
/// The page's one-time invite, hex, after the user root.
#[cfg(not(target_arch = "wasm32"))]
const PAIR_INVITE_HEX: usize = 32;

/// A login code, read.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingLink {
    /// The page that wrote the link (`https://tilo.sh/`).
    pub site: String,
    /// The workspace's host (`https://eu1.tilo.sh`).
    pub host_origin: String,
    /// The workspace the person is in on that page.
    pub workspace: Uuid,
    /// The person whose new device this computer would become.
    pub user_public_key: [u8; 32],
    /// The one-time invite the page opened, which its own tab answers:
    /// `None` when the link names a person but no invite.
    pub invite: Option<[u8; 16]>,
}

#[cfg(not(target_arch = "wasm32"))]
impl PairingLink {
    /// Read `url` as a login code.
    ///
    /// # Errors
    ///
    /// One sentence saying what the link is missing or damaged, and what to
    /// ask the page for instead.
    pub fn parse(url: &str) -> Result<Self, String> {
        let (site, entry) = entry(url)?;
        let link = Link::parse(url, &WebEntries::new(vec![entry]))
            .map_err(|error| format!("{error}. Ask the page for a login code."))?;
        if link.mode != Mode::Desktop {
            return Err("This link opens one app, not a workspace. Open the page's \
                        \"Set up with your agent\" and copy the link it gives you."
                .to_owned());
        }
        let Scope::Room { host, id, .. } = link.target.scope.clone() else {
            return Err(
                "This link opens a person's own desktop, not a workspace. Ask the page \
                        for a login code for a workspace."
                    .to_owned(),
            );
        };
        let target = &link.target;
        if target.app.is_some()
            || target.record.is_some()
            || target.resource.is_some()
            || target.view.is_some()
            || target.discuss.is_some()
        {
            return Err(
                "This link opens something inside Tilo, not a login. Ask the page for \
                        a login code."
                    .to_owned(),
            );
        }
        let secrets = &link.secrets;
        if secrets.invite.is_some()
            || secrets.ticket.is_some()
            || secrets.token.is_some()
            || secrets.install.is_some()
        {
            return Err(
                "This link carries something else this computer cannot use. Ask the page \
                        for a login code."
                    .to_owned(),
            );
        }
        let pair = secrets.pair.as_ref().ok_or_else(|| {
            "This link has no pairing code, so it logs nothing in. Ask the page for a sign-in \
             link."
                .to_owned()
        })?;
        let (user_public_key, invite) = pair_of(pair.expose())?;
        Ok(Self {
            site,
            host_origin: host.as_str().to_owned(),
            workspace: id,
            user_public_key,
            invite,
        })
    }

    /// The workspace as its host names it: the id in the link, hyphenated.
    #[must_use]
    pub fn workspace_id(&self) -> String {
        self.workspace.hyphenated().to_string()
    }
}

/// The page a link was written on, and its entry: everything before its `#`,
/// minus a query the site may add. A login code is read without knowing the
/// site in advance, which is the point: any Tilo page can hand one out.
#[cfg(not(target_arch = "wasm32"))]
fn entry(url: &str) -> Result<(String, WebEntry), String> {
    let refused = || {
        "This is not a web address. Copy the whole link the page gives you, like \
         https://tilo.sh/#/w/<workspace>?pair=<code>."
            .to_owned()
    };
    let before_route = url.split_once('#').map_or(url, |(before, _)| before);
    if before_route.is_empty() {
        return Err(refused());
    }
    let base = before_route
        .split_once('?')
        .map_or(before_route, |(base, _)| base);
    let base = if base.ends_with('/') {
        base.to_owned()
    } else {
        format!("{base}/")
    };
    let entry = WebEntry::new(&base).map_err(|_| refused())?;
    Ok((base, entry))
}

/// The user root and the invite the page wrote as one hex string.
#[cfg(not(target_arch = "wasm32"))]
fn pair_of(pair: &str) -> Result<([u8; 32], Option<[u8; 16]>), String> {
    let damaged = || {
        "The pairing code in this link is damaged. Ask the page for a new login code.".to_owned()
    };
    let lowercase_hex = pair
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let (key, invite) = match pair.len() {
        PAIR_USER_KEY_HEX if lowercase_hex => (pair, None),
        length if length == PAIR_USER_KEY_HEX + PAIR_INVITE_HEX && lowercase_hex => {
            let (key, invite) = pair.split_at(PAIR_USER_KEY_HEX);
            (key, Some(invite))
        }
        _ => return Err(damaged()),
    };
    let mut user_public_key = [0u8; 32];
    hex::decode_to_slice(key, &mut user_public_key).map_err(|_| damaged())?;
    let invite = match invite {
        None => None,
        Some(invite) => {
            let mut bytes = [0u8; 16];
            hex::decode_to_slice(invite, &mut bytes).map_err(|_| damaged())?;
            Some(bytes)
        }
    };
    Ok((user_public_key, invite))
}

/// A workspace's public door, as the site names it (`entry.json`'s home
/// `admission`): its path, which names the workspace it lets people into. A
/// device enters through it on its session with the workspace's host
/// ([`tilo_relay_protocol::entering`]).
pub struct Door {
    /// The door's path, below its origin (`…/directory/<workspace>/`).
    pub path: String,
}

impl Door {
    /// The workspace the door lets people into, as its path names it.
    #[must_use]
    pub fn workspace(&self) -> Option<String> {
        let mut segments = self.path.split('/').filter(|segment| !segment.is_empty());
        segments.find(|segment| *segment == "directory")?;
        segments.next().map(str::to_owned)
    }
}

/// The door of the site whose `entry.json` is `entry` to the workspace it
/// keeps on the host at `host_origin`: its origin and path. Only HTTPS, but
/// for a door on loopback.
#[must_use]
pub fn site_door(entry: &serde_json::Value, host_origin: &str) -> Option<(String, String)> {
    let home = entry.get("home")?;
    let home_host = home.pointer("/host/origin")?.as_str()?;
    if home_host.trim_end_matches('/') != host_origin.trim_end_matches('/') {
        return None;
    }
    let admission = home.get("admission")?.as_str()?;
    let (scheme, rest) = admission.split_once("://")?;
    let (authority, path) = rest.split_at(rest.find('/')?);
    let secure = tilo_net::Url::parse(admission).is_ok_and(|url| tilo_net::secure_origin(&url));
    if authority.is_empty() || !secure {
        return None;
    }
    Some((format!("{scheme}://{authority}"), path.to_owned()))
}

/// The door of the site at `site` to its workspace on the host at
/// `host_origin`, as its `entry.json` names it. A computer reads a site's
/// `entry.json` through its own HTTPS client; a page's entry names its door.
#[cfg(not(target_arch = "wasm32"))]
pub async fn door_at(site: &https::Origin, host_origin: &str) -> Option<Door> {
    let (status, body) = site.get("/entry.json").await.ok()?;
    if status != 200 {
        return None;
    }
    let entry: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let (_, path) = site_door(&entry, host_origin)?;
    Some(Door { path })
}

/// A computer that is now one of the person's devices.
#[cfg(not(target_arch = "wasm32"))]
pub struct SignIn {
    /// The link it was signed in with.
    pub link: PairingLink,
    /// The agent that runs here, as the person's page names it.
    pub agent: String,
    /// This device's label, as the person's devices list it.
    pub label: String,
    /// Where the workspace's host is reached.
    pub host: Host,
    /// This device's certificate: a member of the person's, for thirty days.
    pub cert: DeviceCert,
    /// The host's certificate, pinned.
    pub host_cert: DeviceCert,
}

/// Ask `link`'s host to have the person's root certify `signer`'s device as
/// the agent `agent` on the computer labeled `label`. `on_pending` receives
/// the pairing code to show while the person's other device answers.
///
/// # Errors
///
/// The person declined or nobody answered in time ([`CertifyError::Expired`]),
/// the host was not reached or refused, or what it issued is not a certificate
/// for this device.
#[cfg(not(target_arch = "wasm32"))]
pub async fn sign_in(
    client: &dyn HttpClient,
    link: &PairingLink,
    agent: &str,
    label: &str,
    signer: &dyn DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<SignIn, CertifyError> {
    let info = https::host_info(client).await?;
    let host = match &info.address {
        Some(address) => Host::published(address),
        None => {
            return Err(CertifyError::Denied(
                "This workspace's host takes no device connections, so no computer can sign in to it."
                    .to_owned(),
            ));
        }
    };
    let invite = link.invite.unwrap_or([0; 16]);
    let (cert, host_cert) = crate::certify::certify(
        &host,
        link.user_public_key,
        label,
        invite,
        signer,
        on_pending,
    )
    .await?;
    Ok(SignIn {
        link: link.clone(),
        agent: agent.to_owned(),
        label: label.to_owned(),
        host,
        cert,
        host_cert,
    })
}

/// What a workspace said as it admitted a device.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Admission {
    /// The workspace's UUID.
    pub workspace: String,
    /// Its relay workspace.
    pub workspace_id: u64,
    /// Its name.
    pub name: String,
    /// What the device's person may do there.
    pub role: tilo_relay_protocol::workspaces::WorkspaceRole,
}

#[cfg(test)]
mod tests {
    use super::PairingLink;

    /// The workspace every test login code opens.
    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    fn key(seed: u8) -> String {
        hex::encode([seed; 32])
    }

    fn invite(seed: u8) -> String {
        hex::encode([seed; 16])
    }

    /// The link the page writes: its workspace on its own host, with the
    /// person's root and the invite it opened.
    #[test]
    fn reads_the_link_the_page_writes() {
        let user = key(0xab);
        let pair = format!("{user}{}", invite(0x7f));
        let link = PairingLink::parse(&format!(
            "https://tilo.sh/#/@eu1.tilo.sh/w/{WORKSPACE}?pair={pair}"
        ))
        .unwrap();
        assert_eq!(link.site, "https://tilo.sh/");
        assert_eq!(link.host_origin, "https://eu1.tilo.sh");
        assert_eq!(link.workspace_id(), WORKSPACE);
        assert_eq!(link.user_public_key, [0xab; 32]);
        assert_eq!(link.invite, Some([0x7f; 16]));
    }

    /// A page that hosts its own workspaces writes no host, and a workspace
    /// it names by its readable name too: both are the same link.
    #[test]
    fn reads_a_link_whose_page_is_the_host_and_that_names_the_workspace() {
        let pair = format!("{}{}", key(1), invite(2));
        let own = PairingLink::parse(&format!("https://eu1.tilo.sh/#/w/{WORKSPACE}?pair={pair}"))
            .unwrap();
        assert_eq!(own.site, "https://eu1.tilo.sh/");
        assert_eq!(own.host_origin, "https://eu1.tilo.sh");
        let named = PairingLink::parse(&format!(
            "https://tilo.sh/#/@eu1.tilo.sh/w/lounge~{WORKSPACE}?pair={pair}"
        ))
        .unwrap();
        assert_eq!(named.workspace_id(), WORKSPACE);
    }

    /// A link a person's own page wrote for a person it already knows carries
    /// no invite: their device approves the request itself.
    #[test]
    fn reads_a_link_with_the_person_and_no_invite() {
        let link = PairingLink::parse(&format!("https://tilo.sh/#/w/{WORKSPACE}?pair={}", key(3)))
            .unwrap();
        assert_eq!(link.user_public_key, [3; 32]);
        assert_eq!(link.invite, None);
    }

    /// Everything that is not exactly such a link is refused, with one
    /// sentence, never repaired.
    #[test]
    fn refuses_anything_else() {
        let user = key(4);
        let pair = format!("{user}{}", invite(5));
        let workspace = format!("https://tilo.sh/#/w/{WORKSPACE}");
        let refused = [
            // Not a link at all.
            String::new(),
            "hello".to_owned(),
            "file:///etc/passwd#/w/x".to_owned(),
            "javascript:alert(1)".to_owned(),
            format!("ftp://tilo.sh/#/w/{WORKSPACE}?pair={pair}"),
            // Plain http, which only this computer's own pages may use.
            format!("http://tilo.sh/#/w/{WORKSPACE}?pair={pair}"),
            // No workspace, or no pairing code.
            "https://tilo.sh/".to_owned(),
            "https://tilo.sh/#/me".to_owned(),
            workspace.clone(),
            format!("{workspace}?pair="),
            // A code of the wrong shape.
            format!("{workspace}?pair={}", "ab"),
            format!("{workspace}?pair={}", "AB".repeat(48)),
            format!("{workspace}?pair={}", "zz".repeat(48)),
            format!("{workspace}?pair={pair}0"),
            // Something inside Tilo instead of a workspace.
            format!("https://tilo.sh/app/#/w/{WORKSPACE}?pair={pair}"),
            format!("https://tilo.sh/#/w/{WORKSPACE}/so.tilo.chat?pair={pair}"),
            format!("https://tilo.sh/#/w/{WORKSPACE}/so.tilo.chat/-/note-1?pair={pair}"),
            // Another secret beside the code.
            format!("https://tilo.sh/#/w/{WORKSPACE}?invite=abcd&pair={pair}"),
            format!("https://tilo.sh/#/w/{WORKSPACE}?pair={pair}&invite=abcd"),
            // A damaged route: a workspace id in another spelling, no `w`, a
            // repeated setting, an unknown one.
            format!(
                "https://tilo.sh/#/w/{}?pair={pair}",
                WORKSPACE.to_uppercase()
            ),
            format!("https://tilo.sh/#/r/{WORKSPACE}?pair={pair}"),
            format!("{workspace}?pair={pair}&pair={pair}"),
            format!("{workspace}?pair={pair}&whose=theirs"),
            format!("https://tilo.sh/#/w/{WORKSPACE}//?pair={pair}"),
            format!("https://tilo.sh/path?x=1#{}/w/{WORKSPACE}?pair={pair}", ""),
        ];
        for url in refused {
            let answer = PairingLink::parse(&url);
            assert!(answer.is_err(), "{url} was read as {answer:?}");
            let sentence = answer.unwrap_err();
            assert!(
                sentence.ends_with('.') && sentence.chars().count() > 20,
                "{url}: {sentence}"
            );
            assert!(!sentence.contains('\n'), "{url}: {sentence}");
        }
    }

    /// Past the grammar's own bound a link is refused rather than clipped.
    #[test]
    fn refuses_a_link_longer_than_the_grammar_opens() {
        let url = format!(
            "https://tilo.sh/#/w/{WORKSPACE}?pair={}{}",
            key(6),
            "0".repeat(9 * 1024)
        );
        assert!(PairingLink::parse(&url).is_err());
    }
}
