//! Input-method placement for native windows: the signed rectangle a held
//! editor's caret occupies, which window owns composition, and how preedit
//! lifecycle events move that ownership between the primary window and its
//! floating children.

use super::*;

/// A held protocol resource can anchor an editor outside its native window.
/// Wayland cursor positions are signed; the shared widget/browser contract
/// stays unchanged, while native handoff translations must not clamp to zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NativeImeWindowState {
    content: crate::ime::InputContent,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl NativeImeWindowState {
    fn from_rect(content: crate::ime::InputContent, rect: KurboRect) -> Self {
        let shared = ImeWindowState::from_rect(content, rect);
        let signed = |value: f64| {
            if value.is_finite() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "rounded coordinate is bounded to the signed Wayland position range"
                )]
                {
                    value
                        .round()
                        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
                }
            } else {
                0
            }
        };
        Self {
            content,
            x: signed(rect.x0),
            y: signed(rect.y0),
            width: shared.width,
            height: shared.height,
        }
    }
}

impl From<NativeImeWindowState> for ImeWindowState {
    fn from(native: NativeImeWindowState) -> Self {
        Self::from_rect(
            native.content,
            KurboRect::from_origin_size(
                (f64::from(native.x), f64::from(native.y)),
                (f64::from(native.width), f64::from(native.height)),
            ),
        )
    }
}

impl From<ImeWindowState> for NativeImeWindowState {
    fn from(shared: ImeWindowState) -> Self {
        Self::from_rect(
            shared.content,
            KurboRect::from_origin_size(
                (f64::from(shared.x), f64::from(shared.y)),
                (f64::from(shared.width), f64::from(shared.height)),
            ),
        )
    }
}

pub(super) fn apply_native_ime(
    window: &Window,
    next: Option<NativeImeWindowState>,
    previous: &mut Option<NativeImeWindowState>,
) {
    if next == *previous {
        return;
    }
    match next {
        Some(intent) => {
            apply_native_ime_content(window, intent);
            window.set_ime_allowed(true);
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(intent.x, intent.y),
                winit::dpi::LogicalSize::new(intent.width, intent.height),
            );
        }
        None => window.set_ime_allowed(false),
    }
    *previous = next;
}

pub(super) fn apply_native_ime_content(window: &Window, intent: NativeImeWindowState) {
    // Wayland takes the field's whole content type; other platforms take
    // no more than winit's own purposes.
    let (hint, purpose) = intent.content.wayland();
    tilo_presentation::text_input::set_content_type(window, hint, purpose);
    window.set_ime_purpose(intent.content.winit_purpose());
}

pub(super) fn secondary_ime_state(
    root: &dyn Widget,
    id: &str,
    desktop_rect: KurboRect,
    focused: bool,
) -> Option<NativeImeWindowState> {
    if !focused {
        return None;
    }
    let (content, rect) = root.floating_surface_ime(id)?;
    Some(NativeImeWindowState::from_rect(
        content,
        rect - desktop_rect.origin().to_vec2(),
    ))
}

pub(super) fn primary_floating_ime_state(
    root: &dyn Widget,
    id: &str,
    coordinate_offset: Vec2,
) -> Option<NativeImeWindowState> {
    let (content, rect) = root.floating_surface_ime(id)?;
    Some(NativeImeWindowState::from_rect(
        content,
        rect - coordinate_offset,
    ))
}

pub(super) fn pending_floating_ime_state(
    app: &App,
    preedit_active: bool,
    previous: Option<NativeImeWindowState>,
) -> Option<NativeImeWindowState> {
    preedit_active.then_some(())?;
    app.pending_floating_ime_source()?;
    previous
}

pub(super) fn primary_ime_state_for_held_child(
    primary: Option<ImeWindowState>,
    primary_origin: Vec2,
    child_origin: Vec2,
) -> Option<NativeImeWindowState> {
    let primary = primary?;
    let rect = KurboRect::new(
        f64::from(primary.x),
        f64::from(primary.y),
        f64::from(primary.x.saturating_add(primary.width)),
        f64::from(primary.y.saturating_add(primary.height)),
    ) + primary_origin
        - child_origin;
    Some(NativeImeWindowState::from_rect(primary.content, rect))
}

/// Refresh a held child's caret when the returned editor projects one, while
/// keeping its real protocol alive across a transient missing projection.
/// Only the exact retained, still-composing source may preserve prior state;
/// every definitive lifecycle end returns the projected `None` and disables.
pub(super) fn retained_return_ime_state(
    projected: Option<NativeImeWindowState>,
    retained: Option<NativeImeWindowState>,
    exact_source_active: bool,
) -> Option<NativeImeWindowState> {
    if exact_source_active {
        projected.or(retained)
    } else {
        projected
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrimaryPreeditUpdate {
    Active,
    PendingClear,
    Finished,
}

pub(super) fn preedit_after_event(event: &WindowEvent) -> Option<PrimaryPreeditUpdate> {
    match event {
        WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) if text.is_empty() => {
            Some(PrimaryPreeditUpdate::PendingClear)
        }
        WindowEvent::Ime(winit::event::Ime::Preedit(_, _)) => Some(PrimaryPreeditUpdate::Active),
        WindowEvent::Ime(winit::event::Ime::Commit(_) | winit::event::Ime::Disabled) => {
            Some(PrimaryPreeditUpdate::Finished)
        }
        _ => None,
    }
}

#[cfg(test)]
mod floating_ime_continuation_tests {
    use super::*;

    #[test]
    fn returned_caret_keeps_signed_child_coordinates_and_content_type() {
        let primary = ImeWindowState {
            content: crate::ime::InputContent {
                purpose: crate::ime::InputPurpose::Email,
                hints: crate::ime::InputHints {
                    multiline: true,
                    ..crate::ime::InputHints::default()
                },
            },
            x: 20,
            y: 30,
            width: 2,
            height: 24,
        };
        let projected = primary_ime_state_for_held_child(
            Some(primary),
            Vec2::new(100.0, 80.0),
            Vec2::new(300.0, 200.0),
        )
        .unwrap();
        assert_eq!((projected.x, projected.y), (-180, -90));
        assert_eq!((projected.width, projected.height), (2, 24));
        assert_eq!(projected.content, primary.content);
        assert_eq!(
            retained_return_ime_state(None, Some(projected), true),
            Some(projected)
        );
        assert_eq!(
            retained_return_ime_state(None, Some(projected), false),
            None
        );
    }

    #[test]
    fn empty_preedit_does_not_retire_custody_before_the_commit_in_the_same_batch() {
        assert_eq!(
            preedit_after_event(&WindowEvent::Ime(winit::event::Ime::Preedit(
                String::new(),
                None
            ))),
            Some(PrimaryPreeditUpdate::PendingClear)
        );
        assert_eq!(
            preedit_after_event(&WindowEvent::Ime(winit::event::Ime::Commit("文字".into()))),
            Some(PrimaryPreeditUpdate::Finished)
        );
        assert_eq!(
            preedit_after_event(&WindowEvent::Ime(winit::event::Ime::Disabled)),
            Some(PrimaryPreeditUpdate::Finished)
        );
        assert!(!routes_to_floating_ime(&WindowEvent::Focused(false)));
    }
}

pub(super) fn routes_to_floating_ime(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
    )
}
