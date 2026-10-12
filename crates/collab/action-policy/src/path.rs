//! Nodes of the published tree: room › app › resource kind › resource › item ›
//! field › action.
//!
//! An action's node is a concrete path. A grant's node is a pattern over the
//! same levels that may leave a level open (`resource:*`, "every resource of
//! this kind") or skip any depth (`**`, "anywhere below"). A grant covers its
//! node and everything under it.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The level of a segment in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Level {
    /// A room (a workspace, a shared space).
    Room,
    /// An app published in the room.
    App,
    /// A resource kind the app publishes (`snippets.library`).
    Kind,
    /// One resource (record) of that kind.
    Resource,
    /// One item of a collection.
    Item,
    /// A field, possibly nested; field edits are actions under it.
    Field,
    /// An action: a verb's public id or a field edit (`set`, `splice`, ...).
    Action,
}

impl Level {
    pub const ALL: [Self; 7] = [
        Self::Room,
        Self::App,
        Self::Kind,
        Self::Resource,
        Self::Item,
        Self::Field,
        Self::Action,
    ];

    /// The tag of the level in the text form of a path.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::App => "app",
            Self::Kind => "kind",
            Self::Resource => "resource",
            Self::Item => "item",
            Self::Field => "field",
            Self::Action => "action",
        }
    }

    /// The level as people read it ("every resource").
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::App => "app",
            Self::Kind => "kind",
            Self::Resource => "resource",
            Self::Item => "item",
            Self::Field => "field",
            Self::Action => "action",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.tag() == tag)
    }
}

/// One segment of a path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Segment {
    /// A named node at a level.
    Name(Level, String),
    /// Every node at a level.
    Any(Level),
    /// Any number of segments, including none.
    Rest,
}

impl Segment {
    /// How specific the segment is when comparing grants: a name beats an open
    /// level, which beats any depth.
    const fn weight(&self) -> u8 {
        match self {
            Self::Name(..) => 2,
            Self::Any(_) => 1,
            Self::Rest => 0,
        }
    }

    /// Whether this pattern segment covers one segment of a path.
    fn covers(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Name(level, id), Self::Name(other_level, other_id)) => {
                level == other_level && id == other_id
            }
            (Self::Any(level), Self::Name(other_level, _) | Self::Any(other_level)) => {
                level == other_level
            }
            _ => false,
        }
    }

    #[must_use]
    pub const fn level(&self) -> Option<Level> {
        match self {
            Self::Name(level, _) | Self::Any(level) => Some(*level),
            Self::Rest => None,
        }
    }

    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Name(_, id) => Some(id),
            _ => None,
        }
    }
}

/// A node, or a pattern of nodes, in the published tree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreePath(Vec<Segment>);

/// How specific a grant's node is. Greater is more specific: names beat open
/// levels position by position, and a deeper node beats its ancestors.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity(Vec<u8>);

impl TreePath {
    /// The root of the tree: covers every room.
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub fn room(id: impl Into<String>) -> Self {
        Self::root().push(Level::Room, id)
    }

    /// Appends a named segment.
    #[must_use]
    pub fn push(mut self, level: Level, id: impl Into<String>) -> Self {
        self.0.push(Segment::Name(level, id.into()));
        self
    }

    #[must_use]
    pub fn app(self, id: impl Into<String>) -> Self {
        self.push(Level::App, id)
    }

    #[must_use]
    pub fn kind(self, id: impl Into<String>) -> Self {
        self.push(Level::Kind, id)
    }

    #[must_use]
    pub fn resource(self, id: impl Into<String>) -> Self {
        self.push(Level::Resource, id)
    }

    #[must_use]
    pub fn item(self, id: impl Into<String>) -> Self {
        self.push(Level::Item, id)
    }

    /// Appends a field path; dots separate nested fields.
    #[must_use]
    pub fn field(mut self, path: &str) -> Self {
        for part in path.split('.').filter(|part| !part.is_empty()) {
            self.0.push(Segment::Name(Level::Field, part.to_owned()));
        }
        self
    }

    #[must_use]
    pub fn action(self, id: impl Into<String>) -> Self {
        self.push(Level::Action, id)
    }

    /// Appends "every node at this level".
    #[must_use]
    pub fn any(mut self, level: Level) -> Self {
        self.0.push(Segment::Any(level));
        self
    }

    /// Appends "anywhere below". A trailing `rest` adds nothing (a node already
    /// covers everything under it) and is dropped when the path is compared.
    #[must_use]
    pub fn rest(mut self) -> Self {
        if self.0.last() != Some(&Segment::Rest) {
            self.0.push(Segment::Rest);
        }
        self
    }

    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// This path without the room it starts at, if it starts at one: where
    /// it applies within its room.
    #[must_use]
    pub fn within_room(&self) -> Self {
        let room = matches!(
            self.0.first(),
            Some(Segment::Name(Level::Room, _) | Segment::Any(Level::Room))
        );
        Self(self.0.iter().skip(usize::from(room)).cloned().collect())
    }

    /// The parent node, or `None` at the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let (_, parent) = self.0.split_last()?;
        Some(Self(parent.to_vec()))
    }

    /// The first named id at `level`.
    #[must_use]
    pub fn id_at(&self, level: Level) -> Option<&str> {
        self.0.iter().find_map(|segment| match segment {
            Segment::Name(at, id) if *at == level => Some(id.as_str()),
            _ => None,
        })
    }

    /// `self` followed by every segment of `rest`.
    #[must_use]
    pub fn join(&self, rest: &Self) -> Self {
        self.clone().joined(rest)
    }

    /// [`Self::join`], taking `self`.
    #[must_use]
    pub fn joined(self, rest: &Self) -> Self {
        let mut path = self;
        path.0.reserve(rest.0.len());
        for segment in &rest.0 {
            path = match segment {
                Segment::Rest => path.rest(),
                segment => {
                    path.0.push(segment.clone());
                    path
                }
            };
        }
        path
    }

    /// The same path with a leading `room:<from>` renamed to `room:<to>`.
    #[must_use]
    pub fn with_room(&self, from: &str, to: &str) -> Self {
        let mut path = self.clone();
        if let Some(Segment::Name(Level::Room, id)) = path.0.first_mut()
            && id == from
        {
            to.clone_into(id);
        }
        path
    }

    /// The room this path is in.
    #[must_use]
    pub fn room_id(&self) -> Option<&str> {
        self.id_at(Level::Room)
    }

    /// Whether `self`, read as a grant's node, covers `path`: `path` is the
    /// node itself or lies below it.
    #[must_use]
    pub fn covers(&self, path: &Self) -> bool {
        covers(self.trimmed(), &path.0)
    }

    /// Whether some node lies at or below both `self` and `other`: a grant on
    /// one can decide an action under the other.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        overlaps(self.trimmed(), other.trimmed())
    }

    /// Whether `self` lies at or below `ancestor`.
    #[must_use]
    pub fn is_within(&self, ancestor: &Self) -> bool {
        ancestor.covers(self)
    }

    #[must_use]
    pub fn specificity(&self) -> Specificity {
        Specificity(self.trimmed().iter().map(Segment::weight).collect())
    }

    /// How many segments the path has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the path is the root.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Drops every segment after the first `len`: back to an ancestor a
    /// caller extended, to extend it again.
    pub fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }

    /// Appends a named segment in place.
    pub fn push_name(&mut self, level: Level, id: impl Into<String>) {
        self.0.push(Segment::Name(level, id.into()));
    }

    /// `self.specificity().cmp(&other.specificity())`, without building either.
    pub(crate) fn cmp_specificity(&self, other: &Self) -> std::cmp::Ordering {
        let theirs = other.trimmed().iter().map(Segment::weight);
        self.trimmed().iter().map(Segment::weight).cmp(theirs)
    }

    fn trimmed(&self) -> &[Segment] {
        let mut segments = self.0.as_slice();
        while let [init @ .., Segment::Rest] = segments {
            segments = init;
        }
        segments
    }
}

fn covers(pattern: &[Segment], path: &[Segment]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return true;
    };
    match first {
        Segment::Rest => {
            let mut tail = path;
            loop {
                if covers(rest, tail) {
                    return true;
                }
                let Some((_, shorter)) = tail.split_first() else {
                    return false;
                };
                tail = shorter;
            }
        }
        _ => path
            .split_first()
            .is_some_and(|(head, tail)| first.covers(head) && covers(rest, tail)),
    }
}

fn overlaps(left: &[Segment], right: &[Segment]) -> bool {
    let (Some((first, left_rest)), Some((second, right_rest))) =
        (left.split_first(), right.split_first())
    else {
        return true;
    };
    match (first, second) {
        // Any depth either matches nothing or absorbs the other side's next
        // segment.
        (Segment::Rest, _) | (_, Segment::Rest) => {
            overlaps(left_rest, right) || overlaps(left, right_rest)
        }
        _ => compatible(first, second) && overlaps(left_rest, right_rest),
    }
}

fn compatible(left: &Segment, right: &Segment) -> bool {
    match (left, right) {
        (Segment::Name(level, id), Segment::Name(other_level, other_id)) => {
            level == other_level && id == other_id
        }
        (Segment::Name(level, _) | Segment::Any(level), Segment::Any(other_level))
        | (Segment::Any(level), Segment::Name(other_level, _)) => level == other_level,
        _ => false,
    }
}

impl fmt::Display for TreePath {
    /// `room:r/app:mail/kind:mail.message/resource:*/action:send`, `**` for any
    /// depth, `/` for the root. Ids escape `%` and `/` as `%25` and `%2F`, and an
    /// id that is exactly `*` as `%2A`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("/");
        }
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("/")?;
            }
            match segment {
                Segment::Name(level, id) => {
                    write!(f, "{}:", level.tag())?;
                    if id == "*" {
                        f.write_str("%2A")?;
                    } else {
                        for ch in id.chars() {
                            match ch {
                                '%' => f.write_str("%25")?,
                                '/' => f.write_str("%2F")?,
                                _ => fmt::Write::write_char(f, ch)?,
                            }
                        }
                    }
                }
                Segment::Any(level) => write!(f, "{}:*", level.tag())?,
                Segment::Rest => f.write_str("**")?,
            }
        }
        Ok(())
    }
}

/// A path that does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathError(pub String);

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid tree path: {}", self.0)
    }
}

impl std::error::Error for PathError {}

impl FromStr for TreePath {
    type Err = PathError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "/" || text.is_empty() {
            return Ok(Self::root());
        }
        let mut path = Self::root();
        for part in text.split('/') {
            if part == "**" {
                path = path.rest();
                continue;
            }
            let (tag, id) = part
                .split_once(':')
                .ok_or_else(|| PathError(format!("segment `{part}` has no level")))?;
            let level =
                Level::from_tag(tag).ok_or_else(|| PathError(format!("unknown level `{tag}`")))?;
            if id == "*" {
                path = path.any(level);
            } else if id.is_empty() {
                return Err(PathError(format!("segment `{part}` has no id")));
            } else {
                path = path.push(level, unescape(id)?);
            }
        }
        Ok(path)
    }
}

fn unescape(id: &str) -> Result<String, PathError> {
    let mut out = String::with_capacity(id.len());
    let mut chars = id.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let code: String = chars.by_ref().take(2).collect();
        out.push(match code.as_str() {
            "25" => '%',
            "2F" | "2f" => '/',
            "2A" | "2a" => '*',
            _ => return Err(PathError(format!("bad escape in `{id}`"))),
        });
    }
    Ok(out)
}

impl Serialize for TreePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TreePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
