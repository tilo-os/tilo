//! Default-off image AOT runtime. Weights cannot supply executable code.
use super::{
    MAX_TEXT_BYTES, SealedSpeechModel, SpeechConfig, SpeechError, SpeechRecognition, worker,
};
use sha2::{Digest as _, Sha256};
use std::ffi::{CString, c_void};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::ptr::NonNull;
use zeroize::Zeroizing;

const IMAGE: &str = "/usr/lib64/tilo-speech/whistle/x86-64-v3";
const OBJECTS: [(&str, usize, &str); 3] = [
    (
        "libwasmtime.so",
        17_465_504,
        "2d232d8bc62ece8ded8179cbb354f17729dadfdf04ff018a96cbbdaf075433dd",
    ),
    (
        "aot-bridge.so",
        36_544,
        "d628553aea35bf059b9e30676c123868193aee8e59b1783f43028690f22abe34",
    ),
    (
        "needle.aot",
        3_437_656,
        "a74daea810fcccb4053ff8639f7bae9b7010fd9d9e1ad80a0deecc17c2cb3d26",
    ),
];
const MODEL_SIZE: usize = 16_919_407;
const MODEL_SHA: &str = "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb";
type Create =
    unsafe extern "C" fn(*const u8, usize, *const u8, usize, *const u8, usize) -> *mut c_void;
type Call = unsafe extern "C" fn(*mut c_void, i32, *const f32, usize, *mut u8, usize) -> i32;
type Free = unsafe extern "C" fn(*mut c_void);

fn cpu_compatible() -> bool {
    #[cfg(target_arch = "x86_64")]
    return std::is_x86_feature_detected!("avx2")
        && std::is_x86_feature_detected!("avx")
        && std::is_x86_feature_detected!("fma")
        && std::is_x86_feature_detected!("bmi1")
        && std::is_x86_feature_detected!("bmi2")
        && std::is_x86_feature_detected!("lzcnt")
        && std::is_x86_feature_detected!("sse3")
        && std::is_x86_feature_detected!("ssse3")
        && std::is_x86_feature_detected!("sse4.1")
        && std::is_x86_feature_detected!("sse4.2")
        && std::is_x86_feature_detected!("popcnt")
        && std::is_x86_feature_detected!("cmpxchg16b");
    #[cfg(not(target_arch = "x86_64"))]
    false
}

fn image_object(path: &Path, size: usize, digest: &str) -> Result<Zeroizing<Vec<u8>>, SpeechError> {
    // Open every parent without following links. Unprivileged accounts cannot
    // replace entries anywhere on this fixed image-owned path.
    for parent in path.ancestors().skip(1) {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|_| SpeechError::ModelReceipt)?;
        let metadata = file.metadata().map_err(|_| SpeechError::ModelReceipt)?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(SpeechError::ModelReceipt);
        }
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| SpeechError::ModelReceipt)?;
    let metadata = file.metadata().map_err(|_| SpeechError::ModelReceipt)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() != size as u64
    {
        return Err(SpeechError::ModelReceipt);
    }
    let mut bytes = Zeroizing::new(vec![0; size]);
    file.read_exact(&mut bytes)
        .map_err(|_| SpeechError::ModelReceipt)?;
    if hex::encode(Sha256::digest(&bytes)) != digest {
        return Err(SpeechError::ModelReceipt);
    }
    Ok(bytes)
}

fn configuration(config: &SpeechConfig) -> Result<Zeroizing<Vec<u8>>, SpeechError> {
    if !["auto", "en", "de", "fr", "es", "it", "nl", "pl"].contains(&config.language.as_str()) {
        return Err(SpeechError::UnsupportedLanguage);
    }
    if config.vocabulary.len() > 256 {
        return Err(SpeechError::Protocol);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    bytes.extend_from_slice(config.language.as_bytes());
    bytes.push(0);
    let mut total = 0_usize;
    for (index, word) in config.vocabulary.iter().enumerate() {
        total = total
            .checked_add(word.len().saturating_add(1))
            .ok_or(SpeechError::Protocol)?;
        if word.is_empty()
            || word.chars().count() > 128
            || total > 16 * 1024
            || word.chars().any(|c| c.is_control() || c.is_whitespace())
            || word.contains("<|")
            || word.contains("|>")
        {
            return Err(SpeechError::Protocol);
        }
        if index != 0 {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(word.as_bytes());
    }
    bytes.push(0);
    Ok(bytes)
}

struct Library(NonNull<c_void>);
impl Library {
    fn symbol(&self, name: &std::ffi::CStr) -> Result<NonNull<c_void>, SpeechError> {
        // SAFETY: live root-owned, exact-digest library and a static NUL symbol.
        NonNull::new(unsafe { libc::dlsym(self.0.as_ptr(), name.as_ptr()) })
            .ok_or(SpeechError::ModelReceipt)
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: decoder retirement precedes this exclusively owned handle.
        unsafe {
            libc::dlclose(self.0.as_ptr());
        }
    }
}

struct Native<'a> {
    pointer: NonNull<c_void>,
    call: Call,
    free: Free,
    _library: &'a Library,
}
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
struct NativeReply {
    text: String,
}
impl Native<'_> {
    fn transcribe(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        // Production segmentation keeps every complete-context call below the
        // engine's 30-second limit without lowering the 120-second session cap.
        if samples.len() > 30 * super::SAMPLE_RATE {
            return Err(SpeechError::AudioTooLong);
        }
        let mut bytes = Zeroizing::new(vec![0_u8; MAX_TEXT_BYTES + 1]);
        // SAFETY: exact source-pinned ABI, exclusive live pointer and valid
        // bounded slices. Operation 2 is the qualified full-context decoder.
        let length = unsafe {
            (self.call)(
                self.pointer.as_ptr(),
                2,
                samples.as_ptr(),
                samples.len(),
                bytes.as_mut_ptr(),
                bytes.len(),
            )
        };
        let length = usize::try_from(length).map_err(|_| SpeechError::Recognition)?;
        let data = bytes
            .get(..length)
            .filter(|_| length <= MAX_TEXT_BYTES)
            .ok_or(SpeechError::TextTooLong)?;
        let reply: NativeReply =
            serde_json::from_slice(data).map_err(|_| SpeechError::Recognition)?;
        if reply.text.len() > MAX_TEXT_BYTES {
            return Err(SpeechError::TextTooLong);
        }
        Ok(reply.text.clone())
    }
}
impl Drop for Native<'_> {
    fn drop(&mut self) {
        // SAFETY: matching destroy function; borrowing library is still live.
        unsafe {
            (self.free)(self.pointer.as_ptr());
        }
    }
}
struct Decoder<'a> {
    native: Native<'a>,
    recognition: SpeechRecognition,
}
impl worker::SpeechDecoder for Decoder<'_> {
    fn push(&mut self, _: &[f32]) -> Result<(), SpeechError> {
        Ok(())
    }
    fn draft(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        self.recognition
            .recognize_with(samples, false, |chunk| self.native.transcribe(chunk))
    }
    fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        self.recognition
            .recognize_with(samples, true, |chunk| self.native.transcribe(chunk))
    }
}

pub(super) fn serve(
    model: SealedSpeechModel,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), SpeechError> {
    if !cpu_compatible() || model.size() != MODEL_SIZE as u64 {
        return Err(SpeechError::ModelUnavailable);
    }
    model.check_seals()?;
    let mut weights = Zeroizing::new(vec![0; MODEL_SIZE]);
    model
        .file()
        .read_exact(&mut weights)
        .map_err(|_| SpeechError::ModelReceipt)?;
    if hex::encode(Sha256::digest(&weights)) != MODEL_SHA {
        return Err(SpeechError::ModelReceipt);
    }
    let mut artifact = None;
    for (name, size, digest) in OBJECTS {
        let bytes = image_object(&Path::new(IMAGE).join(name), size, digest)?;
        if name == "needle.aot" {
            artifact = Some(bytes);
        }
    }
    let artifact = artifact.ok_or(SpeechError::ModelReceipt)?;
    let path =
        CString::new(format!("{IMAGE}/aot-bridge.so")).map_err(|_| SpeechError::ModelReceipt)?;
    // SAFETY: fixed immutable image path, root-owned parents and exact native
    // digests. The production owner starts this child with a cleared environment.
    // $ORIGIN dependencies are the adjacent verified image runtime, never weights.
    let library = Library(
        NonNull::new(unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) })
            .ok_or(SpeechError::ModelReceipt)?,
    );
    // SAFETY: these exact signatures belong to the authenticated bridge.
    let create: Create =
        unsafe { std::mem::transmute(library.symbol(c"tilo_whistle_review_new")?.as_ptr()) };
    // SAFETY: matching exact native ABI, library remains borrowed by Native.
    let call: Call =
        unsafe { std::mem::transmute(library.symbol(c"tilo_whistle_review_call")?.as_ptr()) };
    // SAFETY: matching destruction entry point for the same source-pinned ABI.
    let free: Free =
        unsafe { std::mem::transmute(library.symbol(c"tilo_whistle_review_free")?.as_ptr()) };
    let (session, config) = worker::configure(input)?;
    let identity = (model.id().to_owned(), model.revision().to_owned());
    let bytes = configuration(&config)?;
    super::confine_worker()?;
    // SAFETY: deserialization/model preparation occur only after confinement;
    // exact pinned artifact and sealed catalogue weights remain live for the call.
    let pointer = NonNull::new(unsafe {
        create(
            artifact.as_ptr(),
            artifact.len(),
            weights.as_ptr(),
            weights.len(),
            bytes.as_ptr(),
            bytes.len(),
        )
    })
    .ok_or(SpeechError::Recognition)?;
    drop(artifact);
    drop(weights);
    drop(bytes);
    drop(config);
    drop(model);
    let mut decoder = Decoder {
        native: Native {
            pointer,
            call,
            free,
            _library: &library,
        },
        recognition: SpeechRecognition::default(),
    };
    worker::serve_decoder(
        (&identity.0, &identity.1),
        session,
        &mut decoder,
        input,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_configuration_preserves_unicode_spellings_and_rejects_control_tokens() {
        let mut config = SpeechConfig {
            language: "fr".into(),
            vocabulary: vec!["Élodie".into(), "Straße".into()],
        };
        assert_eq!(
            configuration(&config).unwrap().as_slice(),
            "fr\0Élodie\nStraße\0".as_bytes()
        );
        for invalid in [
            "",
            "two words",
            "line\nbreak",
            "nul\0byte",
            "<|prompt",
            "prompt|>",
        ] {
            config.vocabulary = vec![invalid.into()];
            assert!(matches!(configuration(&config), Err(SpeechError::Protocol)));
        }
        config.vocabulary.clear();
        config.language = "unavailable".into();
        assert!(matches!(
            configuration(&config),
            Err(SpeechError::UnsupportedLanguage)
        ));
    }

    #[test]
    fn native_configuration_enforces_word_count_character_and_byte_limits() {
        let mut config = SpeechConfig {
            language: "auto".into(),
            vocabulary: vec!["word".into(); 256],
        };
        assert!(configuration(&config).is_ok());
        config.vocabulary.push("extra".into());
        assert!(matches!(configuration(&config), Err(SpeechError::Protocol)));
        config.vocabulary = vec!["é".repeat(128)];
        assert!(configuration(&config).is_ok());
        config.vocabulary = vec!["é".repeat(129)];
        assert!(matches!(configuration(&config), Err(SpeechError::Protocol)));
        config.vocabulary = vec!["é".repeat(128); 64];
        assert!(matches!(configuration(&config), Err(SpeechError::Protocol)));
    }
}
