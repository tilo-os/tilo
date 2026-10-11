//! Shared floating-surface presentation and lifecycle policy.
use smithay::reexports::wayland_server::backend::ClientId;
use std::sync::Arc;

/// Immutable association with the actual source Wayland connection.
/// A token never substitutes for this connection and room identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloatingToplevelOwner {
    pub client: ClientId,
    pub room: Option<crate::shell::state::RoomId>,
}
pub const OSK_APP_ID: &str = "so.tilo.osk";
/// Frozen pixels from the final app-owned frame. The shell moves this exact
/// image under the pointer while the authenticated secondary surface warms,
/// then drops it atomically on the first native buffer.
#[derive(Clone, Debug)]
pub struct FloatingTransferFrame {
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// Primary-app rect the frozen transfer was cut from. The proxy remains here
/// until the app explicitly confirms that its cleared primary frame presented;
/// moving it sooner would reveal the same card in both owners.
#[derive(Clone, Copy, Debug)]
pub struct FloatingTransferSource {
    pub rect: vello::kurbo::Rect,
}

#[derive(Clone, Debug)]
pub struct PendingFloatingToplevel {
    pub owner: Option<Arc<FloatingToplevelOwner>>,
    pub token: String,
    pub connection: u64,
    pub public_id: String,
    pub app_id: String,
    pub content_rect: vello::kurbo::Rect,
    pub input_rect: Option<vello::kurbo::Rect>,
    pub rect: vello::kurbo::Rect,
    pub transfer_frame: Option<FloatingTransferFrame>,
    pub transfer_source: Option<FloatingTransferSource>,
    pub suspended: bool,
}

#[derive(Clone, Debug)]
pub struct FloatingToplevel {
    pub owner: Option<Arc<FloatingToplevelOwner>>,
    pub surface_id: u64,
    pub token: String,
    pub connection: u64,
    pub public_id: String,
    pub app_id: String,
    pub content_rect: vello::kurbo::Rect,
    pub input_rect: Option<vello::kurbo::Rect>,
    pub rect: vello::kurbo::Rect,
    pub transfer_frame: Option<FloatingTransferFrame>,
    pub transfer_source: Option<FloatingTransferSource>,
    /// Hidden while the activity is attached inside its app. The Wayland
    /// surface and authenticated capability deliberately remain alive so a
    /// later detach can resume without renderer or identity churn.
    pub suspended: bool,
    /// True only after the first commit carrying pixels. Constructing an
    /// `xdg_toplevel` alone is not a visible ownership transfer.
    pub presented: bool,
}

#[derive(Clone, Debug)]
pub enum FloatingToplevelEvent {
    Mapped(Box<FloatingToplevel>),
    Destroyed { connection: u64, public_id: String },
}

pub fn suspend_floating_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
) -> bool {
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        let changed = !surface.suspended;
        surface.suspended = true;
        return changed;
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        let changed = !pending.suspended;
        pending.suspended = true;
        return changed;
    }
    false
}

pub fn resume_floating_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
    geometry: (vello::kurbo::Rect, vello::kurbo::Rect),
    transfer_frame: Option<FloatingTransferFrame>,
    transfer_source: Option<FloatingTransferSource>,
) -> Option<String> {
    let (rect, content_rect) = geometry;
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        let input_rect = ui::floating_surface::translate_input_rect(
            surface.input_rect,
            surface.content_rect.origin(),
            content_rect.origin(),
        )
        .map(|input| input.intersect(rect));
        if ui::floating_surface::protocol_input_rect(
            rect,
            input_rect.map(ui::floating_surface::protocol_rect_array),
        )
        .is_err()
        {
            return None;
        }
        surface.input_rect = input_rect;
        surface.rect = rect;
        surface.content_rect = content_rect;
        surface.transfer_frame = transfer_frame;
        surface.transfer_source = transfer_source;
        surface.suspended = false;
        // Keep the frozen app frame on top until a fresh buffer commits; the
        // retained window's previous buffer must never flash stale.
        surface.presented = false;
        return Some(surface.token.clone());
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        let input_rect = ui::floating_surface::translate_input_rect(
            pending.input_rect,
            pending.content_rect.origin(),
            content_rect.origin(),
        )
        .map(|input| input.intersect(rect));
        if ui::floating_surface::protocol_input_rect(
            rect,
            input_rect.map(ui::floating_surface::protocol_rect_array),
        )
        .is_err()
        {
            return None;
        }
        pending.input_rect = input_rect;
        pending.rect = rect;
        pending.content_rect = content_rect;
        pending.transfer_frame = transfer_frame;
        pending.transfer_source = transfer_source;
        pending.suspended = false;
        return Some(pending.token.clone());
    }
    None
}

pub fn complete_floating_return_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
) -> bool {
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        if !surface.suspended {
            return false;
        }
        let changed = surface.transfer_source.take().is_some();
        surface.transfer_frame = None;
        return changed;
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        if !pending.suspended {
            return false;
        }
        let changed = pending.transfer_source.take().is_some();
        pending.transfer_frame = None;
        return changed;
    }
    false
}

pub fn clear_floating_source_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
) -> bool {
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        // A late detach acknowledgement must never clear the newer reverse
        // handoff proxy after the surface has already been suspended.
        return !surface.suspended && surface.transfer_source.take().is_some();
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        return !pending.suspended && pending.transfer_source.take().is_some();
    }
    false
}

pub fn dismiss_floating_entries(
    pending: &mut Vec<PendingFloatingToplevel>,
    live: &mut Vec<FloatingToplevel>,
    connection: u64,
    public_id: &str,
) -> bool {
    let pending_before = pending.len();
    pending
        .retain(|candidate| candidate.connection != connection || candidate.public_id != public_id);
    let live_before = live.len();
    live.retain(|surface| surface.connection != connection || surface.public_id != public_id);
    pending_before != pending.len() || live_before != live.len()
}

#[cfg(test)]
mod floating_lifecycle_tests {
    use super::*;

    fn live_surface() -> FloatingToplevel {
        FloatingToplevel {
            owner: None,
            surface_id: 41,
            token: "5e8f16a2a2e24bf7ad86758f2f28e316".into(),
            connection: 7,
            public_id: "maps-navigation:active".into(),
            app_id: "so.tilo.maps".into(),
            content_rect: vello::kurbo::Rect::new(100.0, 80.0, 420.0, 260.0),
            input_rect: None,
            rect: vello::kurbo::Rect::new(92.0, 72.0, 428.0, 268.0),
            transfer_frame: None,
            transfer_source: None,
            suspended: false,
            presented: true,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn floating_surfaces_resume_only_on_their_authenticated_connection() {
        fn suspend_and_resume_reuses_the_exact_authenticated_surface() {
            let mut pending = Vec::new();
            let mut live = vec![live_surface()];
            assert!(suspend_floating_entries(
                &mut pending,
                &mut live,
                7,
                "maps-navigation:active"
            ));
            assert!(live[0].suspended);

            let next_window = vello::kurbo::Rect::new(692.0, 52.0, 1_028.0, 248.0);
            let next_content = vello::kurbo::Rect::new(700.0, 60.0, 1_020.0, 240.0);
            let token = resume_floating_entries(
                &mut pending,
                &mut live,
                7,
                "maps-navigation:active",
                (next_window, next_content),
                None,
                None,
            );
            assert_eq!(token.as_deref(), Some("5e8f16a2a2e24bf7ad86758f2f28e316"));
            assert_eq!(live[0].surface_id, 41);
            assert_eq!(live[0].rect, next_window);
            assert!(!live[0].suspended);
            assert!(!live[0].presented, "resume waits for fresh pixels");
        }

        fn return_proxy_survives_suspend_until_primary_destination_is_presented() {
            let mut pending = Vec::new();
            let mut live = vec![live_surface()];
            let return_rect = live[0].rect;
            live[0].transfer_frame = Some(FloatingTransferFrame {
                rgba: std::sync::Arc::new(vec![0xff; 4 * 4 * 4]),
                width: 4,
                height: 4,
            });
            live[0].transfer_source = Some(FloatingTransferSource { rect: return_rect });

            assert!(suspend_floating_entries(
                &mut pending,
                &mut live,
                7,
                "maps-navigation:active"
            ));
            assert!(live[0].suspended);
            assert_eq!(live[0].transfer_source.unwrap().rect, return_rect);
            assert!(live[0].transfer_frame.is_some());
            assert!(
                !clear_floating_source_entries(
                    &mut pending,
                    &mut live,
                    7,
                    "maps-navigation:active"
                ),
                "a late detach acknowledgement cannot tear down the newer return proxy"
            );
            assert!(live[0].transfer_source.is_some());

            assert!(complete_floating_return_entries(
                &mut pending,
                &mut live,
                7,
                "maps-navigation:active"
            ));
            assert!(live[0].transfer_source.is_none());
            assert!(live[0].transfer_frame.is_none());
            assert!(
                !complete_floating_return_entries(
                    &mut pending,
                    &mut live,
                    7,
                    "maps-navigation:active"
                ),
                "the destination acknowledgement is idempotent"
            );
        }

        fn capability_cannot_be_resumed_from_another_connection() {
            let mut pending = Vec::new();
            let mut live = vec![live_surface()];
            assert!(
                resume_floating_entries(
                    &mut pending,
                    &mut live,
                    99,
                    "maps-navigation:active",
                    (vello::kurbo::Rect::ZERO, vello::kurbo::Rect::ZERO),
                    None,
                    None,
                )
                .is_none()
            );
            assert_eq!(live[0].connection, 7);
            assert!(live[0].presented);
        }

        fn permanent_dismiss_cannot_be_resumed() {
            let mut pending = Vec::new();
            let mut live = vec![live_surface()];
            assert!(dismiss_floating_entries(
                &mut pending,
                &mut live,
                7,
                "maps-navigation:active"
            ));
            assert!(live.is_empty());
            assert!(
                resume_floating_entries(
                    &mut pending,
                    &mut live,
                    7,
                    "maps-navigation:active",
                    (vello::kurbo::Rect::ZERO, vello::kurbo::Rect::ZERO),
                    None,
                    None,
                )
                .is_none()
            );
        }

        suspend_and_resume_reuses_the_exact_authenticated_surface();
        return_proxy_survives_suspend_until_primary_destination_is_presented();
        capability_cannot_be_resumed_from_another_connection();
        permanent_dismiss_cannot_be_resumed();
    }
}
