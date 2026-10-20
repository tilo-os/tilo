//! The ids lock file `app.ids` (implementation.md section 1.1).
//!
//! A sorted text file of `qualified.name = id` lines. Declarations that disappear move to a
//! `# retired` section and their ids are never handed out again.

use crate::ast::{Item, Module};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const HEADER: &str =
    "# tilolang ids: generated, commit it. Edit only through `tilolang ids --rename`.";
const RETIRED: &str = "# retired";
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// The source of randomness for new ids; tests inject a seeded one.
pub trait Entropy {
    fn next_u64(&mut self) -> u64;
}

/// A deterministic generator (splitmix64) for tests and reproducible builds.
#[derive(Clone, Debug)]
pub struct SeededEntropy(pub u64);

impl Entropy for SeededEntropy {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Randomness from the operating system.
#[derive(Debug, Default)]
pub struct OsEntropy {
    fallback: Option<SeededEntropy>,
}

impl Entropy for OsEntropy {
    fn next_u64(&mut self) -> u64 {
        use std::io::Read;
        if self.fallback.is_none() {
            let mut buf = [0u8; 8];
            if std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut buf))
                .is_ok()
            {
                return u64::from_le_bytes(buf);
            }
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            self.fallback = Some(SeededEntropy(nanos ^ u64::from(std::process::id())));
        }
        self.fallback.as_mut().map_or(0, Entropy::next_u64)
    }
}

/// Why an ids operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdsError {
    /// A line of the lock file could not be read.
    Syntax { line: usize, message: String },
    /// `rename` was asked to rename something that has no id.
    UnknownName(String),
    /// `rename` would overwrite an existing declaration.
    NameTaken(String),
}

impl fmt::Display for IdsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, message } => write!(f, "app.ids:{line}: {message}"),
            Self::UnknownName(n) => write!(
                f,
                "`{n}` has no id in app.ids; names in the lock are: use `tilolang ids <dir>` to list them"
            ),
            Self::NameTaken(n) => write!(
                f,
                "`{n}` already has an id in app.ids; pick a name that is not declared"
            ),
        }
    }
}

impl std::error::Error for IdsError {}

/// The ids of one app package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdsLock {
    entries: BTreeMap<String, String>,
    retired: BTreeSet<(String, String)>,
}

impl IdsLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a lock file.
    pub fn parse(text: &str) -> Result<Self, IdsError> {
        let mut lock = Self::new();
        let mut retired = false;
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            let t = raw.trim();
            if t.is_empty() {
                continue;
            }
            if t == RETIRED {
                retired = true;
                continue;
            }
            if t.starts_with('#') {
                continue;
            }
            let Some((name, id)) = t.split_once('=') else {
                return Err(IdsError::Syntax {
                    line,
                    message: "expected `name = id`".into(),
                });
            };
            let (name, id) = (name.trim(), id.trim());
            if name.is_empty() || id.is_empty() || id.contains(char::is_whitespace) {
                return Err(IdsError::Syntax {
                    line,
                    message: "expected `name = id`".into(),
                });
            }
            if retired {
                lock.retired.insert((name.to_string(), id.to_string()));
            } else if lock
                .entries
                .insert(name.to_string(), id.to_string())
                .is_some()
            {
                return Err(IdsError::Syntax {
                    line,
                    message: format!("`{name}` appears twice"),
                });
            }
        }
        Ok(lock)
    }

    /// The lock file text, sorted.
    pub fn to_text(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        for (name, id) in &self.entries {
            out.push_str(&format!("{name} = {id}\n"));
        }
        if !self.retired.is_empty() {
            out.push('\n');
            out.push_str(RETIRED);
            out.push('\n');
            for (name, id) in &self.retired {
                out.push_str(&format!("{name} = {id}\n"));
            }
        }
        out
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(String::as_str)
    }

    /// Every live `(name, id)`.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(n, i)| (n.as_str(), i.as_str()))
    }

    /// Every retired `(name, id)`.
    pub fn retired(&self) -> impl Iterator<Item = (&str, &str)> {
        self.retired.iter().map(|(n, i)| (n.as_str(), i.as_str()))
    }

    fn id_in_use(&self, id: &str) -> bool {
        self.entries.values().any(|v| v == id) || self.retired.iter().any(|(_, v)| v == id)
    }

    /// Renames a declaration and everything under it (`Album` also renames `Album.title`),
    /// keeping every id.
    pub fn rename(&mut self, old: &str, new: &str) -> Result<(), IdsError> {
        if !self.entries.contains_key(old) {
            return Err(IdsError::UnknownName(old.to_string()));
        }
        let moved = |name: &str, from: &str| -> Option<String> {
            let rest = name.strip_prefix(from)?;
            (rest.is_empty() || rest.starts_with('.') || rest.starts_with('#'))
                .then(|| rest.to_string())
        };
        let renames: Vec<(String, String)> = self
            .entries
            .keys()
            .filter_map(|n| moved(n, old).map(|rest| (n.clone(), format!("{new}{rest}"))))
            .collect();
        for (_, target) in &renames {
            if self.entries.contains_key(target) && !renames.iter().any(|(from, _)| from == target)
            {
                return Err(IdsError::NameTaken(target.clone()));
            }
        }
        let mut taken = Vec::new();
        for (from, _) in &renames {
            if let Some(id) = self.entries.remove(from) {
                taken.push(id);
            }
        }
        for ((_, to), id) in renames.into_iter().zip(taken) {
            self.entries.insert(to, id);
        }
        Ok(())
    }

    /// Makes the lock match `names`: names without an id get a fresh one, and ids of names that
    /// are gone move to the retired section.
    pub fn sync(&mut self, names: &[String], rng: &mut dyn Entropy) {
        let wanted: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        let gone: Vec<String> = self
            .entries
            .keys()
            .filter(|n| !wanted.contains(n.as_str()))
            .cloned()
            .collect();
        for name in gone {
            if let Some(id) = self.entries.remove(&name) {
                self.retired.insert((name, id));
            }
        }
        for name in names {
            if !self.entries.contains_key(name) {
                let id = self.fresh(rng);
                self.entries.insert(name.clone(), id);
            }
        }
    }

    fn fresh(&self, rng: &mut dyn Entropy) -> String {
        loop {
            let mut bits = rng.next_u64();
            let mut id = String::with_capacity(8);
            for _ in 0..8 {
                id.push(char::from(
                    ALPHABET.get((bits & 31) as usize).copied().unwrap_or(b'a'),
                ));
                bits >>= 5;
            }
            if !self.id_in_use(&id) {
                return id;
            }
        }
    }
}

/// The ids-lock name of a scenario: its name in quotes, which no other declaration can have.
pub fn scenario_key(name: &str) -> String {
    format!("scenario \"{name}\"")
}

/// The qualified name of every declaration that gets an id: types (`Album`), fields
/// (`Album.title`), actions, flows, views and fns (`play`), anchors (`album_page#play`) and
/// scenarios (`scenario "a week of notes"`).
pub fn declared_names(m: &Module) -> Vec<String> {
    let mut out = Vec::new();
    for item in &m.items {
        match item {
            Item::Use(_) | Item::Root(_) | Item::DefaultView(_) | Item::RowView(_) => {}
            Item::Scenario(s) => out.push(scenario_key(&s.name)),
            Item::Type(t) => {
                out.push(t.name.name.clone());
                for f in &t.fields {
                    out.push(format!("{}.{}", t.name.name, f.name.name));
                }
            }
            Item::Enum(e) => {
                out.push(e.name.name.clone());
                for v in &e.variants {
                    out.push(format!("{}.{}", e.name.name, v.name.name));
                    for f in &v.fields {
                        out.push(format!("{}.{}.{}", e.name.name, v.name.name, f.name.name));
                    }
                }
            }
            Item::Action(a) => out.push(a.name.name.clone()),
            Item::Flow(f) => out.push(f.name.name.clone()),
            Item::Fn(f) => out.push(f.name.name.clone()),
            Item::View(v) => {
                out.push(v.name.name.clone());
                for s in &v.body {
                    if let crate::ast::UiKind::State(d) = &s.kind {
                        out.push(format!("{}.{}", v.name.name, d.name.name));
                    }
                }
                collect_anchors(&v.name.name, &v.body, &mut out);
            }
        }
    }
    out
}

fn collect_anchors(view: &str, stmts: &[crate::ast::UiStmt], out: &mut Vec<String>) {
    use crate::ast::UiKind;
    for s in stmts {
        match &s.kind {
            UiKind::Call(c) => {
                if let Some(a) = &c.anchor {
                    out.push(format!("{view}#{}", a.name));
                }
                if let Some(b) = &c.block {
                    collect_anchors(view, b, out);
                }
            }
            UiKind::For { body, .. } => collect_anchors(view, body, out),
            UiKind::If { then, els, .. } => {
                collect_anchors(view, then, out);
                if let Some(e) = els {
                    collect_anchors(view, e, out);
                }
            }
            UiKind::Match { arms, .. } => {
                for arm in arms {
                    collect_anchors(view, &arm.body, out);
                }
            }
            UiKind::Let { .. } | UiKind::State(_) | UiKind::Fact { .. } => {}
        }
    }
}
