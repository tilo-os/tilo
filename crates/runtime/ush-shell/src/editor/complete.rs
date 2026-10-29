//! Tab completion: what the word before the cursor can become.
//!
//! - In a command's place, a command: the shell's built-ins, Tilo's own
//!   commands, the functions and aliases defined, and the environment's
//!   tools ([`Host::programs`]).
//! - After `$`, a variable's name.
//! - A word starting with `-`, after one of the shell's own commands, one of
//!   that command's options.
//! - Anything else, a path, read through the host
//!   ([`Host::read_dir`](crate::Host::read_dir)), so it offers only what the
//!   environment's grants let the shell see.

use crate::shell::Shell;

/// What completing the word before the cursor found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// Where the word being completed starts, in characters of the line.
    pub start: usize,
    /// What the word becomes when it is the only candidate, or the part all
    /// candidates share.
    pub insert: String,
    /// Whether that is the only candidate, complete: a space follows it.
    pub whole: bool,
    /// Every candidate, as listed to the person (`dir/` for a directory).
    pub candidates: Vec<String>,
}

/// What the word before the cursor is.
#[derive(Debug, PartialEq, Eq)]
enum Place {
    Command,
    Option { command: String },
    Path { dirs_only: bool },
}

/// The words of `before` as the shell would split them, roughly: enough to
/// find the word being completed, the command it belongs to and whether it
/// is in a command's place. Answers the words, each with where it starts,
/// and whether the last one is inside quotes.
fn words(before: &str) -> (Vec<(usize, String)>, Option<char>, bool) {
    let mut words: Vec<(usize, String)> = Vec::new();
    let mut current: Option<(usize, String)> = None;
    let mut quote: Option<char> = None;
    // Whether the next word starts a command: at the start, and after a
    // separator.
    let mut command_next = true;
    let mut last_command_place = true;
    let mut chars = before.chars().enumerate();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                if let Some((_, next)) = chars.next()
                    && let Some((_, word)) = current.as_mut()
                {
                    word.push(next);
                }
                continue;
            } else if let Some((_, word)) = current.as_mut() {
                word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                if current.is_none() {
                    current = Some((i, String::new()));
                    last_command_place = command_next;
                    command_next = false;
                }
            }
            '\\' => {
                let next = chars.next().map(|(_, n)| n);
                let word = current.get_or_insert_with(|| {
                    last_command_place = command_next;
                    command_next = false;
                    (i, String::new())
                });
                if let Some(next) = next {
                    word.1.push(next);
                }
            }
            c if c.is_whitespace() => {
                if let Some(word) = current.take() {
                    words.push(word);
                }
            }
            '|' | '&' | ';' | '(' | ')' | '{' | '}' | '`' => {
                if let Some(word) = current.take() {
                    words.push(word);
                }
                words.clear();
                command_next = true;
            }
            '<' | '>' => {
                if let Some(word) = current.take() {
                    words.push(word);
                }
            }
            c => {
                let word = current.get_or_insert_with(|| {
                    last_command_place = command_next;
                    command_next = false;
                    (i, String::new())
                });
                word.1.push(c);
                // A keyword or an assignment before a command keeps the
                // next word in a command's place.
            }
        }
        if current.is_none()
            && let Some((_, word)) = words.last()
            && (matches!(
                word.as_str(),
                "then" | "do" | "else" | "elif" | "if" | "while" | "until" | "!" | "time"
            ) || (word.contains('=') && !word.starts_with('=') && words.len() == 1))
        {
            command_next = true;
        }
    }
    let ended = current.is_none();
    if let Some(word) = current {
        words.push(word);
    }
    if ended {
        // The cursor is after a space: a new, empty word.
        words.push((before.chars().count(), String::new()));
        last_command_place = command_next;
    }
    (words, quote, last_command_place)
}

impl Shell {
    /// Complete the word that ends at the cursor, `before` being the line up
    /// to it.
    pub(crate) fn complete(&self, before: &str) -> Found {
        let (words, quote, command_place) = words(before);
        let Some((start, word)) = words.last().cloned() else {
            return Found::default();
        };
        let command = if command_place {
            None
        } else {
            words.first().map(|(_, w)| w.clone())
        };
        let place = if let Some(at) = word.rfind('$').filter(|_| quote != Some('\'')) {
            let rest = word.get(at..).unwrap_or_default();
            let braced = rest.starts_with("${");
            let prefix = word.get(..at).unwrap_or_default().to_owned();
            return self.complete_variable(start, &prefix, rest, braced);
        } else if command_place && !word.contains('/') {
            Place::Command
        } else if word.starts_with('-') && command.is_some() {
            Place::Option {
                command: command.unwrap_or_default(),
            }
        } else {
            Place::Path {
                dirs_only: matches!(command.as_deref(), Some("cd" | "pushd" | "rmdir")),
            }
        };
        let (candidates, insert_of): (Vec<String>, Insert) = match place {
            Place::Command => (self.command_names(&word), |c, q| escape(c, q)),
            Place::Option { command } => (
                crate::commands::options::of(&command)
                    .iter()
                    .map(|o| o.spelling.to_owned())
                    .filter(|s| s.starts_with(&word))
                    .collect(),
                |c, _| c.to_owned(),
            ),
            Place::Path { dirs_only } => (self.paths(&word, dirs_only), |c, q| escape(c, q)),
        };
        let mut found = Found {
            start,
            ..Found::default()
        };
        found.whole = candidates.len() == 1
            && !candidates
                .first()
                .is_some_and(|c| c.ends_with('/') || c.ends_with('='));
        let shared = shared_prefix(&candidates);
        // The word as typed is replaced from its start, so a quote it opened
        // stays open with what follows.
        let opened = quote.map(String::from).unwrap_or_default();
        found.insert = if shared.is_empty() && candidates.is_empty() {
            String::new()
        } else {
            let body = insert_of(&shared, quote);
            if found.whole && quote.is_some() {
                format!("{opened}{body}{opened}")
            } else {
                format!("{opened}{body}")
            }
        };
        if candidates.is_empty() {
            found.insert.clear();
        }
        // Listed by their last part, as bash lists paths.
        found.candidates = candidates
            .iter()
            .map(|c| {
                let trimmed = c.strip_suffix('/').unwrap_or(c);
                match trimmed.rfind('/') {
                    Some(at) => c.get(at.saturating_add(1)..).unwrap_or(c).to_owned(),
                    None => c.clone(),
                }
            })
            .collect();
        found
    }

    fn complete_variable(&self, start: usize, prefix: &str, word: &str, braced: bool) -> Found {
        let typed = word.trim_start_matches('$').trim_start_matches('{');
        let mut names: Vec<String> = self
            .vars
            .all()
            .into_iter()
            .map(|(name, _)| name.clone())
            .filter(|name| name.starts_with(typed))
            .collect();
        names.sort();
        names.dedup();
        let shared = shared_prefix(&names);
        let whole = names.len() == 1;
        let open = if braced { "${" } else { "$" };
        let close = if braced && whole { "}" } else { "" };
        Found {
            start,
            insert: if names.is_empty() {
                String::new()
            } else {
                format!("{prefix}{open}{shared}{close}")
            },
            whole,
            candidates: names,
        }
    }

    /// The commands whose names start with `prefix`, sorted, each once.
    fn command_names(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = crate::builtins::NAMES
            .iter()
            .copied()
            .chain(crate::commands::names())
            .chain(crate::builtins::KEYWORDS.iter().copied())
            .map(str::to_owned)
            .chain(self.functions.keys().cloned())
            .chain(self.aliases.keys().cloned())
            .chain(
                self.host
                    .programs(&self.vars.scalar("PATH").unwrap_or_default()),
            )
            .filter(|name| name.starts_with(prefix))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The paths `word` can become: the entries of its directory whose names
    /// start with what follows its last `/`, each as the word would be with
    /// it (`src/ma` → `src/main.rs`, `src/macros/`).
    fn paths(&self, word: &str, dirs_only: bool) -> Vec<String> {
        let (dir, name) = match word.rfind('/') {
            Some(at) => (
                word.get(..=at).unwrap_or_default(),
                word.get(at.saturating_add(1)..).unwrap_or_default(),
            ),
            None => ("", word),
        };
        let expanded = match dir.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                format!("{}{rest}", self.vars.scalar("HOME").unwrap_or_default())
            }
            _ => dir.to_owned(),
        };
        let path = crate::paths::join(&self.cwd, if expanded.is_empty() { "." } else { &expanded });
        let Ok(entries) = self.host.read_dir(&path) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .into_iter()
            .filter(|e| {
                e.name.starts_with(name) && (name.starts_with('.') || !e.name.starts_with('.'))
            })
            .filter_map(|e| {
                let is_dir = match e.kind {
                    crate::Kind::Dir => true,
                    crate::Kind::Symlink => self
                        .host
                        .metadata(&path.join(&e.name))
                        .is_ok_and(|m| m.is_dir()),
                    _ => false,
                };
                if dirs_only && !is_dir {
                    return None;
                }
                Some(format!("{dir}{}{}", e.name, if is_dir { "/" } else { "" }))
            })
            .collect();
        out.sort();
        out
    }
}

/// How a candidate is typed into the line, inside the quote the word opened.
type Insert = fn(&str, Option<char>) -> String;

/// The longest start every one of `names` shares.
fn shared_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut len = first.chars().count();
    for name in names.iter().skip(1) {
        len = len.min(
            first
                .chars()
                .zip(name.chars())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    first.chars().take(len).collect()
}

/// `name` as it is typed: with a backslash before each character the shell
/// would read as something else, or inside the quote the word opened.
fn escape(name: &str, quote: Option<char>) -> String {
    match quote {
        Some('\'') => name.replace('\'', "'\\''"),
        Some(_) => {
            let mut out = String::new();
            for c in name.chars() {
                if matches!(c, '"' | '\\' | '$' | '`') {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        }
        None => {
            let mut out = String::new();
            for c in name.chars() {
                if c.is_whitespace() || "'\"\\$`&|;<>()*?[]#!{}~".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_at_the_cursor_and_its_place() {
        let (w, q, cmd) = words("ls -l src/ma");
        assert_eq!(w.last().unwrap(), &(6, "src/ma".to_owned()));
        assert_eq!((q, cmd), (None, false));
        let (w, _, cmd) = words("cat a | gr");
        assert_eq!(w.last().unwrap().1, "gr");
        assert!(cmd, "after a pipe");
        let (w, _, cmd) = words("FOO=1 ");
        assert_eq!(w.last().unwrap().1, "");
        assert!(cmd, "after an assignment");
        let (w, q, _) = words("cat 'my fi");
        assert_eq!(w.last().unwrap(), &(4, "my fi".to_owned()));
        assert_eq!(q, Some('\''));
        assert_eq!(escape("my file", None), "my\\ file");
    }
}
