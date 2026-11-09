//! Login codes ([`tilo_relay_protocol::login_code`]): the short code a
//! person's tab shows and their agent types into `tilo login`.
//!
//! Codes live in memory only, a bounded number at once. Each is drawn from
//! the operating system's random source, resolves once (the first resolve
//! spends it) within [`CODE_LIFETIME_MS`], and keeps what the computer
//! reports for [`FOLLOW_LIFETIME_MS`] after that. Only the tab holding the
//! code's `follow` secret reads it, and only the computer holding its
//! `report` secret writes it.
//!
//! A code is about 2.2e10 letters' worth and lives ten minutes, so guessing
//! one takes many tries: the host counts resolves that miss, per client
//! address and in all, and answers `429` once either passes its budget for
//! the minute. A budget spent refuses every resolve until the minute ends,
//! so the guesses it would have let through are never looked up.
//!
//! A sign-in code is claimed by a device of a person; the claim mints the
//! one invite that person's stand-in ([`super::stand_in`]) approves a
//! certification request for, once.

use std::collections::HashMap;
use std::net::IpAddr;

use tilo_relay_protocol::login_code::{
    CODE_LETTERS, CODE_LIFETIME_MS, ClaimedFor, Created, FOLLOW_LIFETIME_MS, MAX_AGENT_CHARS,
    MAX_LINK_CHARS, Polled, Report, Resolved, Step, code_from, normalize_code,
};

/// Codes held at once, spent or not; a create past it is refused.
pub(super) const MAX_CODES: usize = 1024;
/// Resolves that may miss per client address in a minute.
pub(super) const MISSES_PER_ADDRESS: u32 = 10;
/// Resolves that may miss in all in a minute.
pub(super) const MISSES_IN_ALL: u32 = 300;
/// Codes one client address may make in a minute.
pub(super) const CREATES_PER_ADDRESS: u32 = 30;
/// The window the budgets count in.
pub(super) const WINDOW_MS: u64 = 60 * 1000;
/// Client addresses counted at once; past it, only the totals count.
const MAX_COUNTED_ADDRESSES: usize = 4096;
/// The longest failure sentence a report carries.
pub(super) const MAX_DETAIL_CHARS: usize = 300;

/// Why a code operation was refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The request is not one this route takes (`400`).
    Malformed(&'static str),
    /// No code or secret like this is live (`404`).
    Unknown,
    /// Too many misses or creates this minute (`429`).
    TooMany,
    /// The host holds as many codes as it keeps (`503`).
    Full,
    /// The code is not in the state this asks for (`409`).
    Conflict(&'static str),
}

impl Refusal {
    pub(super) fn message(&self) -> &'static str {
        match self {
            Self::Malformed(message) | Self::Conflict(message) => message,
            Self::Unknown => "no such login code, or it expired; ask the page for a new one",
            Self::TooMany => "too many tries; wait a minute",
            Self::Full => "the host is making too many login codes; try again soon",
        }
    }
}

/// A claim a device of a person made for a sign-in code.
struct Claim {
    user: [u8; 32],
    invite: [u8; 16],
    by: String,
    expires_at_ms: u64,
    /// The stand-in approved the request naming the invite: it approves no
    /// second one.
    approved: bool,
}

/// What a computer did with a code it resolved.
struct Resolution {
    report: [u8; 32],
    at_ms: u64,
    steps: Vec<Step>,
    agent: Option<String>,
    detail: Option<String>,
    claim: Option<Claim>,
}

struct Entry {
    code: String,
    link: Option<String>,
    expires_at_ms: u64,
    resolution: Option<Resolution>,
}

impl Entry {
    /// When the host forgets this code.
    fn gone_at_ms(&self) -> u64 {
        self.resolution
            .as_ref()
            .map_or(self.expires_at_ms, |resolved| {
                resolved.at_ms.saturating_add(FOLLOW_LIFETIME_MS)
            })
    }
}

/// A count within one window.
#[derive(Clone, Copy, Default)]
struct Count {
    window: u64,
    count: u32,
}

impl Count {
    fn at(&mut self, window: u64) -> &mut u32 {
        if self.window != window {
            *self = Self { window, count: 0 };
        }
        &mut self.count
    }
}

/// Counts per client address and in all, by minute.
#[derive(Default)]
struct Budget {
    by_address: HashMap<IpAddr, Count>,
    in_all: Count,
}

impl Budget {
    /// Whether `client` and everyone together are still within `per_address`
    /// and `in_all` this minute.
    fn within(
        &mut self,
        client: Option<IpAddr>,
        now_ms: u64,
        per_address: u32,
        in_all: u32,
    ) -> bool {
        let window = now_ms / WINDOW_MS;
        if *self.in_all.at(window) >= in_all {
            return false;
        }
        client.is_none_or(|client| {
            self.by_address
                .get_mut(&client)
                .is_none_or(|count| *count.at(window) < per_address)
        })
    }

    /// Count one for `client` and in all.
    fn spend(&mut self, client: Option<IpAddr>, now_ms: u64) {
        let window = now_ms / WINDOW_MS;
        let total = self.in_all.at(window);
        *total = total.saturating_add(1);
        let Some(client) = client else { return };
        if !self.by_address.contains_key(&client) && self.by_address.len() >= MAX_COUNTED_ADDRESSES
        {
            self.by_address.retain(|_, count| count.window == window);
            if self.by_address.len() >= MAX_COUNTED_ADDRESSES {
                return;
            }
        }
        let count = self.by_address.entry(client).or_default().at(window);
        *count = count.saturating_add(1);
    }
}

#[derive(Default)]
struct State {
    /// Every live code, by its follow secret.
    entries: HashMap<[u8; 32], Entry>,
    /// Codes not yet resolved, to their follow secret.
    unspent: HashMap<String, [u8; 32]>,
    /// Report secrets, to their follow secret.
    reports: HashMap<[u8; 32], [u8; 32]>,
    misses: Budget,
    creates: Budget,
}

impl State {
    fn prune(&mut self, now_ms: u64) {
        let Self {
            entries,
            unspent,
            reports,
            ..
        } = self;
        entries.retain(|_, entry| {
            let live = entry.gone_at_ms() > now_ms;
            if !live {
                unspent.remove(&entry.code);
                if let Some(resolved) = &entry.resolution {
                    reports.remove(&resolved.report);
                }
            }
            live
        });
        unspent.retain(|_, follow| {
            entries
                .get(follow)
                .is_some_and(|entry| entry.resolution.is_none() && entry.expires_at_ms > now_ms)
        });
    }

    fn by_report(&mut self, report: &str) -> Result<&mut Entry, Refusal> {
        let report = secret(report).ok_or(Refusal::Malformed("a report secret is 32 bytes hex"))?;
        let follow = self.reports.get(&report).ok_or(Refusal::Unknown)?;
        self.entries.get_mut(follow).ok_or(Refusal::Unknown)
    }
}

/// The host's login codes.
pub(super) struct LoginCodes {
    state: std::sync::Mutex<State>,
    /// This host's own origin as pages reach it, which a code's link must
    /// name; `None` when the host knows no origin to check against.
    host_origin: Option<String>,
}

fn secret(hex_text: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(hex_text, &mut bytes).ok()?;
    Some(bytes)
}

impl LoginCodes {
    pub(super) fn new(host_origin: Option<String>) -> Self {
        Self {
            state: std::sync::Mutex::default(),
            host_origin,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        use tilo_lock::MutexRecover as _;
        self.state.lock_recover()
    }

    /// Run `f` on the codes still live at `now_ms`, under the lock.
    fn pruned<R>(&self, now_ms: u64, f: impl FnOnce(&mut State) -> R) -> R {
        let mut state = self.lock();
        state.prune(now_ms);
        let answer = f(&mut state);
        drop(state);
        answer
    }

    /// Make a code naming `link` (setting up) or nothing (signing in).
    pub(super) fn create(
        &self,
        link: Option<String>,
        client: Option<IpAddr>,
        now_ms: u64,
    ) -> Result<Created, Refusal> {
        if let Some(link) = &link {
            pairing_link(link, self.host_origin.as_deref()).map_err(Refusal::Malformed)?;
        }
        let mut state = self.lock();
        state.prune(now_ms);
        if !state
            .creates
            .within(client, now_ms, CREATES_PER_ADDRESS, u32::MAX)
        {
            return Err(Refusal::TooMany);
        }
        if state.entries.len() >= MAX_CODES {
            return Err(Refusal::Full);
        }
        state.creates.spend(client, now_ms);
        let code = loop {
            let code = code_from(rand::random::<[u8; CODE_LETTERS]>());
            if !state.unspent.contains_key(&code) {
                break code;
            }
        };
        let follow: [u8; 32] = rand::random();
        let expires_at_ms = now_ms.saturating_add(CODE_LIFETIME_MS);
        state.unspent.insert(code.clone(), follow);
        state.entries.insert(
            follow,
            Entry {
                code: code.clone(),
                link,
                expires_at_ms,
                resolution: None,
            },
        );
        drop(state);
        Ok(Created {
            code,
            follow: hex::encode(follow),
            expires_at_ms,
        })
    }

    /// Resolve `typed`, spending the code.
    pub(super) fn resolve(
        &self,
        typed: &str,
        client: Option<IpAddr>,
        now_ms: u64,
    ) -> Result<Resolved, Refusal> {
        let mut state = self.lock();
        state.prune(now_ms);
        if !state
            .misses
            .within(client, now_ms, MISSES_PER_ADDRESS, MISSES_IN_ALL)
        {
            return Err(Refusal::TooMany);
        }
        let follow = normalize_code(typed).and_then(|code| state.unspent.remove(&code));
        let Some(entry) = follow.and_then(|follow| state.entries.get_mut(&follow)) else {
            state.misses.spend(client, now_ms);
            return Err(Refusal::Unknown);
        };
        let report: [u8; 32] = rand::random();
        entry.resolution = Some(Resolution {
            report,
            at_ms: now_ms,
            steps: vec![Step::Resolved],
            agent: None,
            detail: None,
            claim: None,
        });
        let resolved = Resolved {
            link: entry.link.clone(),
            sign_in: entry.link.is_none(),
            report: hex::encode(report),
        };
        if let Some(follow) = follow {
            state.reports.insert(report, follow);
        }
        drop(state);
        Ok(resolved)
    }

    /// Record what the computer that resolved a code reports.
    pub(super) fn report(&self, report: &Report, now_ms: u64) -> Result<(), Refusal> {
        if report.step == Step::Approved {
            return Err(Refusal::Malformed(
                "the host records an approval itself, when it approves",
            ));
        }
        if report
            .agent
            .as_ref()
            .is_some_and(|agent| agent.chars().count() > MAX_AGENT_CHARS || agent.trim().is_empty())
        {
            return Err(Refusal::Malformed("an agent's name is 1 to 64 characters"));
        }
        if report
            .detail
            .as_ref()
            .is_some_and(|detail| detail.chars().count() > MAX_DETAIL_CHARS)
        {
            return Err(Refusal::Malformed(
                "a failure's sentence is at most 300 characters",
            ));
        }
        self.pruned(now_ms, |state| {
            let resolved = state
                .by_report(&report.report)?
                .resolution
                .as_mut()
                .ok_or(Refusal::Unknown)?;
            record(resolved, report.step);
            if let Some(agent) = &report.agent {
                resolved.agent = Some(agent.trim().to_owned());
            }
            if report.step == Step::Failed {
                resolved.detail.clone_from(&report.detail);
            }
            Ok(())
        })
    }

    /// Check that `report` names a sign-in code no device claimed yet.
    pub(super) fn claimable(&self, report: &str, now_ms: u64) -> Result<(), Refusal> {
        self.pruned(now_ms, |state| check_claimable(state.by_report(report)?))
    }

    /// Claim the sign-in code resolved with `report` for `user`, by device
    /// `by`: mint the invite that person's stand-in approves.
    pub(super) fn claim(
        &self,
        report: &str,
        user: [u8; 32],
        by: String,
        now_ms: u64,
    ) -> Result<u64, Refusal> {
        let mut state = self.lock();
        state.prune(now_ms);
        let entry = state.by_report(report)?;
        check_claimable(entry)?;
        let gone_at_ms = entry.gone_at_ms();
        let resolved = entry.resolution.as_mut().ok_or(Refusal::Unknown)?;
        let expires_at_ms = now_ms.saturating_add(CODE_LIFETIME_MS).min(gone_at_ms);
        resolved.claim = Some(Claim {
            user,
            invite: rand::random(),
            by,
            expires_at_ms,
            approved: false,
        });
        drop(state);
        Ok(expires_at_ms)
    }

    /// Whether a live claim for `user` minted `invite` and nothing was
    /// approved for it yet.
    pub(super) fn claimed(&self, user: &[u8; 32], invite: &[u8; 16], now_ms: u64) -> bool {
        self.pruned(now_ms, |state| {
            state
                .entries
                .values()
                .filter_map(|entry| entry.resolution.as_ref()?.claim.as_ref())
                .any(|claim| open_claim(claim, user, invite, now_ms))
        })
    }

    /// Take the claim for `user` that minted `invite`, so it approves
    /// nothing again; `true` when there was one to take.
    pub(super) fn take_claim(&self, user: &[u8; 32], invite: &[u8; 16], now_ms: u64) -> bool {
        self.pruned(now_ms, |state| {
            let claim = state.entries.values_mut().find_map(|entry| {
                entry
                    .resolution
                    .as_mut()?
                    .claim
                    .as_mut()
                    .filter(|claim| open_claim(claim, user, invite, now_ms))
            });
            claim.is_some_and(|claim| {
                claim.approved = true;
                true
            })
        })
    }

    /// Record that the request the claim for `user` minted `invite` for was
    /// approved.
    pub(super) fn approved(&self, user: &[u8; 32], invite: &[u8; 16]) {
        let mut state = self.lock();
        if let Some(resolved) = state.entries.values_mut().find_map(|entry| {
            let resolved = entry.resolution.as_mut()?;
            let claim = resolved.claim.as_ref()?;
            (claim.user == *user && claim.invite == *invite).then_some(resolved)
        }) {
            record(resolved, Step::Approved);
        }
    }

    /// Where the code `follow` names is.
    pub(super) fn follow(&self, follow: &str, now_ms: u64) -> Result<Polled, Refusal> {
        let follow = secret(follow).ok_or(Refusal::Malformed("a follow secret is 32 bytes hex"))?;
        Ok(self.pruned(now_ms, |state| Self::polled(state, &follow)))
    }

    fn polled(state: &State, follow: &[u8; 32]) -> Polled {
        // A secret the host does not know is one whose code it forgot: the
        // secret is too long to guess, so saying so tells nobody anything.
        let Some(entry) = state.entries.get(follow) else {
            return Polled {
                resolved: false,
                expired: true,
                agent: None,
                steps: Vec::new(),
                detail: None,
                claimed: None,
            };
        };
        let Some(resolved) = &entry.resolution else {
            return Polled {
                resolved: false,
                expired: false,
                agent: None,
                steps: Vec::new(),
                detail: None,
                claimed: None,
            };
        };
        Polled {
            resolved: true,
            expired: false,
            agent: resolved.agent.clone(),
            steps: resolved.steps.clone(),
            detail: resolved.detail.clone(),
            claimed: resolved.claim.as_ref().map(|claim| ClaimedFor {
                user: hex::encode(claim.user),
                invite: hex::encode(claim.invite),
                by: claim.by.clone(),
            }),
        }
    }
}

fn open_claim(claim: &Claim, user: &[u8; 32], invite: &[u8; 16], now_ms: u64) -> bool {
    claim.user == *user
        && claim.invite == *invite
        && !claim.approved
        && claim.expires_at_ms > now_ms
}

fn check_claimable(entry: &Entry) -> Result<(), Refusal> {
    if entry.link.is_some() {
        return Err(Refusal::Conflict(
            "this code sets a computer up; only a sign-in code is claimed",
        ));
    }
    match &entry.resolution {
        Some(resolved) if resolved.claim.is_none() => Ok(()),
        Some(_) => Err(Refusal::Conflict("this code is claimed already")),
        None => Err(Refusal::Unknown),
    }
}

/// Add `step` to what a code shows, once.
fn record(resolved: &mut Resolution, step: Step) {
    if !resolved.steps.contains(&step) {
        resolved.steps.push(step);
    }
}

/// Check `link` is a Tilo pairing link for this host: a web address whose
/// route carries `?pair=` with a person's root and an invite, and which names
/// this host (or is written on it). `tilo login` reads it with the full
/// grammar; this only keeps the host from naming anything else.
pub(super) fn pairing_link(link: &str, host_origin: Option<&str>) -> Result<(), &'static str> {
    if link.chars().count() > MAX_LINK_CHARS {
        return Err("a pairing link is at most 2048 characters");
    }
    if link.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("a pairing link has no spaces");
    }
    let (base, route) = link
        .split_once("#/")
        .ok_or("a pairing link is a Tilo link, like https://tilo.sh/#/w/<workspace>?pair=<code>")?;
    let site = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .ok_or("a pairing link is a web address")?;
    let site_authority = site.split(['/', '?']).next().unwrap_or_default();
    if site_authority.is_empty() || site_authority.contains('@') {
        return Err("a pairing link is a web address");
    }
    let (path, query) = route
        .split_once('?')
        .ok_or("a pairing link carries a pairing code")?;
    let pair = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("pair="))
        .ok_or("a pairing link carries a pairing code")?;
    let hex_lower = |text: &str| {
        text.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if pair.len() != 96 || !hex_lower(pair) {
        return Err("a pairing link names a person's root and a one-time invite");
    }
    let named_host = path
        .strip_prefix('@')
        .map(|rest| rest.split('/').next().unwrap_or_default());
    if !path.split('/').any(|segment| segment == "w") {
        return Err("a pairing link opens a workspace");
    }
    if let Some(origin) = host_origin {
        let ours = origin
            .split_once("://")
            .map_or(origin, |(_, authority)| authority)
            .trim_end_matches('/');
        // A loopback host is written with its scheme (`@https:127.0.0.1:9781`),
        // as the link grammar writes one: the authority is what follows it.
        let theirs = named_host.unwrap_or(site_authority);
        let theirs = theirs
            .strip_prefix("https:")
            .or_else(|| theirs.strip_prefix("http:"))
            .unwrap_or(theirs);
        if !theirs.eq_ignore_ascii_case(ours) {
            return Err("a pairing link names another host");
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn link() -> String {
        format!(
            "https://tilo.sh/#/@eu1.tilo.sh/w/6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71?pair={}{}",
            hex::encode([0xab; 32]),
            hex::encode([0x7f; 16])
        )
    }

    fn report(report: &str, step: Step) -> Report {
        Report {
            report: report.into(),
            step,
            agent: None,
            detail: None,
        }
    }

    #[test]
    fn a_code_resolves_once_and_its_tab_follows_each_step_once() {
        let codes = LoginCodes::new(Some("https://eu1.tilo.sh".into()));
        let created = codes.create(Some(link()), None, NOW).unwrap();
        assert_eq!(created.expires_at_ms, NOW + CODE_LIFETIME_MS);
        let polled = codes.follow(&created.follow, NOW).unwrap();
        assert!(!polled.resolved && !polled.expired);

        let typed = created.code.to_ascii_lowercase().replace('-', " ");
        let resolved = codes.resolve(&typed, None, NOW + 1).unwrap();
        assert_eq!(resolved.link, Some(link()));
        assert!(!resolved.sign_in);
        assert_eq!(
            codes.resolve(&created.code, None, NOW + 2),
            Err(Refusal::Unknown),
            "spent"
        );

        let mut logged_in = report(&resolved.report, Step::LoggedIn);
        logged_in.agent = Some("Claude Code".into());
        codes.report(&logged_in, NOW + 3).unwrap();
        codes.report(&logged_in, NOW + 4).unwrap();
        codes
            .report(&report(&resolved.report, Step::ToolsAdded), NOW + 5)
            .unwrap();
        assert_eq!(
            codes.report(&report(&hex::encode([1; 32]), Step::Entered), NOW),
            Err(Refusal::Unknown)
        );
        assert!(matches!(
            codes.report(&report(&resolved.report, Step::Approved), NOW),
            Err(Refusal::Malformed(_))
        ));
        let polled = codes.follow(&created.follow, NOW + 6).unwrap();
        assert!(polled.resolved);
        assert_eq!(polled.agent.as_deref(), Some("Claude Code"));
        assert_eq!(
            polled.steps,
            [Step::Resolved, Step::LoggedIn, Step::ToolsAdded]
        );

        let later = NOW + 1 + FOLLOW_LIFETIME_MS;
        assert!(codes.follow(&created.follow, later).unwrap().expired);
        assert_eq!(
            codes.report(&report(&resolved.report, Step::Entered), later),
            Err(Refusal::Unknown)
        );
    }

    #[test]
    fn a_code_nobody_resolves_expires() {
        let codes = LoginCodes::new(None);
        let created = codes.create(None, None, NOW).unwrap();
        let expired = NOW + CODE_LIFETIME_MS;
        assert_eq!(
            codes.resolve(&created.code, None, expired),
            Err(Refusal::Unknown)
        );
        assert!(codes.follow(&created.follow, expired).unwrap().expired);
    }

    #[test]
    fn guessing_codes_runs_out_of_tries() {
        let codes = LoginCodes::new(None);
        let created = codes.create(None, None, NOW).unwrap();
        let guesser = Some(IpAddr::from([203, 0, 113, 9]));
        for _ in 0..MISSES_PER_ADDRESS {
            assert_eq!(
                codes.resolve("222-2222", guesser, NOW),
                Err(Refusal::Unknown)
            );
        }
        assert_eq!(
            codes.resolve(&created.code, guesser, NOW),
            Err(Refusal::TooMany),
            "even a right code is not looked up past the budget"
        );
        let other = Some(IpAddr::from([198, 51, 100, 1]));
        assert!(codes.resolve(&created.code, other, NOW).is_ok());
        assert_eq!(
            codes.resolve("222-2222", guesser, NOW + WINDOW_MS),
            Err(Refusal::Unknown),
            "the next minute has a budget again"
        );

        let codes = LoginCodes::new(None);
        for index in 0..MISSES_IN_ALL {
            let client = IpAddr::from(index.to_be_bytes());
            let _ = codes.resolve("222-2222", Some(client), NOW);
        }
        assert_eq!(
            codes.resolve("222-2222", Some(IpAddr::from([9, 9, 9, 9])), NOW),
            Err(Refusal::TooMany),
            "misses from many addresses share one budget"
        );
    }

    #[test]
    fn the_host_holds_a_bounded_number_of_codes() {
        let codes = LoginCodes::new(None);
        for index in 0..MAX_CODES {
            let client = IpAddr::from(u32::try_from(index).unwrap().to_be_bytes());
            codes.create(None, Some(client), NOW).unwrap();
        }
        assert_eq!(codes.create(None, None, NOW), Err(Refusal::Full));
        assert!(codes.create(None, None, NOW + CODE_LIFETIME_MS).is_ok());
        let client = Some(IpAddr::from([1, 1, 1, 1]));
        let codes = LoginCodes::new(None);
        for _ in 0..CREATES_PER_ADDRESS {
            codes.create(None, client, NOW).unwrap();
        }
        assert_eq!(codes.create(None, client, NOW), Err(Refusal::TooMany));
    }

    #[test]
    fn a_sign_in_claim_mints_one_invite_approved_once() {
        let codes = LoginCodes::new(None);
        let created = codes.create(None, None, NOW).unwrap();
        let resolved = codes.resolve(&created.code, None, NOW).unwrap();
        assert!(resolved.sign_in && resolved.link.is_none());
        let user = [5; 32];
        codes
            .claim(&resolved.report, user, "Claude Code on box".into(), NOW)
            .unwrap();
        assert_eq!(
            codes.claimable(&resolved.report, NOW),
            Err(Refusal::Conflict("this code is claimed already"))
        );
        let claimed = codes.follow(&created.follow, NOW).unwrap().claimed.unwrap();
        assert_eq!(claimed.user, hex::encode(user));
        assert_eq!(claimed.by, "Claude Code on box");
        let mut invite = [0; 16];
        hex::decode_to_slice(&claimed.invite, &mut invite).unwrap();
        assert!(!codes.claimed(&[6; 32], &invite, NOW), "another person");
        assert!(codes.claimed(&user, &invite, NOW));
        assert!(codes.take_claim(&user, &invite, NOW));
        assert!(!codes.take_claim(&user, &invite, NOW), "once");
        codes.approved(&user, &invite);
        assert_eq!(
            codes.follow(&created.follow, NOW).unwrap().steps,
            [Step::Resolved, Step::Approved]
        );
    }

    #[test]
    fn a_setup_code_is_not_claimed() {
        let codes = LoginCodes::new(None);
        let created = codes.create(Some(link()), None, NOW).unwrap();
        let resolved = codes.resolve(&created.code, None, NOW).unwrap();
        assert!(matches!(
            codes.claimable(&resolved.report, NOW),
            Err(Refusal::Conflict(_))
        ));
    }

    #[test]
    fn a_code_names_only_a_pairing_link_for_this_host() {
        let ours = Some("https://eu1.tilo.sh");
        assert_eq!(pairing_link(&link(), ours), Ok(()));
        assert_eq!(pairing_link(&link(), None), Ok(()));
        assert!(pairing_link(&link(), Some("https://us1.tilo.sh")).is_err());
        let own = link().replace("tilo.sh/#/@eu1.tilo.sh/", "eu1.tilo.sh/#/");
        assert_eq!(pairing_link(&own, ours), Ok(()));
        // A loopback host, written with its scheme as the link grammar writes it.
        let loopback = link().replace(
            "tilo.sh/#/@eu1.tilo.sh/",
            "127.0.0.1:19782/#/@https:127.0.0.1:9781/",
        );
        assert_eq!(
            pairing_link(&loopback, Some("https://127.0.0.1:9781")),
            Ok(())
        );
        assert!(pairing_link(&loopback, Some("https://127.0.0.1:9782")).is_err());
        assert!(pairing_link(&link().replace("pair=", "invite="), ours).is_err());
        assert!(pairing_link(&link()[..link().len() - 2], ours).is_err());
        assert!(pairing_link("javascript:alert(1)", None).is_err());
        let long = format!("{}&x={}", link(), "a".repeat(MAX_LINK_CHARS));
        assert!(pairing_link(&long, None).is_err());
    }
}
