//! The owned CPU decoder, over the published whisper.cpp bindings.
//!
//! One confined worker owns one context. Every call replaces the decoder state,
//! so a draft's fallback random source, detected language and tokens never
//! reach the final that replaces it.
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::os::unix::fs::FileExt as _;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use whisper_rs_sys as sys;
use zeroize::Zeroizing;

use super::{
    MAX_MODEL_BYTES, MAX_SAMPLES, MAX_TEXT_BYTES, SAMPLE_RATE, SealedSpeechModel, SpeechError,
};

/// The parent's Finish hint. Its handler performs one lock-free store and never
/// allocates, logs or touches model or audio state; the flag stays set until
/// the process retires.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_: c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// The decoder's stop poll. Reading one atomic flag is all it does, so the
/// pinned engine can call it from inside a graph, and the state stays valid for
/// whichever run follows.
unsafe extern "C" fn stop_requested(_: *mut c_void) -> bool {
    stop()
}

fn stop() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// Bytes the pinned parser may read: a sealed descriptor, or verified weights
/// this call already holds. Neither is reopened by path.
#[derive(Clone, Copy)]
pub(super) enum Source<'a> {
    Descriptor(&'a File),
    Bytes(&'a [u8]),
}

impl Source<'_> {
    fn read_at(&self, offset: usize, output: &mut [u8]) -> bool {
        match *self {
            Self::Descriptor(file) => {
                u64::try_from(offset).is_ok_and(|offset| file.read_exact_at(output, offset).is_ok())
            }
            Self::Bytes(bytes) => {
                let Some(available) = offset
                    .checked_add(output.len())
                    .and_then(|end| bytes.get(offset..end))
                else {
                    return false;
                };
                output.copy_from_slice(available);
                true
            }
        }
    }
}

/// The pinned parser's read position over one [`Source`].
pub(super) struct Cursor<'a> {
    source: Source<'a>,
    length: usize,
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(source: Source<'a>, length: usize) -> Self {
        Self {
            source,
            length,
            offset: 0,
        }
    }

    /// Bounded size of the bytes behind this cursor.
    pub(super) fn length(&self) -> usize {
        self.length
    }

    /// The loader the pinned parser reads through. It retains only this
    /// pointer, so it must outlive the synchronous parse.
    pub(super) fn loader(&mut self) -> sys::whisper_model_loader {
        sys::whisper_model_loader {
            context: std::ptr::from_mut(self).cast(),
            read: Some(read_model),
            eof: Some(model_at_end),
            close: Some(close_model),
        }
    }
}

/// # Safety
/// `context` is the cursor this loader was built from, and `output` has `count`
/// writable bytes.
unsafe extern "C" fn read_model(context: *mut c_void, output: *mut c_void, count: usize) -> usize {
    // SAFETY: the parser hands back exactly the cursor it was given, which
    // outlives the parse, and `count` is the size of its own buffer.
    let cursor = unsafe { &mut *context.cast::<Cursor<'_>>() };
    let output = unsafe { std::slice::from_raw_parts_mut(output.cast::<u8>(), count) };
    let remaining = cursor.length.saturating_sub(cursor.offset);
    if remaining < count || !cursor.source.read_at(cursor.offset, output) {
        // Three header fields are probed at exact EOF, and a field that crosses
        // the end is a truncated model. Neither may read uninitialized bytes.
        output.fill(0);
        return 0;
    }
    cursor.offset = cursor.offset.saturating_add(count);
    count
}

/// # Safety
/// `context` is the cursor this loader was built from.
unsafe extern "C" fn model_at_end(context: *mut c_void) -> bool {
    // SAFETY: same live cursor as [`read_model`].
    let cursor = unsafe { &*context.cast::<Cursor<'_>>() };
    cursor.offset == cursor.length
}

unsafe extern "C" fn close_model(_: *mut c_void) {}

/// The worker never exposes the model's own log lines, and no model-controlled
/// message reaches diagnostics.
extern "C" fn discard_log(_: sys::ggml_log_level, _: *const c_char, _: *mut c_void) {}

/// The pinned GGML header names the architecture that follows it. Flash
/// attention is enabled for the base and small graphs, whose measured CPU path
/// is faster with it; tiny keeps its measured non-flash path. This chooses an
/// allocation strategy only: the parser still reads the receipt-checked input.
fn bounded_cpu_flash(header: &[u8; 40]) -> bool {
    let word = |offset: usize| -> Option<u32> {
        header
            .get(offset..offset.checked_add(4)?)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_le_bytes)
    };
    // Head counts precede layer counts in the pinned GGML header.
    let architecture = |state: u32, head: u32, layer: u32| {
        word(12) == Some(state)
            && word(16) == Some(head)
            && word(20) == Some(layer)
            && word(28) == Some(state)
            && word(32) == Some(head)
            && word(36) == Some(layer)
    };
    word(0) == Some(0x6767_6d6c) && (architecture(512, 8, 6) || architecture(768, 12, 12))
}

/// Bounded decode parameters for one call. Only a draft carries the stop poll:
/// a final must always run to completion.
fn full_params(
    threads: c_int,
    vocabulary: &[c_int],
    language: *const c_char,
    draft: bool,
) -> sys::whisper_full_params {
    // SAFETY: the pinned defaults initialize every field of the struct.
    let mut params = unsafe {
        sys::whisper_full_default_params(sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY)
    };
    // Greedy fallback defaults to five candidates and a sevenfold KV arena.
    // Keep one decoder within the worker budget even on uncertain speech.
    params.greedy.best_of = 1;
    params.n_threads = threads;
    params.translate = false;
    params.no_context = true;
    params.print_special = false;
    params.print_progress = false;
    params.print_realtime = false;
    params.print_timestamps = false;
    params.suppress_blank = true;
    params.suppress_nst = true;
    params.language = language;
    if draft {
        // The pinned engine polls this after the encoder graph and after every
        // decoded token, so a stop ends the run promptly and leaves the
        // context valid for the final.
        params.abort_callback = Some(stop_requested);
    }
    if !vocabulary.is_empty() {
        params.prompt_tokens = vocabulary.as_ptr();
        params.prompt_n_tokens = c_int::try_from(vocabulary.len()).unwrap_or(c_int::MAX);
        params.n_max_text_ctx = 65;
        params.carry_initial_prompt = true;
    }
    params
}

/// An exclusively owned CPU decoder. Its raw pointers are neither `Send` nor
/// `Sync`. Keep this in the worker; parent cancellation terminates that owned
/// process even while inference is busy. No Rust callback is installed beyond
/// the stop poll, which reads one atomic flag.
pub struct SpeechEngine {
    model: NonNull<sys::whisper_context>,
    state: Option<NonNull<sys::whisper_state>>,
    pristine: bool,
    threads: c_int,
    flash_attention: bool,
    draft_mode: bool,
    vocabulary: Vec<c_int>,
    /// Language this session's drafts already detected. A final detects from
    /// its own audio, so the kept text is never decided from a short draft.
    detected: Option<c_int>,
}

impl SpeechEngine {
    /// Install the owned worker's lock-free provisional-inference stop signal.
    /// Call before Ready; failure never advertises support or opens capture.
    pub fn enable_draft_interrupt() -> Result<(), SpeechError> {
        // SAFETY: a zeroed sigaction is SIG_DFL with no flags, and the handler
        // only stores a lock-free flag.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = request_stop as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: both masks are initialized below before the kernel sees them.
        let mut unblock: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: the masks are plain initializers, and `action` and `unblock`
        // outlive every call that reads them.
        let prepared = unsafe {
            libc::sigemptyset(&raw mut action.sa_mask) == 0
                && libc::sigemptyset(&raw mut unblock) == 0
                && libc::sigaddset(&raw mut unblock, libc::SIGUSR1) == 0
        };
        STOP.store(false, Ordering::Relaxed);
        // SAFETY: as above; the signal stays unblocked for the process lifetime.
        let installed = prepared
            && unsafe { libc::sigaction(libc::SIGUSR1, &raw const action, std::ptr::null_mut()) }
                == 0
            && unsafe {
                libc::sigprocmask(libc::SIG_UNBLOCK, &raw const unblock, std::ptr::null_mut())
            } == 0;
        if installed {
            Ok(())
        } else {
            Err(SpeechError::WorkerStopped)
        }
    }

    /// Mark only the synchronous draft call as eligible for interruption.
    pub fn draft_mode(&mut self, enabled: bool) {
        self.draft_mode = enabled;
    }

    /// Observe the attention strategy actually passed to the loaded engine.
    /// Qualification must not infer this from filenames or timings.
    pub fn flash_attention_enabled(&self) -> bool {
        self.flash_attention
    }

    /// Configure bounded vocabulary tokens without rewriting output. Set before
    /// starting a new recognition session. A failed replacement leaves no
    /// binding behind; do not reuse a recognition draft cache across changes.
    pub fn set_vocabulary(&mut self, hints: &[String]) -> Result<(), SpeechError> {
        self.vocabulary.clear();
        if hints.len() > 256 {
            return Err(SpeechError::Protocol);
        }
        let mut text = Zeroizing::new(String::new());
        for hint in hints {
            if hint.is_empty()
                || hint.chars().count() > 128
                || !hint.chars().any(char::is_alphabetic)
                || hint.chars().any(|c| c.is_control() || c.is_whitespace())
                || hint.contains("<|")
                || hint.contains("|>")
            {
                return Err(SpeechError::Protocol);
            }
            if text.len().saturating_add(hint.len()).saturating_add(1) > 16 * 1024 {
                return Err(SpeechError::Protocol);
            }
            text.push_str(hint);
            text.push('\n');
        }
        if hints.is_empty() {
            return Ok(());
        }
        // Retain at most 64 ordinary tokens, ending at whole-word boundaries.
        // Half of the text context is the most a prompt may occupy.
        // SAFETY: this engine owns the live context.
        let budget = unsafe { sys::whisper_n_text_ctx(self.model.as_ptr()) }
            .saturating_div(2)
            .saturating_sub(1)
            .min(64);
        if budget <= 0 {
            return Err(SpeechError::Protocol);
        }
        let budget = usize::try_from(budget).map_err(|_| SpeechError::Protocol)?;
        let mut selected = Vec::new();
        for line in text.lines() {
            if line.is_empty() || line.len() > 512 {
                return Err(SpeechError::Protocol);
            }
            // Every ordinary word carries its leading space, and the buffer is
            // zero-terminated after the word.
            let mut word = [0_u8; 514];
            let (space, rest) = word.split_at_mut(1);
            space.copy_from_slice(b" ");
            rest.get_mut(..line.len())
                .ok_or(SpeechError::Protocol)?
                .copy_from_slice(line.as_bytes());
            let mut tokens = [0; 64];
            // SAFETY: `word` is NUL-terminated, `tokens` has exactly 64 slots,
            // and both outlive this synchronous call.
            let count = unsafe {
                sys::whisper_tokenize(
                    self.model.as_ptr(),
                    word.as_ptr().cast(),
                    tokens.as_mut_ptr(),
                    64,
                )
            };
            let Ok(count) = usize::try_from(count) else {
                break;
            };
            if count > budget.saturating_sub(selected.len()) {
                break;
            }
            // SAFETY: as above.
            let end = unsafe { sys::whisper_token_eot(self.model.as_ptr()) };
            for token in tokens.get(..count).ok_or(SpeechError::Protocol)? {
                if *token < 0 || *token >= end {
                    return Err(SpeechError::Protocol);
                }
                selected.push(*token);
            }
        }
        if selected.is_empty() {
            return Err(SpeechError::Protocol);
        }
        self.vocabulary = selected;
        Ok(())
    }

    /// Parse the verified immutable descriptor after worker confinement.
    /// No file path is reopened and no Rust callback crosses this boundary.
    pub fn load_file(model: &SealedSpeechModel) -> Result<Self, SpeechError> {
        model.check_seals()?;
        let mut header = [0_u8; 40];
        if model.size() < 64 || model.file().read_exact_at(&mut header, 0).is_err() {
            return Err(SpeechError::ModelReceipt);
        }
        let length = usize::try_from(model.size()).map_err(|_| SpeechError::ModelReceipt)?;
        Self::load_from(Source::Descriptor(model.file()), &header, length)
    }

    /// Load already receipt-checked model bytes synchronously in the worker.
    pub fn load(bytes: &[u8]) -> Result<Self, SpeechError> {
        if bytes.len() < 64 || u64::try_from(bytes.len()).map_or(true, |len| len > MAX_MODEL_BYTES)
        {
            return Err(SpeechError::ModelReceipt);
        }
        let header = bytes
            .get(..40)
            .and_then(|header| <[u8; 40]>::try_from(header).ok())
            .ok_or(SpeechError::ModelReceipt)?;
        Self::load_from(Source::Bytes(bytes), &header, bytes.len())
    }

    fn load_from(
        source: Source<'_>,
        header: &[u8; 40],
        length: usize,
    ) -> Result<Self, SpeechError> {
        if header.get(..4) != Some(b"lmgg".as_slice()) {
            return Err(SpeechError::ModelReceipt);
        }
        let flash_attention = bounded_cpu_flash(header);
        // SAFETY: installs one process-global no-op log callback, before this
        // worker parses or runs anything.
        unsafe { sys::whisper_log_set(Some(discard_log), std::ptr::null_mut()) };
        // SAFETY: the pinned defaults initialize every field.
        let mut params = unsafe { sys::whisper_context_default_params() };
        params.use_gpu = false;
        params.flash_attn = flash_attention;
        let mut cursor = Cursor::new(source, length);
        let mut loader = cursor.loader();
        // The parser copies the weights first, so the verified input can be
        // released before decoder arenas are allocated.
        // SAFETY: the loader and the cursor it reads live through this call;
        // the parser retains neither.
        let model = unsafe { sys::whisper_init_with_params_no_state(&raw mut loader, params) };
        NonNull::new(model)
            .map(|model| Self {
                model,
                state: None,
                pristine: false,
                threads: 2,
                flash_attention,
                draft_mode: false,
                vocabulary: Vec::new(),
                detected: None,
            })
            .ok_or(SpeechError::Recognition)
    }

    /// Match the worker's one- or two-CPU budget without oversubscribing it.
    /// Query the available CPUs before confinement, then pass only this count.
    pub fn set_threads(&mut self, threads: usize) -> Result<(), SpeechError> {
        let threads = c_int::try_from(threads).map_err(|_| SpeechError::Protocol)?;
        if !(1..=2).contains(&threads) {
            return Err(SpeechError::Protocol);
        }
        self.threads = threads;
        Ok(())
    }

    /// Allocate CPU decoder arenas after releasing the verified input bytes.
    /// Call before announcing readiness or opening a microphone.
    pub fn prepare(&mut self) -> Result<(), SpeechError> {
        if self.state.is_none() {
            // SAFETY: this engine owns the live context and takes ownership of
            // whatever state the pinned engine allocates.
            self.state = NonNull::new(unsafe { sys::whisper_init_state(self.model.as_ptr()) });
            self.pristine = self.state.is_some();
        }
        self.state.map(|_| ()).ok_or(SpeechError::Recognition)
    }

    /// Confirm engine language support before opening a microphone.
    /// # Errors
    /// Rejects invalid/unsupported language codes; auto never translates.
    pub fn language(&self, language: &str) -> Result<CString, SpeechError> {
        if language.is_empty()
            || language.len() > 16
            || !language.bytes().all(|byte| byte.is_ascii_lowercase())
        {
            return Err(SpeechError::UnsupportedLanguage);
        }
        let language = CString::new(language).map_err(|_| SpeechError::UnsupportedLanguage)?;
        if self.accepts(&language) {
            Ok(language)
        } else {
            Err(SpeechError::UnsupportedLanguage)
        }
    }

    fn accepts(&self, language: &CStr) -> bool {
        if language.to_bytes() == b"auto" {
            return true;
        }
        // SAFETY: this engine owns the live context and the language is
        // NUL-terminated.
        let id = unsafe { sys::whisper_lang_id(language.as_ptr()) };
        // SAFETY: as above; these are metadata queries on the same context.
        if unsafe { sys::whisper_is_multilingual(self.model.as_ptr()) } == 0 {
            return id == 0;
        }
        // Pinned 1.8.3 vocabulary::num_languages; newer language IDs must not be
        // offered on older models whose vocabulary has no token for them.
        // SAFETY: as above.
        id >= 0
            && id
                < unsafe { sys::whisper_model_n_vocab(self.model.as_ptr()) }.saturating_sub(51_766)
    }

    /// Decode 16 kHz mono samples, replacing a draft rather than appending
    /// competing hypotheses. Caller supplies complete bounded phrase audio.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        language: &CString,
    ) -> Result<String, SpeechError> {
        if samples.len() > MAX_SAMPLES || samples.iter().any(|sample| !sample.is_finite()) {
            return Err(SpeechError::AudioTooLong);
        }
        if samples.len() < SAMPLE_RATE / 4 || samples.iter().all(|sample| *sample == 0.0) {
            return Ok(String::new());
        }
        let count = c_int::try_from(samples.len()).map_err(|_| SpeechError::AudioTooLong)?;
        if self.draft_mode && stop() {
            return Err(SpeechError::DraftInterrupted);
        }
        // Readiness allocated one pristine arena. Consume it once rather than
        // discarding it, and replace every later state: fallback RNG, language
        // detection and tokens never carry across drafts and finals.
        if self.state.is_some() && !self.pristine {
            self.release_state();
        }
        self.prepare()?;
        self.pristine = false;
        let state = self.state.ok_or(SpeechError::Recognition)?;
        // Detection costs one encoder pass of this call's audio. Drafts reuse
        // the language this session already detected; a final always detects.
        let detect = self.draft_mode && language.to_bytes() == b"auto";
        let language = match self.detected.filter(|_| detect) {
            // SAFETY: the pinned language table outlives every call.
            Some(id) => unsafe { sys::whisper_lang_str(id) },
            None => language.as_ptr(),
        };
        let params = full_params(self.threads, &self.vocabulary, language, self.draft_mode);
        // FFT preprocessing allocates per thread. Compute mel serially inside
        // this call, then keep both threads for recognition.
        // SAFETY: the model, state and finite samples live through the call;
        // the pinned API copies the mel and retains no input pointer.
        if unsafe {
            sys::whisper_pcm_to_mel_with_state(
                self.model.as_ptr(),
                state.as_ptr(),
                samples.as_ptr(),
                count,
                1,
            )
        } != 0
        {
            return Err(SpeechError::Recognition);
        }
        if self.draft_mode && stop() {
            return Err(SpeechError::DraftInterrupted);
        }
        // SAFETY: as above; the parameters reference only this engine's state
        // and vocabulary and are consumed by this call.
        let result = unsafe {
            sys::whisper_full_with_state(
                self.model.as_ptr(),
                state.as_ptr(),
                params,
                std::ptr::null(),
                0,
            )
        };
        // The pinned decoder frees the state when its KV allocation fails.
        // Never leave an owning handle pointing at it.
        if result == -7 {
            self.state = None;
        }
        if self.draft_mode && stop() {
            return Err(SpeechError::DraftInterrupted);
        }
        if result != 0 {
            return Err(SpeechError::Recognition);
        }
        if detect {
            // This call detected the language and left it on the state it ran
            // on; the next draft starts from a fresh state.
            // SAFETY: the call above succeeded, so its state is still live.
            self.detected = Some(unsafe { sys::whisper_full_lang_id_from_state(state.as_ptr()) });
        }
        segments(state)
    }

    fn release_state(&mut self) {
        if let Some(state) = self.state.take() {
            // SAFETY: this engine owns the state and releases it once.
            unsafe { sys::whisper_free_state(state.as_ptr()) };
        }
    }
}

impl Drop for SpeechEngine {
    fn drop(&mut self) {
        self.release_state();
        // SAFETY: exactly this owner releases this live context exactly once.
        unsafe { sys::whisper_free(self.model.as_ptr()) };
    }
}

/// Join the decoded segments within one worker text budget.
fn segments(state: NonNull<sys::whisper_state>) -> Result<String, SpeechError> {
    let mut text = Zeroizing::new(String::new());
    // SAFETY: the state is live here and owns the text of its segments.
    let count = unsafe { sys::whisper_full_n_segments_from_state(state.as_ptr()) };
    for index in 0..count {
        // SAFETY: `index` is one of this state's segment indexes.
        let segment =
            unsafe { sys::whisper_full_get_segment_text_from_state(state.as_ptr(), index) };
        let segment = NonNull::new(segment.cast_mut()).ok_or(SpeechError::Recognition)?;
        // SAFETY: the pinned API returns a NUL-terminated string the state owns.
        let segment = unsafe { CStr::from_ptr(segment.as_ptr()) }.to_bytes();
        if text.len().saturating_add(segment.len()) > MAX_TEXT_BYTES {
            return Err(SpeechError::TextTooLong);
        }
        text.push_str(std::str::from_utf8(segment).map_err(|_| SpeechError::Recognition)?);
    }
    let text = text.as_str();
    // ASR segment whitespace is not a spoken Enter/paragraph command.
    // Normalize whitespace before the compositor's control-character gate.
    if text
        .chars()
        .any(|ch| ch.is_control() && !ch.is_whitespace())
    {
        return Err(SpeechError::Recognition);
    }
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_draft_run_polls_the_stop_signal() {
        let language = c"en";
        let draft = full_params(2, &[], language.as_ptr(), true);
        assert!(draft.abort_callback.is_some());
        assert!(
            full_params(2, &[], language.as_ptr(), false)
                .abort_callback
                .is_none()
        );
    }

    #[test]
    fn the_finish_signal_sets_the_polled_flag() {
        SpeechEngine::enable_draft_interrupt().expect("the owned worker installs its signal");
        assert!(!stop());
        // SAFETY: raises the installed handler in this thread; it only stores
        // the flag this test reads.
        assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0);
        assert!(stop());
    }

    #[test]
    fn flash_attention_follows_the_reviewed_header() {
        let header = |state: u32, head: u32, layer: u32| {
            let mut header = [0_u8; 40];
            let mut put = |offset: usize, value: u32| {
                if let Some(field) = header.get_mut(offset..offset.wrapping_add(4)) {
                    field.copy_from_slice(&value.to_le_bytes());
                }
            };
            put(0, 0x6767_6d6c);
            put(12, state);
            put(16, head);
            put(20, layer);
            put(28, state);
            put(32, head);
            put(36, layer);
            header
        };
        assert!(bounded_cpu_flash(&header(512, 8, 6)));
        assert!(bounded_cpu_flash(&header(768, 12, 12)));
        assert!(!bounded_cpu_flash(&header(384, 6, 4)));
        assert!(!bounded_cpu_flash(&[0_u8; 40]));
    }
}
