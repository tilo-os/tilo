//! Bounded standard Draft 21 authorization tokens and Cloud's Type 0 emission.
use crate::{DecodeError, Error, coding::Decode, ietf};

/// Maximum opaque application authorization payload, excluding the standard Token header.
pub const MAX_AUTHORIZATION_BYTES: usize = 8192;

/// Maximum structurally parsed SETUP tokens in the bounded admission profile.
pub const MAX_AUTHORIZATION_TOKENS: usize = 16;

/// A structurally valid standard authorization token. The host verifies its value.
#[derive(Clone)]
pub struct AuthorizationToken {
	token_type: u64,
	value: Vec<u8>,
}
impl AuthorizationToken {
	/// Standard Token Type; zero denotes an out-of-band application agreement.
	pub fn token_type(&self) -> u64 {
		self.token_type
	}
	/// Opaque bytes, never interpreted as an application grant by this library.
	pub fn value(&self) -> &[u8] {
		&self.value
	}
}
impl std::fmt::Debug for AuthorizationToken {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("AuthorizationToken")
			.field("token_type", &self.token_type)
			.field("value_len", &self.value.len())
			.finish()
	}
}

pub(crate) fn encode(value: &[u8]) -> Result<Vec<u8>, Error> {
	if value.len() > MAX_AUTHORIZATION_BYTES {
		return Err(DecodeError::MessageTooLarge {
			size: value.len(),
			max: MAX_AUTHORIZATION_BYTES,
		}
		.into());
	}
	// Draft 21 sections 8.9 and 9.1.4: USE_VALUE, out-of-band Type 0,
	// then the opaque value to the end of the option (no inner length).
	let mut token = Vec::with_capacity(value.len() + 2);
	token.extend_from_slice(&[3, 0]);
	token.extend_from_slice(value);
	Ok(token)
}

pub(crate) fn decode(token: &[u8], version: ietf::Version, remaining: usize) -> Result<AuthorizationToken, Error> {
	if version != ietf::Version::Draft21 {
		return Err(Error::Version);
	}
	let mut value = token;
	let alias_type = u64::decode(&mut value, version)?;

	match alias_type {
		0 | 2 => {
			// Decode the full structure first, then apply SETUP's stricter rule.
			let _alias = u64::decode(&mut value, version)?;
			if !value.is_empty() {
				return Err(DecodeError::ExpectedEnd.into());
			}
			return Err(Error::ProtocolViolation);
		}
		1 => {
			// MAX_AUTH_TOKEN_CACHE_SIZE defaults to zero. Draft 21 requires a
			// REGISTER that cannot fit to behave as USE_VALUE during SETUP.
			let _alias = u64::decode(&mut value, version)?;
		}
		3 => {}
		_ => return Err(DecodeError::InvalidValue.into()),
	}

	let token_type = u64::decode(&mut value, version)?;
	let max = remaining.min(MAX_AUTHORIZATION_BYTES);
	if value.len() > max {
		return Err(DecodeError::MessageTooLarge { size: value.len(), max }.into());
	}
	Ok(AuthorizationToken {
		token_type,
		value: value.to_vec(),
	})
}
