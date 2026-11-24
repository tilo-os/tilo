//! The parts of the page's runtime a bundle carries as modules of their own,
//! which a page fetches when it first reaches for them (`tools/web/wasm-split`):
//!
//! - `peer`: the certified peer and its session with a workspace's host, the
//!   certificates and profile store that session keeps, the invitations that
//!   lead to one, and the sign-in certificate a page's root signs — what a
//!   page needs once a person enters a workspace, not to draw its first
//!   screen.
//! - `accounts`: the accounts a profile keeps, projected for the page from
//!   the Settings record, which only a profile that holds any reads.
//!
//! The linked module's `tilo_split_exports` section names `<module> <export>`
//! per line, an export ending in `*` standing for every export with that
//! prefix. The splitter leaves those exports of the start module, answered by
//! their module once the page loaded it (`web/host.mjs` loads one on demand).

/// The section's lines: `<module> <export>`.
const EXPORTS: &str = "\
peer wasmpeer_*
peer open_peer_store
peer close_peer_store
peer flush_peer_store
peer profile_value
peer set_profile_value
peer savedPeerCertificate
peer certifyWithRoot
peer channelVault
peer hostRefusal
peer entryRequest
peer reconnectCertificate
peer inspectInvitation
peer invitationEntry
peer invitedServer
peer openInvitation
peer signInAnswer
peer signInCertificate
accounts project_browser_accounts
";

const _: () = {
    #[unsafe(link_section = "tilo_split_exports")]
    #[used]
    static SPLIT_EXPORTS: [u8; EXPORTS.len()] = bytes(EXPORTS);
};

/// `text`'s bytes as an array, for the section's static.
const fn bytes<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0_u8; N];
    out.copy_from_slice(text.as_bytes());
    out
}
