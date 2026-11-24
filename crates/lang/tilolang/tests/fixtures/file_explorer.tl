app so.tilo.file-explorer "Files" (
    icon: .folder,
    category: .utility,
    tagline: "Browse your files",
)

/// One window on this device's files: the place it shows.
type Files {
    place: Text = ""
}

root files: Files

default view files for Files

view files(f: Files) {
    page { text(f.place) }
}

/// Shows the place `path` names: the folder itself, or the folder holding the file.
action show(f: Files, path: Text) {
    require path.trim() != "" "A place is named by its path"
    f.place = path.trim()
}
