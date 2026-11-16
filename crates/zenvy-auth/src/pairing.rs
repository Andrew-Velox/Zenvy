use crate::{b64, random_bytes};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

const MAX_ATTEMPTS: u32 = 5;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairError {
    #[error("no pairing is in progress")]
    NotActive,
    #[error("pairing code expired")]
    Expired,
    #[error("wrong pairing code")]
    Wrong,
    #[error("too many wrong attempts; pairing cancelled")]
    TooManyAttempts,
}

struct Session {
    secret: String,
    expires: Instant,
    attempts: u32,
}

/// One-time pairing secret: 128 random bits, short TTL, limited wrong attempts, and consumed by the
/// first successful use. At most one pairing session exists at a time.
#[derive(Default)]
pub struct PairingManager {
    current: Option<Session>,
}

impl PairingManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts (replacing any earlier session) and returns the secret to embed in the QR code.
    pub fn start(&mut self, ttl: Duration, now: Instant) -> String {
        let secret = b64(&random_bytes::<16>());
        self.current = Some(Session {
            secret: secret.clone(),
            expires: now + ttl,
            attempts: 0,
        });
        secret
    }

    pub fn is_active(&self, now: Instant) -> bool {
        self.current.as_ref().is_some_and(|s| now < s.expires)
    }

    /// Validates a candidate secret. On success the session is **consumed**.
    pub fn consume(&mut self, candidate: &str, now: Instant) -> Result<(), PairError> {
        let s = self.current.as_mut().ok_or(PairError::NotActive)?;
        if now >= s.expires {
            self.current = None;
            return Err(PairError::Expired);
        }
        if bool::from(s.secret.as_bytes().ct_eq(candidate.as_bytes())) {
            self.current = None;
            return Ok(());
        }
        s.attempts += 1;
        if s.attempts >= MAX_ATTEMPTS {
            self.current = None;
            return Err(PairError::TooManyAttempts);
        }
        Err(PairError::Wrong)
    }

    pub fn cancel(&mut self) {
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_use() {
        let mut m = PairingManager::new();
        let t0 = Instant::now();
        let s = m.start(Duration::from_secs(120), t0);
        assert_eq!(m.consume(&s, t0), Ok(()));
        assert_eq!(
            m.consume(&s, t0),
            Err(PairError::NotActive),
            "a used secret must not work twice"
        );
    }

    #[test]
    fn expires() {
        let mut m = PairingManager::new();
        let t0 = Instant::now();
        let s = m.start(Duration::from_secs(120), t0);
        assert_eq!(
            m.consume(&s, t0 + Duration::from_secs(121)),
            Err(PairError::Expired)
        );
        assert!(!m.is_active(t0));
    }

    #[test]
    fn wrong_guesses_burn_the_session() {
        let mut m = PairingManager::new();
        let t0 = Instant::now();
        let s = m.start(Duration::from_secs(120), t0);
        for _ in 0..4 {
            assert_eq!(m.consume("nope", t0), Err(PairError::Wrong));
        }
        assert_eq!(m.consume("nope", t0), Err(PairError::TooManyAttempts));
        assert_eq!(
            m.consume(&s, t0),
            Err(PairError::NotActive),
            "even the right secret is dead after brute force"
        );
    }

    #[test]
    fn restart_invalidates_the_old_secret() {
        let mut m = PairingManager::new();
        let t0 = Instant::now();
        let old = m.start(Duration::from_secs(120), t0);
        let new = m.start(Duration::from_secs(120), t0);
        assert_ne!(old, new);
        assert_eq!(m.consume(&old, t0), Err(PairError::Wrong));
        assert_eq!(m.consume(&new, t0), Ok(()));
    }

    #[test]
    fn secret_has_128_bits() {
        let s = PairingManager::new().start(Duration::from_secs(1), Instant::now());
        assert_eq!(crate::unb64(&s).unwrap().len(), 16);
    }
}
