//! Isolated native qualification on the real speech pipe. Never an installed
//! worker or catalogue authority. Build without Whisper; reviewed native link
//! inputs are supplied explicitly by the remote qualification build.
#[cfg(feature = "whisper")]
compile_error!(
    "native qualification requires --no-default-features; never link both ggml generations"
);

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{CString, c_void};
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::PathBuf;
    use std::ptr::NonNull;

    use clap::Parser;
    use local_model_service::speech::{
        DraftPolicy, MAX_TEXT_BYTES, SpeechConfig, SpeechError, SpeechReply, worker,
    };
    use zeroize::Zeroizing;

    const MODEL_BYTES: u64 = 742_090_464;
    // Measured only after the fixed raw SHA256
    // 3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1
    // passed on this same immutable file. This is a private review identity,
    // never a catalogue receipt or permission to select another model.
    const MODEL_VERITY_SHA: &str =
        "76850de5f80310fa2e909a33ba93958c97b0437984c44ec0b90f7170b0dbdf73";
    const MODEL_REVISION: &str = "ea30d66debe3740a08b573244286791d423d6b3e";

    #[derive(Parser)]
    struct Cli {
        #[arg(long)]
        model: PathBuf,
        /// Emit bounded non-content timing counters in isolated review only.
        #[arg(long)]
        phase_review: bool,
        /// Completed sessions in one private review worker; never an idle daemon.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
        review_sessions: u8,
        /// Pinned model trained contexts; default preserves existing geometry.
        #[arg(long, default_value_t = 1, value_parser = parse_right_context)]
        review_right_context: u8,
    }
    fn parse_right_context(value: &str) -> Result<u8, String> {
        match value {
            "0" => Ok(0),
            "1" => Ok(1),
            "3" => Ok(3),
            _ => Err("review requires a trained context: 0, 1 or 3".into()),
        }
    }
    #[repr(C)]
    struct NativeConfig {
        descriptor: i32,
        threads: i32,
        right_context: i32,
        language: *const std::ffi::c_char,
        vocabulary: *const std::ffi::c_char,
    }
    unsafe extern "C" {
        fn tilo_nemo_load(config: *const NativeConfig, status: *mut i32) -> *mut c_void;
        fn tilo_nemo_reset(
            context: *mut c_void,
            language: *const std::ffi::c_char,
            vocabulary: *const std::ffi::c_char,
        ) -> i32;
        fn tilo_nemo_push(context: *mut c_void, samples: *const f32, count: usize) -> i32;
        fn tilo_nemo_text(
            context: *mut c_void,
            complete: bool,
            bytes: *mut u8,
            capacity: usize,
        ) -> i32;
        fn tilo_nemo_free(context: *mut c_void);
    }

    // This witness is private to a fixed review receipt. It cannot manufacture
    // catalogue authority, admit a different size, or install a model.
    struct ImmutableReviewModel(File);
    impl ImmutableReviewModel {
        fn open(path: &std::path::Path) -> Result<Self, SpeechError> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(path)
                .map_err(|_| SpeechError::ModelUnavailable)?;
            let metadata = file.metadata().map_err(|_| SpeechError::ModelReceipt)?;
            if !metadata.is_file() || metadata.len() != MODEL_BYTES {
                return Err(SpeechError::ModelReceipt);
            }
            // FS_IOC_MEASURE_VERITY: fsverity_digest header plus a 64-byte
            // capacity. SHA256 (algorithm1, size32) is required. This query
            // never enables verity or changes a filesystem/permission.
            let mut verity = [0_u8; 68];
            verity
                .get_mut(2..4)
                .ok_or(SpeechError::ModelReceipt)?
                .copy_from_slice(&64_u16.to_ne_bytes());
            // SAFETY: live owned fd and correctly sized writable digest buffer.
            let measured =
                unsafe { libc::ioctl(file.as_raw_fd(), 0xc004_6686, verity.as_mut_ptr()) };
            if measured != 0
                || verity.get(..4)
                    != Some(
                        [1_u16.to_ne_bytes(), 32_u16.to_ne_bytes()]
                            .concat()
                            .as_slice(),
                    )
            {
                return Err(SpeechError::ModelReceipt);
            }
            // Authenticate the exact expected digest, not just the presence
            // of verity. The kernel verifies pages on every read/mmap; this
            // descriptor remains the native reader's immutable anchor.
            // Rehashing the complete object on each session is unnecessary.
            if hex::encode(verity.get(4..36).ok_or(SpeechError::ModelReceipt)?) != MODEL_VERITY_SHA
            {
                return Err(SpeechError::ModelReceipt);
            }
            Ok(Self(file))
        }
    }

    struct NativeDecoder(NonNull<c_void>);
    impl NativeDecoder {
        fn load(
            model: &ImmutableReviewModel,
            config: &SpeechConfig,
            right_context: u8,
        ) -> Result<Self, SpeechError> {
            let (language, words) = Self::spellings(config)?;
            let native = NativeConfig {
                descriptor: model.0.as_raw_fd(),
                threads: i32::try_from(
                    std::thread::available_parallelism().map_or(1, |count| count.get().min(2)),
                )
                .map_err(|_| SpeechError::WorkerStopped)?,
                right_context: i32::from(right_context),
                language: language.as_ptr(),
                vocabulary: words.as_ptr().cast(),
            };
            let mut status = -3;
            // SAFETY: exclusive immutable model fd and valid NUL strings live
            // through synchronous load; native code copies request state and
            // catches exceptions. No callback or Rust owner crosses the ABI.
            let pointer = unsafe { tilo_nemo_load(&raw const native, &raw mut status) };
            NonNull::new(pointer).map(Self).ok_or(if status == -2 {
                SpeechError::UnsupportedLanguage
            } else {
                SpeechError::Recognition
            })
        }
        fn spellings(config: &SpeechConfig) -> Result<(CString, Zeroizing<Vec<u8>>), SpeechError> {
            if config.language.is_empty()
                || config.language.len() > 32
                || config.vocabulary.len() > 256
            {
                return Err(SpeechError::Protocol);
            }
            let language = CString::new(config.language.as_str())
                .map_err(|_| SpeechError::UnsupportedLanguage)?;
            let mut words = Zeroizing::new(Vec::new());
            for hint in &config.vocabulary {
                if hint.is_empty()
                    || hint.chars().count() > 128
                    || !hint.chars().any(char::is_alphabetic)
                    || hint.chars().any(|ch| ch.is_control() || ch.is_whitespace())
                    || hint.contains("<|")
                    || hint.contains("|>")
                {
                    return Err(SpeechError::Protocol);
                }
                if words.len().saturating_add(hint.len()).saturating_add(1) > MAX_TEXT_BYTES {
                    return Err(SpeechError::Protocol);
                }
                words.extend_from_slice(hint.as_bytes());
                words.push(b'\n');
            }
            words.push(0);
            Ok((language, words))
        }
        fn reset(&mut self, config: &SpeechConfig) -> Result<(), SpeechError> {
            let (language, words) = Self::spellings(config)?;
            // SAFETY: live exclusive context and validated NUL strings remain
            // valid through synchronous reset. Native code accepts only a
            // completed session and creates fresh decoder/context state.
            match unsafe {
                tilo_nemo_reset(self.0.as_ptr(), language.as_ptr(), words.as_ptr().cast())
            } {
                0 => Ok(()),
                -2 => Err(SpeechError::UnsupportedLanguage),
                _ => Err(SpeechError::Recognition),
            }
        }
        fn text(&mut self, complete: bool) -> Result<String, SpeechError> {
            let mut output = Zeroizing::new(vec![0_u8; MAX_TEXT_BYTES]);
            // SAFETY: exclusively owned context, synchronous call and exactly
            // bounded writable output; exceptions stay inside C++.
            let result = unsafe {
                tilo_nemo_text(self.0.as_ptr(), complete, output.as_mut_ptr(), output.len())
            };
            let length = usize::try_from(result).map_err(|_| {
                if result == -4 {
                    SpeechError::TextTooLong
                } else {
                    SpeechError::Recognition
                }
            })?;
            let text = std::str::from_utf8(output.get(..length).ok_or(SpeechError::TextTooLong)?)
                .map_err(|_| SpeechError::Recognition)?;
            if text
                .chars()
                .any(|ch| ch.is_control() && !ch.is_whitespace())
            {
                return Err(SpeechError::Recognition);
            }
            Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
        }
    }
    impl worker::SpeechDecoder for NativeDecoder {
        fn draft_policy(&self) -> DraftPolicy {
            DraftPolicy::Incremental
        }
        fn push(&mut self, samples: &[f32]) -> Result<(), SpeechError> {
            // SAFETY: live exclusive context and borrowed PCM remain valid for
            // synchronous copying; the shared protocol already bounds audio.
            if unsafe { tilo_nemo_push(self.0.as_ptr(), samples.as_ptr(), samples.len()) } == 0 {
                Ok(())
            } else {
                Err(SpeechError::Recognition)
            }
        }
        fn draft(&mut self, _samples: &[f32]) -> Result<String, SpeechError> {
            self.text(false)
        }
        fn finish(&mut self, _samples: &[f32]) -> Result<String, SpeechError> {
            self.text(true)
        }
    }
    impl Drop for NativeDecoder {
        fn drop(&mut self) {
            // SAFETY: exactly this owner releases the context exactly once.
            unsafe { tilo_nemo_free(self.0.as_ptr()) };
        }
    }

    pub fn main() -> std::process::ExitCode {
        let args = Cli::parse();
        let input = std::io::stdin();
        let output = std::io::stdout();
        let mut input = input.lock();
        let mut output = output.lock();
        let result = (|| {
            // SAFETY: fresh single-threaded owned child; no descriptor owners
            // beyond the standard pipes exist before prepare closes them.
            unsafe {
                worker::prepare()?;
            }
            // SAFETY: argument-free identity queries cannot fail.
            if unsafe { libc::geteuid() == 0 || libc::geteuid() != libc::getuid() } {
                return Err(SpeechError::ModelUnavailable);
            }
            let verify_started = std::time::Instant::now();
            let model = ImmutableReviewModel::open(&args.model)?;
            let verify_ms = verify_started.elapsed().as_secs_f64() * 1000.0;
            let (session, config) = worker::configure(&mut input)?;
            local_model_service::speech::confine_worker()?;
            let native_started = std::time::Instant::now();
            let mut decoder = NativeDecoder::load(&model, &config, args.review_right_context)?;
            let native_ms = native_started.elapsed().as_secs_f64() * 1000.0;
            if args.phase_review {
                eprintln!(
                    "NATIVE_PREPARATION {{\"verify_ms\":{verify_ms},\"native_ms\":{native_ms}}}"
                );
            }
            drop(model);
            drop(config);
            worker::serve_decoder(
                ("nemotron-q8-review", MODEL_REVISION),
                session,
                &mut decoder,
                &mut input,
                &mut output,
            )?;
            // This bounded private review process exits after at most two
            // completed sessions. Cancellation never enters this reuse path.
            for _ in 1..args.review_sessions {
                let (session, config) = worker::configure(&mut input)?;
                let reset_started = std::time::Instant::now();
                decoder.reset(&config)?;
                let native_ms = reset_started.elapsed().as_secs_f64() * 1000.0;
                drop(config);
                if args.phase_review {
                    eprintln!("NATIVE_PREPARATION {{\"verify_ms\":0,\"native_ms\":{native_ms}}}");
                }
                worker::serve_decoder(
                    ("nemotron-q8-review", MODEL_REVISION),
                    session,
                    &mut decoder,
                    &mut input,
                    &mut output,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = worker::reply(&mut output, &SpeechReply::Error { sequence: 0, error });
            std::process::ExitCode::FAILURE
        } else {
            std::process::ExitCode::SUCCESS
        }
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        linux::main()
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::process::ExitCode::FAILURE
    }
}
