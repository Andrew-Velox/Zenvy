use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::io::Read;

pub const AUTH_CONTEXT: &[u8] = b"zenvy-auth-v1\0";

/// Cryptographically secure random bytes straight from the kernel (no extra dependency).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("/dev/urandom must be readable");
    b
}

pub fn new_nonce() -> [u8; 32] {
    random_bytes::<32>()
}

/// The message a device signs: context || device_id || 0 || nonce. Binding the device id stops a
/// signature for one device being replayed as another; the context stops cross-protocol reuse.
fn message(device_id: &str, nonce: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(AUTH_CONTEXT.len() + device_id.len() + 1 + nonce.len());
    m.extend_from_slice(AUTH_CONTEXT);
    m.extend_from_slice(device_id.as_bytes());
    m.push(0);
    m.extend_from_slice(nonce);
    m
}

pub fn verify_signature(pubkey: &[u8], device_id: &str, nonce: &[u8], sig: &[u8]) -> bool {
    let (Ok(pk), Ok(sig)) = (<[u8; 32]>::try_from(pubkey), <[u8; 64]>::try_from(sig)) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    vk.verify_strict(&message(device_id, nonce), &Signature::from_bytes(&sig))
        .is_ok()
}

/// 4-digit code shown on both the PC and the phone during pairing, derived from the phone's
/// public key, so the user can see the request is from the phone they are holding.
pub fn confirmation_code(pubkey: &[u8]) -> String {
    let h = Sha256::digest(pubkey);
    format!(
        "{:04}",
        u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 10_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn valid_signature_verifies() {
        let k = key(1);
        let nonce = [7u8; 32];
        let sig = k.sign(&message("dev1", &nonce));
        assert!(verify_signature(
            k.verifying_key().as_bytes(),
            "dev1",
            &nonce,
            &sig.to_bytes()
        ));
    }

    #[test]
    fn wrong_device_nonce_key_or_garbage_fail() {
        let k = key(1);
        let nonce = [7u8; 32];
        let sig = k.sign(&message("dev1", &nonce)).to_bytes();
        let pk = k.verifying_key().to_bytes();
        assert!(
            !verify_signature(&pk, "dev2", &nonce, &sig),
            "replay as another device"
        );
        assert!(
            !verify_signature(&pk, "dev1", &[8u8; 32], &sig),
            "replay with another nonce"
        );
        assert!(
            !verify_signature(key(2).verifying_key().as_bytes(), "dev1", &nonce, &sig),
            "other key"
        );
        assert!(!verify_signature(&pk, "dev1", &nonce, &[0u8; 64]));
        assert!(!verify_signature(&pk, "dev1", &nonce, &sig[..63]));
        assert!(!verify_signature(&pk[..31], "dev1", &nonce, &sig));
        assert!(!verify_signature(&[0u8; 32], "dev1", &nonce, &sig));
    }

    #[test]
    fn signature_for_bare_nonce_is_not_accepted() {
        // A signature over the nonce without our context must not verify (cross-protocol reuse).
        let k = key(1);
        let nonce = [7u8; 32];
        let sig = k.sign(&nonce).to_bytes();
        assert!(!verify_signature(
            k.verifying_key().as_bytes(),
            "dev1",
            &nonce,
            &sig
        ));
    }

    #[test]
    fn nonces_are_fresh() {
        assert_ne!(new_nonce(), new_nonce());
    }

    #[test]
    fn code_is_four_digits_and_stable() {
        let c = confirmation_code(&[1, 2, 3]);
        assert_eq!(c.len(), 4);
        assert!(c.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(c, confirmation_code(&[1, 2, 3]));
    }
}
