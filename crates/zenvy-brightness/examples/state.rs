//! Developer tool: detect displays and print the state the phone would receive (read-only).
//! With `--rewrite-current` it also writes each display's *current* value back, which proves the write
//! path works without visibly changing anything.
#[tokio::main]
async fn main() {
    let a = zenvy_brightness::BrightnessAdapter::detect(
        Some("ddcutil".into()),
        Some("brightnessctl".into()),
    )
    .await;
    println!("{}", serde_json::to_string_pretty(&a.state()).unwrap());
    if std::env::args().any(|x| x == "--rewrite-current") {
        let mut rx = a.watch();
        for d in a.state().displays {
            a.set(&d.id, d.percent).unwrap();
        }
        let ok = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .is_ok();
        a.refresh().await;
        println!(
            "rewrote current values, worker finished: {ok}\nafter refresh: {}",
            serde_json::to_string(&a.state()).unwrap()
        );
    }
}
