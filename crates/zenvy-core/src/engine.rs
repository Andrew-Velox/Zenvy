//! Adaptive configuration engine (PLAN.md §7.4): choose a backend per capability from probed
//! candidates, honoring a user override only when it is actually usable.

use std::collections::BTreeMap;
use zenvy_proto::{Candidate, CapState, CapabilityRecord};

/// `candidates` are ordered by preference (least privileged / best fidelity first).
/// `user_override` names a backend; if that backend is not available the automatic choice is kept
/// and the reason says so (an override is never silently ignored).
pub fn resolve(candidates: Vec<Candidate>, user_override: Option<&str>) -> CapabilityRecord {
    let mut note = None;
    let mut chosen = None;
    if let Some(want) = user_override {
        match candidates.iter().find(|c| c.backend == want) {
            Some(c) if c.state == CapState::Available => chosen = Some(c.clone()),
            Some(c) => {
                note = Some(format!(
                    "override '{want}' ignored: it is {}",
                    c.state.label()
                ))
            }
            None => note = Some(format!("override '{want}' ignored: unknown backend")),
        }
    }
    if chosen.is_none() {
        chosen = candidates
            .iter()
            .find(|c| c.state == CapState::Available)
            .cloned();
    }
    match chosen {
        Some(c) => CapabilityRecord {
            state: CapState::Available,
            backend: Some(c.backend.clone()),
            reason: note.or(c.reason.clone()),
            remediation: None,
            candidates,
        },
        None => {
            // Most actionable state wins: permission problems can be fixed, errors can be retried.
            let any = |s| candidates.iter().any(|c| c.state == s);
            let state = if any(CapState::PermissionRequired) {
                CapState::PermissionRequired
            } else if any(CapState::Error) {
                CapState::Error
            } else if !candidates.is_empty()
                && candidates.iter().all(|c| c.state == CapState::Unsupported)
            {
                CapState::Unsupported
            } else {
                CapState::Unavailable
            };
            let reason = note.or_else(|| {
                let parts: Vec<String> = candidates
                    .iter()
                    .filter_map(|c| c.reason.as_ref().map(|r| format!("{}: {r}", c.backend)))
                    .collect();
                (!parts.is_empty()).then(|| parts.join("; "))
            });
            CapabilityRecord {
                state,
                backend: None,
                reason,
                remediation: None,
                candidates,
            }
        }
    }
}

/// Convenience: build `backend overrides` lookups from a config map.
pub fn override_for<'a>(overrides: &'a BTreeMap<String, String>, cap: &str) -> Option<&'a str> {
    overrides.get(cap).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(b: &str, s: CapState) -> Candidate {
        Candidate {
            backend: b.into(),
            state: s,
            reason: (s != CapState::Available).then(|| format!("{b} is {}", s.label())),
        }
    }

    #[test]
    fn first_available_wins() {
        let r = resolve(
            vec![
                c("uinput", CapState::PermissionRequired),
                c("vptr", CapState::Available),
            ],
            None,
        );
        assert_eq!(
            (r.state, r.backend.as_deref()),
            (CapState::Available, Some("vptr"))
        );
    }

    #[test]
    fn valid_override_beats_preference() {
        let r = resolve(
            vec![c("a", CapState::Available), c("b", CapState::Available)],
            Some("b"),
        );
        assert_eq!(r.backend.as_deref(), Some("b"));
    }

    #[test]
    fn unusable_override_is_reported_not_ignored_silently() {
        let r = resolve(
            vec![c("a", CapState::Available), c("b", CapState::Unavailable)],
            Some("b"),
        );
        assert_eq!(r.backend.as_deref(), Some("a"));
        assert!(r.reason.unwrap().contains("override 'b' ignored"));
    }

    #[test]
    fn permission_beats_unavailable_when_nothing_works() {
        let r = resolve(
            vec![
                c("a", CapState::Unavailable),
                c("b", CapState::PermissionRequired),
            ],
            None,
        );
        assert_eq!(r.state, CapState::PermissionRequired);
        assert!(r.backend.is_none());
    }

    #[test]
    fn all_unsupported_is_unsupported() {
        assert_eq!(
            resolve(vec![c("a", CapState::Unsupported)], None).state,
            CapState::Unsupported
        );
        assert_eq!(resolve(vec![], None).state, CapState::Unavailable);
    }
}
