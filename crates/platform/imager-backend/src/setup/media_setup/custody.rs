//! Join archive-policy review to independently authenticated backend custody.
//! The UI cannot manufacture a backend handle from its review or public facts.
use super::*;

pub(super) fn hold(
    reviewed: &ImportReview,
    path: &Path,
    password: &[u8],
    cancel: &AtomicBool,
) -> io::Result<PreparedMediaHandle> {
    let held = PreparedMediaHandle::review(path, password, cancel)?;
    if held.info().sealed_bytes != reviewed.info().sealed_bytes
        || held.ciphertext_digest() != reviewed.ciphertext_digest()
    {
        return Err(changed(
            "Prepared ciphertext changed between archive and custody review",
        ));
    }
    Ok(held)
}
