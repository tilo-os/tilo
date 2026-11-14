//! What the splitter reads of each function's code, kept by that code's bytes
//! so a change to a few bodies costs those bodies and no more
//! (`WASM_SPLIT_CACHE`, beside what cache.rs keeps).
//!
//! The splitter walks every bound function's body twice: once for the
//! functions it calls (the call graph) and once for the immediates it holds
//! (the static data it reaches). Both read one body and nothing else, so
//! each body's answers are the same whatever the rest of the module says:
//! they are kept under the body's hash, and a run walks only the bodies whose
//! hash it did not see before.
//!
//! Which bodies a run may read back is settled by the module's structure:
//! a module whose sections other than its code, its count of imported
//! functions and its count of local functions are the same as the run that
//! filled the cache has the same functions in the same order, so the answers
//! belong to the same bodies. A module whose functions moved instead (one
//! added or removed) is read through the renumbering the bind patch wrote
//! ([`Renumber`]): a body whose bytes do the same thing, its indices called
//! differently, is the same body, and its answers are moved to the new
//! indices.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Raised whenever what a run reads or writes changes, so a file an older
/// splitter filled is never read as this one's.
const MAGIC: &[u8; 8] = b"WSSANL\0\x02";

/// [`Analysis::structure`] is two 64-bit hashes written as hex.
pub const STRUCTURE_LEN: usize = 32;

/// Reading a cache file: bytes, and how far into them the reader is.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(len)?)?;
        self.at += len;
        Some(slice)
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// A run of numbers, as they were written: a count, then the numbers.
    fn numbers(&mut self) -> Option<Vec<u32>> {
        let len = self.u64()? as usize;
        let raw = self.take(len.checked_mul(4)?)?;
        Some(
            raw.chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().expect("4 bytes")))
                .collect(),
        )
    }
}

/// What one function body holds (see the module comment).
#[derive(Default, Clone)]
pub struct Body {
    /// The body's bytes, hashed.
    pub hash: u64,
    /// What the body does, the numbers it holds named by what they name
    /// rather than by their index (`crate::content`): the identity that
    /// survives a renumbering.
    pub content: u64,
    /// The functions the body calls or names, by index in the module (its
    /// imports first), in the order the walk met them.
    pub callees: Vec<u32>,
    /// Every immediate the body holds that can name an address, sorted and
    /// without repeats.
    pub values: Vec<u32>,
}

/// Every local function's answers, in index order among the module's local
/// functions (its imports come before them).
#[derive(Default)]
pub struct Analysis {
    pub structure: String,
    pub bodies: Vec<Body>,
}

impl Analysis {
    /// The answers for the module whose non-code sections and function counts
    /// hash to `structure`, if this one holds an entry for every one of its
    /// `bodies` with the hash given: the same structure and the same body
    /// bytes mean the same answers.
    pub fn matching(&self, structure: &str, hashes: &[u64]) -> Option<HashMap<usize, Body>> {
        if self.structure != structure || self.bodies.len() != hashes.len() {
            return None;
        }
        let mut hit = HashMap::with_capacity(hashes.len());
        for (index, (body, hash)) in self.bodies.iter().zip(hashes).enumerate() {
            if body.hash == *hash {
                hit.insert(index, body.clone());
            }
        }
        Some(hit)
    }

    /// The answers of the bodies a renumbering left as they were: body
    /// `old` is body `new` when what it does (`content`) is the same, and
    /// its callees are then the ones it names at their new indices. `None`
    /// when a callee has no new index, so the run walks that body again.
    pub fn matching_renumbered(
        &self,
        renumber: &Renumber,
        content: &[u64],
        hashes: &[u64],
    ) -> Option<HashMap<usize, Body>> {
        if self.bodies.len() != renumber.map.len() {
            return None;
        }
        let mut hit = HashMap::with_capacity(self.bodies.len());
        for (old, body) in self.bodies.iter().enumerate() {
            let Some(new) = renumber.map[old] else {
                continue;
            };
            let new = new as usize;
            if *content.get(new)? != body.content {
                continue;
            }
            let mut callees = Vec::with_capacity(body.callees.len());
            let mut resolved = true;
            for callee in &body.callees {
                let index = *callee as usize;
                if index < renumber.old_imports as usize {
                    callees.push(*callee);
                } else {
                    let local = index - renumber.old_imports as usize;
                    match renumber.map.get(local).copied().flatten() {
                        Some(target) => {
                            callees.push(renumber.new_imports + target);
                        }
                        None => {
                            resolved = false;
                            break;
                        }
                    }
                }
            }
            if !resolved {
                continue;
            }
            hit.insert(
                new,
                Body {
                    hash: hashes[new],
                    content: content[new],
                    callees,
                    values: body.values.clone(),
                },
            );
        }
        Some(hit)
    }
}

/// The file beside the emitted modules that keeps the analysis.
pub struct AnalysisCache {
    dir: PathBuf,
}

impl AnalysisCache {
    /// The cache `WASM_SPLIT_CACHE` names, if it names one (empty names none).
    /// A directory the splitter cannot write in names none either: the cache
    /// is a build's speed, never its outcome.
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var_os("WASM_SPLIT_CACHE")?;
        (!dir.is_empty()).then(|| Self::at(Path::new(&dir)))
    }

    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("analysis.bin")
    }

    /// What an earlier run kept, if there is one this splitter can read.
    pub fn load(&self) -> Option<Analysis> {
        let bytes = std::fs::read(self.path()).ok()?;
        let mut read = Reader {
            bytes: &bytes,
            at: 0,
        };
        (read.take(MAGIC.len())? == MAGIC).then_some(())?;
        let structure = String::from_utf8(read.take(STRUCTURE_LEN)?.to_vec()).ok()?;
        let count = read.u64()? as usize;
        let mut bodies = Vec::with_capacity(count);
        for _ in 0..count {
            let hash = read.u64()?;
            let content = read.u64()?;
            let callees = read.numbers()?;
            let values = read.numbers()?;
            bodies.push(Body {
                hash,
                content,
                callees,
                values,
            });
        }
        Some(Analysis { structure, bodies })
    }

    /// Keep `analysis` as what a run of this module reads. Written beside and
    /// renamed in, so a split that stops leaves no half file behind.
    pub fn store(&self, analysis: &Analysis) -> Result<()> {
        let mut out = Vec::with_capacity(estimate(analysis));
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(analysis.structure.as_bytes());
        out.extend_from_slice(&[0; STRUCTURE_LEN][analysis.structure.len().min(STRUCTURE_LEN)..]);
        out.extend_from_slice(&(analysis.bodies.len() as u64).to_le_bytes());
        for body in &analysis.bodies {
            out.extend_from_slice(&body.hash.to_le_bytes());
            out.extend_from_slice(&body.content.to_le_bytes());
            for numbers in [&body.callees, &body.values] {
                out.extend_from_slice(&(numbers.len() as u64).to_le_bytes());
                for number in numbers {
                    out.extend_from_slice(&number.to_le_bytes());
                }
            }
        }
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.path();
        let partial = self.dir.join(format!(".analysis.{}", std::process::id()));
        std::fs::write(&partial, &out).with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

fn estimate(analysis: &Analysis) -> usize {
    MAGIC.len()
        + analysis.structure.len()
        + 8
        + analysis
            .bodies
            .iter()
            .map(|body| 8 + 8 + 8 + body.callees.len() * 4 + 8 + body.values.len() * 4)
            .sum::<usize>()
}

/// The functions the bind patch made of the previous bound module's
/// (`tools/web/wasm-split/src/bin/bind_patch.rs`, its `renumber.bin`): what
/// each function's index became, or `None` when it is gone.
pub struct Renumber {
    /// The imports of the module it was made from, and of this one: the
    /// function indices start after them.
    pub old_imports: u32,
    pub new_imports: u32,
    /// Each old local function's new index, in old order.
    pub map: Vec<Option<u32>>,
    /// Each of the new linked module's functions' index in the bound module
    /// (imports first), in linked order, or `None` when the bind dropped it.
    pub linked: Vec<Option<u32>>,
}

impl Renumber {
    /// The linked module's function a bound function is, by the bound
    /// function's local index.
    pub fn linked_of(&self, bound_local: usize) -> Option<usize> {
        self.linked
            .iter()
            .position(|target| *target == Some(self.new_imports + bound_local as u32))
    }

    /// The renumbering `WASM_SPLIT_RENUMBER` names, if it names one written
    /// for `bound` (the module this run splits).
    pub fn from_env(bound: &[u8]) -> Option<Self> {
        let path = std::env::var_os("WASM_SPLIT_RENUMBER")?;
        if path.is_empty() {
            return None;
        }
        Self::read(Path::new(&path), bound)
    }

    /// The renumbering a file holds, for a test that drives the splitter
    /// through one.
    #[cfg(test)]
    pub(crate) fn read_file(path: &Path, bound: &[u8]) -> Option<Self> {
        Self::read(path, bound)
    }

    fn read(path: &Path, bound: &[u8]) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let mut read = Reader {
            bytes: &bytes,
            at: 0,
        };
        if read.take(RENUMBER_MAGIC.len())? != RENUMBER_MAGIC {
            return None;
        }
        if read.u64()? != hash_bytes(bound) {
            return None;
        }
        let old_imports = u32::from_le_bytes(read.take(4)?.try_into().ok()?);
        let new_imports = u32::from_le_bytes(read.take(4)?.try_into().ok()?);
        let len = u32::from_le_bytes(read.take(4)?.try_into().ok()?) as usize;
        let mut map = Vec::with_capacity(len);
        for _ in 0..len {
            let target = u32::from_le_bytes(read.take(4)?.try_into().ok()?);
            map.push((target != u32::MAX).then_some(target));
        }
        let len = u32::from_le_bytes(read.take(4)?.try_into().ok()?) as usize;
        let mut linked = Vec::with_capacity(len);
        for _ in 0..len {
            let target = u32::from_le_bytes(read.take(4)?.try_into().ok()?);
            linked.push((target != u32::MAX).then_some(target));
        }
        Some(Self {
            old_imports,
            new_imports,
            map,
            linked,
        })
    }
}

const RENUMBER_MAGIC: &[u8; 8] = b"WSBFNM\0\x02";

/// Write the renumbering the bind patch writes, for a test that drives the
/// splitter through one (split.rs's `read_plan_back`).
#[cfg(test)]
pub(crate) fn write_renumbering(
    path: &Path,
    bound: &[u8],
    old_imports: u32,
    new_imports: u32,
    map: &[Option<u32>],
    linked: &[Option<u32>],
) -> std::io::Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(RENUMBER_MAGIC);
    out.extend_from_slice(&hash_bytes(bound).to_le_bytes());
    out.extend_from_slice(&old_imports.to_le_bytes());
    out.extend_from_slice(&new_imports.to_le_bytes());
    out.extend_from_slice(&(map.len() as u32).to_le_bytes());
    for target in map {
        out.extend_from_slice(&target.unwrap_or(u32::MAX).to_le_bytes());
    }
    out.extend_from_slice(&(linked.len() as u32).to_le_bytes());
    for target in linked {
        out.extend_from_slice(&target.unwrap_or(u32::MAX).to_le_bytes());
    }
    std::fs::write(path, out)
}

/// The bytes' hash, as the bind patch writes it.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// A body's bytes, hashed: FNV-1a over its 8-byte words, with its length
/// mixed in, so two bodies of the same word and different padding differ.
pub fn hash_body(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ (bytes.len() as u64);
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        hash ^= u64::from_le_bytes(word.try_into().expect("8 bytes"));
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for byte in words.remainder() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body that does the same thing at a new index reads back, its
    /// callees moved with it; one that names a function with no new index is
    /// walked again.
    #[test]
    fn a_renumbered_body_reads_back_at_its_new_index() {
        let analysis = Analysis {
            structure: "0123456789abcdef0123456789abcdef".to_string(),
            bodies: vec![
                Body {
                    hash: 1,
                    content: 10,
                    callees: vec![2, 3],
                    values: vec![7],
                },
                Body {
                    hash: 2,
                    content: 20,
                    callees: vec![],
                    values: vec![],
                },
                Body {
                    hash: 3,
                    content: 30,
                    callees: vec![5],
                    values: vec![],
                },
            ],
        };
        // One import, then: every old local is one index later in the new
        // module, whose first local is one the bind added.
        let renumber = Renumber {
            old_imports: 1,
            new_imports: 1,
            map: vec![Some(1), Some(2), Some(3)],
            linked: vec![None, Some(2), Some(3), Some(4)],
        };
        let content = [0, 10, 20, 30];
        let hashes = [0, 11, 22, 33];
        let read = analysis
            .matching_renumbered(&renumber, &content, &hashes)
            .expect("read back");
        let body = read.get(&1).expect("the first kept body");
        assert_eq!(body.hash, 11);
        assert_eq!(body.content, 10);
        assert_eq!(body.values, vec![7]);
        // Absolute 2 is the old second local, now the new third; absolute 3
        // is the old third, now the new fourth.
        assert_eq!(body.callees, vec![3, 4]);
        assert!(read.contains_key(&2), "the second body did not read back");
        assert!(
            !read.contains_key(&3),
            "a body naming a function with no new index read back"
        );
        assert_eq!(renumber.linked_of(1), Some(1));
    }

    /// What a run keeps is what the next one reads back.
    #[test]
    fn a_stored_analysis_reads_back() {
        let dir = std::env::temp_dir().join(format!("wasm-split-analysis-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = AnalysisCache::at(&dir);
        let analysis = Analysis {
            structure: "0123456789abcdef0123456789abcdef".to_string(),
            bodies: vec![
                Body {
                    hash: 7,
                    content: 11,
                    callees: vec![3, 9],
                    values: vec![0, 7, 0x1000],
                },
                Body::default(),
            ],
        };
        cache.store(&analysis).expect("store");
        let read = cache.load().expect("load");
        assert_eq!(read.structure, analysis.structure);
        assert_eq!(read.bodies.len(), 2);
        assert_eq!(read.bodies[0].hash, 7);
        assert_eq!(read.bodies[0].callees, vec![3, 9]);
        assert_eq!(read.bodies[0].values, vec![0, 7, 0x1000]);
        assert_eq!(read.bodies[1].hash, 0);
        assert_eq!(read.bodies[1].callees, Vec::<u32>::new());
        let hashes = [7u64, 0];
        assert!(read.matching(&analysis.structure, &hashes).is_some());
        assert_eq!(
            read.matching(&analysis.structure, &[8, 0])
                .expect("a body with other bytes is skipped, not the rest")
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![1],
            "a body with other bytes has other answers"
        );
        assert!(
            read.matching("ffffffffffffffffffffffffffffffff", &hashes)
                .is_none(),
            "a module with another structure reads nothing"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
