app so.tilo.print-centre "Print Centre" (
    icon: .printer,
    category: .system,
    tagline: "The printers, their jobs, and the scanner on this computer",
)

/// What this window prints: the document another app handed over, and what it is called.
type PrintCentre {
    printed: Text = ""
}

root print_centre: PrintCentre

default view window for PrintCentre

view window(p: PrintCentre) {
    page { text(p.printed) }
}

/// Takes a document another app hands over as what this window prints.
action print_file(p: PrintCentre, file: File) {
    p.printed = file.name
}
