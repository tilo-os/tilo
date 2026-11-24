app so.tilo.word-processor "Writer" (
    icon: .file_text,
    category: .office,
    tagline: "Formatted documents you write alone or together",
    keywords: ["word", "docs", "document", "documents", "writer", "microsoft word", "docx", "odt", "rtf", "libreoffice writer", "wordpad", "textverarbeitung", "dokument", "schreiben", "brief", "lebenslauf", "bericht"],
)

use so.tilo.lang.discussion as discussion

/// A document: what a person writes, together with anyone else at it. Its words are its body —
/// the paragraphs, headings, lists, quotes, code and tables the shared editor edits — and who
/// wrote what, when, comes from the log rather than from a field.
///
/// A document is named by its title; an empty one is "Untitled Document". The app is the one
/// Writer document of a workspace, as a desk holds one sheet being written, and a file opened
/// into it replaces what it held — the move a person makes opening a file in a word processor.
type Document (untitled: "Untitled Document") {
    title: Text
    body: RichText
    /// The page's margins, in points at 100%: where its writing starts and stops. The page band
    /// draws them and sets them.
    margin_top: Float = 96.0
    margin_right: Float = 96.0
    margin_bottom: Float = 96.0
    margin_left: Float = 96.0
    /// The paper the page is set for, as the panel offers it: A4, Letter or Legal.
    paper: Text = "A4"
    /// What the writing was started as: a blank page, a letter, a report, a résumé or meeting
    /// notes. The page's setup panel says it, so a person reads how the document began.
    started: Text = "blank"
    /// Who reacted to the document, by emoji: the person and agent each pill in the window's bar
    /// stands for, and whose own reaction a press on it takes back.
    reactions: Map<Set<Actor>>
    /// Why the last thing asked of the document did not happen, such as a file no codec reads;
    /// empty while nothing went wrong. The page shows it over the writing until it is dismissed.
    notice: Text = ""
    /// The address of the picture the person last chose to write in, and how many times they
    /// have chosen one: the page hands both to the editor, which writes the picture in where
    /// they are writing.
    chosen: Text = ""
    chosen_count: Int merge counter = 0
}

root Document

default view writer for Document

/// Starts the document over: a blank page, or the Markdown one of the templates starts with.
/// A document that holds words is replaced all the same — the change is one the person takes
/// back, as every change is, and the templates are named in the File menu beside New.
primary action new_document(d: Document, template: Text = "blank") -> Document (
    icon: .file_plus,
    label: "New document",
    key: "Ctrl+N",
) {
    d.title = ""
    d.body = RichText.from_markdown(template_body(template))
    d.started = template
    d
}

/// The Markdown each template starts from: the pages a person starting a letter, a report, a
/// résumé or minutes writes on, and nothing for a blank page.
fn template_body(kind: Text) -> Text {
    if kind == "letter" {
        "# Letter\n\n[Your Name]\n[Street Address]\n[City, State ZIP]\n[Email Address]\n\n[Date]\n\n[Recipient Name]\n[Company / Organization]\n[Street Address]\n[City, State ZIP]\n\nDear [Recipient Name],\n\nWrite your opening paragraph here. State the purpose of your letter clearly and concisely so the reader knows immediately why you are writing.\n\nSincerely,\n\n[Your Name]\n"
    } else if kind == "report" {
        "# Report Title\n\n**Author:** [Your Name]\n**Date:** [Date]\n**Status:** Draft\n\n## Executive Summary\n\nSummarize the report's purpose, key findings and recommendations in a few sentences.\n\n## Background\n\nWhat led here, and what the reader needs to know to read the rest.\n\n## Findings\n\nWhat the work found, one point a paragraph.\n\n## Recommendations\n\nWhat should happen next, and who does it.\n"
    } else if kind == "resume" {
        "# [Your Name]\n\n[Email] · [Phone] · [City]\n\n## Summary\n\nOne or two sentences on what you do and what you are looking for.\n\n## Experience\n\n### [Role] — [Company]\n\n[Month Year] – [Month Year]\n\n- What you did, and what it changed.\n\n## Education\n\n### [Degree], [School]\n\n[Year]\n\n## Skills\n\n[Skills]\n"
    } else if kind == "meeting-notes" {
        "# Meeting notes\n\n**Date:** [Date]\n**Present:** [Names]\n\n## Agenda\n\n- What the meeting is for.\n\n## Notes\n\nWhat was said, as it matters.\n\n## Decisions\n\n- What was decided.\n\n## Next steps\n\n- [ ] Who does what, and by when.\n"
    } else { "" }
}

/// Takes away the reason the page shows over the writing: the person read it.
action dismiss_notice(d: Document) {
    d.notice = ""
}

/// Adds a paragraph of `text` at the end of the document, as Markdown says it.
action append_paragraph(d: Document, text: Text) {
    require text != "" "The paragraph needs text"
    d.body = RichText(d.body, RichText.from_markdown(text))
}

/// Replaces the whole body from Markdown: headings, lists, pipe tables, emphasis and links. A
/// picture is not written into the Markdown; it is placed with `place_image`.
action replace_body(d: Document, markdown: Text) {
    d.body = RichText.from_markdown(markdown)
}

/// What the document says as Markdown, ready for an agent or a file.
action read_content(d: Document) -> Text (label: "Read the document") {
    markdown_of(d)
}

/// The document's words as Markdown, each picture it shows written at the address of its bytes.
fn markdown_of(d: Document) -> Text {
    d.body.markdown(links: Map<Text>(), from: "")
}

/// Finds `find` in the document and replaces the first of it, or every one, with `replace`. A
/// paragraph's marks stay around the words that stay.
action replace_text(d: Document, find: Text, replace: Text, all: Bool = false) {
    require find != "" "Name the text to find"
    require d.body.text.contains(find) "The document does not hold {find}"
    if all {
        d.body = RichText.from_markdown(markdown_of(d).replace(find, replace))
    } else {
        d.body = RichText.from_markdown(replace_first(markdown_of(d), find, replace))
    }
}

/// `text` with the first `find` replaced by `replace`, and the rest as it was.
fn replace_first(text: Text, find: Text, replace: Text) -> Text {
    if let at = text.find(find) {
        text.slice(0, at) + replace + text.slice(at + find.len(), text.len())
    } else { text }
}

/// Inserts an empty table of `rows` by `columns` where the writing ends, its head the first row.
action insert_table(d: Document, rows: Int = 2, columns: Int = 2) (
    icon: .table,
    label: "Insert table",
) {
    let n = columns.max(1)
    let head = table_row(fill(n, "Column"))
    let rule = table_row(fill(n, "---"))
    let body = fill(rows.max(1), table_row(fill(n, ""))).join("\n")
    d.body = RichText(d.body, RichText.from_markdown("\n{head}\n{rule}\n{body}\n"))
}

/// `n` copies of `text`.
fn fill(n: Int, text: Text) -> List<Text> {
    if n <= 0 { [] } else { fill(n - 1, text) + [text] }
}

/// One row of a Markdown table with these cells.
fn table_row(cells: List<Text>) -> Text {
    "| " + cells.join(" | ") + " |"
}

/// The cells of a Markdown table row: its bars taken off, each cell's spaces trimmed.
fn table_cells(line: Text) -> List<Text> {
    let without = without_prefix(without_suffix(line.trim(), "|"), "|")
    without.split("|").map(c -> c.trim())
}

/// `text` without `prefix` where it opens with it.
fn without_prefix(text: Text, prefix: Text) -> Text {
    if text.starts_with(prefix) { text.slice(prefix.len(), text.len()) } else { text }
}

/// `text` without `suffix` where it ends with it.
fn without_suffix(text: Text, suffix: Text) -> Text {
    if suffix != "" and text.ends_with(suffix) {
        text.slice(0, text.len() - suffix.len())
    } else { text }
}

/// Adds a row like the last of the document's first table, under it.
action add_row(d: Document) (icon: .plus, label: "Add row", group: "Table") {
    applies has_table(d) "The document holds no table"
    let lines = markdown_of(d).split("\n")
    let last = table_lines(lines).last() ?? ""
    let row = table_row(fill(table_cells(last).len(), ""))
    let at = lines.index_of(last) ?? 0
    d.body = RichText.from_markdown(
        (lines.take(at + 1) + [row] + lines.filter(l -> (lines.index_of(l) ?? 0) > at)).join("\n"),
    )
}

/// Adds a cell to every row of the document's first table.
action add_column(d: Document) (icon: .plus, label: "Add column", group: "Table") {
    applies has_table(d) "The document holds no table"
    d.body = RichText.from_markdown(
        markdown_of(d)
            .split("\n")
            .map(l -> if l.trim().starts_with("|") { "{l}  |" } else { l })
            .join("\n"),
    )
}

/// Deletes a row of the document's first table, counted from its head at 1.
destructive action delete_row(d: Document, row: Int) (
    icon: .trash,
    label: "Delete row",
    group: "Table",
) {
    let lines = markdown_of(d).split("\n")
    let at = lines.index_of(table_lines(lines).at(row - 1) ?? "") ?? -1
    d.body = RichText.from_markdown(without_line(lines, at))
}

/// `lines` without the one at `at`, and as it was when there is no such line.
fn without_line(lines: List<Text>, at: Int) -> Text {
    lines.filter(l -> (lines.index_of(l) ?? -1) != at).join("\n")
}

/// Deletes a column of the document's first table, counted from its left at 1.
destructive action delete_column(d: Document, column: Int) (
    icon: .trash,
    label: "Delete column",
    group: "Table",
) {
    d.body = RichText.from_markdown(
        markdown_of(d).split("\n").map(l -> without_cell(l, column)).join("\n"),
    )
}

/// A Markdown table row without the cell at `column`, counted from its left at 1.
fn without_cell(line: Text, column: Int) -> Text {
    if line.trim().starts_with("|") {
        let cells = table_cells(line)
        table_row(cells.filter(c -> (cells.index_of(c) ?? 0) != column - 1))
    } else { line }
}

/// Deletes the document's first table and everything it holds.
destructive action delete_table(d: Document) (icon: .trash, label: "Delete table", group: "Table") {
    let lines = markdown_of(d).split("\n")
    d.body = RichText.from_markdown(
        lines.filter(l -> not table_lines(lines).contains(l)).join("\n"),
    )
}

/// Every line of `lines` that is one of a Markdown table's rows, in order.
fn table_lines(lines: List<Text>) -> List<Text> {
    lines.filter(l -> l.trim().starts_with("|"))
}

/// Whether the document holds a table.
fn has_table(d: Document) -> Bool {
    not table_lines(markdown_of(d).split("\n")).is_empty()
}

/// Sets the page's margins, in points from the page's edges.
action set_page_margins(d: Document, top: Float, right: Float, bottom: Float, left: Float) (
    label: "Set page margins",
) {
    d.margin_top = top
    d.margin_right = right
    d.margin_bottom = bottom
    d.margin_left = left
}

/// Sets the paper the page is written on: A4, Letter or Legal.
action set_paper(d: Document, paper: Text) (label: "Set paper") {
    d.paper = paper
}

/// Writes a table of contents at the head of the document. It is a field, not a copy: the page
/// draws the document's headings into it as they stand, so an entry a person writes or takes
/// away shows there at once. A document without headings is refused: there is nothing to list.
action generate_toc(d: Document) (icon: .list, label: "Table of contents") {
    require not headings_of(d).is_empty() "Add headings first"
    require not has_toc(d) "The document already has a table of contents"
    d.body = RichText.from_markdown("{toc_line()}\n\n{markdown_of(d)}")
}

/// The line a table of contents stands at in the document: the field a page fills with the
/// headings, written as Markdown writes it.
fn toc_line() -> Text {
    "[TOC]"
}

/// Whether the document already lists its headings in a table of contents.
fn has_toc(d: Document) -> Bool {
    markdown_of(d).split("\n").any(l -> l.trim() == toc_line())
}

/// The document's headings, as its words say them: each line that opens with `#`, its markers
/// taken off.
fn headings_of(d: Document) -> List<Text> {
    markdown_of(d)
        .split("\n")
        .filter(l -> l.trim().starts_with("#"))
        .map(l -> without_hashes(l.trim()))
}

/// Gives the document's words a link at the end of it, reading `text` or the address itself.
action set_hyperlink(d: Document, target: Text, text: Text = "") (label: "Set hyperlink") {
    require target != "" "Name the address the link goes to"
    let said = if text == "" { target } else { text }
    d.body = RichText(d.body, RichText.from_markdown("\n[{said}]({target})\n"))
}

/// Insert a page break at the end of the writing.
action insert_page_break(d: Document) (label: "Insert page break") {
    d.body = RichText(d.body, RichText.from_markdown("\n---\n"))
}

/// Puts `markdown` at the end of the writing, as a paste does.
action paste(d: Document, text: Text = "", markdown: Text = "") (label: "Paste") {
    require text != "" or markdown != "" "Nothing to paste"
    let said = if markdown == "" { text } else { markdown }
    d.body = RichText(d.body, RichText.from_markdown(said))
}

/// Replaces the whole body with `text`, as Markdown.
action replace_selection(d: Document, text: Text) {
    d.body = RichText.from_markdown(text)
}

/// Writes a picture in where the person is writing: the person picks the file, and the page
/// hands the address of its content to the editor, which puts it at the caret. The document
/// keeps the address rather than the picture.
action place_image(d: Document, image: File) (icon: .image, label: "Insert image") {
    d.chosen = image.url
    d.chosen_count += 1
}

/// The first picture the document shows, as content another app can take.
action share_image(d: Document) -> File (label: "Share image") {
    require first_picture(markdown_of(d)) != "" "The document shows no picture"
    let address = first_picture(markdown_of(d))
    File.at("image.png", address)
}

/// The address of the first picture `markdown` shows, and nothing when it shows none.
fn first_picture(markdown: Text) -> Text {
    let at = markdown.find("![](") ?? -1
    if at < 0 {
        ""
    } else {
        let rest = markdown.slice(at + 4, markdown.len())
        if let end = rest.find(")") { rest.slice(0, end) } else { "" }
    }
}

/// Deletes the picture whose address is `image`.
destructive action delete_image(d: Document, image: Text) (icon: .trash, label: "Delete image") {
    require image != "" "Name the picture to delete"
    d.body = RichText.from_markdown(markdown_of(d).replace("![]({image})", ""))
}

/// Adds your reaction with an emoji to the document, or takes it back when you had reacted with
/// it: a press on one of the pills in the bar, or the pick the add button opened.
action react(d: Document, emoji: Text) (icon: .smile_plus, label: "React") {
    let mine = if let who = d.reactions.get(emoji) { who.any(p -> p == invoker) } else { false }
    if mine { d.reactions.remove(emoji, invoker) } else { d.reactions.add(emoji, invoker) }
}

/// Says something about the document, in the thread beside its page. When `at` names a
/// paragraph and `start..end` cover words in it, the remark is anchored there: the editor marks
/// the words and stands the comment's marker in the margin beside them, so it always points at
/// what it was made about. The comment and its anchor are one change.
action comment_on(
    d: Document,
    text: RichText,
    at: Text = "",
    start: Int = 0,
    end: Int = 0,
) -> discussion.Comment (icon: .message_square, label: "Comment", did: "commented") {
    require not text.is_empty() "Write a comment first"
    let c = discussion.comment(discussion.comments, text).made_from(d)
    if at != "" and end > start {
        d.body = d.body.marked(name: "comment", value: c.id(), at: at, start: start, end: end)
    }
    c
}

/// Opens a file into the document: a Word, OpenDocument, RTF, EPUB or HTML file read by the
/// device's codecs as the Markdown the editor shows, Markdown as it stands, and plain text as
/// one paragraph a line. A file no codec here reads is refused with what it is. Opening a file
/// is one change, which the person takes back as any other.
action open_document(d: Document, file: File) -> Document (
    icon: .file_plus,
    label: "Open document",
    opens: [
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.oasis.opendocument.text",
    "application/rtf",
    "application/epub+zip",
    "text/html",
    "text/markdown",
    "text/plain",
    .docx,
    .odt,
    .rtf,
    .epub,
    .html,
    .md,
    .markdown,
    .txt,
],
) {
    // A plain text file is one paragraph a line, as a word processor reads one; every other
    // format is read by the device's codecs, as the Markdown an editor shows.
    let read = if format_of(file.name) == "text" { file.text() } else { file.markdown() }
    match read {
        .ok(said) => {
            let markdown = if format_of(file.name) == "text" { plain_markdown(said) } else { said }
            d.body = RichText.from_markdown(markdown)
            d.title = stem_of(file.name)
            d.started = "blank"
            d.notice = ""
            d
        }
        .err(why) => {
            d.notice = why
            d
        }
    }
}

/// Plain text as Markdown: every line a paragraph of its own, as a word processor reads one.
fn plain_markdown(text: Text) -> Text {
    text.split("\n").filter(l -> l.trim() != "").join("\n\n")
}

/// The file's name without its last extension.
fn stem_of(name: Text) -> Text {
    let parts = name.split(".")
    if parts.len() <= 1 { name } else { parts.take(parts.len() - 1).join(".") }
}

/// What a file's extension says it is: `markdown`, `text`, or the extension itself.
fn format_of(name: Text) -> Text {
    let ext = name.split(".").last() ?? ""
    if ext.lower() == "md" or ext.lower() == "markdown" {
        "markdown"
    } else if ext.lower() == "txt" { "text" } else { ext.lower() }
}

/// The document as a Markdown file, named by its title.
action export_content(d: Document) -> File (icon: .download, label: "Export the document") {
    File.of_text("{file_name(d)}.md", markdown_of(d))
}

/// The document as a plain-text file: its words, a paragraph a line.
action export_text(d: Document) -> File (icon: .download, label: "Export as plain text") {
    File.of_text("{file_name(d)}.txt", d.body.text)
}

/// The document as a Word, OpenDocument, RTF, EPUB or PDF file, named by its title: what a
/// person sends someone who does not write in Tilo. The bytes are the device's codecs'.
action export_document(d: Document, format: Text) -> File (
    icon: .download,
    label: "Export the document",
) {
    File.of_document("{file_name(d)}.{format}", markdown_of(d), format)
}

/// The document as one HTML page: its headings, lists and paragraphs, and its words.
action export_html(d: Document) -> File (icon: .download, label: "Export as HTML") {
    File.of_text("{file_name(d)}.html", html_of(d))
}

/// The document as one page of HTML, its lines as the elements a browser shows of them.
fn html_of(d: Document) -> Text {
    let body = markdown_of(d).split("\n").map(l -> html_line(l)).filter(l -> l != "").join("\n")
    "<!doctype html>\n<html>\n<head><meta charset=\"utf-8\"><title>{file_name(d)}</title></head>\n<body>\n{body}\n</body>\n</html>\n"
}

/// One Markdown line as the HTML a reader shows of it: a heading, a list item, or a paragraph.
/// A table's row is left out: what a caller exports for a page is its words.
fn html_line(line: Text) -> Text {
    let t = line.trim()
    if t.starts_with("#") {
        let level = hashes(t).max(1).min(6)
        "<h{level}>{without_hashes(t)}</h{level}>"
    } else if t.starts_with("- ") {
        "<li>{t.slice(2, t.len())}</li>"
    } else if t.starts_with("|") or t == "" { "" } else { "<p>{t}</p>" }
}

/// How many `#` a heading opens with.
fn hashes(t: Text) -> Int {
    if t.starts_with("#") { 1 + hashes(t.slice(1, t.len())) } else { 0 }
}

/// The words of a heading, its `#` markers and the spaces around them taken off.
fn without_hashes(t: Text) -> Text {
    if t.starts_with("#") { without_hashes(t.slice(1, t.len())) } else { t.trim() }
}

/// What the document is called as a file: its title, or "Document" while it has none.
fn file_name(d: Document) -> Text {
    if d.title.trim() == "" { "Document" } else { safe_name(d.title.trim()) }
}

/// A title as a file's name: the characters a file's name may not hold become a dash.
fn safe_name(title: Text) -> Text {
    title.replace("/", "-").replace("\\", "-").replace(":", "-")
}

/// Reads a file and answers what it says, without touching the document: what an agent reads a
/// document through before it decides what to do with it.
action read_file(d: Document, file: File) -> Text (label: "Read a file") {
    match file.text() {
        .ok(said) => said,
        .err(_) => "",
    }
}
