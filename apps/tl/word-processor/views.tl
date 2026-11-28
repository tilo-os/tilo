/// The document's window: the page a person writes on, the toolbar over it, the map of its
/// headings beside it and the panels that set it up. Its history, its search and its undo are
/// the window's, as every window's are.
view writer(d: Document) {
    // The formatting the person last asked of the editor, and how many asks there were: each ask
    // is told apart from the one before, so asking for bold twice is two commands.
    state asked: Int = 0
    state format_ask: Option<Text> = .none
    // Which panel stands beside the page: the page's setup, the review of proposed changes, or
    // none.
    state panel: Text = ""
    state ruler: Bool = false
    state outline: Bool = false
    // The tracked changes the editor reports, by id and kind (0 inserted, 1 deleted), so the
    // review panel takes or rejects one at a time.
    state change_ids: List<Text> = []
    state change_kinds: List<Int> = []
    // The headings the editor reports, and the one a person chose in the map of them.
    state heading_lines: List<Text> = []
    state heading_levels: List<Int> = []
    state reveal: Option<Text> = .none
    // The words the window's search holds, and what they are replaced with: the page's search
    // finds, and the app writes the replacement over what was found.
    state found: Text = ""
    state replacement: Text = ""
    // How much larger the page reads than its own size, and how many sheets it fills at the width
    // it is laid out at: the editor reports the count as the width and the words change.
    state zoom: Float = 1.0
    state page_count: Int = 0
    // The sheet the caret stands on, or the one the view's top shows: the "N" of "Page N of M".
    state page_shown: Int = 0
    // Where the person's caret or selection stands, as the editor reports it: the block it
    // starts in, and where it starts and ends there. What a comment written now is anchored to.
    state comment_at: Text = ""
    state comment_start: Int = 0
    state comment_end: Int = 0
    // The remark being written in the thread beside the page, and the comment whose marker in
    // the margin the person pressed to read it.
    state comment_draft: RichText = RichText("")
    state comment_open: Text = ""
    ui.NavShell(content_kind: .document, title: meta(d).title) {
        // What a person types in the window's search shows the paragraph holding it: the page
        // moves there and the caret stands at its start.
        on search(words: Text): set(reveal, words_place(d, words, asked))
            .then(set(asked, asked + 1))
            .then(set(found, words))
        actions: {
            toolbar(d, format_ask, asked, panel, ruler, outline, zoom)
            spacer()
            file_menu(d)
        }
        sidebar: {
            if outline {
                std.headings("Outline", heading_lines, heading_levels, reveal)
            }
        }
        inspector: {
            if panel != "" {
                side_panel(
                    d,
                    format_ask,
                    asked,
                    panel,
                    change_ids,
                    change_kinds,
                    comment_draft,
                    comment_open,
                    comment_at,
                    comment_start,
                    comment_end,
                )
            }
        }
        content: {
            stack(fill: 1.0) {
                if ruler {
                    ruler_band(d, format_ask, asked)
                }
                if found.trim() != "" {
                    find_and_replace(d, found, replacement)
                }
                if d.view_only {
                    ui.Banner(
                        "This document is open to view only",
                        message: "Save a copy with Save as to write in it.",
                        kind: .warning,
                    )
                }
                if d.notice != "" {
                    ui.Banner(
                        if d.notice.starts_with("Couldn’t save") {
                            "The document could not be saved"
                        } else { "The document could not be opened" },
                        message: d.notice,
                        kind: .warning,
                    ) {
                        on dismiss: dismiss_notice(d)
                    }
                }
                ui.Scroll {
                    page {
                        stack(gap: .group) {
                            ui.TextInput(
                                bind: d.title,
                                title: true,
                                placeholder: "Untitled Document",
                            )
                            ui.Reactions(reactions: d.reactions) {
                                on toggle(e): react(d, e)
                            }
                            stack(gap: .related) {
                                ui.BlockEditor(
                                    bind: d.body,
                                    placeholder: "Start writing",
                                    reveal: reveal,
                                    format: format_ask,
                                    page: d.paper,
                                    header: if d.header == "" { d.title } else { d.header },
                                    footer: d.footer,
                                    margin_top: d.margin_top,
                                    margin_right: d.margin_right,
                                    margin_bottom: d.margin_bottom,
                                    margin_left: d.margin_left,
                                    zoom: zoom,
                                    read_only: d.view_only,
                                    spellcheck: d.spelling,
                                    spell_language: if d.spell_language == "" {
                                        .none
                                    } else { .some(d.spell_language) },
                                    insert: if d.chosen == "" {
                                        .none
                                    } else { .some("{d.chosen}:{d.chosen_count}") },
                                    slash: ["image"],
                                ) {
                                    on headings(words, levels): set(heading_lines, words)
                                        .then(
                                            set(heading_levels, levels),
                                        )
                                    on changes(ids, kinds): set(change_ids, ids)
                                        .then(
                                            set(change_kinds, kinds),
                                        )
                                    on selection(block, range): set(comment_at, block)
                                        .then(
                                            set(comment_start, range.at(0) ?? 0),
                                        )
                                        .then(
                                            set(comment_end, range.at(1) ?? 0),
                                        )
                                    on comment(id): set(comment_open, id)
                                        .then(
                                            set(panel, "comments"),
                                        )
                                    on page_count(count): set(page_count, count)
                                    on page_shown(sheet): set(page_shown, sheet)
                                    on slash_image: file.choose(
                                        place_image(d),
                                        accept: ["image/png", "image/jpeg", "image/webp"],
                                    )
                                }
                                std.linked(d, compact: true)
                            }
                        }
                    }
                }
                status_band(d, page_shown, page_count, zoom)
            }
        }
    }
}

/// The line under the title while the window's search holds words: the words the page was
/// searched for, and what a person may write over them. The search is the window's, as every
/// window's is; replacing is the app's, and it is one action, taken back as one change.
template view find_and_replace(d: Document, found: Text, replacement: State<Text>) {
    row(gap: .related, align: .center, padding: .related) {
        text("Replace {found} with", role: .caption, secondary: true)
        ui.TextInput(bind: replacement, placeholder: "The replacement", fill)
        ui.Button("Replace", icon: .arrow_right) {
            on press: replace_text(d, found, replacement)
        }
        ui.Button("Replace all", icon: .chevrons_right) {
            on press: replace_text(d, found, replacement, all: true)
        }
    }
}

/// The toolbar over the page: the words' formatting, what is inserted, and the panels.
template view toolbar(
    d: Document,
    format_ask: State<Option<Text>>,
    asked: State<Int>,
    panel: State<Text>,
    ruler: State<Bool>,
    outline: State<Bool>,
    zoom: State<Float>,
) {
    ui.Toolbar {
        ui.ToolbarButton("Bold", icon: .bold, priority: 1) {
            on press: set(asked, asked + 1).then(set(format_ask, "bold:{asked + 1}"))
        }
        ui.ToolbarButton("Italic", icon: .italic, priority: 2) {
            on press: set(asked, asked + 1).then(set(format_ask, "italic:{asked + 1}"))
        }
        ui.ToolbarButton("Underline", icon: .underline, priority: 3) {
            on press: set(asked, asked + 1).then(set(format_ask, "underline:{asked + 1}"))
        }
        ui.ToolbarButton("Strikethrough", icon: .strikethrough, priority: 4) {
            on press: set(asked, asked + 1).then(set(format_ask, "strike:{asked + 1}"))
        }
        ui.ToolbarButton("Superscript", icon: .superscript, priority: 5) {
            on press: set(asked, asked + 1).then(set(format_ask, "super:{asked + 1}"))
        }
        ui.ToolbarButton("Subscript", icon: .subscript, priority: 6) {
            on press: set(asked, asked + 1).then(set(format_ask, "sub:{asked + 1}"))
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Bulleted list", icon: .list, priority: 7) {
            on press: set(asked, asked + 1).then(set(format_ask, "bullets:{asked + 1}"))
        }
        ui.ToolbarButton("Numbered list", icon: .list_ordered, priority: 8) {
            on press: set(asked, asked + 1).then(set(format_ask, "numbers:{asked + 1}"))
        }
        ui.ToolbarButton("Heading 1", icon: .heading_1, priority: 9) {
            on press: set(asked, asked + 1).then(set(format_ask, "heading:1:{asked + 1}"))
        }
        ui.ToolbarButton("Heading 2", icon: .heading_2, priority: 10) {
            on press: set(asked, asked + 1).then(set(format_ask, "heading:2:{asked + 1}"))
        }
        ui.ToolbarButton("Heading 3", priority: 11) {
            on press: set(asked, asked + 1).then(set(format_ask, "heading:3:{asked + 1}"))
        }
        ui.ToolbarButton("Body text", icon: .text_cursor, priority: 12) {
            on press: set(asked, asked + 1).then(set(format_ask, "heading:0:{asked + 1}"))
        }
        ui.ToolbarButton("Text", icon: .palette, toggled: panel == "format", priority: 13) {
            on press: set(panel, if panel == "format" { "" } else { "format" })
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Insert table", icon: .table, priority: 14) {
            on press: insert_table(d)
        }
        ui.ToolbarButton("Insert image", icon: .image, priority: 15) {
            on press: file.choose(place_image(d), accept: ["image/png", "image/jpeg", "image/webp"])
        }
        ui.ToolbarButton("Insert page break", priority: 16) {
            on press: insert_page_break(d)
        }
        ui.ToolbarButton("Table of contents", icon: .list_tree, priority: 17) {
            on press: generate_toc(d)
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Styles", icon: .panel_right, toggled: panel == "styles", priority: 19) {
            on press: set(panel, if panel == "styles" { "" } else { "styles" })
        }
        // A comment on the words the person has selected: the thread beside the page says what
        // is said about the document, and what is written there is anchored where they are.
        ui.ToolbarButton(
            "Comment",
            icon: .message_square_plus,
            toggled: panel == "comments",
            priority: 18,
        ) {
            on press: set(panel, if panel == "comments" { "" } else { "comments" })
        }
        ui.ToolbarButton(
            "Review",
            icon: .message_square,
            toggled: panel == "review",
            priority: 20,
        ) {
            on press: set(panel, if panel == "review" { "" } else { "review" })
        }
        ui.ToolbarButton("Ruler", icon: .ruler, toggled: ruler, priority: 21) {
            on press: set(ruler, not ruler)
        }
        ui.ToolbarButton("Outline", icon: .list_tree, toggled: outline, priority: 22) {
            on press: set(outline, not outline)
        }
        ui.ToolbarSeparator()
        // The page's size: smaller, larger, or the size its menu names.
        ui.ToolbarButton("Zoom out", icon: .zoom_out, priority: 23) {
            on press: set(zoom, (zoom * 0.8).clamp(0.4, 3.0))
        }
        ui.ToolbarButton("{zoom_percent(zoom)}%", priority: 24) {
            menu: {
                for step in zoom_steps() {
                    ui.MenuItem("{zoom_percent(step)}%", checked: zoom == step) {
                        on choose: set(zoom, step)
                    }
                }
            }
        }
        ui.ToolbarButton("Zoom in", icon: .zoom_in, priority: 25) {
            on press: set(zoom, (zoom * 1.25).clamp(0.4, 3.0))
        }
        ui.ToolbarSpacer()
    }
}

/// The File menu: a new page from a template, a file opened into the document, the document
/// saved back to the file it was opened from, and what it is written out as — Markdown, plain
/// text, HTML, Word, OpenDocument, rich text, EPUB or a PDF.
view file_menu(d: Document) {
    ui.MenuButton("File", icon: .file_plus, role: .bar) {
        ui.MenuItem("Open…", icon: .folder, key: "Ctrl+O") {
            on choose: file.choose(open_document(d), accept: opened_kinds(), edit: true)
        }
        // Save writes the document back to the file it was opened from, in that file's format; a
        // page that has never been a file asks where it goes, as Markdown.
        ui.MenuItem("Save", icon: .hard_drive, key: "Ctrl+S") {
            on choose: if d.source == "" {
                file.save(export_document(d, saved_format(d)), name: saved_name(d))
            } else { save(d) }
        }
        ui.MenuItem("Save as Markdown…", icon: .download) {
            on choose: file.save(export_content(d), name: "{file_name(d)}.md")
        }
        ui.MenuItem("Save as plain text…", icon: .download) {
            on choose: file.save(export_text(d), name: "{file_name(d)}.txt")
        }
        ui.MenuItem("Export as HTML…", icon: .download) {
            on choose: file.save(export_html(d), name: "{file_name(d)}.html")
        }
        ui.MenuItem("Save as Word (.docx)…", icon: .download) {
            on choose: file.save(export_document(d, "docx"), name: "{file_name(d)}.docx")
        }
        ui.MenuItem("Save as OpenDocument (.odt)…", icon: .download) {
            on choose: file.save(export_document(d, "odt"), name: "{file_name(d)}.odt")
        }
        ui.MenuItem("Save as rich text (.rtf)…", icon: .download) {
            on choose: file.save(export_document(d, "rtf"), name: "{file_name(d)}.rtf")
        }
        ui.MenuItem("Save as EPUB…", icon: .download) {
            on choose: file.save(export_document(d, "epub"), name: "{file_name(d)}.epub")
        }
        ui.MenuItem("Export as PDF…", icon: .download) {
            on choose: file.save(export_document(d, "pdf"), name: "{file_name(d)}.pdf")
        }
        ui.MenuItem("Start from a letter", icon: .file_text) {
            on choose: new_document(d, "letter")
        }
        ui.MenuItem("Start from a report", icon: .file_text) {
            on choose: new_document(d, "report")
        }
        ui.MenuItem("Start from a résumé", icon: .file_text) {
            on choose: new_document(d, "resume")
        }
        ui.MenuItem("Start from meeting notes", icon: .file_text) {
            on choose: new_document(d, "meeting-notes")
        }
    }
}

/// The band over the page while the ruler is on: the margins the page writes between, each set
/// where it stands, and the page's paper.
template view ruler_band(d: Document, format_ask: State<Option<Text>>, asked: State<Int>) {
    row(gap: .section, align: .center, padding: .related) {
        stack(gap: .tight) {
            text("Margins in points", role: .caption, secondary: true)
            row(gap: .tight, align: .center) {
                ui.NumberInput(bind: d.margin_left, label: "Left", min: 0.0, max: 288.0)
                ui.NumberInput(bind: d.margin_top, label: "Top", min: 0.0, max: 288.0)
                ui.NumberInput(bind: d.margin_right, label: "Right", min: 0.0, max: 288.0)
                ui.NumberInput(bind: d.margin_bottom, label: "Bottom", min: 0.0, max: 288.0)
            }
        }
        stack(gap: .tight) {
            text("Paper", role: .caption, secondary: true)
            row(gap: .tight) {
                ui.Chip("A4", selected: d.paper == "A4") {
                    on press: set_paper(d, "A4")
                }
                ui.Chip("Letter", selected: d.paper == "Letter") {
                    on press: set_paper(d, "Letter")
                }
                ui.Chip("Legal", selected: d.paper == "Legal") {
                    on press: set_paper(d, "Legal")
                }
            }
        }
        spacer()
        stack(gap: .tight) {
            text("Paragraph", role: .caption, secondary: true)
            row(gap: .tight) {
                ui.Chip("Body") {
                    on press: set(asked, asked + 1).then(set(format_ask, "heading:0:{asked + 1}"))
                }
                ui.Chip("Heading 1") {
                    on press: set(asked, asked + 1).then(set(format_ask, "heading:1:{asked + 1}"))
                }
                ui.Chip("Heading 2") {
                    on press: set(asked, asked + 1).then(set(format_ask, "heading:2:{asked + 1}"))
                }
                ui.Chip("Heading 3") {
                    on press: set(asked, asked + 1).then(set(format_ask, "heading:3:{asked + 1}"))
                }
                ui.Chip("Bulleted") {
                    on press: set(asked, asked + 1).then(set(format_ask, "bullets:{asked + 1}"))
                }
                ui.Chip("Numbered") {
                    on press: set(asked, asked + 1).then(set(format_ask, "numbers:{asked + 1}"))
                }
            }
            row(gap: .tight) {
                text("Align", role: .caption, secondary: true)
                ui.Chip("Left") {
                    on press: set(asked, asked + 1).then(set(format_ask, "align:left:{asked + 1}"))
                }
                ui.Chip("Centre") {
                    on press: set(asked, asked + 1)
                        .then(set(format_ask, "align:center:{asked + 1}"))
                }
                ui.Chip("Right") {
                    on press: set(asked, asked + 1).then(set(format_ask, "align:right:{asked + 1}"))
                }
                ui.Chip("Justified") {
                    on press: set(asked, asked + 1)
                        .then(set(format_ask, "align:justify:{asked + 1}"))
                }
            }
            row(gap: .tight) {
                text("Indent", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "indent:{step}:{asked + 1}"),
                            )
                    }
                }
            }
        }
    }
}

/// What one tracked change did, read by its place among the ids the editor reported.
fn change_kind(ids: List<Text>, kinds: List<Int>, id: Text) -> Text {
    let at = ids.index_of(id) ?? 0
    if (kinds.at(at) ?? 0) == 1 { "Deleted" } else { "Inserted" }
}

/// The steps a paragraph's indent and spacing are set in, in points: none, then the widths a
/// writer moves text by.
fn steps() -> List<Int> {
    [0, 6, 12, 18, 24]
}

/// The line spacings a paragraph is set to, as the panel names them: the document's own, then
/// the multiples of it a writer sets rows apart by.
fn line_spacings() -> List<Text> {
    ["1", "1.15", "1.5", "2"]
}

/// The panel beside the page: the page's setup, the words' own look, or the review of what
/// others proposed.
template view side_panel(
    d: Document,
    format_ask: State<Option<Text>>,
    asked: State<Int>,
    panel: State<Text>,
    change_ids: List<Text>,
    change_kinds: List<Int>,
    comment_draft: State<RichText>,
    comment_open: Text,
    comment_at: Text,
    comment_start: Int,
    comment_end: Int,
) {
    ui.Scroll {
        stack(gap: .group, padding: .related) {
            row(gap: .tight, align: .center) {
                text(panel_name(panel), role: .heading)
                spacer()
                ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
                    on press: set(panel, "")
                }
            }
            if panel == "styles" {
                page_setup(d, format_ask, asked)
            } else if panel == "format" {
                text_format(format_ask, asked)
            } else if panel == "comments" {
                if not brought_comments(d).is_empty() {
                    section("From the file") {
                        for c in brought_comments(d) {
                            stack(gap: .tight) {
                                text("{c.author} · {c.date}", role: .caption, secondary: true)
                                text(c.text, role: .body)
                            }
                        }
                    }
                }
                discussion.thread_where(
                    d,
                    comment_draft,
                    comment_on(d, comment_draft, comment_at, comment_start, comment_end),
                    open: comment_open,
                )
            } else {
                review_panel(d, format_ask, asked, change_ids, change_kinds)
            }
        }
    }
}

/// What the panel beside the page is called.
fn panel_name(panel: Text) -> Text {
    if panel == "styles" {
        "Page setup"
    } else if panel == "format" {
        "Text"
    } else if panel == "comments" { "Comments" } else { "Review" }
}

/// The words' own look, as the toolbar's commands set it: the face, the size, the ink and the
/// band behind it. A person selects words and presses one; with nothing selected it is the look
/// the words they type next take.
view text_format(format_ask: State<Option<Text>>, asked: State<Int>) {
    stack(gap: .group) {
        section("Face") {
            row(gap: .tight, wrap: true) {
                ui.Chip("Sans") {
                    on press: set(asked, asked + 1).then(set(format_ask, "font:sans:{asked + 1}"))
                }
                ui.Chip("Serif") {
                    on press: set(asked, asked + 1).then(set(format_ask, "font:serif:{asked + 1}"))
                }
                ui.Chip("Mono") {
                    on press: set(asked, asked + 1).then(set(format_ask, "font:mono:{asked + 1}"))
                }
            }
        }
        section("Size") {
            row(gap: .tight, wrap: true) {
                for size in sizes() {
                    ui.Chip("{size}") {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "size:{size}:{asked + 1}"),
                            )
                    }
                }
            }
        }
        section("Text colour") {
            ui.ColorSwatchPicker(swatches: text_colors(), columns: 6, label: "Text colour") {
                on change(index): set(asked, asked + 1)
                    .then(
                        set(format_ask, "color:{color_at(text_colors(), index)}:{asked + 1}"),
                    )
            }
        }
        section("Highlight") {
            ui.ColorSwatchPicker(swatches: highlight_colors(), columns: 6, label: "Highlight") {
                on change(index): set(asked, asked + 1)
                    .then(
                        set(
                            format_ask,
                            "highlight:{color_at(highlight_colors(), index)}:{asked + 1}",
                        ),
                    )
            }
        }
        // A picture the caret stands in: its size, which way it faces, how much of it shows and
        // how it sits among the words.
        section("Picture") {
            text("With the caret in a picture, these change it.", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                ui.Chip("Smaller") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:size:down:{asked + 1}"),
                        )
                }
                ui.Chip("Larger") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:size:up:{asked + 1}"),
                        )
                }
                ui.Chip("Flip horizontally") {
                    on press: set(asked, asked + 1)
                        .then(set(format_ask, "picture:flip:h:{asked + 1}"))
                }
                ui.Chip("Flip vertically") {
                    on press: set(asked, asked + 1)
                        .then(set(format_ask, "picture:flip:v:{asked + 1}"))
                }
            }
            row(gap: .tight, wrap: true) {
                ui.Chip("Crop") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:crop:in:{asked + 1}"),
                        )
                }
                ui.Chip("Show it all again") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:crop:reset:{asked + 1}"),
                        )
                }
            }
            row(gap: .tight, wrap: true) {
                text("Placement", role: .caption, secondary: true)
                ui.Chip("In line") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:place:inline:{asked + 1}"),
                        )
                }
                ui.Chip("Free") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:place:free:{asked + 1}"),
                        )
                }
            }
            row(gap: .tight, wrap: true) {
                text("Text wraps", role: .caption, secondary: true)
                ui.Chip("None") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:wrap:none:{asked + 1}"),
                        )
                }
                ui.Chip("Left") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:wrap:left:{asked + 1}"),
                        )
                }
                ui.Chip("Right") {
                    on press: set(asked, asked + 1)
                        .then(
                            set(format_ask, "picture:wrap:right:{asked + 1}"),
                        )
                }
            }
        }
    }
}

/// The sizes a person sets a run to, in points.
fn sizes() -> List<Int> {
    [8, 9, 10, 11, 12, 14, 16, 18, 24, 36, 48]
}

/// The inks a person sets words to: the page's own black and greys, then the colours that read
/// as a note, a link, a correction and a mark. A style's ink is one of the same.
fn text_colors() -> List<Text> {
    style_inks()
}

/// The bands a person marks words with, pale enough that the words still read.
fn highlight_colors() -> List<Text> {
    ["#fff8c5", "#ddf4ff", "#dafbe1", "#ffebe9", "#fbefff", "#ffe0c2", "#e6e6e6", "#ffffff"]
}

/// The colour at `index` of `colors`, and the page's ink when there is none.
fn color_at(colors: List<Text>, index: Int) -> Text {
    colors.at(index) ?? "#1f2328"
}

/// How the page is set up: its paper, its margins, the paragraph styles a person may give a
/// block, and what the document was started as.
view page_setup(d: Document, format_ask: State<Option<Text>>, asked: State<Int>) {
    stack(gap: .group) {
        section("Paper") {
            row(gap: .tight, wrap: true) {
                ui.Chip("A4", selected: d.paper == "A4") {
                    on press: set_paper(d, "A4")
                }
                ui.Chip("Letter", selected: d.paper == "Letter") {
                    on press: set_paper(d, "Letter")
                }
                ui.Chip("Legal", selected: d.paper == "Legal") {
                    on press: set_paper(d, "Legal")
                }
            }
        }
        section("Margins") {
            stack(gap: .tight) {
                ui.NumberInput(bind: d.margin_top, label: "Top", min: 0.0, max: 288.0)
                ui.NumberInput(bind: d.margin_right, label: "Right", min: 0.0, max: 288.0)
                ui.NumberInput(bind: d.margin_bottom, label: "Bottom", min: 0.0, max: 288.0)
                ui.NumberInput(bind: d.margin_left, label: "Left", min: 0.0, max: 288.0)
            }
        }
        section("Spelling") {
            row(gap: .related, align: .center) {
                text("Check spelling in this document", role: .body)
                spacer()
                ui.Toggle(bind: d.spelling)
            }
            ui.TextInput(
                bind: d.spell_language,
                placeholder: "The dictionary's language, such as de-DE; empty for your own",
            )
        }
        section("Header and footer") {
            stack(gap: .tight) {
                ui.TextInput(bind: d.header, placeholder: "Header: the document's title")
                ui.TextInput(bind: d.footer, placeholder: "Footer, before the page number")
                text(
                    "Each page's top margin carries the header, and its foot the footer and the page number.",
                    role: .caption,
                    secondary: true,
                )
            }
        }
        section("Paragraph") {
            row(gap: .tight, wrap: true) {
                text("Before", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "before:{step}:{asked + 1}"),
                            )
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                text("After", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "after:{step}:{asked + 1}"),
                            )
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                text("Line spacing", role: .caption, secondary: true)
                for step in line_spacings() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "spacing:{step}:{asked + 1}"),
                            )
                    }
                }
            }
        }
        style_sheet(d, format_ask, asked)
        section("Set up") {
            text("Started as a {d.started} page.", role: .caption, secondary: true)
            text(
                "The page's margins and paper are the document's own, on every device.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// The document's named styles: pressing one sets the paragraph the caret is in in it and chooses
/// it here, where its face, size, weight, ink, alignment and spacing are set, and every paragraph
/// set in it follows. A new style starts from the one chosen; one the document made can be
/// duplicated and deleted.
view style_sheet(d: Document, format_ask: State<Option<Text>>, asked: State<Int>) {
    state chosen: Text = "Body"
    state draft: Text = ""
    section("Styles") {
        stack(gap: .tight) {
            row(gap: .tight, wrap: true) {
                for name in d.body.style_names() {
                    ui.Chip(name, selected: name == chosen) {
                        on press: set(chosen, name)
                            .then(set(asked, asked + 1))
                            .then(
                                set(format_ask, "style:{name}:{asked + 1}"),
                            )
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                text("Face", role: .caption, secondary: true)
                for face in ["sans", "serif", "mono"] {
                    ui.Chip(
                        face_name(face),
                        selected: d.body.style_look(style: chosen, key: "font") == face,
                    ) {
                        on press: set_style_look(d, chosen, font: .some(face))
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                text("Size", role: .caption, secondary: true)
                for size in style_sizes() {
                    ui.Chip(
                        "{size.round()} pt",
                        selected: d.body.style_look(style: chosen, key: "size")
                            == "{size.round()}",
                    ) {
                        on press: set_style_look(d, chosen, size: .some(size))
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                ui.Chip("Bold", selected: d.body.style_look(style: chosen, key: "bold") == "true") {
                    on press: set_style_look(
                        d,
                        chosen,
                        bold: .some(d.body.style_look(style: chosen, key: "bold") != "true"),
                    )
                }
                ui.Chip(
                    "Italic",
                    selected: d.body.style_look(style: chosen, key: "italic") == "true",
                ) {
                    on press: set_style_look(
                        d,
                        chosen,
                        italic: .some(d.body.style_look(style: chosen, key: "italic") != "true"),
                    )
                }
                for side in ["left", "center", "right", "justify"] {
                    ui.Chip(
                        align_name(side),
                        selected: d.body.style_look(style: chosen, key: "align") == side,
                    ) {
                        on press: set_style_look(d, chosen, align: .some(side))
                    }
                }
            }
            ui.ColorSwatchPicker(
                swatches: text_colors(),
                columns: 8,
                selected: text_colors().index_of(d.body.style_look(style: chosen, key: "color")),
                label: "Style colour",
            ) {
                on change(index): set_style_ink(d, chosen, index)
            }
            row(gap: .tight, wrap: true) {
                text("After", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set_style_look(d, chosen, after: .some(step.to_float()))
                    }
                }
            }
            row(gap: .tight, align: .center) {
                ui.TextInput(bind: draft, placeholder: "A new style's name", fill)
                ui.Button("New style", icon: .plus) {
                    on press: new_style(d, draft, chosen)
                        .then(set(chosen, draft.trim()))
                        .then(set(draft, ""))
                }
            }
            row(gap: .tight, wrap: true) {
                ui.Button("Duplicate", icon: .copy, role: .plain) {
                    on press: duplicate_style(d, chosen).then(set(chosen, "{chosen} copy"))
                }
                if not built_in_styles().contains(chosen) {
                    ui.Button("Delete", icon: .trash, role: .plain) {
                        on press: delete_style(d, chosen).then(set(chosen, "Body"))
                    }
                }
            }
        }
    }
}

/// A face as the style panel names it.
fn face_name(face: Text) -> Text {
    if face == "serif" { "Serif" } else if face == "mono" { "Mono" } else { "Sans" }
}

/// An alignment as the style panel names it.
fn align_name(side: Text) -> Text {
    if side == "center" {
        "Centre"
    } else if side == "right" { "Right" } else if side == "justify" { "Justified" } else { "Left" }
}

/// The sizes a style is set to, in points.
fn style_sizes() -> List<Float> {
    [10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 24.0, 32.0]
}

/// What others suggested for the document: tracked changes a person accepts or rejects, and the
/// suggestions waiting for their yes.
view review_panel(
    d: Document,
    format_ask: State<Option<Text>>,
    asked: State<Int>,
    change_ids: List<Text>,
    change_kinds: List<Int>,
) {
    stack(gap: .group) {
        section("Tracked changes") {
            if change_ids.is_empty() {
                text("No changes stand in the words.", role: .caption, secondary: true)
            }
            for id in change_ids {
                row(gap: .tight, align: .center, wrap: true) {
                    text(change_kind(change_ids, change_kinds, id), role: .caption)
                    ui.Button("Accept", role: .plain, size: .small) {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "change:accept:{id}:{asked + 1}"),
                            )
                    }
                    ui.Button("Reject", role: .plain, size: .small) {
                        on press: set(asked, asked + 1)
                            .then(
                                set(format_ask, "change:reject:{id}:{asked + 1}"),
                            )
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                ui.Button("Accept all changes", icon: .check) {
                    on press: set(asked, asked + 1)
                        .then(set(format_ask, "changes:accept:{asked + 1}"))
                }
                ui.Button("Reject all changes", icon: .x) {
                    on press: set(asked, asked + 1)
                        .then(set(format_ask, "changes:reject:{asked + 1}"))
                }
            }
        }
        std.suggestions(d)
        text(
            "A person or agent that suggests a change to the words leaves it here until you answer it.",
            role: .caption,
            secondary: true,
        )
    }
}

/// What stands under the page: how much the document says, which sheet of how many the person
/// is on, and what the page is set for.
view status_band(d: Document, page_shown: Int, page_count: Int, zoom: Float) {
    row(gap: .related, align: .center, padding: .related) {
        spacer()
        text("{d.body.text.words()} words", role: .caption, secondary: true)
        text("·", role: .caption, secondary: true)
        text("{d.body.text.len()} characters", role: .caption, secondary: true)
        if page_count > 0 {
            text("·", role: .caption, secondary: true)
            if page_shown > 0 {
                text("Page {page_shown} of {page_count}", role: .caption, secondary: true)
            } else {
                text("{page_count} pages", role: .caption, secondary: true)
            }
        }
        if zoom != 1.0 {
            text("·", role: .caption, secondary: true)
            text("{zoom_percent(zoom)}%", role: .caption, secondary: true)
        }
        text("·", role: .caption, secondary: true)
        text(d.paper, role: .caption, secondary: true)
    }
}

/// A zoom as the person reads it, in percent.
fn zoom_percent(zoom: Float) -> Int {
    (zoom * 100.0).round()
}

/// The zooms the page's menu offers, the size it is set at counted once.
fn zoom_steps() -> List<Float> {
    [0.5, 0.75, 1.0, 1.25, 1.5, 2.0]
}

/// Where `words` stand in the document, as the page asks it to show them: the place of the
/// paragraph holding them, counted from the top at 0, then a count that changes with each ask,
/// so the same words asked for again show again. Nothing when the document holds no such words.
fn words_place(d: Document, words: Text, asked: Int) -> Option<Text> {
    if words.trim() == "" {
        .none
    } else {
        let lines = markdown_of(d).split("\n")
        if let line = lines.find(l -> l.lower().contains(words.lower())) {
            .some("¶{lines.index_of(line) ?? 0}:{asked + 1}")
        } else { .none }
    }
}
