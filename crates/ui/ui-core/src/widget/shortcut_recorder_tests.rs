use super::test_support::*;
use super::*;

#[test]
fn binding_display_names_keys_and_offers_reset_only_when_changed() {
    {
        let unchanged = recorder("Super+Left", "Super+Left");
        assert!(unchanged.reset_rect().is_none());
        let changed = recorder("Super+H", "Super+Left");
        assert!(changed.reset_rect().is_some());
    }

    {
        let recorder = recorder("Super+Down", "Super+Left");
        assert_eq!(
            recorder.binding().keycap_labels(PlatformConventions::new(
                tilo_ui_base::platform::Platform::Linux
            )),
            ["Super", "Down"]
        );
        assert_eq!(
            recorder.binding().keycap_labels(PlatformConventions::new(
                tilo_ui_base::platform::Platform::MacOs
            )),
            ["⌘", "↓"]
        );
    }
}

#[test]
fn no_shortcut_reads_as_none_rather_than_a_keycap() {
    let theme = tilo_ui_base::theme::Theme::light();
    let mut text = TextRenderer::new();
    let mut empty = recorder("", "Ctrl+K");
    empty.measure_keys(&mut text, &theme);
    let none = tilo_ui_base::i18n::widget_format("widget-shortcut-none", &[]);
    let (width, _) = text.measure_style(&none, None, &theme.text(TextRole::Label));
    assert_eq!(empty.keys_width, f64::from(width).ceil());
    assert!(empty.reset_rect().is_some(), "the default is a press away");
}
