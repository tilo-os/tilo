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
    // The headings the editor reports, and the one a person chose in the map of them.
    state heading_lines: List<Text> = []
    state heading_levels: List<Int> = []
    state reveal: Option<Text> = .none
    ui.NavShell(content_kind: .document, title: meta(d).title) {
        // What a person types in the window's search shows the paragraph holding it: the page
        // moves there and the caret stands at its start.
        on search(words: Text): set(reveal, words_place(d, words, asked)).then(
            set(asked, asked + 1),
        )
        actions: {
            toolbar(d, format_ask, asked, panel, ruler, outline)
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
                side_panel(d, format_ask, asked, panel)
            }
        }
        content: {
            stack(fill: 1.0) {
                if ruler {
                    ruler_band(d, format_ask, asked)
                }
                if d.notice != "" {
                    ui.Banner(
                        "The document could not be opened",
                        message: d.notice,
                        kind: .warning,
                    ) {
                        on dismiss: dismiss_notice(d)
                    }
                }
                ui.Scroll {
                    page {
                        stack(gap: .group) {
                            ui.TextInput(bind: d.title, title: true, placeholder: "Untitled Document")
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
                                    header: d.title,
                                    margin_top: d.margin_top,
                                    margin_right: d.margin_right,
                                    margin_bottom: d.margin_bottom,
                                    margin_left: d.margin_left,
                                    insert: if d.chosen == "" {
                                        .none
                                    } else {
                                        .some("{d.chosen}:{d.chosen_count}")
                                    },
                                    slash: ["image"],
                                ) {
                                    on headings(words, levels): set(heading_lines, words).then(
                                        set(heading_levels, levels),
                                    )
                                    on slash_image: file.choose(
                                        place_image(d),
                                        accept: ["image/png", "image/jpeg", "image/webp"],
                                    )
                                }
                                std.linked(d, compact: true)
                            }
                            std.after_body {
                                discussion.thread(d)
                            }
                        }
                    }
                }
                status_band(d)
            }
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
            on press: file.choose(
                place_image(d),
                accept: ["image/png", "image/jpeg", "image/webp"],
            )
        }
        ui.ToolbarButton("Insert page break", priority: 16) {
            on press: insert_page_break(d)
        }
        ui.ToolbarButton("Table of contents", icon: .list_tree, priority: 17) {
            on press: generate_toc(d)
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Styles", icon: .panel_right, toggled: panel == "styles", priority: 18) {
            on press: set(panel, if panel == "styles" { "" } else { "styles" })
        }
        ui.ToolbarButton("Review", icon: .message_square, toggled: panel == "review", priority: 19) {
            on press: set(panel, if panel == "review" { "" } else { "review" })
        }
        ui.ToolbarButton("Ruler", icon: .ruler, toggled: ruler, priority: 20) {
            on press: set(ruler, not ruler)
        }
        ui.ToolbarButton("Outline", icon: .list_tree, toggled: outline, priority: 21) {
            on press: set(outline, not outline)
        }
        ui.ToolbarSpacer()
    }
}

/// The File menu: a new page from a template, a file opened into the document, and the document
/// written out as Markdown, plain text or a web page.
view file_menu(d: Document) {
    ui.MenuButton("File", icon: .file_plus, role: .bar) {
        ui.MenuItem("Open…", icon: .folder, key: "Ctrl+O") {
            on choose: file.choose(
                open_document(d),
                accept: ["text/markdown", "text/plain"],
            )
        }
        ui.MenuItem("Save as Markdown…", icon: .download, key: "Ctrl+S") {
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
                    on press: set(asked, asked + 1).then(set(format_ask, "align:center:{asked + 1}"))
                }
                ui.Chip("Right") {
                    on press: set(asked, asked + 1).then(set(format_ask, "align:right:{asked + 1}"))
                }
                ui.Chip("Justified") {
                    on press: set(asked, asked + 1).then(set(format_ask, "align:justify:{asked + 1}"))
                }
            }
            row(gap: .tight) {
                text("Indent", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1).then(
                            set(format_ask, "indent:{step}:{asked + 1}"),
                        )
                    }
                }
            }
        }
    }
}

/// The steps a paragraph's indent and spacing are set in, in points: none, then the widths a
/// writer moves text by.
fn steps() -> List<Int> {
    [0, 6, 12, 18, 24]
}

/// The styles every document has, as the paragraph panel names them.
fn style_names() -> List<Text> {
    ["Body", "Heading 1", "Heading 2", "Heading 3"]
}

/// The panel beside the page: the page's setup, the words' own look, or the review of what
/// others proposed.
template view side_panel(
    d: Document,
    format_ask: State<Option<Text>>,
    asked: State<Int>,
    panel: State<Text>,
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
            } else {
                review_panel(d, format_ask, asked)
            }
        }
    }
}

/// What the panel beside the page is called.
fn panel_name(panel: Text) -> Text {
    if panel == "styles" { "Page setup" } else if panel == "format" { "Text" } else { "Review" }
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
                        on press: set(asked, asked + 1).then(
                            set(format_ask, "size:{size}:{asked + 1}"),
                        )
                    }
                }
            }
        }
        section("Text colour") {
            ui.ColorSwatchPicker(
                swatches: text_colors(),
                columns: 6,
                label: "Text colour",
            ) {
                on change(index): set(asked, asked + 1).then(
                    set(
                        format_ask,
                        "color:{color_at(text_colors(), index)}:{asked + 1}",
                    ),
                )
            }
        }
        section("Highlight") {
            ui.ColorSwatchPicker(
                swatches: highlight_colors(),
                columns: 6,
                label: "Highlight",
            ) {
                on change(index): set(asked, asked + 1).then(
                    set(
                        format_ask,
                        "highlight:{color_at(highlight_colors(), index)}:{asked + 1}",
                    ),
                )
            }
        }
    }
}

/// The sizes a person sets a run to, in points.
fn sizes() -> List<Int> {
    [8, 9, 10, 11, 12, 14, 16, 18, 24, 36, 48]
}

/// The inks a person sets words to: the page's own black and greys, then the colours that read
/// as a note, a link, a correction and a mark.
fn text_colors() -> List<Text> {
    ["#1f2328", "#57606a", "#0969da", "#1a7f37", "#9a6700", "#cf222e", "#8250df", "#bf3989"]
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
        section("Paragraph") {
            row(gap: .tight, wrap: true) {
                for name in style_names() {
                    ui.Chip("{name}") {
                        on press: set(asked, asked + 1).then(
                            set(format_ask, "style:{name}:{asked + 1}"),
                        )
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                text("Before", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1).then(
                            set(format_ask, "before:{step}:{asked + 1}"),
                        )
                    }
                }
            }
            row(gap: .tight, wrap: true) {
                text("After", role: .caption, secondary: true)
                for step in steps() {
                    ui.Chip("{step}") {
                        on press: set(asked, asked + 1).then(
                            set(format_ask, "after:{step}:{asked + 1}"),
                        )
                    }
                }
            }
        }
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

/// What others proposed for the document, each proposal a card a person accepts or dismisses.
view review_panel(d: Document, format_ask: State<Option<Text>>, asked: State<Int>) {
    stack(gap: .group) {
        section("Tracked changes") {
            row(gap: .tight, wrap: true) {
                ui.Button("Accept all changes", icon: .check) {
                    on press: set(asked, asked + 1).then(
                        set(format_ask, "changes:accept:{asked + 1}"),
                    )
                }
                ui.Button("Reject all changes", icon: .x) {
                    on press: set(asked, asked + 1).then(
                        set(format_ask, "changes:reject:{asked + 1}"),
                    )
                }
            }
        }
        std.suggestions(d)
        text(
            "A person or agent that suggests a change to the words leaves it here for you to accept.",
            role: .caption,
            secondary: true,
        )
    }
}

/// What stands under the page: how much the document says, and what the page is set for.
view status_band(d: Document) {
    row(gap: .related, align: .center, padding: .related) {
        spacer()
        text("{d.body.text.words()} words", role: .caption, secondary: true)
        text("·", role: .caption, secondary: true)
        text("{d.body.text.len()} characters", role: .caption, secondary: true)
        text("·", role: .caption, secondary: true)
        text(d.paper, role: .caption, secondary: true)
    }
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
        } else {
            .none
        }
    }
}
