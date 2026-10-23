use std::fmt::Write as _;
use ui::a11y::Role;
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::block_editor::{Block, BlockEditor};
use vello::Scene;

fn render_table(
    source: &str,
    width: f32,
    visual: bool,
    scale: f32,
) -> (Framed<BlockEditor>, Scene) {
    let mut editor = BlockEditor::new().with_wrap_lines(true);
    editor.set_blocks(vec![
        Block::table(source, visual),
        Block::paragraph("After table"),
    ]);
    let editor = crate::component_support::framed_block_editor(
        editor,
        width,
        (0.0, 0.0),
        Theme::dark().with_text_scale(scale),
    );
    assert_eq!(
        editor.blocks()[0].text(),
        source,
        "responsive rendering never rewrites canonical table text"
    );
    let scene = Framed::harness(&editor).scene().clone();
    (editor, scene)
}

fn table_source(rows: usize, columns: usize, final_value: &str) -> String {
    let mut source = format!(
        "|{}|\n|{}|",
        vec!["H"; columns].join("|"),
        vec!["---"; columns].join("|")
    );
    for row in 1..rows {
        let mut cells = vec!["x"; columns];
        if row == rows.saturating_sub(1)
            && let Some(last) = cells.last_mut()
        {
            *last = final_value;
        }
        let _ = write!(source, "\n|{}|", cells.join("|"));
    }
    source
}

#[expect(
    clippy::float_cmp,
    reason = "a stacked field spans the full 390 px viewport exactly; the edge is assigned, not computed"
)]
#[test]
fn table_previews_paint_large_wrapped_and_narrow_tables_completely() {
    eprintln!("phase table_paints_beyond_twelve_rows_and_eight_columns");
    {
        let full = table_source(15, 10, "VISIBLE");
        let blank = table_source(15, 10, "");
        let (editor, scene) = render_table(&full, 1200.0, true, 1.0);
        let (_, blank_scene) = render_table(&blank, 1200.0, true, 1.0);
        assert!(
            editor.blocks()[1].line.get_rect().y0 > 15.0 * 34.0,
            "the following paragraph must clear every table row"
        );
        assert!(
            scene.encoding().draw_tags.len() > blank_scene.encoding().draw_tags.len(),
            "the final cell at row15/column10 must actually paint, not merely exist in semantics"
        );
        let document = editor.accessibility().unwrap();
        let table = document.children[0]
            .children
            .iter()
            .find(|node| node.role == Role::Table)
            .unwrap();
        assert_eq!(table.children.len(), 15);
        assert!(table.children.iter().all(|row| row.children.len() == 10));
    }

    eprintln!("phase wrapped_table_cells_reserve_height_before_the_next_block");
    {
        let short = "| Column | Another |\n| --- | --- |\n| short | short |";
        let long = "| Column | Another |\n| --- | --- |\n| A long sentence whose entire contents must remain readable by wrapping across several lines inside this cell. | short |";
        for scale in [1.0, 1.5] {
            let (short_editor, _) = render_table(short, 400.0 * scale, true, scale);
            let (long_editor, _) = render_table(long, 400.0 * scale, true, scale);
            assert!(
                long_editor.blocks()[1].line.get_rect().y0
                    > short_editor.blocks()[1].line.get_rect().y0 + 30.0 * f64::from(scale),
                "wrapped cells must grow the table's allocated height"
            );
            let (source_editor, _) = render_table(long, 400.0 * scale, false, scale);
            assert!(
                source_editor.blocks()[0].line.get_rect().height() > 3.0 * 20.0 * f64::from(scale),
                "source mode keeps the complete wrapped source editable"
            );
            assert!(
                source_editor.blocks()[1].line.get_rect().y0
                    > long_editor.blocks()[1].line.get_rect().y0
            );
        }
    }

    eprintln!("phase narrow_tables_stack_all_fields_without_losing_the_last_value");
    {
        let full = table_source(3, 10, "LAST FIELD");
        let blank = table_source(3, 10, "");
        let (wide, _) = render_table(&full, 1200.0, true, 1.0);
        let (narrow, scene) = render_table(&full, 390.0, true, 1.0);
        let (_, blank_scene) = render_table(&blank, 390.0, true, 1.0);
        assert!(
            narrow.blocks()[1].line.get_rect().y0 > wide.blocks()[1].line.get_rect().y0 * 3.0,
            "narrow tables must stack labeled fields instead of squeezing ten tiny columns"
        );
        assert!(
            scene.encoding().draw_tags.len() > blank_scene.encoding().draw_tags.len(),
            "the final responsive field must actually paint"
        );
        assert_eq!(narrow.blocks()[1].line.get_rect().x1, 390.0);
    }
}
