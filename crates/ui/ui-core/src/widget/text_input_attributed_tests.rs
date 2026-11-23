//! A single-line field with attributed text: the style inputs that invalidate its shape.
use super::super::*;
use super::draw_to_populate;

tilo_test_env::scenario! {
    fn an_attributed_single_line_reshapes_when_its_style_changes;

    phase attributed_single_line_style_inputs_invalidate_cached_shape;
    {
        use crate::richtext::{BaselineShift, FontFamily};
        let _settled = crate::motion::testing::settled();
        let mut input = TextInput::local().with_initial_value("WWW");
        input.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
        let mut attributes = SingleLineAttributes {
            spans: vec![RichSpan::new("WWW")],
            ..Default::default()
        };
        input.set_single_line_attributes(attributes.clone());
        draw_to_populate(&mut input);
        let original_width = input.single_layout.as_ref().unwrap().width();
        attributes.spans[0].size = Some(28.0);
        input.set_single_line_attributes(attributes.clone());
        draw_to_populate(&mut input);
        let sized_width = input.single_layout.as_ref().unwrap().width();
        assert!(sized_width > original_width * 1.5);
        attributes.spans[0].family = Some(FontFamily::Mono);
        input.set_single_line_attributes(attributes.clone());
        draw_to_populate(&mut input);
        assert!((input.single_layout.as_ref().unwrap().width() - sized_width).abs() > 1.0);
        attributes.spans[0].background = Some([40, 50, 60]);
        attributes.spans[0].baseline = BaselineShift::Super;
        attributes.spans[0].color = Some([100, 110, 120]);
        attributes.underlines = vec![(0, 1)];
        attributes.strikes = vec![(1, 3)];
        input.set_single_line_attributes(attributes);
        draw_to_populate(&mut input);
        let layout = input.single_layout.as_ref().unwrap();
        assert_eq!(layout.backgrounds, [(0, 3, [40, 50, 60])]);
        assert_eq!(layout.baselines, [(0, 3, BaselineShift::Super)]);
        assert_eq!(layout.underlines, [(0, 1)]);
        assert_eq!(layout.strikes, [(1, 3)]);
        assert!(
            layout
                .buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .all(|g| g.color_opt == Some(cosmic_text::Color::rgb(100, 110, 120)))
        );
    }
}
