use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

struct Entry {
    failures: u32,
    locked_until: Option<Instant>,
    last: Instant,
}

/// Failure-based lockout with exponential backoff, keyed by e.g. client IP.
/// 3 free failures, then 2s, 4s, 8s … capped at 15 minutes. A success clears the key.
pub struct Limiter<K: Hash + Eq + Clone> {
    map: HashMap<K, Entry>,
}

impl<K: Hash + Eq + Clone> Default for Limiter<K> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Err(retry_after)` while locked out.
    pub fn check(&self, key: &K, now: Instant) -> Result<(), Duration> {
        match self.map.get(key).and_then(|e| e.locked_until) {
            Some(t) if now < t => Err(t - now),
            _ => Ok(()),
        }
    }

    pub fn fail(&mut self, key: &K, now: Instant) {
        // Forget stale entries so the map cannot grow without bound under a scan.
        if self.map.len() > 1024 {
            self.map
                .retain(|_, e| now.duration_since(e.last) < Duration::from_secs(3600));
        }
        let e = self.map.entry(key.clone()).or_insert(Entry {
            failures: 0,
            locked_until: None,
            last: now,
        });
        e.failures += 1;
        e.last = now;
        if e.failures > 3 {
            let secs = (1u64 << (e.failures - 3).min(10)).min(900);
            e.locked_until = Some(now + Duration::from_secs(secs));
        }
    }

    pub fn success(&mut self, key: &K) {
        self.map.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_and_reset() {
        let mut l = Limiter::<&str>::new();
        let t = Instant::now();
        for _ in 0..3 {
            l.fail(&"ip", t);
            assert!(l.check(&"ip", t).is_ok());
        }
        l.fail(&"ip", t);
        assert_eq!(l.check(&"ip", t), Err(Duration::from_secs(2)));
        assert!(l.check(&"ip", t + Duration::from_secs(3)).is_ok());
        l.fail(&"ip", t + Duration::from_secs(3));
        assert_eq!(
            l.check(&"ip", t + Duration::from_secs(3)),
            Err(Duration::from_secs(4))
        );
        assert!(l.check(&"other", t).is_ok(), "other clients are unaffected");
        l.success(&"ip");
        assert!(l.check(&"ip", t).is_ok());
    }

    #[test]
    fn lockout_is_capped() {
        let mut l = Limiter::<u8>::new();
        let t = Instant::now();
        for _ in 0..40 {
            l.fail(&1, t);
        }
        assert!(l.check(&1, t).unwrap_err() <= Duration::from_secs(900));
    }
}
