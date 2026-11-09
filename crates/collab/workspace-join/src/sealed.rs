//! The link that hands an invitation over without its kilobytes.
//!
//! A signed invitation is about a kilobyte and a half: certificates, transport
//! hints and signatures, none of which compress. Sealed, it fits a site's
//! shelf instead of the link: a fresh key seals the signed bytes, the site
//! keeps the ciphertext under a random handle, and the link carries only where
//! the handle is and, after its `#`, the key that opens it. A fragment is
//! never sent to a server, so the site holds bytes it cannot read.
//!
//! `https://tilo.sh/i/<handle>#<key>` is what a person hands over, and
//! what a code to scan carries. The long form — the invitation's own text —
//! is what every build has handed over, and a redeemer still takes it.

use base64::Engine as _;
use tilo_seal::{Context, Label, SymmetricKey};

/// The route a site serves a sealed invitation at, below its origin.
pub const SEALED_ROUTE: &str = "/i/";
/// Bytes of randomness behind a handle.
const HANDLE_BYTES: usize = 12;
/// A handle as the link spells it, and as the site's route checks it.
pub const HANDLE_CHARS: usize = 16;
/// The key that opens a sealed invitation, as the fragment spells it.
pub const KEY_CHARS: usize = 43;
/// The secret that takes a sealed invitation back, as the inviter keeps it.
pub const DELETE_CHARS: usize = 43;
/// The longest link a sealed invitation makes, origin and all.
pub const MAX_LINK_CHARS: usize = 512;

const ENGINE: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// An invitation sealed under a fresh key, with where a site keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    /// The handle the site keeps `ciphertext` under.
    pub handle: String,
    /// The signed invitation's bytes, sealed.
    pub ciphertext: Vec<u8>,
    /// The key that opens them: it travels in the link's fragment, never in
    /// a request.
    pub key: SymmetricKey,
    /// The secret that deletes them: the inviter keeps it, and it is in no
    /// link.
    pub delete: String,
}

impl Sealed {
    /// The ciphertext as a site's shelf carries it, base64url.
    #[must_use]
    pub fn sealed_text(&self) -> String {
        ENGINE.encode(&self.ciphertext)
    }

    /// The link a person hands over, at `site`'s origin.
    ///
    /// # Errors
    ///
    /// `site` is not an origin a link can name, or the link is longer than a
    /// code to scan should carry.
    pub fn link(&self, site: &str) -> Result<String, String> {
        let site = site.trim().trim_end_matches('/');
        if site.is_empty() || site.chars().any(char::is_whitespace) || !site.contains("://") {
            return Err("that is not a site an invitation can name".into());
        }
        let link = format!(
            "{site}{SEALED_ROUTE}{}#{}",
            self.handle,
            ENGINE.encode(self.key.expose_secret())
        );
        if link.chars().count() > MAX_LINK_CHARS {
            return Err("the link is longer than a code to scan".into());
        }
        Ok(link)
    }
}

/// Seal `invitation`, the signed bytes of one, under a fresh key at a fresh
/// handle.
///
/// # Errors
///
/// No randomness, or the invitation cannot be sealed.
pub fn seal(invitation: &[u8]) -> Result<Sealed, String> {
    let mut handle = [0; HANDLE_BYTES];
    tilo_seal::fill_random(&mut handle).map_err(|error| error.to_string())?;
    let handle = ENGINE.encode(handle);
    let mut delete = [0; 32];
    tilo_seal::fill_random(&mut delete).map_err(|error| error.to_string())?;
    let key = SymmetricKey::generate().map_err(|error| error.to_string())?;
    let ciphertext = tilo_seal::envelope::seal(
        &key,
        Context::new(Label::SealedInvitation, handle.as_bytes()),
        invitation,
    )
    .map_err(|error| error.to_string())?;
    Ok(Sealed {
        handle,
        ciphertext,
        key,
        delete: ENGINE.encode(delete),
    })
}

/// The signed invitation behind `ciphertext`, opened with `key`, which was
/// sealed at `handle`.
///
/// # Errors
///
/// The handle, the key or the ciphertext is not one this invitation was
/// sealed as: a key that does not open it, or bytes moved to another handle.
pub fn open(ciphertext: &[u8], handle: &str, key: &str) -> Result<Vec<u8>, String> {
    let key = open_key(key)?;
    let opened = tilo_seal::envelope::open(
        &key,
        Context::new(Label::SealedInvitation, handle.as_bytes()),
        ciphertext,
    )
    .map_err(|_| "that link does not open the invitation it names".to_string())?;
    Ok(opened.to_vec())
}

/// The signed invitation a site's shelf answered with, decoded from the
/// text it carries and opened with the link's `key`.
///
/// # Errors
///
/// As [`open`], and a shelf text that is not base64url.
pub fn open_sealed(sealed: &str, handle: &str, key: &str) -> Result<Vec<u8>, String> {
    let ciphertext = ENGINE
        .decode(sealed)
        .map_err(|_| "that site answered with something that is not an invitation".to_string())?;
    open(&ciphertext, handle, key)
}

/// The key a link's fragment spells, when it is one.
fn open_key(key: &str) -> Result<SymmetricKey, String> {
    if key.chars().count() != KEY_CHARS {
        return Err("that link carries no invitation key".into());
    }
    let bytes = ENGINE
        .decode(key)
        .map_err(|_| "that link carries no invitation key".to_string())?;
    SymmetricKey::from_slice(&bytes).map_err(|_| "that link carries no invitation key".into())
}

/// A sealed invitation's link, read apart: where the site is, the handle it
/// keeps the bytes under, and the key that opens them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedLink {
    /// The site's origin, without the [`SEALED_ROUTE`].
    pub site: String,
    /// The handle the site keeps the invitation under.
    pub handle: String,
    /// The key the fragment carries.
    pub key: String,
}

impl SealedLink {
    /// What a link of another site carries instead of the shelf's route: the
    /// handle and the key, which together open the invitation at its shelf.
    /// A page whose own link opens it puts this in its fragment, so the link
    /// stays short and the site it is opened at reads its own route.
    #[must_use]
    pub fn ref_text(&self) -> String {
        format!("{}{}", self.handle, self.key)
    }

    /// The link as a person hands it over.
    #[must_use]
    pub fn to_text(&self) -> String {
        format!(
            "{}{SEALED_ROUTE}{}#{}",
            self.site.trim_end_matches('/'),
            self.handle,
            self.key
        )
    }
}

/// The sealed invitation `text` is a link to, when it is one.
#[must_use]
pub fn parse_link(text: &str) -> Option<SealedLink> {
    let text = text.trim();
    if text.chars().count() > MAX_LINK_CHARS
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let (before, key) = text.split_once('#')?;
    if !spelled(key, KEY_CHARS) {
        return None;
    }
    let (site, handle) = before.rsplit_once(SEALED_ROUTE)?;
    if !spelled(handle, HANDLE_CHARS)
        || !(site.starts_with("https://") || site.starts_with("http://"))
    {
        return None;
    }
    Some(SealedLink {
        site: site.to_owned(),
        handle: handle.to_owned(),
        key: key.to_owned(),
    })
}

/// The handle and key a link's fragment carries ([`SealedLink::ref_text`]),
/// read apart: the shelf that keeps the bytes, and the key that opens them.
#[must_use]
pub fn parse_ref(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    if !spelled(text, HANDLE_CHARS + KEY_CHARS) {
        return None;
    }
    Some(text.split_at(HANDLE_CHARS))
}

/// Whether `text` is `chars` characters of base64url.
fn spelled(text: &str, chars: usize) -> bool {
    text.chars().count() == chars
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::*;

    const INVITATION: &[u8] = b"the signed invitation's own bytes";

    #[test]
    fn a_sealed_invitation_opens_to_what_was_sealed() {
        let sealed = seal(INVITATION).unwrap();
        assert_ne!(
            sealed.ciphertext, INVITATION,
            "the site never holds the invitation"
        );
        let key = ENGINE.encode(sealed.key.expose_secret());
        assert_eq!(
            open(&sealed.ciphertext, &sealed.handle, &key).unwrap(),
            INVITATION
        );
        // What a site answers with is the same bytes it was given.
        assert_eq!(
            open_sealed(&sealed.sealed_text(), &sealed.handle, &key).unwrap(),
            INVITATION
        );
        assert!(open_sealed("not base64url at all!", &sealed.handle, &key).is_err());
        assert_eq!(
            sealed.handle.chars().count(),
            HANDLE_CHARS,
            "a handle as the route spells it"
        );
        assert_eq!(key.chars().count(), KEY_CHARS);
        assert_eq!(sealed.delete.chars().count(), DELETE_CHARS);
        // A fresh key every time: the same invitation twice is two links.
        let again = seal(INVITATION).unwrap();
        assert_ne!(again.handle, sealed.handle);
        assert_ne!(
            again.key.expose_secret().as_slice(),
            sealed.key.expose_secret().as_slice()
        );
    }

    #[test]
    fn a_key_that_is_not_its_own_opens_nothing() {
        let sealed = seal(INVITATION).unwrap();
        let other = seal(INVITATION).unwrap();
        let key = ENGINE.encode(other.key.expose_secret());
        assert!(open(&sealed.ciphertext, &sealed.handle, &key).is_err());
        let own = ENGINE.encode(sealed.key.expose_secret());
        assert!(
            open(&sealed.ciphertext, &other.handle, &own).is_err(),
            "ciphertext moved to another handle is not the invitation it names"
        );
        assert!(open(&sealed.ciphertext, &sealed.handle, "short").is_err());
    }

    #[test]
    fn the_link_names_only_where_it_is_and_the_key() {
        let sealed = seal(INVITATION).unwrap();
        let link = sealed.link("https://tilo.sh/").unwrap();
        let (before, fragment) = link.split_once('#').unwrap();
        assert_eq!(before, format!("https://tilo.sh/i/{}", sealed.handle));
        assert!(!before.contains(&ENGINE.encode(sealed.delete)));
        assert_eq!(
            ENGINE.decode(fragment).unwrap().as_slice(),
            sealed.key.expose_secret().as_slice(),
        );

        let read = parse_link(&link).unwrap();
        assert_eq!(read.site, "https://tilo.sh");
        assert_eq!(read.handle, sealed.handle);
        assert_eq!(read.to_text(), link);

        // A code to scan carries it whole: a version 6 code holds 106 bytes.
        assert!(
            link.len() <= 106,
            "the link is {} characters: {link}",
            link.len()
        );
        assert!(
            parse_link(link.trim()).is_some(),
            "a scanned link reads back as the same one"
        );
    }

    #[test]
    fn an_invitations_own_text_is_not_a_sealed_link() {
        assert!(parse_link(&encode_text(INVITATION)).is_none());
        assert!(parse_link("https://tilo.sh/i/").is_none());
        assert!(parse_link("https://tilo.sh/i/abcdefghijklmnop").is_none());
        assert!(parse_link("https://tilo.sh/i/abcdefghijklmnop#short").is_none());
        assert!(
            parse_link(&format!(
                "ftp://tilo.sh/i/abcdefghijklmnop#{}",
                "a".repeat(KEY_CHARS)
            ))
            .is_none(),
            "a link names a site by http or https"
        );
        assert!(parse_link("not a link with a space/i/abcdefghijklmnop").is_none());
    }

    #[test]
    fn the_fragment_of_a_link_reads_back_as_its_handle_and_key() {
        let sealed = seal(INVITATION).unwrap();
        let key = ENGINE.encode(sealed.key.expose_secret());
        let text = SealedLink {
            site: "https://tilo.sh".to_owned(),
            handle: sealed.handle.clone(),
            key: key.clone(),
        }
        .ref_text();
        let (handle, read) = parse_ref(&text).unwrap();
        assert_eq!(handle, sealed.handle);
        assert_eq!(read, key, "the key the site opens it with");
        // An invitation's own text names no shelf and opens here already.
        assert!(parse_ref(&encode_text(INVITATION)).is_none());
        assert!(parse_ref(&format!("{text}x")).is_none());
        assert!(parse_ref("not base64url at all").is_none());
        let padded = format!(" {text} ");
        assert_eq!(parse_ref(&padded).unwrap(), (handle, read));
    }

    fn encode_text(bytes: &[u8]) -> String {
        crate::encode_invitation_text(bytes)
    }
}
