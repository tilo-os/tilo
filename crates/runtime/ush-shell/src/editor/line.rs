//! The line being edited: its characters, the cursor, the kill ring and
//! undo, with readline's emacs meanings for each edit.

/// A line and where the cursor is in it, in characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    chars: Vec<char>,
    cursor: usize,
}

/// How far a word reaches: readline's `backward-kill-word` (`Alt+Backspace`,
/// `Alt+B`, `Alt+F`) stops at anything not a letter or digit;
/// `unix-word-rubout` (`Ctrl+W`) only at whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Alnum,
    Whitespace,
}

impl Word {
    fn inside(self, c: char) -> bool {
        match self {
            Self::Alnum => c.is_alphanumeric() || c == '_',
            Self::Whitespace => !c.is_whitespace(),
        }
    }
}

impl Line {
    pub fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        Self {
            cursor: chars.len(),
            chars,
        }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// The text before the cursor.
    pub fn before(&self) -> String {
        self.chars
            .get(..self.cursor)
            .unwrap_or_default()
            .iter()
            .collect()
    }

    pub fn insert(&mut self, text: &str) {
        for c in text.chars() {
            self.chars.insert(self.cursor, c);
            self.cursor = self.cursor.saturating_add(1);
        }
    }

    /// Replace the characters from `start` to the cursor with `text`.
    pub fn replace_back_to(&mut self, start: usize, text: &str) {
        let start = start.min(self.cursor);
        self.chars.drain(start..self.cursor);
        self.cursor = start;
        self.insert(text);
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = self.cursor.saturating_add(1).min(self.chars.len());
    }

    pub const fn home(&mut self) {
        self.cursor = 0;
    }

    pub const fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Where the word before the cursor starts.
    fn word_start(&self, word: Word) -> usize {
        let mut at = self.cursor;
        while at > 0
            && self
                .chars
                .get(at.saturating_sub(1))
                .is_some_and(|c| !word.inside(*c))
        {
            at = at.saturating_sub(1);
        }
        while at > 0
            && self
                .chars
                .get(at.saturating_sub(1))
                .is_some_and(|c| word.inside(*c))
        {
            at = at.saturating_sub(1);
        }
        at
    }

    /// Where the word after the cursor ends.
    fn word_end(&self, word: Word) -> usize {
        let mut at = self.cursor;
        let len = self.chars.len();
        while at < len && self.chars.get(at).is_some_and(|c| !word.inside(*c)) {
            at = at.saturating_add(1);
        }
        while at < len && self.chars.get(at).is_some_and(|c| word.inside(*c)) {
            at = at.saturating_add(1);
        }
        at
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start(Word::Alnum);
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end(Word::Alnum);
    }

    /// Take out the characters between `from` and `to`, answering them.
    fn cut(&mut self, from: usize, to: usize) -> String {
        let (from, to) = (from.min(to), to.max(from).min(self.chars.len()));
        let taken: String = self.chars.drain(from..to).collect();
        if self.cursor > to {
            self.cursor = self.cursor.saturating_sub(to.saturating_sub(from));
        } else if self.cursor > from {
            self.cursor = from;
        }
        taken
    }

    /// `Backspace`.
    pub fn delete_back(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cut(self.cursor.saturating_sub(1), self.cursor);
        true
    }

    /// `Delete`, `Ctrl+D` on a line with something in it.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.cut(self.cursor, self.cursor.saturating_add(1));
        true
    }

    /// `Ctrl+K`.
    pub fn kill_to_end(&mut self) -> String {
        self.cut(self.cursor, self.chars.len())
    }

    /// `Ctrl+U`.
    pub fn kill_to_start(&mut self) -> String {
        self.cut(0, self.cursor)
    }

    /// `Ctrl+W` (whitespace) and `Alt+Backspace` (letters and digits).
    pub fn kill_word_back(&mut self, word: Word) -> String {
        let start = self.word_start(word);
        self.cut(start, self.cursor)
    }

    /// `Alt+D`.
    pub fn kill_word_forward(&mut self) -> String {
        let end = self.word_end(Word::Alnum);
        self.cut(self.cursor, end)
    }

    /// `Ctrl+T`: swap the characters around the cursor (the last two at the
    /// end of the line), and move on.
    pub fn transpose(&mut self) {
        let len = self.chars.len();
        if len < 2 || self.cursor == 0 {
            return;
        }
        let at = if self.cursor >= len {
            len.saturating_sub(1)
        } else {
            self.cursor
        };
        self.chars.swap(at.saturating_sub(1), at);
        self.cursor = at.saturating_add(1).min(len);
    }

    /// `Alt+U`, `Alt+L`, `Alt+C`: change the case of the next word.
    pub fn case_word(&mut self, change: fn(&str) -> String) {
        let end = self.word_end(Word::Alnum);
        let start = self.cursor;
        let word: String = self
            .chars
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .collect();
        let changed: Vec<char> = change(&word).chars().collect();
        self.chars.splice(start..end, changed.iter().copied());
        self.cursor = start.saturating_add(changed.len());
    }
}

/// The kill ring: what `Ctrl+K`, `Ctrl+U`, `Ctrl+W` and `Alt+D` took, for
/// `Ctrl+Y` and `Alt+Y`. Kills in a row are one entry.
#[derive(Debug, Default)]
pub struct KillRing {
    ring: Vec<String>,
    /// The entry the last yank put back, for `Alt+Y`.
    yanked: Option<usize>,
}

/// How many kills the ring keeps.
const RING: usize = 16;

impl KillRing {
    /// Keep `text`, joined to the last kill when this one followed it
    /// (`append` at its end, else at its start).
    pub fn kill(&mut self, text: String, continued: bool, forward: bool) {
        if text.is_empty() {
            return;
        }
        match self.ring.last_mut() {
            Some(last) if continued => {
                if forward {
                    last.push_str(&text);
                } else {
                    last.insert_str(0, &text);
                }
            }
            _ => {
                if self.ring.len() >= RING {
                    self.ring.remove(0);
                }
                self.ring.push(text);
            }
        }
        self.yanked = None;
    }

    /// The newest kill, for `Ctrl+Y`.
    pub fn yank(&mut self) -> Option<&str> {
        let at = self.ring.len().checked_sub(1)?;
        self.yanked = Some(at);
        self.ring.get(at).map(String::as_str)
    }

    /// How many characters the last yank put in.
    pub fn yank_len(&self) -> usize {
        self.yanked
            .and_then(|at| self.ring.get(at))
            .map_or(0, |text| text.chars().count())
    }

    /// The kill before the one just yanked, for `Alt+Y`.
    pub fn rotate(&mut self) -> Option<&str> {
        let at = self.yanked?;
        let next = if at == 0 {
            self.ring.len().saturating_sub(1)
        } else {
            at.saturating_sub(1)
        };
        self.yanked = Some(next);
        self.ring.get(next).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_end_where_readline_ends_them() {
        let mut line = Line::new("git commit -m 'fix/the thing'");
        assert_eq!(line.kill_word_back(Word::Whitespace), "thing'");
        assert_eq!(line.kill_word_back(Word::Alnum), "the ");
        line.home();
        line.word_right();
        assert_eq!(line.cursor(), 3);
        assert_eq!(line.kill_word_forward(), " commit");
        assert_eq!(line.text(), "git -m 'fix/");
        line.end();
        line.transpose();
        assert_eq!(line.text(), "git -m 'fi/x");
    }
}
