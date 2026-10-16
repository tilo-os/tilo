//! A value's postcard encoding counted, compared and hashed as it is
//! produced, without materializing the bytes.

use serde::Serialize;

/// The bytes `value` encodes to.
///
/// # Errors
///
/// [`postcard::Error::SerializeBufferFull`] once the encoding passes `limit`,
/// or the value's own encoding error.
pub(crate) fn encoded_len<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> postcard::Result<usize> {
    postcard::serialize_with_flavor(
        value,
        Feed {
            len: 0,
            limit,
            hasher: None,
        },
    )
}

/// Feeds `value`'s encoding into `hasher` and returns its length.
///
/// # Errors
///
/// As [`encoded_len`]; `hasher` may then hold a prefix of the encoding.
pub(crate) fn hash_into<T: Serialize + ?Sized>(
    value: &T,
    hasher: &mut blake3::Hasher,
    limit: usize,
) -> postcard::Result<usize> {
    postcard::serialize_with_flavor(
        value,
        Feed {
            len: 0,
            limit,
            hasher: Some(hasher),
        },
    )
}

/// Whether `value` encodes to exactly `bytes`. No digest substitutes for
/// byte equality.
pub(crate) fn encodes_as<T: Serialize + ?Sized>(value: &T, bytes: &[u8]) -> bool {
    postcard::serialize_with_flavor(value, Compare(bytes)).is_ok()
}

struct Feed<'a> {
    len: usize,
    limit: usize,
    hasher: Option<&'a mut blake3::Hasher>,
}
impl postcard::ser_flavors::Flavor for Feed<'_> {
    type Output = usize;
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.len = self
            .len
            .checked_add(bytes.len())
            .filter(|len| *len <= self.limit)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        if let Some(hasher) = &mut self.hasher {
            hasher.update(bytes);
        }
        Ok(())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.len)
    }
}

struct Compare<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Compare<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.0 = self
            .0
            .strip_prefix(bytes)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(postcard::Error::SerializeBufferFull)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;

    #[test]
    fn counts_compares_and_hashes_the_exact_encoding() {
        let value = ("room", 7u64, [1u8; 4]);
        let bytes = postcard::to_allocvec(&value).unwrap();
        assert_eq!(encoded_len(&value, usize::MAX).unwrap(), bytes.len());
        assert!(encoded_len(&value, bytes.len() - 1).is_err());
        assert!(encodes_as(&value, &bytes));
        assert!(!encodes_as(&value, &bytes[..bytes.len() - 1]));
        assert!(!encodes_as(&value, &[bytes.as_slice(), &[0]].concat()));
        let mut hasher = blake3::Hasher::new();
        assert_eq!(
            hash_into(&value, &mut hasher, usize::MAX).unwrap(),
            bytes.len()
        );
        assert_eq!(hasher.finalize(), blake3::hash(&bytes));
    }
}
