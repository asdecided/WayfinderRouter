//! Authenticated macOS XPC credential-broker client.
//!
//! Unsafe code is confined to the single C ABI call in this crate. Callers see
//! only validated references, sanitized errors, bounded bytes, and the shared
//! non-serializable redacted secret type.

use std::time::Duration;

use thiserror::Error;
use wayfinder_providers::openai_compat::SecretValue;

/// Maximum accepted environment/account reference.
pub const MAX_ACCOUNT_BYTES: usize = 256;
/// Maximum credential reply retained in Rust.
pub const MAX_SECRET_BYTES: usize = 16 * 1_024;
/// Default broker round-trip deadline.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Sanitized broker failure. No variant stores a reference or secret.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum XpcCredentialError {
    /// This binary was not built for macOS.
    #[error("XPC credential broker is unavailable on this platform")]
    UnsupportedPlatform,
    /// The account was empty, oversized, or outside the environment-name grammar.
    #[error("XPC credential reference is invalid")]
    InvalidReference,
    /// The timeout was zero or could not be represented safely.
    #[error("XPC credential timeout is invalid")]
    InvalidTimeout,
    /// The requested Keychain item does not exist.
    #[error("XPC credential is missing")]
    Missing,
    /// The caller was rejected or Keychain access was denied.
    #[error("XPC credential access was denied")]
    Denied,
    /// The broker did not answer before the deadline.
    #[error("XPC credential broker timed out")]
    TimedOut,
    /// The broker or connection was unavailable.
    #[error("XPC credential broker is unavailable")]
    Unavailable,
    /// The reply exceeded the fixed secret bound.
    #[error("XPC credential reply exceeded its bound")]
    ReplyTooLarge,
    /// The broker returned an empty reply.
    #[error("XPC credential broker returned an empty value")]
    Empty,
}

/// Resolve one exact `wayfinder-router` Keychain account through the bundled broker.
pub fn resolve_xpc_credential(account: &str) -> Result<SecretValue, XpcCredentialError> {
    resolve_xpc_credential_with_timeout(account, DEFAULT_TIMEOUT)
}

/// Resolve with an explicit finite deadline.
pub fn resolve_xpc_credential_with_timeout(
    account: &str,
    timeout: Duration,
) -> Result<SecretValue, XpcCredentialError> {
    validate(account, timeout)?;
    platform::resolve(account, timeout)
}

fn validate(account: &str, timeout: Duration) -> Result<(), XpcCredentialError> {
    if account.is_empty()
        || account.len() > MAX_ACCOUNT_BYTES
        || !account.is_ascii()
        || !account.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_uppercase() || byte == b'_' || (index > 0 && byte.is_ascii_digit())
        })
    {
        return Err(XpcCredentialError::InvalidReference);
    }
    if timeout.is_zero() || !timeout.as_secs_f64().is_finite() {
        return Err(XpcCredentialError::InvalidTimeout);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_double, c_int};

    use super::*;

    unsafe extern "C" {
        fn wayfinder_xpc_resolve(
            account: *const c_char,
            output: *mut u8,
            capacity: usize,
            output_length: *mut usize,
            timeout_seconds: c_double,
        ) -> c_int;
    }

    pub(super) fn resolve(
        account: &str,
        timeout: Duration,
    ) -> Result<SecretValue, XpcCredentialError> {
        let account = CString::new(account).map_err(|_| XpcCredentialError::InvalidReference)?;
        let mut bytes = vec![0_u8; MAX_SECRET_BYTES];
        let mut length = 0_usize;
        // SAFETY: the input is NUL-terminated; the output allocation is exactly
        // `capacity` bytes; `length` is writable; and the bridge copies at most
        // the supplied capacity before returning synchronously.
        let status = unsafe {
            wayfinder_xpc_resolve(
                account.as_ptr(),
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut length,
                timeout.as_secs_f64(),
            )
        };
        let error = match status {
            0 => None,
            1 => Some(XpcCredentialError::Missing),
            2 => Some(XpcCredentialError::Denied),
            3 => Some(XpcCredentialError::TimedOut),
            5 => Some(XpcCredentialError::ReplyTooLarge),
            _ => Some(XpcCredentialError::Unavailable),
        };
        if let Some(error) = error {
            bytes.fill(0);
            return Err(error);
        }
        if length == 0 {
            bytes.fill(0);
            return Err(XpcCredentialError::Empty);
        }
        if length > bytes.len() {
            bytes.fill(0);
            return Err(XpcCredentialError::ReplyTooLarge);
        }
        bytes.truncate(length);
        Ok(SecretValue::from_bytes(bytes))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn resolve(
        _account: &str,
        _timeout: Duration,
    ) -> Result<SecretValue, XpcCredentialError> {
        Err(XpcCredentialError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_and_timeouts_are_bounded_before_ipc() {
        for invalid in ["", "lower", "1START", "HAS-DASH", "WHITE SPACE"] {
            assert_eq!(
                validate(invalid, DEFAULT_TIMEOUT),
                Err(XpcCredentialError::InvalidReference)
            );
        }
        assert!(validate("OPENAI_API_KEY", DEFAULT_TIMEOUT).is_ok());
        assert_eq!(
            validate("OPENAI_API_KEY", Duration::ZERO),
            Err(XpcCredentialError::InvalidTimeout)
        );
        assert_eq!(
            validate(&"A".repeat(MAX_ACCOUNT_BYTES + 1), DEFAULT_TIMEOUT),
            Err(XpcCredentialError::InvalidReference)
        );
    }

    #[test]
    fn errors_are_reference_and_secret_free() {
        for error in [
            XpcCredentialError::Missing,
            XpcCredentialError::Denied,
            XpcCredentialError::TimedOut,
            XpcCredentialError::Unavailable,
        ] {
            let rendered = error.to_string();
            assert!(!rendered.contains("OPENAI_API_KEY"));
            assert!(!rendered.contains("dummy-provider-secret"));
        }
    }
}
