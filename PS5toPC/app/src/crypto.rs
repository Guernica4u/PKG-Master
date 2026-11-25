//! Passcode key derivation and AES-XTS decryption for PFS images.
//!
//! No console keys, RSA keys or content keys are embedded. A passcode-derived
//! key only opens packages built with that passcode (fake/debug packages).

use aes::cipher::KeyInit;
use aes::Aes128;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sha3::Sha3_256;
use xts_mode::{get_tweak_default, Xts128};

use crate::binary::{check_bounds, SharedSource, Source};
use crate::error::{Error, Result};

pub const XTS_SECTOR_SIZE: u64 = 0x1000;

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn check_ids(content_id: &str, passcode: &str) -> Result<()> {
    if content_id.len() != 36 {
        return Err(Error::KeyRequired(format!(
            "content id must be 36 characters, got {}",
            content_id.len()
        )));
    }
    if passcode.len() != 32 || !passcode.is_ascii() {
        return Err(Error::KeyRequired(format!(
            "passcode must be 32 characters, got {}",
            passcode.chars().count()
        )));
    }
    Ok(())
}

fn padded_id(content_id: &str) -> [u8; 48] {
    let mut out = [0u8; 48];
    out[..content_id.len()].copy_from_slice(content_id.as_bytes());
    out
}

/// `SHA256( SHA256(BE32(index)) || SHA256(content_id padded to 48) || passcode )`.
/// Index 1 is the EKPFS used to derive the PFS XTS keys.
pub fn compute_keys(content_id: &str, passcode: &str, index: u32) -> Result<[u8; 32]> {
    check_ids(content_id, passcode)?;
    let mut h = Sha256::new();
    h.update(Sha256::digest(index.to_be_bytes()));
    h.update(Sha256::digest(padded_id(content_id)));
    h.update(passcode.as_bytes());
    Ok(h.finalize().into())
}

/// SHA3-256 variant used by some PS5 debug images.
pub fn compute_keys_sha3(content_id: &str, passcode: &str, index: u32) -> Result<[u8; 32]> {
    check_ids(content_id, passcode)?;
    let mut h = Sha3_256::new();
    h.update(Sha3_256::digest(index.to_be_bytes()));
    h.update(Sha3_256::digest(padded_id(content_id)));
    h.update(passcode.as_bytes());
    Ok(h.finalize().into())
}

/// Returns `(data_key, tweak_key)` for a PFS image.
pub fn pfs_xts_keys(ekpfs: &[u8; 32], seed: &[u8], new_crypt: bool) -> ([u8; 16], [u8; 16]) {
    let base = if new_crypt { hmac_sha256(ekpfs, seed) } else { *ekpfs };
    let mut msg = 1u32.to_le_bytes().to_vec();
    msg.extend_from_slice(seed);
    let enc = hmac_sha256(&base, &msg);
    let tweak: [u8; 16] = enc[..16].try_into().unwrap();
    let data: [u8; 16] = enc[16..].try_into().unwrap();
    (data, tweak)
}

/// Decrypts PFS XTS sectors on the fly. The first `start_sector` sectors (the
/// plaintext superblock block) pass through unchanged. Reads are always
/// widened to whole sectors before decrypting, then sliced.
pub struct XtsSource {
    base: SharedSource,
    xts: Xts128<Aes128>,
    start_sector: u64,
}

impl XtsSource {
    pub fn new(base: SharedSource, data_key: [u8; 16], tweak_key: [u8; 16], start_sector: u64) -> Self {
        let xts = Xts128::new(
            Aes128::new(&data_key.into()),
            Aes128::new(&tweak_key.into()),
        );
        XtsSource { base, xts, start_sector }
    }
}

impl Source for XtsSource {
    fn len(&self) -> u64 {
        self.base.len()
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let size = self.len();
        check_bounds(self.name(), offset, buf.len(), size)?;
        if buf.is_empty() {
            return Ok(());
        }
        let ss = XTS_SECTOR_SIZE;
        let first = offset / ss;
        let last = (offset + buf.len() as u64 - 1) / ss;
        let mut written = 0usize;
        let mut sector_buf = vec![0u8; ss as usize];
        for sector in first..=last {
            let start = sector * ss;
            let available = ss.min(size - start) as usize;
            let chunk = &mut sector_buf[..available];
            self.base.read_into(start, chunk)?;
            if sector >= self.start_sector {
                let padded_len = (available + 15) / 16 * 16;
                let mut padded = vec![0u8; padded_len];
                padded[..available].copy_from_slice(chunk);
                self.xts
                    .decrypt_sector(&mut padded, get_tweak_default(sector as u128));
                chunk.copy_from_slice(&padded[..available]);
            }
            let skip = if sector == first { (offset - start) as usize } else { 0 };
            let take = (available - skip).min(buf.len() - written);
            buf[written..written + take].copy_from_slice(&chunk[skip..skip + take]);
            written += take;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        self.base.name()
    }
}
