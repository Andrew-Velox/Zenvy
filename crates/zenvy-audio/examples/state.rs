//! Developer tool: print the audio state the phone would receive, read-only (`cargo run -p zenvy-audio --example state`).
#[tokio::main]
async fn main() {
    let state = zenvy_audio::AudioAdapter::new("pactl")
        .state()
        .await
        .expect("pactl state");
    println!("{}", serde_json::to_string_pretty(&state).unwrap());
}
