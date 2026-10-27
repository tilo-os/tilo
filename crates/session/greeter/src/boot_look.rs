//! The login screen's look as the boot sign-in screen draws it.
//!
//! The boot sign-in screen runs in the initramfs, where Plymouth draws with
//! pre-rendered images and the greeter's face cut to two fixed weights, and
//! no toolkit runs. It takes its face, colours, sizes and weights from
//! `os/usr/share/plymouth/themes/tilo/look.json`: its asset generator cuts the
//! face and draws the avatar discs, the field and the fixed text from it, and
//! `tilo.script` carries the rest (`os/tests/boot_ux_invariants.sh` checks
//! that it matches). This module derives that file from the theme the
//! greeter paints with, and the test below keeps the file equal to it.

use ui::text_render::{TextWeight, UI_SANS_FAMILY};
use ui::theme::Theme;
use ui::tokens::typography::TextRole;
use ui::widget::avatar::{AvatarSize, initial_ink, initials_fill};
use ui::widget::{SecretPrompt, modal_dialog};
use vello::peniko::Color;

/// The theme the greeter paints with.
#[must_use]
pub fn theme() -> Theme {
    Theme::dark()
}

/// `look.json` for `theme`.
#[must_use]
pub fn json(theme: &Theme) -> String {
    let colors = &theme.tokens.colors;
    let faces: Vec<serde_json::Value> = (0..tilo_face::PRESENCE_LEN)
        .map(|slot| {
            let fill = initials_fill(colors.presence.get(slot), theme);
            serde_json::json!({
                "fill": hex(fill),
                "ink": hex(initial_ink(fill, theme)),
            })
        })
        .collect();
    let avatar = AvatarSize::Hero.side(theme);
    let size = |role: TextRole| f64::from(theme.text(role).size);
    let weight = |role: TextRole| theme.text(role).weight.to_u16();
    let look = serde_json::json!({
        "faces": faces,
        "family": UI_SANS_FAMILY,
        "text": hex(colors.text_primary),
        "dim": hex(colors.text_dim),
        "danger": hex(colors.semantic.danger),
        "field_label": hex(modal_dialog::ink(theme).0),
        "field": hex(colors.surfaces.raised),
        "field_radius": theme.tokens.radii.control,
        "field_height": SecretPrompt::field_height(theme),
        "field_label_gap": theme.tokens.spacing.s1,
        "avatar": avatar,
        "initials_size": (avatar * 0.4).round(),
        "initials_weight": theme.text(TextRole::Label).strong().weight.to_u16(),
        // The greeter sets the name semibold whatever its role's weight.
        "name_size": size(TextRole::LargeTitle),
        "name_weight": TextWeight::Semibold.to_u16(),
        "body_size": size(TextRole::Body),
        "body_weight": weight(TextRole::Body),
        "label_size": size(TextRole::Label),
        "label_weight": weight(TextRole::Label),
        // Plymouth places in whole pixels.
        "label_line": f64::from(theme.text(TextRole::Label).line_height_px()).round(),
    });
    let mut text = serde_json::to_string_pretty(&look).unwrap_or_default();
    text.push('\n');
    text
}

fn hex(color: Color) -> String {
    let [r, g, b, _] = color.to_rgba8().to_u8_array();
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::AvatarFace;

    const LOOK: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../os/usr/share/plymouth/themes/tilo/look.json"
    );

    /// Rewrite the file with `TILO_WRITE_BOOT_LOOK=1` after a theme change.
    #[test]
    fn the_boot_screen_has_the_greeters_look() {
        let expected = json(&theme());
        if std::env::var_os("TILO_WRITE_BOOT_LOOK").is_some() {
            std::fs::write(LOOK, &expected).expect("write look.json");
        }
        let committed = std::fs::read_to_string(LOOK).unwrap_or_default();
        assert_eq!(
            committed, expected,
            "look.json is not the greeter's look; run this test with TILO_WRITE_BOOT_LOOK=1"
        );
    }

    #[test]
    fn a_persons_face_is_the_one_the_greeter_paints() {
        let theme = theme();
        let look: serde_json::Value = serde_json::from_str(&json(&theme)).unwrap();
        for (name, username) in [("Ada Example", "ada"), ("ada Example", "ada")] {
            let face = AvatarFace::new(name, username);
            let slot = tilo_face::presence_slot(username);
            assert_eq!(
                look["faces"][slot]["fill"],
                serde_json::Value::String(hex(face.colour(&theme)))
            );
            assert_eq!(face.initials(), tilo_face::initials(name));
        }
    }
}
