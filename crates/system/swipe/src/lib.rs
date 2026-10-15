//! Safe Rust wrapper over the FUTO swipe-library (`SwipeEngine`) via a
//! C-ABI shim (`csrc/swipe_ffi.cpp`).
//!
//! Real ML swipe-typing: the FUTO encoder (universal) + an optional paired
//! decoder + an optional context LM run on `ExecuTorch`, scored against a
//! dictionary trie. See [`FutoSwipe`].
//!
//! When the crate is built WITHOUT the FUTO static libs (env `TILO_SWIPE_LIB`
//! unset at build time → `cfg(futo)` off), [`available`] returns `false` and
//! [`FutoSwipe::new`] returns `None`, so callers (the on-screen keyboard)
//! transparently fall back to their geometric decoder.

/// A decoded swipe candidate: the word and its final score (higher = better).
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub word: String,
    pub score: f32,
}

/// Whether this build links the real FUTO swipe engine.
pub const fn available() -> bool {
    cfg!(futo)
}

#[cfg(futo)]
mod imp {
    use super::Candidate;
    use std::ffi::{CString, c_char, c_void};
    use std::path::Path;

    unsafe extern "C" {
        fn tilo_swipe_create(
            encoder_path: *const c_char,
            decoder_path: *const c_char,
            lm_path: *const c_char,
            lm_vocab_path: *const c_char,
            dict_combined_path: *const c_char,
            beam_width: i32,
            top_k: i32,
        ) -> *mut c_void;
        fn tilo_swipe_has_decoder(h: *mut c_void) -> i32;
        fn tilo_swipe_has_lm(h: *mut c_void) -> i32;
        fn tilo_swipe_recognize(
            h: *mut c_void,
            x: *const f32,
            y: *const f32,
            t: *const f32,
            n: usize,
            top_k: i32,
            words_buf: *mut c_char,
            words_cap: i32,
            scores: *mut f32,
            max: i32,
        ) -> i32;
        fn tilo_swipe_predict_next(
            h: *mut c_void,
            context: *const c_char,
            nctx: i32,
            k: i32,
            words_buf: *mut c_char,
            words_cap: i32,
            scores: *mut f32,
            max: i32,
        ) -> i32;
        fn tilo_swipe_destroy(h: *mut c_void);
    }

    const WORDS_CAP: usize = 8192;
    const MAX_RESULTS: usize = 32;

    /// A loaded FUTO swipe engine. Not thread-safe (the underlying
    /// `SwipeEngine` isn't) — keep it on one thread.
    pub struct FutoSwipe {
        handle: *mut c_void,
    }

    /// Paths to the model assets for [`FutoSwipe::new`].
    #[derive(Clone, Debug, Default)]
    pub struct ModelPaths {
        pub encoder: std::path::PathBuf,
        pub decoder: Option<std::path::PathBuf>,
        pub lm_model: Option<std::path::PathBuf>,
        pub lm_vocab: Option<std::path::PathBuf>,
        pub dict_combined: Option<std::path::PathBuf>,
    }

    fn cstr(p: &Path) -> CString {
        CString::new(p.to_string_lossy().as_bytes()).unwrap_or_default()
    }
    fn cstr_opt(p: &Option<std::path::PathBuf>) -> Option<CString> {
        p.as_deref().map(cstr)
    }
    fn ptr(c: &Option<CString>) -> *const c_char {
        c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())
    }

    impl FutoSwipe {
        /// Load the engine. Returns `None` if the encoder fails to load.
        pub fn new(paths: &ModelPaths, beam_width: i32, top_k: i32) -> Option<Self> {
            let enc = cstr(&paths.encoder);
            let dec = cstr_opt(&paths.decoder);
            let lm = cstr_opt(&paths.lm_model);
            let lmv = cstr_opt(&paths.lm_vocab);
            let dict = cstr_opt(&paths.dict_combined);
            let handle = unsafe {
                tilo_swipe_create(
                    enc.as_ptr(),
                    ptr(&dec),
                    ptr(&lm),
                    ptr(&lmv),
                    ptr(&dict),
                    beam_width,
                    top_k,
                )
            };
            if handle.is_null() {
                None
            } else {
                Some(Self { handle })
            }
        }

        pub fn has_decoder(&self) -> bool {
            unsafe { tilo_swipe_has_decoder(self.handle) != 0 }
        }
        pub fn has_lm(&self) -> bool {
            unsafe { tilo_swipe_has_lm(self.handle) != 0 }
        }

        /// Recognize a swipe. `x`/`y` are normalized to the layout's [0,1]
        /// space; `t` is milliseconds since the first point. Returns up to
        /// `top_k` ranked candidates (best first).
        pub fn recognize(&self, x: &[f32], y: &[f32], t: &[f32], top_k: i32) -> Vec<Candidate> {
            let n = x.len().min(y.len()).min(t.len());
            if n == 0 {
                return Vec::new();
            }
            let mut words = vec![0i8 as c_char; WORDS_CAP];
            let mut scores = vec![0f32; MAX_RESULTS];
            let count = unsafe {
                tilo_swipe_recognize(
                    self.handle,
                    x.as_ptr(),
                    y.as_ptr(),
                    t.as_ptr(),
                    n,
                    top_k,
                    words.as_mut_ptr(),
                    WORDS_CAP as i32,
                    scores.as_mut_ptr(),
                    MAX_RESULTS as i32,
                )
            };
            parse_results(&words, &scores, count)
        }

        /// Context-only next-word prediction (requires an LM). `context` is
        /// the previous words, oldest first.
        pub fn predict_next(&self, context: &[&str], k: i32) -> Vec<Candidate> {
            if !self.has_lm() {
                return Vec::new();
            }
            let mut buf: Vec<u8> = Vec::new();
            for w in context {
                buf.extend_from_slice(w.as_bytes());
                buf.push(0);
            }
            let mut words = vec![0i8 as c_char; WORDS_CAP];
            let mut scores = vec![0f32; MAX_RESULTS];
            let count = unsafe {
                tilo_swipe_predict_next(
                    self.handle,
                    buf.as_ptr() as *const c_char,
                    context.len() as i32,
                    k,
                    words.as_mut_ptr(),
                    WORDS_CAP as i32,
                    scores.as_mut_ptr(),
                    MAX_RESULTS as i32,
                )
            };
            parse_results(&words, &scores, count)
        }
    }

    impl Drop for FutoSwipe {
        fn drop(&mut self) {
            unsafe { tilo_swipe_destroy(self.handle) }
        }
    }

    fn parse_results(words: &[c_char], scores: &[f32], count: i32) -> Vec<Candidate> {
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, words.len()) };
        let mut out = Vec::new();
        let mut off = 0usize;
        for i in 0..count.max(0) as usize {
            let Some(rel) = bytes[off..].iter().position(|&b| b == 0) else {
                break;
            };
            let end = off + rel;
            if let Ok(s) = std::str::from_utf8(&bytes[off..end]) {
                out.push(Candidate {
                    word: s.to_string(),
                    score: scores.get(i).copied().unwrap_or(0.0),
                });
            }
            off = end + 1;
            if off >= bytes.len() {
                break;
            }
        }
        out
    }
}

#[cfg(futo)]
pub use imp::{FutoSwipe, ModelPaths};

#[cfg(not(futo))]
mod stub {
    use super::Candidate;
    use std::path::PathBuf;

    #[derive(Clone, Debug, Default)]
    pub struct ModelPaths {
        pub encoder: PathBuf,
        pub decoder: Option<PathBuf>,
        pub lm_model: Option<PathBuf>,
        pub lm_vocab: Option<PathBuf>,
        pub dict_combined: Option<PathBuf>,
    }

    /// Stub used when the crate is built without the FUTO libs. `new`
    /// always returns `None`, so callers fall back to a geometric decoder.
    pub struct FutoSwipe(());

    impl FutoSwipe {
        pub fn new(_paths: &ModelPaths, _beam_width: i32, _top_k: i32) -> Option<Self> {
            None
        }
        pub fn has_decoder(&self) -> bool {
            false
        }
        pub fn has_lm(&self) -> bool {
            false
        }
        pub fn recognize(&self, _x: &[f32], _y: &[f32], _t: &[f32], _top_k: i32) -> Vec<Candidate> {
            Vec::new()
        }
        pub fn predict_next(&self, _context: &[&str], _k: i32) -> Vec<Candidate> {
            Vec::new()
        }
    }
}

#[cfg(not(futo))]
pub use stub::{FutoSwipe, ModelPaths};
