//! Pure data model for the Authenticator vault.
//!
//! An [`Account`] is one 2FA entry: the issuer + account label shown in
//! the list, plus the cryptographic parameters needed to derive a rotating
//! TOTP code (secret, algorithm, digits, period). Only secret-free account
//! metadata is mirrored into Automerge for collaboration; the encrypted local
//! vault remains the source of bearer keys and live code generation.
//!
//! This module is pure logic — no IO, no rendering, no crypto. Code
//! generation lives in [`crate::totp`]; CRDT persistence in
//! [`crate::doc`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vello::peniko::Color;

use ui::theme::Theme;

/// Stable identifier for one account. Backs every cross-reference and
/// the per-account Map key in the CRDT.
pub type AccountId = Uuid;

/// HMAC hash used by the TOTP construction. Mirrors the `algorithm`
/// parameter of an `otpauth://` URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Algorithm {
    #[default]
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    /// Canonical otpauth spelling (`SHA1` / `SHA256` / `SHA512`).
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha1 => "SHA1",
            Algorithm::Sha256 => "SHA256",
            Algorithm::Sha512 => "SHA512",
        }
    }

    /// Parse the otpauth spelling, case-insensitively. Unknown values
    /// fall back to the spec default `SHA1`.
    pub fn parse(s: &str) -> Algorithm {
        match s.trim().to_ascii_uppercase().as_str() {
            "SHA256" => Algorithm::Sha256,
            "SHA512" => Algorithm::Sha512,
            _ => Algorithm::Sha1,
        }
    }

    /// All three, in selector order — for the algorithm chip row.
    pub const ALL: [Algorithm; 3] = [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha512];

    pub fn index(self) -> usize {
        match self {
            Algorithm::Sha1 => 0,
            Algorithm::Sha256 => 1,
            Algorithm::Sha512 => 2,
        }
    }
}

/// One 2FA account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    /// Service name, e.g. "GitHub". Drives the avatar initials + colour.
    pub issuer: String,
    /// Account label, e.g. "ada@example.com".
    pub label: String,
    /// Base32 (RFC 4648) secret, normalised upper-case, no spaces/padding.
    pub secret: String,
    pub algorithm: Algorithm,
    /// Number of digits in the generated code (6 or 8 in practice).
    pub digits: u8,
    /// Rotation period in seconds (almost always 30).
    pub period: u32,
    /// Index into the theme-derived account palette (see
    /// [`account_palette`]). Wraps modulo the palette length.
    pub color: u8,
    /// Position in the list (smaller = earlier). Re-densified on
    /// reorder / delete.
    pub order: u32,
}

impl Account {
    /// A blank account with spec defaults, ready to be filled by the
    /// add form. `order` is set by the vault on insert.
    pub fn blank(id: AccountId) -> Self {
        Account {
            id,
            issuer: String::new(),
            label: String::new(),
            secret: String::new(),
            algorithm: Algorithm::Sha1,
            digits: 6,
            period: 30,
            color: 0,
            order: 0,
        }
    }

    /// Two-letter avatar initials derived from the issuer (falling back
    /// to the label). Upper-cased, ≤ 2 chars. Multi-word names take the
    /// first letter of each of the first two words ("Amazon Web" → "AW");
    /// a single word takes its first two letters ("GitHub" → "GI") so the
    /// avatar stays distinctive rather than collapsing to one glyph.
    pub fn initials(&self) -> String {
        let source = if self.issuer.trim().is_empty() {
            self.label.trim()
        } else {
            self.issuer.trim()
        };
        let words: Vec<&str> = source.split_whitespace().collect();
        let out: String = match words.as_slice() {
            [] => String::new(),
            [single] => single.chars().take(2).collect(),
            [first, second, ..] => first
                .chars()
                .next()
                .into_iter()
                .chain(second.chars().next())
                .collect(),
        };
        out.to_uppercase().chars().take(2).collect()
    }

    /// Human display title: "Issuer" or, when there is no issuer, the
    /// label. Never empty for a well-formed account.
    pub fn title(&self) -> &str {
        if self.issuer.trim().is_empty() {
            &self.label
        } else {
            &self.issuer
        }
    }

    /// True when the secret decodes to a non-empty key — a cheap
    /// validity gate the add form uses to enable Save.
    pub fn secret_is_valid(&self) -> bool {
        crate::totp::decode_secret(&self.secret).is_some_and(|b| !b.is_empty())
    }
}

/// In-memory vault: an ordered list of accounts plus the runtime ops the
/// UI needs (add / delete / reorder). Pure logic; the app mirrors it to
/// the CRDT.
#[derive(Clone, Debug, Default)]
pub struct Vault {
    pub accounts: Vec<Account>,
}

impl Vault {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accounts sorted by `order`.
    pub fn ordered(&self) -> Vec<Account> {
        let mut v = self.accounts.clone();
        v.sort_by_key(|a| a.order);
        v
    }

    pub fn get(&self, id: AccountId) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Insert (appending at the end) or replace an account by id. The
    /// `order` of a brand-new account is set to the current end; an
    /// existing account keeps its slot.
    pub fn upsert(&mut self, mut acc: Account) {
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.id == acc.id) {
            acc.order = existing.order;
            *existing = acc;
        } else {
            acc.order = self.next_order();
            self.accounts.push(acc);
        }
    }

    /// The `order` value a brand-new account should take to land at the end of
    /// the list (one past the current maximum). Computed at *authoring* time and
    /// baked into the add action so replay stays deterministic.
    pub fn next_order(&self) -> u32 {
        self.accounts
            .iter()
            .map(|a| a.order)
            .max()
            .map_or(0, |m| m + 1)
    }

    /// Remove an account and re-densify the remaining order values.
    pub fn delete(&mut self, id: AccountId) {
        self.accounts.retain(|a| a.id != id);
        self.renumber();
    }

    /// Swap an account one slot earlier (towards index 0). No-op at the
    /// top. Returns true if anything moved.
    pub fn move_up(&mut self, id: AccountId) -> bool {
        let mut ord = self.ordered();
        let Some(pos) = ord.iter().position(|a| a.id == id) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        ord.swap(pos, pos - 1);
        self.apply_order(&ord);
        true
    }

    /// Swap an account one slot later. No-op at the bottom.
    pub fn move_down(&mut self, id: AccountId) -> bool {
        let mut ord = self.ordered();
        let Some(pos) = ord.iter().position(|a| a.id == id) else {
            return false;
        };
        if pos + 1 >= ord.len() {
            return false;
        }
        ord.swap(pos, pos + 1);
        self.apply_order(&ord);
        true
    }

    fn apply_order(&mut self, ordered: &[Account]) {
        for (i, a) in ordered.iter().enumerate() {
            if let Some(target) = self.accounts.iter_mut().find(|x| x.id == a.id) {
                target.order = i as u32;
            }
        }
    }

    /// Rewrite `order` to a contiguous 0..n sequence preserving the
    /// current relative order.
    pub fn renumber(&mut self) {
        let ord = self.ordered();
        self.apply_order(&ord);
    }

    /// Filter accounts by a free-text query against issuer + label,
    /// case-insensitive. Empty query returns everything (ordered).
    pub fn search(&self, query: &str) -> Vec<Account> {
        let q = query.trim().to_lowercase();
        let ordered = self.ordered();
        if q.is_empty() {
            return ordered;
        }
        ordered
            .into_iter()
            .filter(|a| a.issuer.to_lowercase().contains(&q) || a.label.to_lowercase().contains(&q))
            .collect()
    }
}

/// Parse an `otpauth://totp/...` URI into an [`Account`]. Returns
/// `Err(reason)` on malformed input. Handles:
///
/// * `otpauth://totp/Issuer:account@host?secret=...&issuer=...&...`
/// * percent-encoded label / issuer
/// * `secret` (required), `issuer`, `algorithm`, `digits`, `period`
/// * issuer taken from the query param, else from the `Issuer:` label
///   prefix.
///
/// `hotp://` (counter-based) is rejected with a clear message — this
/// app is time-based only.
///
/// # Errors
/// Returns `Err` when the string doesn't start with `otpauth://`, names an
/// unsupported type (`hotp://`, or anything other than `totp`), or omits
/// the required `secret` query parameter.
pub fn parse_otpauth_uri(uri: &str, new_id: AccountId) -> Result<Account, String> {
    let uri = uri.trim();
    let rest = uri
        .strip_prefix("otpauth://")
        .ok_or_else(|| "not an otpauth:// URI".to_string())?;

    // type / label-and-query
    let (otp_type, after_type) = match rest.split_once('/') {
        Some((t, r)) => (t.to_ascii_lowercase(), r),
        None => (rest.to_ascii_lowercase(), ""),
    };
    if otp_type == "hotp" {
        return Err("counter-based hotp:// is not supported — TOTP only".to_string());
    }
    if otp_type != "totp" {
        return Err(format!("unsupported otpauth type '{otp_type}'"));
    }

    let (label_raw, query_raw) = match after_type.split_once('?') {
        Some((l, q)) => (l, q),
        None => (after_type, ""),
    };
    // `otpauth://` is a URI, not a form submission: the label is a *path*
    // segment, so RFC 3986 applies and `+` is a literal plus. Decoding it as a
    // space would rename `Google+` to `Google ` and mangle a leading `+` on a
    // phone-number account name.
    let label = ui::uri::decode(label_raw);

    // Query params.
    let mut secret = String::new();
    let mut issuer_param = String::new();
    let mut algorithm = Algorithm::Sha1;
    let mut digits: u8 = 6;
    let mut period: u32 = 30;
    for pair in query_raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = k.to_ascii_lowercase();
        let val = ui::uri::decode(v);
        match key.as_str() {
            "secret" => secret = normalise_secret(&val),
            "issuer" => issuer_param = val,
            "algorithm" => algorithm = Algorithm::parse(&val),
            "digits" => digits = val.parse().unwrap_or(6).clamp(4, 10),
            "period" => period = val.parse().unwrap_or(30).clamp(5, 300),
            _ => {}
        }
    }

    if secret.is_empty() {
        return Err("otpauth URI is missing the required 'secret' param".to_string());
    }

    // Split "Issuer:account" label. The issuer query param wins when set.
    let (label_issuer, account_label) = match label.split_once(':') {
        Some((iss, acc)) => (iss.trim().to_string(), acc.trim().to_string()),
        None => (String::new(), label.trim().to_string()),
    };
    let issuer = if issuer_param.trim().is_empty() {
        label_issuer
    } else {
        issuer_param.trim().to_string()
    };

    Ok(Account {
        id: new_id,
        issuer,
        label: account_label,
        secret,
        algorithm,
        digits,
        period,
        color: 0,
        order: 0,
    })
}

/// Normalise a base32 secret: strip whitespace + padding, upper-case.
pub fn normalise_secret(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .collect::<String>()
        .to_uppercase()
}

/// Reject obvious pasted TOTP material in a display identity before it can
/// enter a searchable resource, action envelope or collaborative history.
pub fn contains_secret_shaped_text(value: &str) -> bool {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|run| run.len() >= 16 && run.bytes().all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7')))
}

/// The per-account colour palette, sourced entirely from the active
/// [`Theme`]'s tokens — no hard-coded literals — so it tracks the OS
/// theme. Picks visually-distinct hues from the semantic + accent ramps
/// so a roster of accounts reads as a varied palette rather than a wall
/// of indigo.
pub fn account_palette(theme: &Theme) -> Vec<Color> {
    let c = &theme.tokens.colors;
    vec![
        c.accent[5],        // indigo-500
        c.semantic.success, // emerald
        c.semantic.warning, // amber
        c.semantic.danger,  // red
        c.semantic.info,    // blue
        c.accent[7],        // indigo-300 (lighter violet)
        c.neutral[6],       // slate-400
    ]
}

/// Resolve an account's avatar colour from its `color` index against the
/// theme palette (wrapping).
pub fn account_color(theme: &Theme, color_index: u8) -> Color {
    let palette = account_palette(theme);
    // `account_palette` always returns a fixed, non-empty set, so this is
    // unreachable in practice — `.get()` documents that instead of indexing
    // straight into the palette, and falls back to a themed colour (never a
    // hard-coded literal) rather than panicking.
    if palette.is_empty() {
        return theme.tokens.colors.text_primary;
    }
    let idx = (color_index as usize) % palette.len();
    palette
        .get(idx)
        .copied()
        .unwrap_or(theme.tokens.colors.text_primary)
}

/// First-run demo vault: a handful of recognisable services with real,
/// valid base32 secrets so the list renders live-looking codes + rings
/// out of the box (and so the `--shots` frame is populated). These are
/// throwaway demo secrets, not real credentials.
pub fn demo_accounts() -> Vec<Account> {
    let specs: [(&str, &str, &str, Algorithm, u8, u32, u8); 5] = [
        (
            "GitHub",
            "ada@example.com",
            "JBSWY3DPEHPK3PXP",
            Algorithm::Sha1,
            6,
            30,
            0,
        ),
        (
            "Google",
            "ada.lovelace@gmail.com",
            "GEZDGNBVGY3TQOJQGEZDGNBV",
            Algorithm::Sha1,
            6,
            30,
            4,
        ),
        (
            "AWS",
            "iam/ada-ops",
            "NB2W45DFOIZA====",
            Algorithm::Sha256,
            6,
            30,
            2,
        ),
        (
            "Dropbox",
            "ada@work.io",
            "KRSXG5CTMVRXEZLU",
            Algorithm::Sha1,
            8,
            30,
            1,
        ),
        (
            "Discord",
            "ada#4242",
            "MFRGGZDFMZTWQ2LK",
            Algorithm::Sha512,
            6,
            30,
            3,
        ),
    ];
    specs
        .iter()
        .enumerate()
        .map(
            |(i, (issuer, label, secret, alg, digits, period, color))| Account {
                // Fixed ids: a screenshot shows the same accounts every run.
                id: Uuid::from_u128(0x7070_0000_0000_0000_0000_0000_0000_0000 + i as u128),
                issuer: issuer.to_string(),
                label: label.to_string(),
                secret: normalise_secret(secret),
                algorithm: *alg,
                digits: *digits,
                period: *period,
                color: *color,
                order: i as u32,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// otpauth URI parsing.
    #[test]
    fn otpauth_uri_parsing_keeps_issuer_labels_and_rejects_invalid() {
        // Case `parse_full_otpauth_uri`.
        {
            let uri = "otpauth://totp/GitHub:ada%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=GitHub&algorithm=SHA256&digits=8&period=60";
            let acc = parse_otpauth_uri(uri, Uuid::new_v4()).unwrap();
            assert_eq!(acc.issuer, "GitHub");
            assert_eq!(acc.label, "ada@example.com");
            assert_eq!(acc.secret, "JBSWY3DPEHPK3PXP");
            assert_eq!(acc.algorithm, Algorithm::Sha256);
            assert_eq!(acc.digits, 8);
            assert_eq!(acc.period, 60);
        }

        // Case `parse_uri_issuer_from_label_prefix`.
        {
            let uri = "otpauth://totp/ACME%20Co:bob?secret=JBSWY3DPEHPK3PXP";
            let acc = parse_otpauth_uri(uri, Uuid::new_v4()).unwrap();
            assert_eq!(acc.issuer, "ACME Co");
            assert_eq!(acc.label, "bob");
            // spec defaults
            assert_eq!(acc.algorithm, Algorithm::Sha1);
            assert_eq!(acc.digits, 6);
            assert_eq!(acc.period, 30);
        }

        // Case `a_plus_in_a_label_or_issuer_stays_a_plus`.
        {
            // `otpauth://` is a URI, so RFC 3986 governs both the label path
            // segment and the query values: `+` is a literal plus, not a space.
            // A space is spelled `%20`, as `parse_uri_issuer_from_label_prefix`
            // shows. Reading `+` as a space renamed `Google+` to `Google `.
            let uri =
                "otpauth://totp/Google+:a%2B1%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=Notion+";
            let acc = parse_otpauth_uri(uri, Uuid::new_v4()).unwrap();
            assert_eq!(
                acc.issuer, "Notion+",
                "an explicit issuer param keeps its plus"
            );
            assert_eq!(
                acc.label, "a+1@example.com",
                "%2B decodes, the bare + stays"
            );

            // And with no issuer param, the label prefix supplies it — also intact.
            let uri = "otpauth://totp/Google+:ada?secret=JBSWY3DPEHPK3PXP";
            let acc = parse_otpauth_uri(uri, Uuid::new_v4()).unwrap();
            assert_eq!(acc.issuer, "Google+");
            assert_eq!(acc.label, "ada");
        }

        // Case `parse_rejects_hotp_and_missing_secret`.
        {
            assert!(parse_otpauth_uri("otpauth://hotp/x?secret=AAAA", Uuid::new_v4()).is_err());
            assert!(parse_otpauth_uri("otpauth://totp/x?issuer=y", Uuid::new_v4()).is_err());
            assert!(parse_otpauth_uri("https://example.com", Uuid::new_v4()).is_err());
        }
    }

    /// Vault renumbering on reorder and two-letter initials.
    #[test]
    fn vault_reorder_and_initials() {
        // Case `vault_reorder_renumbers`.
        {
            let mut v = Vault::new();
            for a in demo_accounts() {
                v.upsert(a);
            }
            let first = v.ordered()[0].id;
            let second = v.ordered()[1].id;
            assert!(v.move_down(first));
            assert_eq!(v.ordered()[0].id, second);
            assert_eq!(v.ordered()[1].id, first);
            // orders stay contiguous
            for (i, a) in v.ordered().iter().enumerate() {
                assert_eq!(a.order, i as u32);
            }
        }

        // Case `initials_are_two_upper_chars`.
        {
            let mut a = Account::blank(Uuid::new_v4());
            a.issuer = "GitHub".into();
            assert_eq!(a.initials(), "GI");
            a.issuer = "Amazon Web".into();
            assert_eq!(a.initials(), "AW");
        }
    }
}
