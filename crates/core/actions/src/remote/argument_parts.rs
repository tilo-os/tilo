//! An invocation's argument as parts, each of which a change can keep on its own.
//!
//! A call that writes several documents writes each of them for its own readers, and the
//! changes it writes keep the call its caller signed. Signed over one hash of the whole
//! argument, every document's copy would have to keep the whole argument to check against it,
//! so a call that writes a private document and a shared one would hand the private one's part
//! of it to the shared one's readers. Signed over a Merkle root of its parts instead, each
//! change keeps only the parts it holds itself, with the hashes that prove them against the
//! root, and nothing else of the argument.
//!
//! The one rule, for the signer, the writer and every reader:
//!
//! - **Parts.** Every scalar of the argument is a part, named by its path (a JSON pointer:
//!   `/titles/1`); an empty object or list is a part of its own. A text argument is one part at
//!   the empty path; no argument has none. The parts are ordered by their paths' bytes.
//! - **Hashes.** A part's hash binds its path and its canonical value under a salt of its own,
//!   derived from the call's salt and its path ([`part_salt`]): a part kept elsewhere cannot be
//!   guessed from its hash, however few values it could take. The root binds the argument's
//!   form, how many parts it has and the tree over their hashes ([`root`]).
//! - **What a change keeps.** A change keeps the parts its document holds after it: a text
//!   part contained in a text the change wrote, any other part equal to a value it wrote
//!   ([`Held`]). What a document's readers find kept of the argument is what they read in the
//!   document anyway.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::InvokeArgument;

const PART_DOMAIN: &[u8] = b"tilo-argument-part-v1\0";
const NODE_DOMAIN: &[u8] = b"tilo-argument-node-v1\0";
const ROOT_DOMAIN: &[u8] = b"tilo-argument-parts-v1\0";
const SALT_CONTEXT: &str = "tilo argument part salt v1";

/// A call's salt: what each part's own salt is derived from. The caller draws it at random for
/// each call; it travels to the host that runs the call, never into a change.
pub type Salt = [u8; 32];

/// A fresh salt, drawn at random.
#[must_use]
pub fn fresh_salt() -> Salt {
    let mut salt = [0; 32];
    salt[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    salt[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    salt
}

/// The salt of the part at `path`, derived from the call's `salt`.
#[must_use]
pub fn part_salt(salt: &Salt, path: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(SALT_CONTEXT);
    hasher.update(salt);
    hasher.update(path.as_bytes());
    *hasher.finalize().as_bytes()
}

/// One part of an argument: its path and its value, a scalar or an empty container.
#[derive(Clone, Debug, PartialEq)]
pub struct Part<'a> {
    pub path: String,
    pub value: std::borrow::Cow<'a, serde_json::Value>,
}

/// The form an argument takes, as its root binds it.
const fn form(argument: &InvokeArgument) -> u8 {
    match argument {
        InvokeArgument::None => 0,
        InvokeArgument::Text(_) => 1,
        InvokeArgument::Json(_) => 2,
    }
}

/// The parts of `argument`, in their order.
#[must_use]
pub fn parts(argument: &InvokeArgument) -> Vec<Part<'_>> {
    fn walk<'a>(path: &mut String, value: &'a serde_json::Value, out: &mut Vec<Part<'a>>) {
        let len = path.len();
        match value {
            serde_json::Value::Object(object) if !object.is_empty() => {
                for (key, value) in object {
                    path.push('/');
                    path.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    walk(path, value, out);
                    path.truncate(len);
                }
            }
            serde_json::Value::Array(items) if !items.is_empty() => {
                for (at, value) in items.iter().enumerate() {
                    path.push('/');
                    path.push_str(&at.to_string());
                    walk(path, value, out);
                    path.truncate(len);
                }
            }
            value => out.push(Part {
                path: path.clone(),
                value: std::borrow::Cow::Borrowed(value),
            }),
        }
    }
    let mut out = Vec::new();
    match argument {
        InvokeArgument::None => {}
        InvokeArgument::Text(text) => out.push(Part {
            path: String::new(),
            value: std::borrow::Cow::Owned(serde_json::Value::String(text.clone())),
        }),
        InvokeArgument::Json(object) => {
            for (key, value) in object {
                let mut path = format!("/{}", key.replace('~', "~0").replace('/', "~1"));
                walk(&mut path, value, &mut out);
            }
        }
    }
    out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    out
}

/// The hash of the part at `path` holding `value`, under its own salt `salt`.
#[must_use]
pub fn part_hash(salt: &[u8; 32], path: &str, value: &serde_json::Value) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PART_DOMAIN);
    hasher.update(salt);
    hasher.update(&u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(path.as_bytes());
    tilo_canonical_json::write_bytes(value, |chunk| {
        hasher.update(chunk);
    });
    *hasher.finalize().as_bytes()
}

fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

fn root_of(form: u8, count: usize, tree: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ROOT_DOMAIN);
    hasher.update(&[form]);
    hasher.update(&u64::try_from(count).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(tree);
    *hasher.finalize().as_bytes()
}

/// The tree's root over `hashes`, and the hashes beside the path of each of `kept` to it: each
/// level pairs its nodes in order and carries an odd last one up as it is.
fn tree(hashes: &[[u8; 32]], kept: &BTreeSet<usize>) -> ([u8; 32], Vec<[u8; 32]>) {
    if hashes.is_empty() {
        return ([0; 32], Vec::new());
    }
    let mut level = hashes.to_vec();
    let mut known = kept.clone();
    let mut proof = Vec::new();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut next_known = BTreeSet::new();
        for (pair, nodes) in level.chunks(2).enumerate() {
            let first = pair.saturating_mul(2);
            let left = known.contains(&first);
            let right = known.contains(&first.saturating_add(1));
            if left || right {
                next_known.insert(pair);
            }
            if let [left_node, right_node] = nodes {
                if left && !right {
                    proof.push(*right_node);
                } else if right && !left {
                    proof.push(*left_node);
                }
                next.push(node(left_node, right_node));
            } else {
                // The odd last node, carried up as it is.
                next.extend_from_slice(nodes);
            }
        }
        level = next;
        known = next_known;
    }
    let top = level.first().copied().unwrap_or([0; 32]);
    if kept.is_empty() {
        // Nothing kept: the tree's root is its whole proof.
        proof.push(top);
    }
    (top, proof)
}

/// The root a call's statement signs over `argument`'s parts, hashed under `salt`: what the
/// signed statement's argument hash is in this form.
#[must_use]
pub fn root(argument: &InvokeArgument, salt: &Salt) -> [u8; 32] {
    let parts = parts(argument);
    let hashes: Vec<[u8; 32]> = parts
        .iter()
        .map(|part| part_hash(&part_salt(salt, &part.path), &part.path, &part.value))
        .collect();
    root_of(
        form(argument),
        parts.len(),
        &tree(&hashes, &BTreeSet::new()).0,
    )
}

/// A part a change keeps: where it stands among the argument's parts, its path, its value and
/// its own salt, in hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptPart {
    pub at: u32,
    pub path: String,
    pub value: serde_json::Value,
    pub salt: String,
}

/// What a change keeps of the argument of the call it was made for, in the form whose signed
/// statement names the root of the argument's parts: the parts it holds, and the hashes that
/// prove them against the root ([`Self::proves`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptParts {
    /// The argument's form: 0 none, 1 a text, 2 an object.
    pub form: u8,
    /// How many parts the argument has.
    pub of: u32,
    /// The parts kept, in their order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<KeptPart>,
    /// The hashes beside the kept parts' paths to the root, in hex, in the order the tree is
    /// climbed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proof: Vec<String>,
}

impl KeptParts {
    /// What a change keeps of `argument`, signed under `salt`: the parts `keep` names, and the
    /// proof for them.
    #[must_use]
    pub fn of(
        argument: &InvokeArgument,
        salt: &Salt,
        keep: impl Fn(&serde_json::Value) -> bool,
    ) -> Self {
        let parts = parts(argument);
        let salts: Vec<[u8; 32]> = parts
            .iter()
            .map(|part| part_salt(salt, &part.path))
            .collect();
        let hashes: Vec<[u8; 32]> = parts
            .iter()
            .zip(&salts)
            .map(|(part, salt)| part_hash(salt, &part.path, &part.value))
            .collect();
        let kept: BTreeSet<usize> = parts
            .iter()
            .enumerate()
            .filter(|(_, part)| keep(&part.value))
            .map(|(at, _)| at)
            .collect();
        let (_, proof) = tree(&hashes, &kept);
        Self {
            form: form(argument),
            of: u32::try_from(parts.len()).unwrap_or(u32::MAX),
            parts: parts
                .iter()
                .zip(&salts)
                .enumerate()
                .filter(|(at, _)| kept.contains(at))
                .map(|(at, (part, salt))| KeptPart {
                    at: u32::try_from(at).unwrap_or(u32::MAX),
                    path: part.path.clone(),
                    value: part.value.clone().into_owned(),
                    salt: hex::encode(salt),
                })
                .collect(),
            proof: proof.iter().map(hex::encode).collect(),
        }
    }

    /// The root these parts and their proof climb to; `None` for parts that do not climb to
    /// one: out of place, out of order, or a proof too short or too long.
    #[must_use]
    pub fn root(&self) -> Option<[u8; 32]> {
        fn bytes(text: &str) -> Option<[u8; 32]> {
            let mut out = [0; 32];
            hex::decode_to_slice(text, &mut out).ok()?;
            Some(out)
        }
        let count = usize::try_from(self.of).ok()?;
        let mut proof = self.proof.iter().map(|hash| bytes(hash));
        let mut known: BTreeMap<usize, [u8; 32]> = BTreeMap::new();
        let mut last: Option<&str> = None;
        for part in &self.parts {
            let at = usize::try_from(part.at).ok()?;
            // In their order, each once, each among the argument's parts.
            if at >= count
                || known.contains_key(&at)
                || last.is_some_and(|last| last.as_bytes() >= part.path.as_bytes())
                || known.keys().next_back().is_some_and(|&before| before >= at)
            {
                return None;
            }
            last = Some(&part.path);
            known.insert(at, part_hash(&bytes(&part.salt)?, &part.path, &part.value));
        }
        let tree = if count == 0 {
            if !self.parts.is_empty() {
                return None;
            }
            [0; 32]
        } else if known.is_empty() {
            proof.next()??
        } else {
            let mut len = count;
            while len > 1 {
                let mut next = BTreeMap::new();
                let mut nodes = known.iter().peekable();
                while let Some((&at, hash)) = nodes.next() {
                    let pair = at / 2;
                    if at % 2 == 0 {
                        if at.checked_add(1) == Some(len) {
                            next.insert(pair, *hash);
                            continue;
                        }
                        let right = match nodes
                            .next_if(|(sibling, _)| Some(**sibling) == at.checked_add(1))
                        {
                            Some((_, sibling_hash)) => *sibling_hash,
                            None => proof.next()??,
                        };
                        next.insert(pair, node(hash, &right));
                    } else {
                        let left = proof.next()??;
                        next.insert(pair, node(&left, hash));
                    }
                }
                known = next;
                len = len.div_ceil(2);
            }
            *known.get(&0)?
        };
        if proof.next().is_some() {
            return None;
        }
        Some(root_of(self.form, count, &tree))
    }

    /// Whether these parts are parts of the argument whose root a statement signed.
    #[must_use]
    pub fn proves(&self, signed_root: &[u8; 32]) -> bool {
        self.root().as_ref() == Some(signed_root)
    }
}

/// The values a change holds after it is written, as the rule of what it keeps of its call's
/// argument reads them ([`Held::holds`]).
#[derive(Clone, Debug, Default)]
pub struct Held {
    texts: Vec<String>,
    others: BTreeSet<Vec<u8>>,
    marks: bool,
}

impl Held {
    /// Nothing: a change that keeps none of its call's argument.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// What a mark holds, a change that writes nothing and says what it marks in arguments of
    /// its own (a checkpoint's name): those it keeps as they are.
    #[must_use]
    pub fn mark() -> Self {
        Self {
            marks: true,
            ..Self::default()
        }
    }

    /// Whether it is a mark's ([`Self::mark`]).
    #[must_use]
    pub const fn is_mark(&self) -> bool {
        self.marks
    }

    /// Hold every scalar in `value`, and every key of an object in it.
    pub fn add(&mut self, value: &serde_json::Value) {
        match value {
            serde_json::Value::String(text) => self.texts.push(text.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|item| self.add(item)),
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    self.texts.push(key.clone());
                    self.add(value);
                }
            }
            other => {
                self.others.insert(tilo_canonical_json::to_vec(other));
            }
        }
    }

    /// Hold `text`.
    pub fn add_text(&mut self, text: &str) {
        self.texts.push(text.to_owned());
    }

    /// Whether the change holds the part `value`: a text contained in a text it holds, any
    /// other scalar equal to one it holds. An empty object or list is held by none.
    #[must_use]
    pub fn holds(&self, value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => {
                self.texts.iter().any(|held| held.contains(text.as_str()))
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => false,
            other => self.others.contains(&tilo_canonical_json::to_vec(other)),
        }
    }

    /// `value` with every scalar it does not hold made null: the arguments a runtime recorded
    /// for a call, as a change keeps them.
    #[must_use]
    pub fn redact(&self, value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Array(items) => items.iter().map(|item| self.redact(item)).collect(),
            serde_json::Value::Object(object) => serde_json::Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), self.redact(value)))
                    .collect(),
            ),
            scalar if self.holds(scalar) => scalar.clone(),
            _ => serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(json: &str) -> InvokeArgument {
        match serde_json::from_str(json).unwrap() {
            serde_json::Value::Object(object) => InvokeArgument::Json(object),
            _ => unreachable!(),
        }
    }

    /// Every subset of a call's parts proves against its root, and nothing else does: a part
    /// changed, a proof hash changed, a part moved, a proof too short or too long.
    #[test]
    fn any_kept_subset_proves_and_a_tampered_one_does_not() {
        let argument = object(
            r#"{"titles":["one","two","three","four","five"],"note":"x","n":3,"deep":{"a":[true,null],"b":{}}}"#,
        );
        let salt = [9; 32];
        let root = root(&argument, &salt);
        let all = parts(&argument);
        assert_eq!(all.len(), 10);
        for mask in 0u32..(1 << all.len()) {
            let values: Vec<_> = all
                .iter()
                .enumerate()
                .filter(|(at, _)| mask & (1 << at) != 0)
                .map(|(_, part)| part.value.clone().into_owned())
                .collect();
            let kept = KeptParts::of(&argument, &salt, |value| values.contains(value));
            assert!(kept.proves(&root), "{mask:b}");
            if let Some(first) = kept.parts.first() {
                let mut forged = kept.clone();
                forged.parts[0].value = serde_json::json!("forged");
                assert!(!forged.proves(&root), "{mask:b} {first:?}");
            }
            if !kept.proof.is_empty() {
                let mut forged = kept.clone();
                forged.proof[0] = hex::encode([1u8; 32]);
                assert!(!forged.proves(&root));
                let mut short = kept.clone();
                short.proof.pop();
                assert!(!short.proves(&root));
            }
            let mut long = kept.clone();
            long.proof.push(hex::encode([0u8; 32]));
            assert!(!long.proves(&root));
        }
        // No argument, and a text: one rule for every form.
        for argument in [InvokeArgument::None, InvokeArgument::Text("hi".into())] {
            let root = super::root(&argument, &salt);
            assert!(KeptParts::of(&argument, &salt, |_| true).proves(&root));
            assert!(KeptParts::of(&argument, &salt, |_| false).proves(&root));
        }
        // Another salt is another root: a part's hash is not guessed from its value.
        assert_ne!(root, super::root(&argument, &[8; 32]));
    }
}
