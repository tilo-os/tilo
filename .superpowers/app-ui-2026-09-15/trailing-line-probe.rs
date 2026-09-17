use ui::{
    RichSpan, RichTextLayout, TextAlign, TextPosition, TextStyle, configured_font_system,
};

fn print_layout(label: &str, layout: &RichTextLayout, text: &str) {
    let eof = text.len();
    let (x, top, height) = layout.caret_geometry_at(TextPosition::new(eof));
    let all_selection = layout.selection_rects(0, eof);
    let newline_selection = layout.selection_rects(eof.saturating_sub(1), eof);
    println!(
        "{label}: height={:.3} eof_caret=({x:.3},{top:.3},{height:.3}) eof_logical_line={} eof_visual_line={} eof_bands={:?} hit_below={:?} selection_all=(count={},last={:?}) selection_tail={:?}",
        layout.height(),
        layout.logical_line_index(eof),
        layout.visual_line_index(eof),
        layout.logical_line_bands(eof),
        layout.hit_position(0.0, layout.height() + 5.0),
        all_selection.len(),
        all_selection.last(),
        newline_selection,
    );
}

fn probe(text: &str, size: f32, width: f32) {
    let style = TextStyle::new(size).with_line_height(1.25);
    let mut font_system = configured_font_system();
    let plain = RichTextLayout::shape_plain(&mut font_system, text, &style, Some(width));
    let styled = RichTextLayout::shape_styled(
        &mut font_system,
        &[RichSpan::new(text)],
        &style,
        Some(width),
        TextAlign::Left,
        Vec::new(),
        Vec::new(),
    );
    println!("fixture={text:?} bytes={} size={size} width={width}", text.len());
    print_layout("plain ", &plain, text);
    print_layout("styled", &styled, text);
    if text == "x\n" && size == 16.0 {
        assert!((plain.height() - 40.0).abs() < 0.01);
        assert!((styled.height() - 20.0).abs() < 0.01);
        assert!((plain.caret_geometry(text.len()).1 - 20.0).abs() < 0.01);
        assert!((styled.caret_geometry(text.len()).1 - 0.0).abs() < 0.01);
        assert_eq!(plain.hit_position(0.0, 45.0).byte, text.len());
        assert_eq!(styled.hit_position(0.0, 25.0).byte, text.len() - 1);
    }
}

fn main() {
    probe("x", 16.0, 300.0);
    probe("x\n", 16.0, 300.0);
    probe("\n", 16.0, 300.0);
    probe("x\ny", 16.0, 300.0);
    probe(&"A line of the document.\n".repeat(80), 16.0, 390.0);
}
