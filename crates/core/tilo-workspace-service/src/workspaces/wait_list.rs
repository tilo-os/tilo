//! The addresses of people who asked at the entry of a workspace that takes
//! people by invitation, without one: each is an address their invitation
//! goes to ([`WaitListEntry`]), not a person the workspace could admit. The
//! person is not online for this: the workspace keeps it until it leaves the
//! list, so it outlives restarts. One address is one entry, however it was typed
//! ([`contact_key`]); it keeps the form first typed.
//!
//! Where the entry sends mail, an entry is first unconfirmed: hidden from
//! whoever may invite until the link mailed to the address is opened (the
//! workspace keeps only the SHA-256 of its secret), and forgotten when that does
//! not happen within [`UNCONFIRMED_FOR_NS`]. An entry that sends no mail lists
//! an address at once.
//!
//! An entry leaves the list three ways: whoever left it takes it off with a
//! secret they were handed or mailed (the workspace keeps only its SHA-256),
//! someone who may invite takes it off, or someone joins with an invitation
//! sent for it.
//!
//! Kept beside the workspace's record, in `<workspace>.wait-list`, as a versioned
//! format ([`FORMAT`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tilo_files::native::migration::Format;
use tilo_server_protocol::operator::WaitListStatus;
use tilo_server_protocol::workspaces::{WaitListEntry, contact_key};

/// The wait list's on-disk format: a `u32` version, then the entries.
pub(super) const FORMAT: Format = Format {
    name: "workspace wait list",
    version: 2,
    oldest: 2,
    steps: &[],
};

/// How long an address waits to be confirmed before the workspace forgets it.
pub const UNCONFIRMED_FOR_NS: u64 = 48 * 3600 * 1_000_000_000;

/// The least time between two confirmation mails to one address, however
/// often it is left at the entry.
pub const MAIL_EVERY_NS: u64 = 3600 * 1_000_000_000;

/// The least time between two invitation mails to one address.
pub const INVITE_MAIL_EVERY_NS: u64 = 10 * 60 * 1_000_000_000;

/// The removal secrets an entry answers to: one per time its address was
/// left or mailed, the oldest forgotten past this, so a browser that left it
/// lately, or the latest mail, can still take it off.
const MAX_REMOVALS: usize = 8;

/// The confirmation secrets an entry answers to, likewise.
const MAX_CONFIRMS: usize = 8;

/// The invitations an entry remembers having been sent.
const MAX_INVITES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    request_id: [u8; 32],
    /// The address as first typed ([`tilo_server_protocol::workspaces::contact_address`]).
    contact: String,
    /// When it was left; while unconfirmed, when it was last left.
    at_ns: u64,
    /// SHA-256 of each secret that takes it off the list, oldest first.
    removals: Vec<[u8; 32]>,
    /// SHA-256 of each secret that confirms it, oldest first.
    confirms: Vec<[u8; 32]>,
    /// When the address was confirmed; `None` while it is not.
    confirmed_at_ns: Option<u64>,
    /// The language the mails to it are in.
    language: String,
    /// When the last confirmation mail went to it.
    mailed_at_ns: Option<u64>,
    /// The invitations sent for it, oldest first: someone joining with one
    /// takes it off the list.
    invites: Vec<[u8; 32]>,
    /// When the last of them was sent.
    invited_at_ns: Option<u64>,
    /// When an invitation was last mailed to it.
    invite_mailed_at_ns: Option<u64>,
}

impl Entry {
    fn expired(&self, now_ns: u64) -> bool {
        self.confirmed_at_ns.is_none() && now_ns.saturating_sub(self.at_ns) > UNCONFIRMED_FOR_NS
    }

    fn mailbox(&self) -> Mailbox {
        Mailbox {
            contact: self.contact.clone(),
            language: self.language.clone(),
        }
    }
}

/// An address mailed within the last [`MAIL_EVERY_NS`] whose entry is gone:
/// left again, it is not mailed again before then, and the link it was
/// mailed confirms the new entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Mailed {
    /// SHA-256 of its [`contact_key`]: the address itself is not kept once
    /// its entry is gone.
    key_sha256: [u8; 32],
    at_ns: u64,
    /// SHA-256 of each secret that confirmed the gone entry.
    confirms: Vec<[u8; 32]>,
}

fn key_sha256(contact: &str) -> [u8; 32] {
    sha2::Sha256::digest(contact_key(contact).as_bytes()).into()
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    entries: Vec<Entry>,
    mailed: Vec<Mailed>,
}

/// What keeping a request did. `mail`: send the confirmation mail now (at
/// most one per `MAIL_EVERY_NS` to an address).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitListed {
    Added {
        mail: bool,
    },
    /// The workspace kept a request for that address already: the new secrets
    /// take it off, and confirm it, too.
    Already {
        mail: bool,
    },
    /// The workspace keeps as many as it takes.
    Full,
}

/// Where a mail about an entry goes, and in which language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mailbox {
    pub contact: String,
    pub language: String,
}

/// The secrets a request is left with, as their SHA-256 digests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Secrets {
    /// SHA-256 of the secret that takes the entry off.
    pub removal: [u8; 32],
    /// SHA-256 of the secret that confirms it; `None` where the entry sends
    /// no mail, which lists the address at once.
    pub confirm: Option<[u8; 32]>,
}

/// One workspace's wait list.
#[derive(Debug, Default)]
pub(super) struct WaitList {
    entries: Vec<Entry>,
    mailed: Vec<Mailed>,
    path: PathBuf,
    /// What a page reads of the invitations sent to these addresses, without
    /// taking the workspace's lock.
    invitations: super::readings::Invitations,
}

impl WaitList {
    /// The wait list of the workspace recorded at `workspace_path` (its `.workspace` file).
    pub(super) fn open(
        workspace_path: &Path,
        invitations: super::readings::Invitations,
    ) -> Result<Self, String> {
        let path = workspace_path.with_extension("wait-list");
        let stored = match std::fs::read(&path) {
            Ok(bytes) => {
                let (stored, _) =
                    postcard::take_from_bytes::<u32>(&bytes).map_err(|error| error.to_string())?;
                let bytes = FORMAT
                    .bring_up(&path, bytes, stored)
                    .map_err(|error| error.to_string())?;
                postcard::from_bytes::<Stored>(&bytes).map_err(|error| error.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored {
                version: FORMAT.version,
                entries: Vec::new(),
                mailed: Vec::new(),
            },
            Err(error) => return Err(error.to_string()),
        };
        let wait_list = Self {
            entries: stored.entries,
            mailed: stored.mailed,
            path,
            invitations,
        };
        wait_list.publish_invitations();
        Ok(wait_list)
    }

    /// Keep what a page reads of the invitations sent off these addresses
    /// current: the card of an invitation a wait-list person was mailed says
    /// so.
    fn publish_invitations(&self) {
        let sent: std::collections::BTreeSet<[u8; 32]> = self
            .entries
            .iter()
            .flat_map(|entry| entry.invites.iter().copied())
            .collect();
        self.invitations.change(|issued| issued.mailed = sent);
    }

    fn save(&self) -> Result<(), String> {
        let bytes = postcard::to_stdvec(&Stored {
            version: FORMAT.version,
            entries: self.entries.clone(),
            mailed: self.mailed.clone(),
        })
        .map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())
    }

    /// Change the entries with `change`, and write them; on a failed write
    /// they are as they were. Unconfirmed entries past their time go first;
    /// an entry gone within [`MAIL_EVERY_NS`] of its mail is remembered as
    /// [`Mailed`].
    fn changed<T>(
        &mut self,
        now_ns: u64,
        change: impl FnOnce(&mut Vec<Entry>) -> T,
    ) -> Result<T, String> {
        let (before, mailed) = (self.entries.clone(), self.mailed.clone());
        self.entries.retain(|entry| !entry.expired(now_ns));
        let result = change(&mut self.entries);
        let lately = |at_ns: u64| now_ns.saturating_sub(at_ns) < MAIL_EVERY_NS;
        self.mailed.retain(|gone| lately(gone.at_ns));
        for entry in &before {
            if let Some(at_ns) = entry.mailed_at_ns.filter(|at| lately(*at))
                && !self
                    .entries
                    .iter()
                    .any(|kept| kept.request_id == entry.request_id)
            {
                self.mailed.push(Mailed {
                    key_sha256: key_sha256(&entry.contact),
                    at_ns,
                    confirms: entry.confirms.clone(),
                });
            }
        }
        if let Err(error) = self.save() {
            self.entries = before;
            self.mailed = mailed;
            return Err(error);
        }
        self.publish_invitations();
        Ok(result)
    }

    fn live(&self, now_ns: u64) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |entry| !entry.expired(now_ns))
    }

    /// Forget the unconfirmed entries past their time; how many.
    pub(super) fn expire(&mut self, now_ns: u64) -> Result<usize, String> {
        let expired = self
            .entries
            .iter()
            .filter(|entry| entry.expired(now_ns))
            .count();
        if expired == 0 {
            return Ok(0);
        }
        self.changed(now_ns, |_| expired)
    }

    /// Keep the request of `contact` (already a
    /// [`tilo_server_protocol::workspaces::contact_address`]), asked at `now_ns`
    /// in `language`, when the workspace keeps fewer than `max`. Whoever holds
    /// the secret that hashes to `secrets.removal` may take it off again
    /// ([`Self::leave`]); the one that hashes to `secrets.confirm` confirms
    /// it ([`Self::confirm`]).
    pub(super) fn keep(
        &mut self,
        contact: &str,
        secrets: Secrets,
        language: &str,
        now_ns: u64,
        max: usize,
    ) -> Result<WaitListed, String> {
        let key = contact_key(contact);
        let language = tilo_server_protocol::workspaces::mail_language(language);
        let mails = |entry: &Entry| {
            secrets.confirm.is_some()
                && entry
                    .mailed_at_ns
                    .is_none_or(|at| now_ns.saturating_sub(at) >= MAIL_EVERY_NS)
        };
        let kept = self
            .live(now_ns)
            .find(|entry| contact_key(&entry.contact) == key)
            .map(|entry| entry.request_id);
        if let Some(request_id) = kept {
            return self.changed(now_ns, |entries| {
                let Some(entry) = entries
                    .iter_mut()
                    .find(|entry| entry.request_id == request_id)
                else {
                    return WaitListed::Already { mail: false };
                };
                push_capped(&mut entry.removals, secrets.removal, MAX_REMOVALS);
                let mail = mails(entry);
                match secrets.confirm {
                    Some(confirm) => push_capped(&mut entry.confirms, confirm, MAX_CONFIRMS),
                    None => {
                        entry.confirmed_at_ns.get_or_insert(now_ns);
                    }
                }
                if entry.confirmed_at_ns.is_none() {
                    // Its time to be confirmed starts again.
                    entry.at_ns = now_ns;
                }
                if mail {
                    entry.mailed_at_ns = Some(now_ns);
                }
                WaitListed::Already { mail }
            });
        }
        if self.live(now_ns).count() >= max {
            return Ok(WaitListed::Full);
        }
        // An address mailed lately and gone since is not mailed again
        // before its time; that mail's link confirms the new entry.
        let gone = key_sha256(contact);
        let lately = self
            .mailed
            .iter()
            .filter(|mailed| {
                mailed.key_sha256 == gone && now_ns.saturating_sub(mailed.at_ns) < MAIL_EVERY_NS
            })
            .max_by_key(|mailed| mailed.at_ns)
            .cloned();
        let mail = secrets.confirm.is_some() && lately.is_none();
        let removals = vec![secrets.removal];
        let mut confirms = Vec::new();
        if secrets.confirm.is_some() {
            for confirm in lately.iter().flat_map(|mailed| &mailed.confirms) {
                push_capped(&mut confirms, *confirm, MAX_CONFIRMS);
            }
        }
        if let Some(confirm) = secrets.confirm {
            push_capped(&mut confirms, confirm, MAX_CONFIRMS);
        }
        self.changed(now_ns, |entries| {
            entries.push(Entry {
                request_id: rand::random(),
                contact: contact.to_owned(),
                at_ns: now_ns,
                removals,
                confirms,
                confirmed_at_ns: secrets.confirm.is_none().then_some(now_ns),
                language: language.to_owned(),
                mailed_at_ns: if mail {
                    Some(now_ns)
                } else {
                    lately.as_ref().map(|mailed| mailed.at_ns)
                },
                invites: Vec::new(),
                invited_at_ns: None,
                invite_mailed_at_ns: None,
            });
            WaitListed::Added { mail }
        })
    }

    /// Confirm the request whose confirmation secret hashes to `confirm`;
    /// whether there was one (confirmed now or before).
    pub(super) fn confirm(&mut self, confirm: [u8; 32], now_ns: u64) -> Result<bool, String> {
        let Some(request_id) = self
            .live(now_ns)
            .find(|entry| entry.confirms.contains(&confirm))
            .map(|entry| entry.request_id)
        else {
            return Ok(false);
        };
        self.changed(now_ns, |entries| {
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry.request_id == request_id)
            {
                entry.confirmed_at_ns.get_or_insert(now_ns);
            }
            true
        })
    }

    /// Where the request whose removal secret hashes to `removal` stands.
    pub(super) fn state(&self, removal: [u8; 32], now_ns: u64) -> WaitListStatus {
        match self
            .live(now_ns)
            .find(|entry| entry.removals.contains(&removal))
        {
            None => WaitListStatus::Gone,
            Some(entry) if entry.confirmed_at_ns.is_none() => WaitListStatus::Pending,
            Some(entry) if entry.invited_at_ns.is_some() => WaitListStatus::Invited,
            Some(_) => WaitListStatus::Listed,
        }
    }

    /// Take out the request `request_id`; whether there was one.
    pub(super) fn remove(&mut self, request_id: [u8; 32], now_ns: u64) -> Result<bool, String> {
        if !self.has(request_id, now_ns) {
            return Ok(false);
        }
        self.changed(now_ns, |entries| {
            entries.retain(|entry| entry.request_id != request_id);
            true
        })
    }

    /// Take out the request whose removal secret hashes to `removal`, for
    /// whoever left it. Its answer to the person does not depend on whether
    /// there was one; the address is told it left the list (the mailbox)
    /// once it had been confirmed.
    pub(super) fn leave(
        &mut self,
        removal: [u8; 32],
        now_ns: u64,
    ) -> Result<Option<Mailbox>, String> {
        let Some((request_id, told)) = self
            .live(now_ns)
            .find(|entry| entry.removals.contains(&removal))
            .map(|entry| {
                (
                    entry.request_id,
                    entry.confirmed_at_ns.is_some().then(|| entry.mailbox()),
                )
            })
        else {
            return Ok(None);
        };
        self.changed(now_ns, |entries| {
            entries.retain(|entry| entry.request_id != request_id);
            told
        })
    }

    /// Private cancellation identity, without disclosing a pending mailbox.
    pub(super) fn removal_contact(&self, removal: [u8; 32], now_ns: u64) -> Option<[u8; 32]> {
        self.live(now_ns)
            .find(|entry| entry.removals.contains(&removal))
            .map(|entry| key_sha256(&entry.contact))
    }

    /// Whether `request_id` is on the list, confirmed.
    pub(super) fn has(&self, request_id: [u8; 32], now_ns: u64) -> bool {
        self.live(now_ns)
            .any(|entry| entry.request_id == request_id && entry.confirmed_at_ns.is_some())
    }

    /// Note that the invitation `invite_id` was sent for `request_id` at
    /// `now_ns`; whether that entry is on the list.
    pub(super) fn invited(
        &mut self,
        request_id: [u8; 32],
        invite_id: [u8; 32],
        now_ns: u64,
    ) -> Result<bool, String> {
        if !self.has(request_id, now_ns) {
            return Ok(false);
        }
        self.changed(now_ns, |entries| {
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry.request_id == request_id)
            {
                if !entry.invites.contains(&invite_id) {
                    push_capped(&mut entry.invites, invite_id, MAX_INVITES);
                }
                entry.invited_at_ns = Some(now_ns);
            }
            true
        })
    }

    /// Where to mail the invitation last sent for `request_id`, and a new
    /// removal secret for the mail, whose SHA-256 the entry now keeps; or
    /// why not: the entry is not listed, was sent no invitation, or was
    /// mailed one within [`INVITE_MAIL_EVERY_NS`].
    pub(super) fn mail_invite(
        &mut self,
        request_id: [u8; 32],
        now_ns: u64,
    ) -> Result<Result<(Mailbox, [u8; 32]), &'static str>, String> {
        use sha2::Digest as _;
        let Some(entry) = self
            .live(now_ns)
            .find(|entry| entry.request_id == request_id && entry.confirmed_at_ns.is_some())
        else {
            return Ok(Err("that address is no longer on the wait list"));
        };
        if entry.invites.is_empty() {
            return Ok(Err("no invitation was sent for that address"));
        }
        if entry
            .invite_mailed_at_ns
            .is_some_and(|at| now_ns.saturating_sub(at) < INVITE_MAIL_EVERY_NS)
        {
            return Ok(Err("that address was mailed an invitation minutes ago"));
        }
        let mailbox = entry.mailbox();
        let secret: [u8; 32] = rand::random();
        let removal: [u8; 32] = sha2::Sha256::digest(secret).into();
        self.changed(now_ns, |entries| {
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry.request_id == request_id)
            {
                push_capped(&mut entry.removals, removal, MAX_REMOVALS);
                entry.invite_mailed_at_ns = Some(now_ns);
            }
            Ok((mailbox, secret))
        })
    }

    /// Release only this failed enqueue's reservation, leaving newer ones
    /// and all removal tokens intact.
    pub(super) fn mail_failed(&mut self, removal: [u8; 32], now_ns: u64) -> Result<(), String> {
        self.changed(now_ns, |entries| {
            for entry in entries {
                if entry.removals.last() == Some(&removal) {
                    entry.invite_mailed_at_ns = None;
                }
            }
        })
    }

    /// Take out every request someone joined with an invitation sent for:
    /// `redeemed` says whether an invitation was. How many left.
    pub(super) fn joined(
        &mut self,
        redeemed: impl Fn([u8; 32]) -> bool,
        now_ns: u64,
    ) -> Result<usize, String> {
        let left = |entry: &Entry| entry.invites.iter().any(|invite| redeemed(*invite));
        let count = self.entries.iter().filter(|entry| left(entry)).count();
        if count == 0 {
            return Ok(0);
        }
        self.changed(now_ns, |entries| {
            entries.retain(|entry| !left(entry));
            count
        })
    }

    /// The confirmed entries, oldest first.
    pub(super) fn entries(&self, now_ns: u64) -> Vec<WaitListEntry> {
        self.live(now_ns)
            .filter(|entry| entry.confirmed_at_ns.is_some())
            .map(|entry| WaitListEntry {
                request_id: entry.request_id,
                contact: entry.contact.clone(),
                added_at_ns: entry.at_ns,
                invited_at_ns: entry.invited_at_ns,
            })
            .collect()
    }
}

/// An invitation someone who may invite asked the workspace to mail to an
/// address on its wait list ([`Workspaces::mail_wait_list`]).
pub struct WaitListMail {
    /// Completed only after the operator durably accepts the mail.
    pub queued: tokio::sync::oneshot::Sender<bool>,
    pub to: Mailbox,
    /// The invitation's web link.
    pub link: String,
    /// How the person who asked named themself in the workspace, where it has a name for them;
    /// empty while nobody named them.
    pub inviter: String,
    /// A secret that takes the entry off the list, for the mail's removal
    /// link; the entry keeps only its SHA-256.
    pub removal: [u8; 32],
}

/// What whoever runs the server decided about the entry's wait list, set once
/// at its start: where an invitation a member asks for is mailed, and what the
/// entry says to someone it turns away.
#[derive(Default)]
pub(super) struct EntrySettings {
    /// Where invitations to mail to wait-list addresses go
    /// ([`super::Workspaces::mail_wait_list`]).
    pub(super) mail: std::sync::OnceLock<(
        String,
        tokio::sync::mpsc::UnboundedSender<super::WaitListMail>,
    )>,
    /// The page an invitation's link opens ([`super::Workspaces::invite_at`]).
    pub(super) invitations: std::sync::OnceLock<String>,
    /// This host's own web origin (`https://eu1.tilo.sh`), where a page asks
    /// for an invitation a short link names ([`super::Workspaces::short_invitation_link`]).
    pub(super) host: std::sync::OnceLock<String>,
}

/// The page an invitation's link opens when the server names none of its own.
const PUBLIC_INVITE_ENTRY: &str = "https://tilo.sh/";

/// Whether a computer other than this one can open `site`: not one named on this computer or its
/// network alone (`localhost`, `*.localhost`, a single label, a `.local` or `.internal` name, a
/// loopback, private, link-local or unspecified address).
fn reached_from_elsewhere(site: &str) -> bool {
    let rest = site.split_once("://").map_or(site, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if let Some(bracketed) = host.strip_prefix('[') {
        let address = bracketed.split(']').next().unwrap_or_default();
        let address = tilo_failure::ignored!(
            "a bracketed host that is no IPv6 address is reached from nowhere",
            address.parse::<std::net::Ipv6Addr>()
        );
        return address.is_ok_and(|address| {
            !(address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local())
        });
    }
    let name = host.split(':').next().unwrap_or_default();
    if let Ok(address) = tilo_failure::ignored!(
        "a host that is no IPv4 address is a name, read below",
        name.parse::<std::net::Ipv4Addr>()
    ) {
        return !(address.is_loopback()
            || address.is_private()
            || address.is_link_local()
            || address.is_unspecified());
    }
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let last = name.rsplit('.').next().unwrap_or_default();
    name.contains('.') && !matches!(last, "localhost" | "local" | "internal")
}

/// The wait list `entry` keeps, as its refusal and its page name it: where
/// someone asks to be taken off the list, and whether it mails an address a
/// link that confirms it. `None` when the entry keeps no wait list.
#[must_use]
pub(super) fn offered(
    entry: &tilo_action_policy::EntryPolicy,
) -> Option<tilo_server_protocol::directory::EntryWaitList> {
    entry
        .keeps_requests()
        .then(|| tilo_server_protocol::directory::EntryWaitList {
            contact: entry.contact.clone(),
            mail: entry.mail.is_some(),
        })
}

impl super::Workspaces {
    /// The wait list `workspace`'s entry keeps now, as the entry's refusal
    /// names it
    /// ([`EntryRefused::wait_list`](tilo_server_protocol::directory::EntryRefused)).
    pub async fn waiting_offer(
        &self,
        workspace: &str,
    ) -> Option<tilo_server_protocol::directory::EntryWaitList> {
        offered(&self.entry_policy(workspace).await?)
    }

    /// The invitations members ask to mail to addresses on a wait list,
    /// for whoever runs the server to send from the web entry `entry`: until
    /// this is called, the workspaces send no mail and refuse to, and they mail
    /// only invitations on that entry
    /// ([`tilo_server_protocol::workspaces::invitation_on_entry`]). `None` once
    /// called before.
    #[must_use]
    pub fn mail_wait_list(
        &self,
        entry: &str,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<WaitListMail>> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.inner.entry.mail.set((entry.to_owned(), sender)).ok()?;
        Some(receiver)
    }

    /// The page whoever follows an invitation's link opens, as whoever runs the server set its
    /// web entry: the link carries the signed invitation after its `#`, the server's own
    /// addresses in it, so the page dials the server with nothing asked of any site. Without
    /// one, or with one only this computer or its network reaches (`http://tilo.localhost`, a
    /// private address), tilo.sh's page opens it: a link pasted into a message must open
    /// wherever it is followed. `local` is a stack on this machine alone (`TILO_WORKSPACES_LOCAL`):
    /// its addresses are this machine's, so a public page cannot dial it, and its own site is
    /// the only page that opens the link. `false` once called before.
    pub fn invite_at(&self, entry: &str, local: bool) -> bool {
        self.inner
            .entry
            .invitations
            .set(invitation_entry(entry, local))
            .is_ok()
    }

    /// This host's own web origin (`https://eu1.tilo.sh`), which the short
    /// links of its workspaces' invitations name: a page following one asks
    /// it for the invitation the workspace keeps
    /// ([`tilo_server_protocol::web::OPEN_INVITATION_PATH`]). `false` once
    /// called before, or for an origin no link can name.
    #[must_use]
    pub fn invitations_at(&self, origin: &str) -> bool {
        tilo_reference::link::HostOrigin::new(origin.trim_end_matches('/')).is_ok()
            && self.inner.entry.host.set(origin.to_owned()).is_ok()
    }

    /// The short link of `invitation` (its signed bytes), which `workspace`
    /// keeps: the page [`Self::invite_at`] names, opened at this host's
    /// workspace with the secret only the invitation's holder can derive
    /// (`#/@<host>/w/<workspace>?invite=<secret>`). The page asks this host for
    /// the invitation by it, so the link fits a message and the invitation
    /// stays in its workspace, which takes it back when it is cancelled.
    /// `None` where this host has no web origin a page can ask.
    pub(super) fn short_invitation_link(
        &self,
        workspace: &str,
        invitation: &[u8],
    ) -> Option<String> {
        let host = self.inner.entry.host.get()?;
        let host = tilo_reference::link::HostOrigin::new(host.trim_end_matches('/')).ok()?;
        let entry = self
            .inner
            .entry
            .invitations
            .get()
            .map_or(PUBLIC_INVITE_ENTRY, String::as_str);
        let secret = tilo_peer_state::join::native::bearer_text(invitation);
        // A page served by this host names no host: the workspace is its own.
        let own =
            tilo_reference::link::WebEntry::new(entry).is_ok_and(|entry| *entry.origin() == host);
        let at = if own {
            String::new()
        } else {
            format!("/@{}", host.route())
        };
        Some(format!("{entry}#{at}/w/{workspace}?invite={secret}"))
    }

    /// The link that brings whoever follows it into `workspace` with `invitation` (its signed
    /// bytes): the invitation's own text after the workspace's route on the page
    /// [`Self::invite_at`] names.
    pub(super) fn invitation_link(&self, workspace: &str, invitation: &[u8]) -> String {
        let entry = self
            .inner
            .entry
            .invitations
            .get()
            .map_or(PUBLIC_INVITE_ENTRY, String::as_str);
        let text = tilo_peer_state::join::encode_invitation_text(invitation);
        format!("{entry}#/w/{workspace}?invite={text}")
    }

    /// The page the workspace's entry mails from, when it mails at all: a link
    /// an invitation is mailed at leads there
    /// ([`tilo_server_protocol::workspaces::invitation_on_entry`]).
    #[must_use]
    pub fn mail_entry(&self) -> Option<String> {
        self.inner.entry.mail.get().map(|(entry, _)| entry.clone())
    }
}

/// The page an invitation's link opens, from the server's web entry (`entry`) and whether it
/// runs on this machine alone (`local`): the site's own page where another computer reaches it,
/// or where the stack is this machine's and its addresses are loopback, which no public page
/// dials; tilo.sh's page otherwise, which opens wherever the link is followed.
fn invitation_entry(entry: &str, local: bool) -> String {
    let entry = format!("{}/", entry.trim_end_matches('/'));
    if local || reached_from_elsewhere(&entry) {
        entry
    } else {
        PUBLIC_INVITE_ENTRY.to_owned()
    }
}

impl super::HostedWorkspace {
    /// Take off the wait list everyone who joined with an invitation sent
    /// for their entry. A failed write leaves them for the next time.
    pub(super) fn forget_joined_wait_list(&mut self) {
        let log = &self.record.collaboration;
        match self
            .wait_list
            .joined(|invite| log.redeemed(invite), tilo_clock::unix_now_ns())
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(
                workspace = %self.record.workspace,
                count,
                "invited addresses left the wait list on joining"
            ),
            Err(error) => tracing::warn!(
                workspace = %self.record.workspace,
                %error,
                "the wait list kept invited addresses that joined"
            ),
        }
    }
}

fn push_capped(list: &mut Vec<[u8; 32]>, item: [u8; 32], max: usize) {
    list.push(item);
    let over = list.len().saturating_sub(max);
    list.drain(..over);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::readings::Invitations;
    use tilo_files::native::migration::assert_golden_bytes;

    const HOUR: u64 = 3600 * 1_000_000_000;

    #[test]
    fn an_invitation_link_opens_only_where_another_computer_reaches_it() {
        for site in [
            "https://tilo.example",
            "https://tilo.example:8443/",
            "http://203.0.113.7/",
            "http://[2001:db8::1]:80/",
        ] {
            assert!(reached_from_elsewhere(site), "{site}");
        }
        for site in [
            "http://tilo.localhost",
            "http://localhost:3000/",
            "http://printer.local/",
            "http://intranet/",
            "http://127.0.0.1:8080",
            "http://192.168.1.4/",
            "http://[::1]:80/",
            "http://[fd00::1]/",
        ] {
            assert!(!reached_from_elsewhere(site), "{site}");
        }
    }

    #[test]
    fn a_stack_on_this_machine_keeps_its_own_invitation_page() {
        // A stack reached only from this machine: its addresses are loopback, so no public page
        // dials it and its own site is the page that opens a link made there.
        assert_eq!(
            invitation_entry("http://tilo.localhost:24120", true),
            "http://tilo.localhost:24120/"
        );
        // The same site on a stack reachable from elsewhere hands over tilo.sh's page, which
        // opens wherever the link is followed.
        assert_eq!(
            invitation_entry("http://tilo.localhost:24120", false),
            PUBLIC_INVITE_ENTRY
        );
        // A site another computer reaches opens as itself.
        assert_eq!(
            invitation_entry("https://tilo.example", false),
            "https://tilo.example/"
        );
    }

    /// The wait list file this build writes: an address confirmed beside the
    /// mail sent to it and the invitation sent for it, and an address whose
    /// entry is gone within the mail window. Kept so a workspace's list goes
    /// on reading the same way; a field added, removed or reordered changes
    /// these bytes.
    const GOLDEN: &str = include_str!("../../golden/workspace-wait-list/v2.hex");

    fn golden() -> Vec<u8> {
        hex::decode(GOLDEN.split_whitespace().collect::<String>()).unwrap()
    }

    fn kept() -> Stored {
        Stored {
            version: FORMAT.version,
            entries: vec![Entry {
                request_id: [3; 32],
                contact: "ada@example.org".into(),
                at_ns: 5,
                removals: vec![[1; 32]],
                confirms: vec![[2; 32]],
                confirmed_at_ns: Some(6),
                language: "de-DE".into(),
                mailed_at_ns: Some(5),
                invites: vec![[4; 32]],
                invited_at_ns: Some(7),
                invite_mailed_at_ns: Some(8),
            }],
            mailed: vec![Mailed {
                key_sha256: [9; 32],
                at_ns: 10,
                confirms: vec![[5; 32]],
            }],
        }
    }

    #[test]
    fn the_wait_list_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            FORMAT.name,
            &golden(),
            |bytes| {
                let stored: Stored = postcard::from_bytes(bytes).unwrap();
                assert_eq!(stored.version, FORMAT.version);
                assert_eq!(stored.entries[0].contact, "ada@example.org");
                assert_eq!(stored.entries[0].confirmed_at_ns, Some(6));
                assert_eq!(stored.mailed[0].key_sha256, [9; 32]);
            },
            || postcard::to_stdvec(&kept()).unwrap(),
        );
    }

    /// Secrets for an entry that sends no mail.
    fn listed(removal: u8) -> Secrets {
        Secrets {
            removal: [removal; 32],
            confirm: None,
        }
    }

    fn mailed(removal: u8, confirm: u8) -> Secrets {
        Secrets {
            removal: [removal; 32],
            confirm: Some([confirm; 32]),
        }
    }

    #[test]
    fn one_address_is_one_request_up_to_the_cap_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        assert_eq!(
            list.keep("Ada@Example.org", listed(1), "en", 1, 2),
            Ok(WaitListed::Added { mail: false })
        );
        assert_eq!(
            list.keep("ada@example.org", listed(2), "en", 2, 2),
            Ok(WaitListed::Already { mail: false })
        );
        assert_eq!(
            list.keep("bo@example.org", listed(3), "en", 3, 2),
            Ok(WaitListed::Added { mail: false })
        );
        assert_eq!(
            list.keep("cy@example.org", listed(4), "en", 4, 2),
            Ok(WaitListed::Full)
        );

        let restored = WaitList::open(&workspace, Invitations::default()).unwrap();
        let requests: Vec<_> = restored.entries(5);
        assert_eq!(requests.len(), 2);
        // Kept as first typed.
        assert_eq!(requests[0].contact, "Ada@Example.org");
        assert_eq!(requests[0].added_at_ns, 1);
        assert_eq!(requests[0].invited_at_ns, None);
        assert_ne!(requests[0].request_id, requests[1].request_id);

        let mut list = restored;
        assert_eq!(list.remove(requests[0].request_id, 5), Ok(true));
        assert_eq!(list.remove(requests[0].request_id, 5), Ok(false));
        assert_eq!(
            list.keep("cy@example.org", listed(5), "en", 5, 2),
            Ok(WaitListed::Added { mail: false })
        );
        let kept: Vec<_> = WaitList::open(&workspace, Invitations::default())
            .unwrap()
            .entries(6)
            .into_iter()
            .map(|entry| entry.contact)
            .collect();
        assert_eq!(kept, ["bo@example.org", "cy@example.org"]);
    }

    #[test]
    fn whoever_left_an_address_takes_it_off_with_a_secret_only_its_hash_of_which_is_kept() {
        use sha2::Digest as _;
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let secret = [0x5a_u8; 32];
        let again = [0x6b_u8; 32];
        let digest = |secret: &[u8; 32]| -> [u8; 32] { sha2::Sha256::digest(secret).into() };
        let with = |removal: [u8; 32]| Secrets {
            removal,
            confirm: None,
        };
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        list.keep("ada@example.org", with(digest(&secret)), "en", 1, 10)
            .unwrap();
        // The same address left again, from another browser: either secret
        // takes it off.
        list.keep("ADA@example.org", with(digest(&again)), "en", 2, 10)
            .unwrap();
        list.keep("bo@example.org", with(digest(&[7; 32])), "en", 3, 10)
            .unwrap();

        let file = std::fs::read(workspace.with_extension("wait-list")).unwrap();
        let holds = |needle: &[u8]| file.windows(needle.len()).any(|window| window == needle);
        assert!(holds(&digest(&secret)) && holds(&digest(&again)));
        assert!(
            !holds(&secret) && !holds(&again),
            "the secrets are not kept"
        );

        // A wrong secret takes nothing off; the right one takes off that
        // entry alone, once, and says where to tell it left.
        assert_eq!(list.leave(digest(&[9; 32]), 4), Ok(None));
        assert_eq!(list.entries(4).len(), 2);
        assert_eq!(
            list.leave(digest(&again), 4),
            Ok(Some(Mailbox {
                contact: "ada@example.org".into(),
                language: "en".into(),
            }))
        );
        assert_eq!(list.leave(digest(&secret), 4), Ok(None));
        let left: Vec<_> = WaitList::open(&workspace, Invitations::default())
            .unwrap()
            .entries(5)
            .into_iter()
            .map(|entry| entry.contact)
            .collect();
        assert_eq!(left, ["bo@example.org"]);
        let file = std::fs::read(workspace.with_extension("wait-list")).unwrap();
        assert!(
            !file
                .windows(4)
                .any(|window| window.eq_ignore_ascii_case(b"ada@")),
            "nothing of the address is left"
        );
    }

    #[test]
    fn a_mailed_address_is_hidden_until_confirmed_and_mailed_at_most_hourly() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        assert_eq!(
            list.keep("Ada@example.org", mailed(1, 0xc1), "de-DE", 0, 10),
            Ok(WaitListed::Added { mail: true })
        );
        assert!(list.entries(1).is_empty(), "unconfirmed is not listed");
        assert_eq!(list.state([1; 32], 1), WaitListStatus::Pending);
        // Left again within the hour: answered alike, but no second mail.
        assert_eq!(
            list.keep("ada@example.org", mailed(2, 0xc2), "en", HOUR / 2, 10),
            Ok(WaitListed::Already { mail: false })
        );
        assert_eq!(
            list.keep("ada@example.org", mailed(3, 0xc3), "en", HOUR + 1, 10),
            Ok(WaitListed::Already { mail: true })
        );
        assert_eq!(list.confirm([0xee; 32], HOUR + 2), Ok(false));
        // Any mailed link confirms it.
        assert_eq!(list.confirm([0xc2; 32], HOUR + 2), Ok(true));
        assert_eq!(list.confirm([0xc3; 32], HOUR + 3), Ok(true));
        let restored = WaitList::open(&workspace, Invitations::default()).unwrap();
        let entries = restored.entries(HOUR + 4);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].contact, "Ada@example.org");
        assert_eq!(restored.state([2; 32], HOUR + 4), WaitListStatus::Listed);
        // The language it was first left in is the one its mails are in.
        let mut list = restored;
        assert_eq!(
            list.leave([1; 32], HOUR + 5),
            Ok(Some(Mailbox {
                contact: "Ada@example.org".into(),
                language: "de".into(),
            }))
        );
        assert_eq!(list.state([2; 32], HOUR + 5), WaitListStatus::Gone);
    }

    #[test]
    fn an_address_taken_off_and_left_again_within_the_hour_is_not_mailed_again() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        list.keep("ada@example.org", mailed(1, 0xc1), "en", 0, 10)
            .unwrap();
        assert_eq!(list.leave([1; 32], 1), Ok(None));
        // Its address is gone; only a hash of it is kept, until the hour is.
        let bytes = std::fs::read(workspace.with_extension("wait-list")).unwrap();
        assert!(!bytes.windows(3).any(|window| window == b"ada"));
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        assert_eq!(
            list.keep("Ada@Example.org", mailed(2, 0xc2), "en", 2, 10),
            Ok(WaitListed::Added { mail: false })
        );
        // The link mailed before confirms the new entry.
        assert_eq!(list.confirm([0xc1; 32], 3), Ok(true));
        assert_eq!(list.entries(4).len(), 1);
        assert_eq!(
            list.leave([2; 32], 5).unwrap().unwrap().contact,
            "Ada@Example.org"
        );
        // Past the hour it is mailed again.
        assert_eq!(
            list.keep("ada@example.org", mailed(3, 0xc3), "en", HOUR, 10),
            Ok(WaitListed::Added { mail: true })
        );
    }

    #[test]
    fn an_unconfirmed_address_is_forgotten_after_two_days() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        list.keep("ada@example.org", mailed(1, 0xc1), "en", 0, 1)
            .unwrap();
        // While it waits it counts toward the cap.
        assert_eq!(
            list.keep("bo@example.org", mailed(2, 0xc2), "en", HOUR, 1),
            Ok(WaitListed::Full)
        );
        let late = UNCONFIRMED_FOR_NS + 1;
        assert_eq!(list.state([1; 32], late), WaitListStatus::Gone);
        assert_eq!(list.confirm([0xc1; 32], late), Ok(false));
        assert_eq!(list.expire(late), Ok(1));
        assert_eq!(list.expire(late), Ok(0));
        let file = std::fs::read(workspace.with_extension("wait-list")).unwrap();
        assert!(
            !file.windows(4).any(|window| window == b"ada@"),
            "the address is gone from the file"
        );
        assert_eq!(
            list.keep("bo@example.org", mailed(2, 0xc2), "en", late, 1),
            Ok(WaitListed::Added { mail: true })
        );
        // A removal leaves no word for an address never confirmed.
        assert_eq!(list.leave([2; 32], late), Ok(None));
    }

    #[test]
    fn an_invited_entry_says_when_and_leaves_once_someone_joins_with_its_invitation() {
        use sha2::Digest as _;
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut list = WaitList::open(&workspace, Invitations::default()).unwrap();
        list.keep("ada@example.org", listed(1), "en", 1, 10)
            .unwrap();
        list.keep("bo@example.org", listed(2), "en", 2, 10).unwrap();
        list.keep("cy@example.org", mailed(3, 0xc3), "en", 3, 10)
            .unwrap();
        let [ada, bo] = [0, 1].map(|index| list.entries(4)[index].request_id);
        assert!(list.has(ada, 4));
        assert_eq!(list.invited(ada, [0xa1; 32], 50), Ok(true));
        assert_eq!(list.invited([0xee; 32], [0xa2; 32], 50), Ok(false));
        assert_eq!(list.state([1; 32], 50), WaitListStatus::Invited);
        let restored = WaitList::open(&workspace, Invitations::default()).unwrap();
        assert_eq!(restored.entries(51)[0].invited_at_ns, Some(50));
        assert_eq!(restored.entries(51)[1].invited_at_ns, None);

        // The invitation is mailed with a removal secret of its own.
        let mut list = restored;
        assert!(
            list.mail_invite(bo, 52).unwrap().is_err(),
            "bo was not invited"
        );
        let (mailbox, secret) = list.mail_invite(ada, 52).unwrap().unwrap();
        assert_eq!(mailbox.contact, "ada@example.org");
        assert!(
            list.mail_invite(ada, 53).unwrap().is_err(),
            "not mailed twice in minutes"
        );
        let removal: [u8; 32] = sha2::Sha256::digest(secret).into();
        assert_eq!(list.state(removal, 53), WaitListStatus::Invited);

        assert_eq!(list.joined(|invite| invite == [0xff; 32], 54), Ok(0));
        assert_eq!(list.joined(|invite| invite == [0xa1; 32], 54), Ok(1));
        assert!(!list.has(ada, 54) && list.has(bo, 54));
        assert_eq!(
            WaitList::open(&workspace, Invitations::default())
                .unwrap()
                .entries(55)
                .len(),
            1
        );
    }

    #[test]
    fn a_wait_list_a_newer_build_wrote_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let path = workspace.with_extension("wait-list");
        let newer = postcard::to_stdvec(&Stored {
            version: FORMAT.version + 1,
            entries: Vec::new(),
            mailed: Vec::new(),
        })
        .unwrap();
        std::fs::write(&path, &newer).unwrap();
        assert!(WaitList::open(&workspace, Invitations::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), newer);
    }
}
