//! Developer tool (read-only): what the system allows and the raw lock signals. Never changes anything.
use zenvy_power::{LockConfig, PowerAdapter};
#[tokio::main]
async fn main() {
    let p = PowerAdapter::discover(
        "systemctl".into(),
        "loginctl".into(),
        Some("busctl".into()),
        Some("hyprctl".into()),
        LockConfig::default(),
    )
    .await;
    println!("session: {:?}", p.session());
    println!("can: {:?}", p.can());
    println!("lock signals: {:?}", p.lock_signals().await);
    println!("lock state: {:?}", p.lock_state().await);
}
