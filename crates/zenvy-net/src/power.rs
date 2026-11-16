//! Power actions, lock state and unlocking from a phone.
//!
//! * lock runs at once; suspend, reboot and power off need a request, then a confirmation with the token the
//!   agent handed out, then a short cancellable countdown that every phone can see (and the person at the PC can
//!   stop with `zenvyd power-cancel`);
//! * typing a password on the lock screen is rate-limited per device and only happens while the PC is
//!   positively detected as locked.

use crate::app::App;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;
use zenvy_power::PowerAdapter;
use zenvy_proto::{LockState, PowerAction, PowerState};

/// A confirmation token is only good for this long.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(10);
const FAIL_LIMIT: u32 = 3;
const FAIL_WINDOW: Duration = Duration::from_secs(600);
const LOCKOUT: Duration = Duration::from_secs(300);

struct Scheduled {
    id: String,
    abort: AbortHandle,
}

struct Tries {
    fails: u32,
    since: Instant,
    locked_until: Option<Instant>,
}

pub struct PowerRuntime {
    pub adapter: Arc<PowerAdapter>,
    /// Config allows typing the password from a phone.
    pub unlock_enabled: bool,
    /// The PC keyboard layout is US: keystrokes are sent as keycodes, which only mean what we think they mean there.
    pub layout_us: bool,
    /// Delay between confirming and executing.
    pub delay: Duration,
    scheduled: Mutex<Option<Scheduled>>,
    tries: Mutex<HashMap<String, Tries>>,
}

pub fn hex_token() -> String {
    zenvy_auth::random_bytes::<12>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl PowerRuntime {
    pub fn new(
        adapter: Arc<PowerAdapter>,
        unlock_enabled: bool,
        layout_us: bool,
        delay: Duration,
    ) -> Self {
        Self {
            adapter,
            unlock_enabled,
            layout_us,
            delay,
            scheduled: Mutex::new(None),
            tries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn state(&self, input_available: bool) -> PowerState {
        let c = self.adapter.can();
        PowerState {
            lock: self.adapter.lock_state().await,
            can_lock: c.lock,
            can_suspend: c.suspend,
            can_reboot: c.reboot,
            can_poweroff: c.poweroff,
            can_unlock: self.unlock_enabled && self.layout_us && input_available && c.lock,
        }
    }

    pub fn can(&self, action: PowerAction) -> bool {
        let c = self.adapter.can();
        match action {
            PowerAction::Lock => c.lock,
            PowerAction::Suspend => c.suspend,
            PowerAction::Reboot => c.reboot,
            PowerAction::Poweroff => c.poweroff,
        }
    }

    /// Starts the countdown for a confirmed action. Only one can be pending at a time.
    pub fn schedule(&self, app: Arc<App>, action: PowerAction) -> Result<String, &'static str> {
        let mut slot = self.scheduled.lock().unwrap();
        if slot.is_some() {
            return Err("another action is already counting down");
        }
        let id = hex_token();
        let (delay, task_id) = (self.delay, id.clone());
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(rt) = app.power.as_ref() else { return };
            // from here on it can no longer be cancelled
            rt.scheduled.lock().unwrap().take();
            app.audit.record("power_exec", None, action.name());
            app.publish(
                "power.executing",
                json!({ "id": task_id, "action": action.name() }),
            );
            if let Err(message) = rt.adapter.execute(action).await {
                app.publish(
                    "power.failed",
                    json!({ "id": task_id, "action": action.name(), "message": message }),
                );
            }
        });
        *slot = Some(Scheduled {
            id: id.clone(),
            abort: handle.abort_handle(),
        });
        Ok(id)
    }

    /// Stops the pending countdown. `id` of `None` cancels whatever is pending (the person at the PC).
    pub fn cancel(&self, id: Option<&str>) -> bool {
        let mut slot = self.scheduled.lock().unwrap();
        match slot.as_ref() {
            Some(s) if id.is_none_or(|i| i == s.id) => {
                s.abort.abort();
                slot.take();
                true
            }
            _ => false,
        }
    }

    pub fn pending_id(&self) -> Option<String> {
        self.scheduled
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.id.clone())
    }

    /// `Err(seconds)` while this device is locked out of trying passwords.
    pub fn check_unlock(&self, device: &str) -> Result<(), u64> {
        let mut t = self.tries.lock().unwrap();
        let now = Instant::now();
        if let Some(e) = t.get_mut(device) {
            if let Some(until) = e.locked_until {
                if until > now {
                    return Err((until - now).as_secs() + 1);
                }
                t.remove(device);
            } else if now.duration_since(e.since) > FAIL_WINDOW {
                t.remove(device);
            }
        }
        Ok(())
    }

    pub fn unlock_failed(&self, device: &str) {
        let mut t = self.tries.lock().unwrap();
        let now = Instant::now();
        let e = t.entry(device.to_string()).or_insert(Tries {
            fails: 0,
            since: now,
            locked_until: None,
        });
        e.fails += 1;
        if e.fails >= FAIL_LIMIT {
            e.locked_until = Some(now + LOCKOUT);
        }
    }

    pub fn unlock_ok(&self, device: &str) {
        self.tries.lock().unwrap().remove(device);
    }

    /// Locked now *and* still locked a moment later (a flapping signal never lets a password be typed).
    pub async fn locked_steady(&self) -> bool {
        if self.adapter.lock_state().await != LockState::Locked {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.adapter.lock_state().await == LockState::Locked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenvy_power::LockConfig;

    async fn rt() -> PowerRuntime {
        let a = PowerAdapter::discover(
            "/nonexistent/systemctl".into(),
            "/nonexistent/loginctl".into(),
            None,
            None,
            LockConfig {
                processes: vec![],
                detect_cmd: vec![],
                use_compositor: false,
            },
        )
        .await;
        PowerRuntime::new(Arc::new(a), true, true, Duration::from_millis(50))
    }

    #[test]
    fn tokens_are_long_random_hex() {
        let (a, b) = (hex_token(), hex_token());
        assert_eq!(a.len(), 24);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn three_wrong_passwords_lock_a_device_out_and_success_clears_the_count() {
        let r = rt().await;
        assert!(r.check_unlock("d1").is_ok());
        r.unlock_failed("d1");
        r.unlock_failed("d1");
        assert!(
            r.check_unlock("d1").is_ok(),
            "two failures are still allowed"
        );
        r.unlock_failed("d1");
        let wait = r.check_unlock("d1").unwrap_err();
        assert!(
            (290..=301).contains(&wait),
            "locked out for about five minutes ({wait} s)"
        );
        assert!(r.check_unlock("d2").is_ok(), "another device is unaffected");
        r.unlock_ok("d1");
        assert!(
            r.check_unlock("d1").is_ok(),
            "the owner clearing it (a success) resets"
        );
        r.unlock_failed("d3");
        r.unlock_failed("d3");
        r.unlock_ok("d3");
        r.unlock_failed("d3");
        assert!(
            r.check_unlock("d3").is_ok(),
            "a success in between restarts the count"
        );
    }

    #[tokio::test]
    async fn the_state_reflects_what_is_allowed() {
        let r = rt().await;
        let s = r.state(true).await;
        assert_eq!(
            s.lock,
            LockState::Unknown,
            "no loginctl: unknown, never locked"
        );
        assert_eq!(
            s.can_lock,
            r.adapter.session().is_some(),
            "locking needs a session to lock"
        );
        assert!(
            !r.state(false).await.can_unlock,
            "without keyboard injection nothing can be typed"
        );
        let off = PowerRuntime::new(r.adapter.clone(), false, true, Duration::from_secs(1));
        assert!(
            !off.state(true).await.can_unlock,
            "the config switch turns unlocking off"
        );
        let non_us = PowerRuntime::new(r.adapter.clone(), true, false, Duration::from_secs(1));
        assert!(
            !non_us.state(true).await.can_unlock,
            "a non-US layout would type the wrong keys"
        );
    }
}
