//! Developer tool: print the media state the phone would receive, read-only (`cargo run -p zenvy-media --example state`).
#[tokio::main]
async fn main() {
    let media = zenvy_media::MediaAdapter::connect()
        .await
        .expect("session bus");
    println!(
        "{}",
        serde_json::to_string_pretty(&media.state().await.expect("media state")).unwrap()
    );
}
