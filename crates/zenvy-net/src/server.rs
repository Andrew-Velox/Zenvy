use crate::app::{App, ApprovalRequest};
use crate::tls::TlsMaterial;
use crate::ws::ws_handler;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zenvy_auth::{confirmation_code, unb64, PairError, DEFAULT_SCOPES};

const PAGE: &str = include_str!("page.html");

/// Rejects requests whose Host header is not one of ours (DNS-rebinding defence) and adds
/// security headers to every response.
async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    // HTTP/1.1 carries `Host`; HTTP/2 (what Chrome negotiates) carries `:authority`, which hyper
    // exposes on the URI. At least one must be present and every one that is present must be ours.
    let from_header = req
        .headers()
        .get(header::HOST)
        .map(|h| h.to_str().ok().map(str::to_owned));
    let from_uri = req.uri().authority().map(|a| Some(a.to_string()));
    let seen: Vec<Option<String>> = [from_header, from_uri].into_iter().flatten().collect();
    let host_ok = !seen.is_empty()
        && seen.iter().all(|h| {
            h.as_ref().is_some_and(|h| {
                app.cfg
                    .allowed_hosts
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(h))
            })
        });
    if !host_ok {
        return (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    for (k, v) in [
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("content-security-policy", "default-src 'self'; img-src 'self' data:; font-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self' wss:; frame-ancestors 'none'"),
    ] {
        h.insert(k, HeaderValue::from_static(v));
    }
    res
}

const WORKLET: &str = include_str!("audio-worklet.js");

/// The phone speaker's audio-thread player (static code, no secrets).
async fn worklet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        WORKLET,
    )
}

async fn index() -> Html<&'static str> {
    Html(PAGE)
}

async fn hello(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let open = app.pairing.lock().unwrap().is_active(Instant::now());
    Json(json!({ "name": "zenvy", "proto": 1, "pairing_open": open }))
}

#[derive(Deserialize)]
struct PairReq {
    secret: String,
    name: String,
    pubkey: String,
}

fn too_many(retry: Duration) -> Response {
    let mut r = (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response();
    r.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(retry.as_secs().max(1)),
    );
    r
}

async fn pair(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<PairReq>,
) -> Response {
    let ip: IpAddr = peer.ip();
    let now = Instant::now();
    if let Err(retry) = app.limiter.lock().unwrap().check(&ip, now) {
        return too_many(retry);
    }
    let fail = |why: &str| {
        app.limiter.lock().unwrap().fail(&ip, Instant::now());
        app.audit.record("pair_fail", None, &format!("{ip}: {why}"));
        // One generic answer: do not reveal whether a session exists, expired, or was guessed.
        (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "pairing failed" })),
        )
            .into_response()
    };
    let Some(pubkey) = unb64(&req.pubkey).filter(|k| k.len() == 32) else {
        return fail("bad key");
    };

    let consumed = app.pairing.lock().unwrap().consume(&req.secret, now);
    if let Err(e) = consumed {
        return fail(match e {
            PairError::NotActive => "no session",
            PairError::Expired => "expired",
            PairError::Wrong => "wrong secret",
            PairError::TooManyAttempts => "brute force, session cancelled",
        });
    }

    // Second factor: a human at the PC must approve this specific request.
    let tx = app.approvals.lock().unwrap().clone();
    let Some(tx) = tx else {
        return fail("no operator attached");
    };
    let (reply, wait) = tokio::sync::oneshot::channel();
    let code = confirmation_code(&pubkey);
    let name: String = req
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if tx
        .send(ApprovalRequest {
            name: name.clone(),
            code,
            reply,
        })
        .await
        .is_err()
    {
        return fail("operator gone");
    }
    let approved = matches!(
        tokio::time::timeout(Duration::from_secs(60), wait).await,
        Ok(Ok(true))
    );
    if !approved {
        app.audit
            .record("pair_denied", None, &format!("{ip}: name={name}"));
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "pairing was not approved" })),
        )
            .into_response();
    }
    let added = app
        .store
        .lock()
        .unwrap()
        .add(&name, &pubkey, DEFAULT_SCOPES);
    match added {
        Ok(d) => {
            app.limiter.lock().unwrap().success(&ip);
            app.audit.record(
                "pair_approved",
                Some(&d.id),
                &format!("{ip}: name={}", d.name),
            );
            Json(json!({ "device_id": d.id, "name": d.name, "scopes": d.scopes })).into_response()
        }
        Err(e) => {
            app.audit.record("pair_error", None, &e.to_string());
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/audio-worklet.js", get(worklet))
        .route("/api/hello", get(hello))
        .route("/api/pair", post(pair).layer(DefaultBodyLimit::max(4096)))
        .route("/ws", get(ws_handler))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Serves HTTPS until `shutdown` resolves.
pub async fn serve(
    app: Arc<App>,
    addr: SocketAddr,
    tls: &TlsMaterial,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let cfg = axum_server::tls_rustls::RustlsConfig::from_pem(
        tls.cert_pem.clone().into_bytes(),
        tls.key_pem.clone().into_bytes(),
    )
    .await?;
    let handle = axum_server::Handle::new();
    let h2 = handle.clone();
    tokio::spawn(async move {
        shutdown.await;
        h2.graceful_shutdown(Some(Duration::from_secs(2)));
    });
    // TCP_NODELAY: audio frames and pointer moves are small and frequent; Nagle's algorithm would hold them back
    axum_server::bind_rustls(addr, cfg)
        .map(|a| a.acceptor(axum_server::accept::NoDelayAcceptor::new()))
        .handle(handle)
        .serve(router(app).into_make_service_with_connect_info::<SocketAddr>())
        .await
}
