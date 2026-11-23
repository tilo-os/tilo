//! A new file's name: what it is proposed as, and what a typed name makes of it.
//!
//! A computer's Files names a new file in its list and a page names it in the
//! workspace's tree ([`crate::tree`]). Both propose the same name and read a typed
//! name the same way, so the two surfaces do not drift apart: the whole proposal is
//! chosen, a name with an extension of its own stands as typed, and a bare one keeps
//! the extension it was proposed under.

/// The extension of `name` with its dot (`".txt"`), or `None` for a name with none
/// (`"notes"`, or a dotfile such as `".bashrc"`).
fn extension(name: &str) -> Option<&str> {
    name.rfind('.')
        .filter(|&dot| dot > 0)
        .map(|dot| &name[dot..])
}

/// Whether `name` says what kind of file it is: an extension of its own, or a dotfile
/// whose leading dot is part of the name.
fn names_its_kind(name: &str) -> bool {
    extension(name).is_some() || name.starts_with('.')
}

/// The name a new file takes from what a person typed in its field, `typed`, proposed
/// as `proposed`: a name with an extension of its own stands as typed, and one without
/// keeps the proposed extension, so typing `Notes` makes the text file `Notes.txt`, and
/// `Notes.md` is a Markdown file.
#[must_use]
pub fn file_name(typed: &str, proposed: &str) -> String {
    let typed = typed.trim();
    if typed.is_empty() || names_its_kind(typed) {
        return typed.to_owned();
    }
    match extension(proposed) {
        Some(extension) if extension.len() > 1 => format!("{typed}{extension}"),
        _ => typed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn a_typed_name_keeps_the_proposed_extension_only_when_it_has_none() {
        assert_eq!(file_name("Notes", "New file.txt"), "Notes.txt");
        assert_eq!(file_name("notes.txt", "New file.txt"), "notes.txt");
        assert_eq!(file_name("notes.md", "New file.txt"), "notes.md");
        assert_eq!(file_name(".bashrc", "New file.txt"), ".bashrc");
        assert_eq!(file_name("Notes", "New file"), "Notes");
        assert_eq!(file_name("  Notes  ", "New file.txt"), "Notes.txt");
        assert_eq!(file_name("", "New file.txt"), "");
    }
}
