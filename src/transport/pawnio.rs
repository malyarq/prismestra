//! PawnIO user-mode wire format and a pinned, local PIIX4 module image.
//! This module cannot open a kernel device, load a driver or execute an IOCTL.
//! Encoding a request is not permission to run it: a native owner must hold the
//! shared SMBus mutex over the complete ENE transaction, and validate its target.
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub const SHARED_MUTEX: &str = "Global\\Access_SMBUS.HTP.Method";
pub const IOCTL_LOAD: u32 = (41394 << 16) | (0x821 << 2);
pub const IOCTL_EXECUTE: u32 = (41394 << 16) | (0x841 << 2);
pub const MAX_MODULE_BYTES: usize = 2 * 1024 * 1024;
pub const PIIX4_SHA256: &str = "91f9b4b1c39e3d399ce48477a89d8f6bd3e58a2241064a4daec3a1513dff56e5";

/// A verified image, not an installed/loaded module. No driver DLL is needed to
/// check it. Changes to the pinned image require fresh ABI/provenance review.
pub struct Piix4Module(Vec<u8>);
impl Piix4Module {
    /// A synthetic input for fake Device tests only; never a loadable image.
    #[cfg(test)]
    pub(crate) fn synthetic_for_test() -> Self {
        Self(b"synthetic fake-only PIIX4 image".to_vec())
    }
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        verify(&bytes, PIIX4_SHA256)?;
        Ok(Self(bytes))
    }
    pub fn read(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Read PIIX4 module: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_MODULE_BYTES as u64 {
            return Err("Expected a bounded regular PIIX4 module file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_MODULE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::from_bytes(bytes)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
fn verify(bytes: &[u8], expected: &str) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_MODULE_BYTES {
        return Err("PIIX4 module size outside bounds".into());
    }
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        return Err("PIIX4 module SHA-256 mismatch; nothing may be loaded".into());
    }
    Ok(())
}

/// Exact fields of the legacy 9-u64 SMBus input. The complete unused union and
/// native padding are zeroed instead of copying uninitialised stack memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    words: [u64; 9],
}
impl Transfer {
    fn new(address: u8, read: bool, command: u8, kind: u8) -> Result<Self, String> {
        if !super::ene::valid_address(address) {
            return Err("Only explicitly bound ENE addresses0x70..0x76 are allowed".into());
        }
        let mut words = [0; 9];
        words[..4].copy_from_slice(&[address.into(), u64::from(read), command.into(), kind.into()]);
        Ok(Self { words })
    }
    pub fn select_register(address: u8, register: u16) -> Result<Self, String> {
        if !matches!(register,0x1000..=0x100f|0x1c00..=0x1c3f|0x8020|0x80a0|0x8100..=0x8117) {
            return Err("ENE register outside identity/direct-RGB subset".into());
        }
        let mut request = Self::new(address, false, 0, 3)?;
        request.words[4] = u64::from(register.swap_bytes());
        Ok(request)
    }
    /// Pointer ownership/destination must be checked by the enclosing bus state
    /// machine. In isolation this packet does not establish a safe destination.
    pub fn read_selected_byte(address: u8) -> Result<Self, String> {
        Self::new(address, true, 0x81, 2)
    }
    pub fn write_selected_byte(address: u8, value: u8) -> Result<Self, String> {
        let mut request = Self::new(address, false, 1, 2)?;
        request.words[4] = value.into();
        Ok(request)
    }
    pub fn write_selected_rgb(address: u8, rbg: [u8; 3]) -> Result<Self, String> {
        let mut request = Self::new(address, false, 3, 5)?;
        request.words[4] = u64::from_le_bytes([3, rbg[0], rbg[1], rbg[2], 0, 0, 0, 0]);
        Ok(request)
    }
    pub fn words(&self) -> &[u64; 9] {
        &self.words
    }
    pub fn encode(&self) -> Vec<u8> {
        encode("ioctl_smbus_xfer", &self.words)
    }
}

/// Wire prefix is a NUL-terminated/padded32-byte function name, followed by
/// little-endian u64s. Only these source-proven functions are exposed here.
fn encode(name: &str, input: &[u64]) -> Vec<u8> {
    assert!(name.len() < 32 && name.is_ascii());
    let mut bytes = vec![0; 32 + input.len() * 8];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    for (dst, word) in bytes[32..].as_chunks_mut::<8>().0.iter_mut().zip(input) {
        dst.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}
pub fn identity_request() -> Vec<u8> {
    encode("ioctl_identity", &[0])
}
pub fn primary_port_request() -> Vec<u8> {
    encode("ioctl_piix4_port_sel", &[0])
}
/// Exact pinned AMX export accepts signed64 -1. This avoids changing the port
/// selection, but its busy-check may still clear hardware status under the lock.
pub fn query_port_request() -> Vec<u8> {
    encode("ioctl_piix4_port_sel", &[u64::MAX])
}
pub fn always_sleep_request() -> Vec<u8> {
    encode("ioctl_set_sleep_mode", &[2])
}

/// `bytes` must be the successful OS result sliced to its actual returned byte
/// count. Failure must be handled before this function; it is never byte255.
pub fn decode_words(bytes: &[u8], minimum: usize, capacity: usize) -> Result<Vec<u64>, String> {
    if minimum > capacity
        || capacity > 5
        || !bytes.len().is_multiple_of(8)
        || bytes.len() / 8 < minimum
        || bytes.len() / 8 > capacity
    {
        return Err("Invalid PawnIO returned byte count".into());
    }
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| u64::from_le_bytes(*b))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_size_and_pinned_image_gate() {
        assert!(Piix4Module::from_bytes(vec![]).is_err());
        assert!(Piix4Module::from_bytes(vec![0; MAX_MODULE_BYTES + 1]).is_err());
        assert!(Piix4Module::from_bytes(b"wrong module".to_vec()).is_err());
        verify(
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
        assert_eq!(IOCTL_LOAD, 0xa1b22084);
        assert_eq!(IOCTL_EXECUTE, 0xa1b22104);
    }
    #[test]
    fn native_union_layout_and_prefix_are_zero_filled() {
        let selector = Transfer::select_register(0x71, 0x8100).unwrap();
        assert_eq!(*selector.words(), [0x71, 0, 0, 3, 0x81, 0, 0, 0, 0]);
        let block = Transfer::write_selected_rgb(0x73, [1, 3, 2]).unwrap();
        assert_eq!(*block.words(), [0x73, 0, 3, 5, 0x02030103, 0, 0, 0, 0]);
        let encoded = block.encode();
        assert_eq!(encoded.len(), 104);
        assert_eq!(&encoded[..16], b"ioctl_smbus_xfer");
        assert!(encoded[16..32].iter().all(|b| *b == 0));
        assert_eq!(decode_words(&encoded[32..40], 1, 5).unwrap(), [0x73]);
        assert_eq!(
            *Transfer::read_selected_byte(0x71).unwrap().words(),
            [0x71, 1, 0x81, 2, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            Transfer::write_selected_byte(0x71, 255).unwrap().words()[4],
            255
        );
    }
    #[test]
    fn no_arbitrary_bus_or_persistent_registers() {
        for address in 0..=255 {
            assert_eq!(
                Transfer::read_selected_byte(address).is_ok(),
                (0x70..=0x76).contains(&address)
            );
        }
        for register in [0x50, 0x80f8, 0x80f9, 0x8160, 0xffff] {
            assert!(Transfer::select_register(0x71, register).is_err());
        }
        for (request, name, value) in [
            (identity_request(), "ioctl_identity", 0),
            (primary_port_request(), "ioctl_piix4_port_sel", 0),
            (always_sleep_request(), "ioctl_set_sleep_mode", 2),
            (query_port_request(), "ioctl_piix4_port_sel", u64::MAX),
        ] {
            assert_eq!(request.len(), 40);
            assert_eq!(&request[..name.len()], name.as_bytes());
            assert!(request[name.len()..32].iter().all(|b| *b == 0));
            assert_eq!(decode_words(&request[32..], 1, 1).unwrap(), [value]);
        }
    }
    #[test]
    fn short_unaligned_and_overcapacity_results_are_errors() {
        for n in 0..=49 {
            assert_eq!(
                decode_words(&vec![0; n], 1, 5).is_ok(),
                n > 0 && n <= 40 && n % 8 == 0
            );
        }
        assert!(decode_words(&[], 0, 0).is_ok());
        assert!(decode_words(&[0; 8], 2, 1).is_err());
        assert!(decode_words(&[0; 48], 1, 6).is_err());
    }
}
