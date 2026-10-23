use std::collections::{HashSet, VecDeque};

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, Tag};
use sha2::{Digest, Sha256};
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const REPLAY_WINDOW: usize = 4096;

pub const MAX_SECURE_INPUT_BYTES: usize = NONCE_BYTES + common::MAX_PACKET_SIZE + TAG_BYTES;

fn cipher(key: &str, context: &[u8]) -> Result<ChaCha20Poly1305, String> {
    if key.len() < 16 || key.len() > 256 {
        return Err("Pairing key must contain 16 to 256 characters".into());
    }

    let hash = Sha256::digest([b"NerveDesk v2:".as_slice(), context, key.as_bytes()].concat());
    Ok(ChaCha20Poly1305::new_from_slice(&hash).expect("SHA-256 key length"))
}

pub fn seal(key: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    if plaintext.len() > common::MAX_PACKET_SIZE {
        return Err("Input packet is too large".into());
    }
    seal_payload(key, b"input", plaintext, common::MAX_PACKET_SIZE)
}

pub fn seal_payload(
    key: &str,
    context: &[u8],
    plaintext: &[u8],
    max_plaintext: usize,
) -> Result<Vec<u8>, String> {
    if plaintext.len() > max_plaintext {
        return Err("Secure payload is too large".into());
    }
    let mut nonce = [0u8; NONCE_BYTES];
    unsafe { BCryptGenRandom(None, &mut nonce, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .map_err(|e| format!("Random generator: {e}"))?;
    let mut encrypted = plaintext.to_vec();
    let tag = cipher(key, context)?
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), context, &mut encrypted)
        .map_err(|_| "Could not encrypt input packet".to_string())?;
    let mut packet = Vec::with_capacity(NONCE_BYTES + encrypted.len() + TAG_BYTES);
    packet.extend_from_slice(&nonce);
    packet.extend_from_slice(&encrypted);
    packet.extend_from_slice(&tag);
    Ok(packet)
}

pub struct ReplayGuard {
    seen: HashSet<[u8; NONCE_BYTES]>,
    order: VecDeque<[u8; NONCE_BYTES]>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    pub fn open(&mut self, key: &str, packet: &[u8]) -> Result<Vec<u8>, String> {
        self.open_payload(key, b"input", packet, common::MAX_PACKET_SIZE)
    }

    pub fn open_payload(
        &mut self,
        key: &str,
        context: &[u8],
        packet: &[u8],
        max_plaintext: usize,
    ) -> Result<Vec<u8>, String> {
        if packet.len() < NONCE_BYTES + TAG_BYTES
            || packet.len() > NONCE_BYTES + max_plaintext + TAG_BYTES
        {
            return Err("Invalid encrypted input length".into());
        }
        let nonce: [u8; NONCE_BYTES] = packet[..NONCE_BYTES].try_into().unwrap();
        if self.seen.contains(&nonce) {
            return Err("Repeated input packet".into());
        }
        let tag_start = packet.len() - TAG_BYTES;
        let mut plaintext = packet[NONCE_BYTES..tag_start].to_vec();
        cipher(key, context)?
            .decrypt_in_place_detached(
                Nonce::from_slice(&nonce),
                context,
                &mut plaintext,
                Tag::from_slice(&packet[tag_start..]),
            )
            .map_err(|_| "Input authentication failed".to_string())?;
        self.seen.insert(nonce);
        self.order.push_back(nonce);
        if self.order.len() > REPLAY_WINDOW {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_input_rejects_wrong_key_tampering_and_replay() {
        let key = "1234567890abcdef";
        let packet = seal(key, b"cursor").unwrap();
        let mut guard = ReplayGuard::new();
        assert!(guard.open("fedcba0987654321", &packet).is_err());
        let mut tampered = packet.clone();
        tampered[NONCE_BYTES] ^= 1;
        assert!(guard.open(key, &tampered).is_err());
        assert_eq!(guard.open(key, &packet).unwrap(), b"cursor");
        assert!(guard.open(key, &packet).is_err());
    }

    #[test]
    fn pair_ack_is_bound_to_the_current_probe() {
        let key = "1234567890abcdef";
        let probe = seal_payload(key, b"pair-probe", b"NerveDesk", 32).unwrap();
        let received = ReplayGuard::new()
            .open_payload(key, b"pair-probe", &probe, 32)
            .unwrap();
        assert_eq!(received, b"NerveDesk");
        let ack = seal_payload(key, b"pair-ack", &probe[..12], 32).unwrap();
        let challenge = ReplayGuard::new()
            .open_payload(key, b"pair-ack", &ack, 32)
            .unwrap();
        assert_eq!(challenge, &probe[..12]);
        let next_probe = seal_payload(key, b"pair-probe", b"NerveDesk", 32).unwrap();
        assert_ne!(challenge, &next_probe[..12]);
    }
}
