//! What a Notion export becomes: an archive of pages, or a pages listing.

use super::*;

/// A zip holding `files`, in the order given.
fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    buffer.into_inner()
}

fn png() -> Vec<u8> {
    // A 1×1 PNG, the smallest picture the notebook keeps.
    const ONE_PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    base64::engine::general_purpose::STANDARD
        .decode(ONE_PIXEL)
        .unwrap()
}

#[test]
fn an_export_archive_becomes_one_note_per_page_in_name_order() {
    let bytes = archive(&[
        (
            "Roadmap abc.md",
            b"# Roadmap\n\nShip the thing.\n\n- one\n- two\n" as &[u8],
        ),
        (
            "Budget f4d.md",
            b"\xef\xbb\xbf# Budget\n\n| a | b |\n" as &[u8],
        ),
    ]);
    let pages = pages(&bytes).unwrap();
    let titles: Vec<&str> = pages.iter().map(|page| page.title.as_str()).collect();
    assert_eq!(titles, ["Budget f4d", "Roadmap abc"]);
    assert!(pages[1].markdown.contains("Ship the thing."));
    assert!(pages[1].images.is_empty());
}

#[test]
fn a_page_named_untitled_takes_the_heading_it_starts_with() {
    let bytes = archive(&[("Untitled abc.md", b"# Launch checklist\n\n- [ ] invite\n")]);
    let pages = pages(&bytes).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].title, "Launch checklist");
}

#[test]
fn a_picture_the_export_kept_is_given_to_the_notebook() {
    let picture = png();
    let page = b"# Notes\n\n![Sketch](Sketch.png)\n\n![Elsewhere](https://example.test/a.png)\n";
    let bytes = archive(&[("Notes abc.md", page), ("Sketch.png", picture.as_slice())]);
    let pages = pages(&bytes).unwrap();
    assert_eq!(pages.len(), 1);
    let digest = blake3::hash(&picture).to_hex().to_string();
    let markdown = &pages[0].markdown;
    assert!(
        markdown.contains(&format!("![Sketch](asset:{digest})")),
        "{markdown}"
    );
    assert!(
        markdown.contains("![Elsewhere](https://example.test/a.png)"),
        "a picture the export does not hold stays a link: {markdown}"
    );
    assert_eq!(pages[0].images, vec![picture]);
}

#[test]
fn a_database_exported_beside_its_page_is_appended_as_a_table() {
    let csv = "Name,Owner\n\"First, item\",Alex\n\"Second\nline\",Sam\n";
    let bytes = archive(&[
        ("Projects.md", b"# Projects\n\nWhat we are on.\n"),
        ("Projects abc.csv", csv.as_bytes()),
    ]);
    let pages = pages(&bytes).unwrap();
    assert_eq!(pages.len(), 1);
    let markdown = &pages[0].markdown;
    assert!(markdown.contains("| Name | Owner |"), "{markdown}");
    assert!(markdown.contains("| First, item | Alex |"), "{markdown}");
    assert!(markdown.contains("| Second line | Sam |"), "{markdown}");
}

#[test]
fn a_pages_listing_becomes_markdown_between_its_blocks() {
    let listing = serde_json::json!({
        "title": "Release notes",
        "results": [
            {"type": "heading_1", "heading_1": {"rich_text": [{"plain_text": "Release notes"}]}},
            {"type": "paragraph", "paragraph": {"rich_text": [
                {"plain_text": "Shipped ", "annotations": {}},
                {"plain_text": "today", "annotations": {"bold": true}},
            ]}},
            {"type": "bulleted_list_item", "bulleted_list_item": {"rich_text": [{"plain_text": "faster"}]}},
            {"type": "to_do", "to_do": {"checked": true, "rich_text": [{"plain_text": "tests"}]}},
            {"type": "divider", "divider": {}},
            {"type": "code", "code": {"language": "rust", "rich_text": [{"plain_text": "fn main() {}"}]}},
        ],
    });
    let bytes = serde_json::to_vec(&listing).unwrap();
    let pages = pages(&bytes).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].title, "Release notes");
    let markdown = &pages[0].markdown;
    assert!(markdown.contains("# Release notes"), "{markdown}");
    assert!(markdown.contains("Shipped **today**"), "{markdown}");
    assert!(markdown.contains("- faster"), "{markdown}");
    assert!(markdown.contains("- [x] tests"), "{markdown}");
    assert!(markdown.contains("---"), "{markdown}");
    assert!(markdown.contains("```rust"), "{markdown}");
}

#[test]
fn a_listing_of_several_pages_becomes_one_note_each() {
    let listing = serde_json::json!([
        {"title": "One", "results": [{"type": "paragraph", "paragraph": {"rich_text": [{"plain_text": "first"}]}}]},
        {"title": "Two", "results": [{"type": "paragraph", "paragraph": {"rich_text": [{"plain_text": "second"}]}}]},
    ]);
    let bytes = serde_json::to_vec(&listing).unwrap();
    let pages = pages(&bytes).unwrap();
    let titles: Vec<&str> = pages.iter().map(|page| page.title.as_str()).collect();
    assert_eq!(titles, ["One", "Two"]);
    assert!(pages[1].markdown.contains("second"));
}

#[test]
fn a_file_that_is_neither_shape_is_refused() {
    assert!(pages(b"this is not an export").is_err());
    assert!(pages(b"{\"results\": []}").is_err());
    // A zip with no page in it is nothing to import.
    let empty = archive(&[("readme.txt", b"nothing" as &[u8])]);
    assert!(pages(&empty).is_err());
}

#[test]
fn a_csv_becomes_a_table_whose_cells_keep_their_pipes_inside() {
    let table = csv_table("Name,Note\nA,plain\nB,\"has | a pipe\"\n").unwrap();
    assert_eq!(
        table,
        "| Name | Note |\n| --- | --- |\n| A | plain |\n| B | has \\| a pipe |\n"
    );
    assert!(csv_table("").is_none());
}

#[test]
fn the_whole_import_is_one_action_group() {
    let pages = vec![
        ImportedPage {
            title: "One".into(),
            markdown: "first\n".into(),
            images: Vec::new(),
        },
        ImportedPage {
            title: "Two".into(),
            markdown: "second\n".into(),
            images: Vec::new(),
        },
    ];
    let invoke = import_action(&pages);
    assert!(invoke.is_group());
    let members = invoke
        .arguments
        .get(ui::tilo_resource::GROUP_PARAM)
        .unwrap();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 2);
}
