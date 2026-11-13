//! One framed, password-wrapped ciphertext artifact.

use zeroize::Zeroize as _;

use crate::{Context, Label, PasswordParams, SymmetricKey, envelope};

/// The migration password policy: at least 15 characters, at most 4096 bytes.
#[must_use]
pub fn migration_password(password: &[u8]) -> bool {
    password.len() <= MAX_PASSWORD_BYTES
        && std::str::from_utf8(password)
            .is_ok_and(|text| text.chars().count() >= MIN_PASSWORD_CHARS)
}

pub const MIN_PASSWORD_CHARS: usize = 15;
/// Longest accepted migration password, in bytes.
pub const MAX_PASSWORD_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Malformed,
    Unauthenticated,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "password-wrapped artifact: {self:?}")
    }
}

impl std::error::Error for FrameError {}

/// The version-one framing and domains of one kind of password-wrapped
/// artifact.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    magic: &'static [u8; 8],
    wrap_domain: &'static [u8],
    max_wrapped: usize,
    max_content: usize,
}

impl Frame {
    pub const fn new(
        magic: &'static [u8; 8],
        wrap_domain: &'static [u8],
        max_wrapped: usize,
        max_content: usize,
    ) -> Self {
        Self {
            magic,
            wrap_domain,
            max_wrapped,
            max_content,
        }
    }

    /// Largest accepted framed artifact, in bytes.
    pub const fn max_len(&self) -> usize {
        self.max_wrapped
            .saturating_add(self.max_content)
            .saturating_add(16)
    }

    /// Wrap `key` under `password` for `binding` and frame `content`, which
    /// the caller already sealed under `key`.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for an out-of-bounds binding or content,
    /// [`FrameError::Unauthenticated`] when sealing fails.
    pub fn seal(
        &self,
        password: &[u8],
        binding: &[u8],
        key: &SymmetricKey,
        content: &[u8],
    ) -> Result<Vec<u8>, FrameError> {
        if binding.is_empty() || content.len() > self.max_content {
            return Err(FrameError::Malformed);
        }
        let seal_context = self.context(binding);
        let wrapped = envelope::seal_with_password(
            password,
            PasswordParams::DEFAULT,
            Context::new(Label::MigrationPackage, &seal_context),
            key.expose_secret(),
        )
        .map_err(|_| FrameError::Unauthenticated)?;
        let mut bytes = Vec::with_capacity(self.max_len());
        bytes.extend_from_slice(self.magic);
        bytes.extend_from_slice(
            &u32::try_from(wrapped.len())
                .map_err(|_| FrameError::Malformed)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&wrapped);
        bytes.extend_from_slice(
            &u32::try_from(content.len())
                .map_err(|_| FrameError::Malformed)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(content);
        Ok(bytes)
    }

    /// Check framing, bounds and key sources without deriving a key.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for non-canonical framing, bounds or key
    /// sources, [`FrameError::Unauthenticated`] for a non-envelope wrap.
    pub fn parse<'a>(&self, bytes: &'a [u8]) -> Result<(&'a [u8], &'a [u8]), FrameError> {
        if bytes.len() > self.max_len() || bytes.get(..8) != Some(self.magic) {
            return Err(FrameError::Malformed);
        }
        let wrapped_len = length(bytes, 8)?;
        if wrapped_len > self.max_wrapped {
            return Err(FrameError::Malformed);
        }
        let wrapped_end = 12_usize
            .checked_add(wrapped_len)
            .ok_or(FrameError::Malformed)?;
        let wrapped = bytes.get(12..wrapped_end).ok_or(FrameError::Malformed)?;
        let content_len = length(bytes, wrapped_end)?;
        let start = wrapped_end.checked_add(4).ok_or(FrameError::Malformed)?;
        if content_len > self.max_content || start.checked_add(content_len) != Some(bytes.len()) {
            return Err(FrameError::Malformed);
        }
        let parsed = envelope::Envelope::parse(wrapped).map_err(|_| FrameError::Unauthenticated)?;
        if !matches!(parsed.source, envelope::KeySource::Password { params, salt } if params == PasswordParams::DEFAULT && salt.len() == envelope::SALT_LEN)
        {
            return Err(FrameError::Malformed);
        }
        let content = bytes.get(start..).ok_or(FrameError::Malformed)?;
        if !matches!(
            envelope::Envelope::parse(content)
                .map_err(|_| FrameError::Unauthenticated)?
                .source,
            envelope::KeySource::Key(_)
        ) {
            return Err(FrameError::Malformed);
        }
        Ok((wrapped, content))
    }

    /// Open the wrap under `password` and `binding`, returning the content key
    /// and the still-sealed content.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for a non-canonical artifact or empty
    /// binding, [`FrameError::Unauthenticated`] for a wrong password, binding
    /// or changed bytes.
    pub fn open(
        &self,
        password: &[u8],
        binding: &[u8],
        bytes: &[u8],
    ) -> Result<(SymmetricKey, Vec<u8>), FrameError> {
        if binding.is_empty() {
            return Err(FrameError::Malformed);
        }
        let (wrapped, content) = self.parse(bytes)?;
        let seal_context = self.context(binding);
        let key = envelope::open_with_password(
            password,
            Context::new(Label::MigrationPackage, &seal_context),
            wrapped,
        )
        .map_err(|_| FrameError::Unauthenticated)?;
        let mut key: [u8; 32] = key
            .as_slice()
            .try_into()
            .map_err(|_| FrameError::Unauthenticated)?;
        let content_key = SymmetricKey::from_bytes(key);
        key.zeroize();
        Ok((content_key, content.to_vec()))
    }

    fn context(&self, binding: &[u8]) -> Vec<u8> {
        let mut context = self.wrap_domain.to_vec();
        context.extend_from_slice(binding);
        context
    }
}

fn length(bytes: &[u8], offset: usize) -> Result<usize, FrameError> {
    let end = offset.checked_add(4).ok_or(FrameError::Malformed)?;
    let bytes: [u8; 4] = bytes
        .get(offset..end)
        .ok_or(FrameError::Malformed)?
        .try_into()
        .map_err(|_| FrameError::Malformed)?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIND: Frame = Frame::new(b"TILOTST1", b"tilo-test-wrap/v1\0", 256, 4096);
    const PASSWORD: &[u8] = b"a test artifact password";
    const BINDING: &[u8] = b"operation-1";

    #[test]
    fn framing_authenticates_and_refuses_non_canonical_input() {
        let key = SymmetricKey::generate().unwrap();
        let content = envelope::seal(
            &key,
            Context::new(Label::MigrationPackage, b"tilo-test-content/v1\0"),
            b"selected content",
        )
        .unwrap();
        let bytes = KIND.seal(PASSWORD, BINDING, &key, &content).unwrap();
        let (opened, sealed) = KIND.open(PASSWORD, BINDING, &bytes).unwrap();
        let plain = envelope::open(
            &opened,
            Context::new(Label::MigrationPackage, b"tilo-test-content/v1\0"),
            &sealed,
        )
        .unwrap();
        assert_eq!(&*plain, b"selected content");
        assert_eq!(opened.key_id(), key.key_id());
        for (password, binding) in [
            (b"another test password".as_slice(), BINDING),
            (PASSWORD, b"operation-2".as_slice()),
        ] {
            assert_eq!(
                KIND.open(password, binding, &bytes),
                Err(FrameError::Unauthenticated)
            );
        }
        assert_eq!(KIND.open(PASSWORD, b"", &bytes), Err(FrameError::Malformed));
        let mut expensive = bytes.clone();
        expensive[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        for changed in [
            expensive,
            bytes[..bytes.len() - 1].to_vec(),
            [&bytes[..], &[0]].concat(),
        ] {
            assert_eq!(KIND.parse(&changed), Err(FrameError::Malformed));
        }
        assert!(!migration_password(b"fourteen chars"));
        assert!(migration_password(b"fifteen chars!!"));
        assert!(!migration_password(&[0xff; 15]));
    }
}
