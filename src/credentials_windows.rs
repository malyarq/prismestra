// SPDX-License-Identifier: GPL-2.0-or-later
//! Home Assistant token storage in the current user's Windows Credential Manager.
//! Requires windows-sys 0.61 features Win32_Security_Credentials and
//! Win32_Foundation; register this module only on Windows.
//!
//! Independent implementation following Microsoft documentation (2026-11-08):
//! https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew
//! https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreadw
//! https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-creddeletew
//! https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw
//!
//! Generic credentials persist for this user on this PC, without enterprise
//! roaming. Windows targets are case-insensitive; callers should generate stable
//! opaque IDs, preferably lowercase UUIDs, and store only IDs in local bindings.
//! No enumeration, alternate target prefix, file fallback or credential logging.
//! Returned Strings and caller-provided tokens remain ordinary process memory;
//! callers must not log them. Credential Manager is not isolation from other
//! processes running as the same Windows user.

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
    CredDeleteW, CredFree, CredReadW, CredWriteW,
};

const TARGET_PREFIX: &str = "Prismestra/HomeAssistant/v1/";
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_TOKEN_BYTES: usize = CRED_MAX_CREDENTIAL_BLOB_SIZE as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidId,
    InvalidToken,
    InvalidStoredCredential,
    Windows(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Fixed variants/numeric codes only; no token, ID, target or OS text.
        write!(f, "Windows credential storage: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Save or replace exactly this application's credential for `id`.
/// The generic blob stores visible ASCII bytes directly, without a terminator.
pub fn save(id: &str, token: &str) -> Result<(), Error> {
    let mut target = target(id)?;
    validate_token(token.as_bytes())?;
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: token.len() as u32,
        // CredWriteW reads the blob synchronously and does not mutate it.
        CredentialBlob: token.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    // All input pointers remain live through this synchronous call.
    if unsafe { CredWriteW(&credential, 0) } == 0 {
        Err(last_error())
    } else {
        Ok(())
    }
}

/// Read only the exact namespaced target. Missing is distinct from OS failure.
pub fn read(id: &str) -> Result<Option<String>, Error> {
    let target = target(id)?;
    let mut raw = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) } == 0 {
        return missing_or_error(None);
    }
    if raw.is_null() {
        return Err(Error::InvalidStoredCredential);
    }
    // Own the returned allocation before any fallible validation or copy.
    let allocation = ReadAllocation(raw);
    let credential = unsafe { &*allocation.0 };
    if credential.Type != CRED_TYPE_GENERIC
        || credential.CredentialBlobSize == 0
        || credential.CredentialBlobSize as usize > MAX_TOKEN_BYTES
        || credential.CredentialBlob.is_null()
    {
        return Err(Error::InvalidStoredCredential);
    }
    let blob = unsafe {
        std::slice::from_raw_parts(
            credential.CredentialBlob,
            credential.CredentialBlobSize as usize,
        )
    };
    validate_token(blob).map_err(|_| Error::InvalidStoredCredential)?;
    let token = std::str::from_utf8(blob).map_err(|_| Error::InvalidStoredCredential)?;
    Ok(Some(token.to_owned()))
}

/// Delete only the exact namespaced target; false means it was already absent.
pub fn delete(id: &str) -> Result<bool, Error> {
    let target = target(id)?;
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        missing_or_error(false)
    } else {
        Ok(true)
    }
}

fn target(id: &str) -> Result<Vec<u16>, Error> {
    if id.is_empty()
        || id.len() > MAX_ID_BYTES
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::InvalidId);
    }
    Ok(TARGET_PREFIX
        .encode_utf16()
        .chain(id.encode_utf16())
        .chain(Some(0))
        .collect())
}

fn validate_token(bytes: &[u8]) -> Result<(), Error> {
    if bytes.is_empty()
        || bytes.len() > MAX_TOKEN_BYTES
        || !bytes.iter().all(|b| (0x21..=0x7e).contains(b))
    {
        Err(Error::InvalidToken)
    } else {
        Ok(())
    }
}

fn last_error() -> Error {
    Error::Windows(unsafe { GetLastError() })
}

fn missing_or_error<T>(missing: T) -> Result<T, Error> {
    match last_error() {
        Error::Windows(ERROR_NOT_FOUND) => Ok(missing),
        error => Err(error),
    }
}

struct ReadAllocation(*mut CREDENTIALW);
impl Drop for ReadAllocation {
    fn drop(&mut self) {
        // Windows owns this writable blob inside the single returned allocation.
        // Clear valid bounded secret storage before releasing it on every path.
        unsafe {
            let credential = &*self.0;
            if !credential.CredentialBlob.is_null()
                && credential.CredentialBlobSize as usize <= MAX_TOKEN_BYTES
            {
                for offset in 0..credential.CredentialBlobSize as usize {
                    std::ptr::write_volatile(credential.CredentialBlob.add(offset), 0);
                }
            }
            CredFree(self.0.cast());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_cannot_escape_the_fixed_namespace() {
        for id in [
            "", ".", "..", "a/b", "a\\b", "a:b", "a b", "a\0b", "a\nb", "тест",
        ] {
            assert_eq!(target(id), Err(Error::InvalidId));
            assert_eq!(read(id), Err(Error::InvalidId));
            assert_eq!(delete(id), Err(Error::InvalidId));
            assert_eq!(save(id, "fake-token"), Err(Error::InvalidId));
        }
        assert_eq!(target(&"a".repeat(MAX_ID_BYTES + 1)), Err(Error::InvalidId));
        let wide = target("own_test-123").unwrap();
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&wide[..wide.len() - 1]).unwrap(),
            "Prismestra/HomeAssistant/v1/own_test-123"
        );
        assert!(target(&"a".repeat(MAX_ID_BYTES)).is_ok());
    }

    #[test]
    fn tokens_are_visible_ascii_bounded_by_the_actual_windows_blob_limit() {
        assert_eq!(MAX_TOKEN_BYTES, 2560);
        for token in [
            "",
            "a b",
            "a\tb",
            "a\rb",
            "a\nb",
            "a\0b",
            "a\u{7f}",
            "токен",
        ] {
            assert_eq!(validate_token(token.as_bytes()), Err(Error::InvalidToken));
            assert_eq!(save("validation_only", token), Err(Error::InvalidToken));
        }
        assert!(validate_token(&vec![b'a'; MAX_TOKEN_BYTES]).is_ok());
        assert_eq!(
            validate_token(&vec![b'a'; MAX_TOKEN_BYTES + 1]),
            Err(Error::InvalidToken)
        );
        assert!(validate_token(b"fake.jwt-Token_123./+=~!").is_ok());
    }

    #[test]
    fn errors_never_contain_inputs_or_os_messages() {
        let secret = "fake-token-DO-NOT-ECHO";
        let id = "private-target-DO-NOT-ECHO";
        for error in [
            Error::InvalidId,
            Error::InvalidToken,
            Error::InvalidStoredCredential,
            Error::Windows(1312),
        ] {
            let formatted = format!("{error} {error:?}");
            assert!(!formatted.contains(secret));
            assert!(!formatted.contains(id));
            assert!(!formatted.contains(TARGET_PREFIX));
        }
    }

    #[test]
    #[ignore = "explicit OS mutation: creates, reads and removes one unique own fake credential"]
    fn own_fake_credential_roundtrip() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let id = format!(
            "test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        struct Cleanup<'a>(&'a str);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = delete(self.0);
            }
        }
        save(&id, "fake-ha-token-never-a-real-secret").unwrap();
        let _cleanup = Cleanup(&id);
        assert_eq!(
            read(&id).unwrap().as_deref(),
            Some("fake-ha-token-never-a-real-secret")
        );
        save(&id, "fake-replacement-token").unwrap();
        assert_eq!(
            read(&id).unwrap().as_deref(),
            Some("fake-replacement-token")
        );
        assert_eq!(delete(&id), Ok(true));
        assert_eq!(read(&id), Ok(None));
        assert_eq!(delete(&id), Ok(false));
    }
}
