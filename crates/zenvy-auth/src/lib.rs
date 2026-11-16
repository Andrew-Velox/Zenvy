//! Authentication & pairing (PLAN.md §9). No networking here: pure, testable logic.
//!
//! Model: each phone holds an Ed25519 key (non-extractable in WebCrypto). Pairing registers its
//! public key after a one-time secret *and* explicit approval on the PC. Every connection then
//! proves possession of the key by signing a fresh server nonce, so there is no bearer token to
//! steal from logs, URLs or storage.

mod audit;
mod challenge;
mod pairing;
mod ratelimit;
mod store;

pub use audit::AuditLog;
pub use challenge::{confirmation_code, new_nonce, random_bytes, verify_signature, AUTH_CONTEXT};
pub use pairing::{PairError, PairingManager};
pub use ratelimit::Limiter;
pub use store::{Device, DeviceStore, StoreError, DEFAULT_SCOPES};

use base64::Engine;
pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
pub fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
}
