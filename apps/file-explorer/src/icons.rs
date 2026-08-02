use crate::model::Entry;

pub fn icon_for(entry: &Entry) -> &'static str {
    if entry.is_dir { return "󰉋"; }
    let m = entry.mime.as_str();
    if m.starts_with("image/") { return "󰋩"; }
    if m.starts_with("video/") { return "󰿎"; }
    if m.starts_with("audio/") { return "󰝚"; }
    if m.contains("pdf")       { return "󰈦"; }
    // Editable documents (odt/docx/markdown) — the formats qdoc parses.
    if crate::files_app::is_document(&entry.path) { return "󰈙"; }
    "󰈔"
}
