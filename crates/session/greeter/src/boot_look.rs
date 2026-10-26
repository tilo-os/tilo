//! The login screen's look as the boot sign-in screen draws it.
//!
//! The boot sign-in screen runs in the initramfs, where Plymouth draws with
//! pre-rendered images and one font, and no toolkit runs. It takes its
//! colours and sizes from `os/usr/share/plymouth/themes/tilo/look.json`:
//! its asset generator draws the avatar discs, the field and the fixed text
//! from it, and `tilo.script` carries the rest (`os/tests/boot_ux_invariants.sh`
//! checks that it matches). This module derives that file from the theme the
//! greeter paints with, and the test below keeps the file equal to it.

use ui::theme::Theme;
use ui::tokens::typography::TextRole;
use ui::widget::ControlSize;
use ui::widget::avatar::{AvatarSize, initial_ink, initials_fill};
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
    let look = serde_json::json!({
        "faces": faces,
        "text": hex(colors.text_primary),
        "dim": hex(colors.text_dim),
        "placeholder": hex(colors.text_tertiary),
        "danger": hex(colors.semantic.danger),
        "field": hex(colors.surfaces.raised),
        "field_radius": theme.tokens.radii.control,
        "field_height": ControlSize::Large.height(theme),
        "avatar": avatar,
        "initials_size": (avatar * 0.4).round(),
        "name_size": size(TextRole::LargeTitle),
        "body_size": size(TextRole::Body),
        "label_size": size(TextRole::Label),
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
        for (name, username) in [("Ada Example", "ada"), ("Quinten Example", "quinten")] {
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
