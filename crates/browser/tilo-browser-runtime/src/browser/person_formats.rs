//! The formats and the zone the person reads dates in, told by this page to the workspace's
//! server ([`tilo_actions::remote::person_formats`]).
//!
//! The server renders the person's views where no device of theirs does (what their agent reads
//! of an app, above all), and a page is often the only device they have open. So once the page
//! is in a workspace it reads what the server keeps: formats kept there are the person's, and
//! the page shows them too; with none kept, the page's own become theirs. It then tells the
//! server the zone the browser is in, when that is not the one the server has, and from then on
//! sends the formats again whenever the person changes them in Region & Formats. A zone the
//! person chose themselves is kept by the server and wins over the browser's: the page reads its
//! dates in it too, so what the person and their agent read agrees.

use tilo_actions::remote::Caller;
use tilo_actions::remote::person_formats::{self, BrowserZone, PersonFormats, SetFormats};
use tilo_server_protocol::WorkspaceId;
use tilo_session_actions::SessionActions;
use ui::format::region::RegionFormats;

use super::told_notices::ask;
use crate::browser::sleep_ms;

/// The settings section the formats are kept in.
const SECTION: &str = "locale";
/// How often the page looks whether the person changed their formats.
const LOOK_MS: u32 = 1_000;
/// How often the page reads again what the server keeps: another device of theirs may have
/// changed it.
const READ_AGAIN_MS: u32 = 30_000;
/// The longest wait before a server that did not answer is asked again.
const RETRY_MAX_MS: u32 = 30_000;

/// Keep what the server the page's peer `peer` holds workspace `id` with knows of `person`'s
/// formats and zone current, while the page holds that peer.
pub(super) fn follow(peer: u32, id: WorkspaceId, workspace: &str, person: &str) {
    let caller = Caller::local_person(person.to_owned()).in_workspace(workspace.to_owned());
    wasm_bindgen_futures::spawn_local(async move {
        let mut retry = 1_000;
        // What the server and the page agreed on last: a change of the page's formats past it
        // is the person's, and is sent.
        let mut agreed: Option<RegionFormats> = None;
        let mut since_read = READ_AGAIN_MS;
        while let Some(api) = crate::peer_sync::actions_of(peer, id) {
            let told = if since_read >= READ_AGAIN_MS {
                agree(&api, &caller).await.inspect(|_| since_read = 0)
            } else {
                send_changed(&api, &caller, agreed).await
            };
            if let Some(formats) = told {
                agreed = Some(formats);
                retry = 1_000;
                sleep_ms(LOOK_MS).await;
                since_read = since_read.saturating_add(LOOK_MS);
            } else {
                sleep_ms(retry).await;
                retry = retry.saturating_mul(2).min(RETRY_MAX_MS);
            }
        }
    });
}

/// The formats the page shows now.
fn page_formats() -> RegionFormats {
    RegionFormats::from_section(settings_store::load_section(SECTION))
}

/// Read what the server keeps and agree with it: its formats shown on the page, or the page's
/// sent where it keeps none; and the browser's zone told where it has another. The formats
/// agreed on, or `None` where the server did not answer.
async fn agree(api: &SessionActions, caller: &Caller) -> Option<RegionFormats> {
    let kept: PersonFormats = ask(
        api,
        caller,
        person_formats::target(person_formats::READ),
        &serde_json::json!({}),
    )
    .await?;
    show_chosen_zone(kept.chosen_zone.as_deref());
    let browser = tilo_clock::local_zone_name();
    if let Some(zone) = browser.filter(|zone| kept.browser_zone.as_ref() != Some(zone)) {
        let _: PersonFormats = ask(
            api,
            caller,
            person_formats::target(person_formats::BROWSER_ZONE),
            &BrowserZone { zone },
        )
        .await?;
    }
    match kept.formats {
        Some(formats) => {
            let formats = RegionFormats::from_section(formats);
            if formats != page_formats()
                && let Err(error) = settings_store::update_section(SECTION, formats.to_section())
            {
                tracing::warn!(%error, "the person's formats were not shown on the page");
            }
            Some(formats)
        }
        None => send(api, caller, page_formats()).await,
    }
}

/// Read the page's dates in the zone the person chose, or in the browser's where they chose
/// none, as the server reads them for their agent; a change redraws every window, as new
/// formats do.
fn show_chosen_zone(chosen: Option<&str>) {
    if ui::format::region::install_zone_here(ui::format::region::chosen_zone(chosen)) {
        crate::browser::wake_mounts();
    }
}

/// Send the page's formats where they changed since the server and the page last agreed: the
/// formats agreed on then, or `None` where the server did not answer.
async fn send_changed(
    api: &SessionActions,
    caller: &Caller,
    agreed: Option<RegionFormats>,
) -> Option<RegionFormats> {
    let shown = page_formats();
    if agreed == Some(shown) {
        return agreed;
    }
    send(api, caller, shown).await
}

/// Keep `formats` as the person's on the server: they are agreed on once it answers.
async fn send(
    api: &SessionActions,
    caller: &Caller,
    formats: RegionFormats,
) -> Option<RegionFormats> {
    let kept: PersonFormats = ask(
        api,
        caller,
        person_formats::target(person_formats::SET),
        &SetFormats {
            formats: formats.to_section(),
        },
    )
    .await?;
    Some(kept.formats.map_or(formats, RegionFormats::from_section))
}
