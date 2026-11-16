//! Spike 5: HTTPS + WSS server (self-signed) with a probe page for the phone browser.
use axum::{extract::ws::{Message, WebSocketUpgrade}, response::{Html, IntoResponse}, routing::{get, post}, Router};
use std::net::SocketAddr;
async fn ws(u: WebSocketUpgrade) -> impl IntoResponse {
    u.on_upgrade(|mut s| async move { while let Some(Ok(m)) = s.recv().await { if let Message::Text(t) = m { if s.send(Message::Text(t)).await.is_err() { break } } } })
}
#[tokio::main]
async fn main() {
    let ip = std::env::args().nth(1).unwrap_or("192.168.0.230".into());
    let cert = rcgen::generate_simple_self_signed(vec![ip.clone(), "localhost".into()]).unwrap();
    let cfg = axum_server::tls_rustls::RustlsConfig::from_pem(cert.cert.pem().into_bytes(), cert.key_pair.serialize_pem().into_bytes()).await.unwrap();
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("page.html")) }))
        .route("/ws", get(ws))
        .route("/sw.js", get(|| async { ([("content-type","text/javascript")], "self.addEventListener('fetch',()=>{});") }))
        .route("/report", post(|b: String| async move { println!("REPORT {b}"); "ok" }));
    let addr: SocketAddr = format!("{ip}:7879").parse().unwrap();
    println!("listening https://{addr}");
    axum_server::bind_rustls(addr, cfg).serve(app.into_make_service()).await.unwrap();
}
