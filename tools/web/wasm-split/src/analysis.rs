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
//! belong to the same bodies. Any other module reads nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Raised whenever what a run reads or writes changes, so a file an older
/// splitter filled is never read as this one's.
const MAGIC: &[u8; 8] = b"WSSANL\0\x01";

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
            let callees = read.numbers()?;
            let values = read.numbers()?;
            bodies.push(Body {
                hash,
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
            .map(|body| 8 + 8 + body.callees.len() * 4 + 8 + body.values.len() * 4)
            .sum::<usize>()
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
