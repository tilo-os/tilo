//! The once-only link that adds a new device of the person, and the invites it carries.
//!
//! The link is the workspace link with `pair=<person><invite>`: the person's root in hex, then a
//! random invite. Opening it on a phone or in a browser asks the workspace's host to certify that
//! device as the person's, and one of their devices that holds the root answers, showing both
//! screens the same code. The invite is what makes the link once-only: it is made here, works for
//! [`INVITE_LIFETIME`] and is dropped once answered, so a link seen over someone's shoulder does
//! not become a device.
//!
//! One invite is open at a time, because one is what a sheet shows: asking again while it still
//! works answers the same link, and asking once it ran out makes a new one — the code on screen is
//! never one that admits nothing.

use std::time::Duration;

use tilo_reference::link::{
    HostOrigin, Link, LinkSecrets, LinkTarget, Mode, Scope, Secret, WebEntry,
};

/// How long a link works: ten minutes, as the sheet says.
pub const INVITE_LIFETIME: Duration = Duration::from_secs(600);

/// One link as the sheet shows it: where it goes, the invite it carries, and the millisecond it
/// stops admitting a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub link: String,
    pub invite: [u8; 16],
    pub expires_at_ms: u64,
}

/// The invite this device made and has not spent.
struct Open {
    invite: [u8; 16],
    link: String,
    expires_at_ms: u64,
}

/// The invites this device made: at most one open, minted from what a link needs and renewed as it
/// runs out.
pub struct Invites {
    ttl: Duration,
    open: Option<Open>,
}

impl Default for Invites {
    fn default() -> Self {
        Self {
            ttl: INVITE_LIFETIME,
            open: None,
        }
    }
}

impl Invites {
    /// The link to the workspace `room` on `origin`, through the site `site`, as the person
    /// `person` whose device a newcomer becomes: the one open, or a fresh one when it ran out.
    ///
    /// # Errors
    ///
    /// `room` is no workspace id, or `site` or `origin` is no web address a link may name.
    pub fn at(
        &mut self,
        site: &str,
        origin: &str,
        room: &str,
        person: &[u8; 32],
        now_ms: u64,
    ) -> Result<Shown, String> {
        if let Some(open) = self
            .open
            .as_ref()
            .filter(|open| open.expires_at_ms > now_ms)
        {
            return Ok(Shown {
                link: open.link.clone(),
                invite: open.invite,
                expires_at_ms: open.expires_at_ms,
            });
        }
        let invite = fresh_invite()?;
        let mut secret = String::with_capacity(96);
        secret.push_str(&hex::encode(person));
        secret.push_str(&hex::encode(invite));
        let link = link(site, origin, room, &secret)?;
        let expires_at_ms =
            now_ms.saturating_add(u64::try_from(self.ttl.as_millis()).unwrap_or(u64::MAX));
        self.open = Some(Open {
            invite,
            link: link.clone(),
            expires_at_ms,
        });
        Ok(Shown {
            link,
            invite,
            expires_at_ms,
        })
    }
}

/// A fresh invite: sixteen random bytes.
fn fresh_invite() -> Result<[u8; 16], String> {
    let mut invite = [0; 16];
    getrandom::fill(&mut invite).map_err(|error| error.to_string())?;
    Ok(invite)
}

/// The workspace link to `room` on `origin`, through `site`, carrying `secret` as its pairing.
fn link(site: &str, origin: &str, room: &str, secret: &str) -> Result<String, String> {
    let entry = WebEntry::new(site).map_err(|error| error.to_string())?;
    let host = HostOrigin::new(origin).map_err(|error| error.to_string())?;
    let id = room
        .parse()
        .map_err(|_| "that workspace is not one a link can name".to_owned())?;
    let scope = Scope::Room {
        host,
        id,
        slug: None,
    };
    let pair = Secret::new(secret).map_err(|error| error.to_string())?;
    Link {
        mode: Mode::Desktop,
        target: LinkTarget::new(scope),
        secrets: LinkSecrets {
            pair: Some(pair),
            ..LinkSecrets::default()
        },
    }
    .to_url(&entry)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::*;
    use tilo_reference::link::WebEntries;

    const SITE: &str = "https://tilo.coffee/";
    const ORIGIN: &str = "https://eu1.tilo.coffee";
    const ROOM: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const PERSON: [u8; 32] = [0xab; 32];
    const START: u64 = 1_700_000_000_000;

    /// The `pair` secret the link `url` carries, as the page and the host read it.
    fn pair_of(url: &str) -> String {
        let entries = WebEntries::new(vec![WebEntry::new(SITE).unwrap()]);
        let link = Link::parse(url, &entries).expect("the link this device made reads back");
        link.secrets
            .pair
            .expect("a pairing link")
            .expose()
            .to_owned()
    }

    #[test]
    fn the_code_carries_the_person_and_the_invite_it_was_made_with() {
        let mut invites = Invites::default();
        let shown = invites.at(SITE, ORIGIN, ROOM, &PERSON, START).unwrap();
        assert_eq!(
            pair_of(&shown.link),
            format!("{}{}", hex::encode(PERSON), hex::encode(shown.invite))
        );
        assert_eq!(shown.expires_at_ms, START + 600_000);
        assert!(
            shown.link.contains("/#/@eu1.tilo.coffee/w/"),
            "{}",
            shown.link
        );
    }

    #[test]
    fn a_code_that_ran_out_is_replaced_by_one_that_works() {
        let mut invites = Invites::default();
        let first = invites.at(SITE, ORIGIN, ROOM, &PERSON, START).unwrap();
        // While it works the sheet shows the same code: a screen that redrew it every pass would
        // hand out a new invite each time.
        let again = invites
            .at(SITE, ORIGIN, ROOM, &PERSON, START + 599_999)
            .unwrap();
        assert_eq!(again, first);
        let renewed = invites
            .at(SITE, ORIGIN, ROOM, &PERSON, first.expires_at_ms)
            .unwrap();
        assert_ne!(renewed.invite, first.invite);
        assert_ne!(renewed.link, first.link);
        assert_eq!(renewed.expires_at_ms, first.expires_at_ms + 600_000);
        assert_eq!(
            pair_of(&renewed.link),
            format!("{}{}", hex::encode(PERSON), hex::encode(renewed.invite))
        );
    }

    /// What a camera reads from the code the sheet draws: the link, byte for byte, read by a
    /// decoder that knows nothing of the widget.
    #[test]
    fn a_scanner_reads_the_link_from_the_code_the_sheet_draws() {
        use ui::widget::QrWidget;
        let mut invites = Invites::default();
        let shown = invites.at(SITE, ORIGIN, ROOM, &PERSON, START).unwrap();
        let drawn = QrWidget::new(shown.link.clone().into_bytes());
        let px = tilo_sheets::pair_device::module_px(drawn.total_modules());
        let code = drawn.with_module_px(px);
        assert!(code.error().is_none(), "the link fits a code");
        let quiet = code.quiet_modules();
        let modules = code.total_modules().saturating_sub(quiet.saturating_mul(2));
        let scale = usize::try_from(px).unwrap();
        let side = u32::try_from(code.total_modules() * scale).unwrap();
        let mut luma = image::GrayImage::from_pixel(side, side, image::Luma([255]));
        for y in 0..modules {
            for x in 0..modules {
                if !code.module(x, y) {
                    continue;
                }
                let left = u32::try_from((x + quiet) * scale).unwrap();
                let top = u32::try_from((y + quiet) * scale).unwrap();
                for dy in 0..u32::try_from(scale).unwrap() {
                    for dx in 0..u32::try_from(scale).unwrap() {
                        luma.put_pixel(left + dx, top + dy, image::Luma([0]));
                    }
                }
            }
        }
        let mut prepared = rqrr::PreparedImage::prepare(luma);
        let grids = prepared.detect_grids();
        let grid = grids.first().expect("a scanner finds the code");
        let mut read = Vec::new();
        grid.decode_to(&mut read).expect("a scanner reads the code");
        assert_eq!(String::from_utf8(read).unwrap(), shown.link);
    }

    #[test]
    fn a_workspace_no_link_can_name_says_so() {
        let mut invites = Invites::default();
        assert!(
            invites
                .at(SITE, ORIGIN, "not a workspace", &PERSON, START)
                .is_err()
        );
    }
}
