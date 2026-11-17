use crate::app::{App, ApprovalRequest, Decision, Hub, SameName};
use crate::tls::TlsMaterial;
use crate::ws::ws_handler;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
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
    let same_name: Vec<SameName> = app
        .store
        .lock()
        .unwrap()
        .list()
        .iter()
        .filter(|d| d.name == name)
        .map(|d| SameName {
            id: d.id.clone(),
            last_seen: d.last_seen,
            online: app.online.is_online(&d.id),
        })
        .collect();
    if tx
        .send(ApprovalRequest {
            name: name.clone(),
            code,
            same_name: same_name.clone(),
            reply,
        })
        .await
        .is_err()
    {
        return fail("operator gone");
    }
    let decision = match tokio::time::timeout(Duration::from_secs(60), wait).await {
        Ok(Ok(d)) => d,
        _ => Decision::default(),
    };
    let approved = decision.approve;
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
            // The operator chose to replace an earlier record of this phone: it is removed and its permissions carry over.
            let mut scopes = d.scopes.clone();
            if let Some(old) = decision
                .replace
                .as_deref()
                .filter(|o| same_name.iter().any(|s| s.id == *o))
            {
                let old_scopes: Vec<String> = app
                    .store
                    .lock()
                    .unwrap()
                    .get(old)
                    .map(|x| x.scopes.clone())
                    .unwrap_or_default();
                let removed = app.store.lock().unwrap().revoke(old).ok().flatten();
                if removed.is_some() {
                    for sc in old_scopes
                        .iter()
                        .filter(|s| !DEFAULT_SCOPES.contains(&s.as_str()))
                    {
                        if let Ok(Some(nd)) = app.store.lock().unwrap().set_scope(&d.id, sc, true) {
                            scopes = nd.scopes;
                        }
                    }
                    app.audit.record(
                        "pair_replaced",
                        Some(&d.id),
                        &format!("replaced an earlier record of {}", d.name),
                    );
                    let _ = app.hub.send(Hub::Revoked(old.to_string()));
                }
            }
            Json(json!({ "device_id": d.id, "name": d.name, "scopes": scopes })).into_response()
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

/// `Content-Disposition` for a download: an ASCII fallback plus the real name percent-encoded (RFC 6266).
fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._- ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let pct: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{pct}")
}

/// A phone streams a file it asked permission for (`file offer` over the WebSocket gave the one-time ticket).
/// The ticket is the credential; the size must match what was promised; nothing is kept unless it is complete.
async fn upload(
    State(app): State<Arc<App>>,
    Path(ticket): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    use crate::files::UploadError;
    let Some(files) = app.cfg.files.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if app.is_locked() {
        return (StatusCode::LOCKED, "the PC is locked").into_response();
    }
    let Some(offer) = files.take_upload(&ticket) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared != Some(offer.size) {
        return (
            StatusCode::BAD_REQUEST,
            "the size does not match what was offered",
        )
            .into_response();
    }
    let Some(_running) = files.begin_upload() else {
        return (StatusCode::TOO_MANY_REQUESTS, "too many uploads at once").into_response();
    };
    match files
        .receive(&offer, Box::pin(body.into_data_stream()))
        .await
    {
        Ok((name, size)) => {
            app.audit.record(
                "file_received",
                Some(&offer.device),
                &crate::files::human_size(size),
            );
            let who = app
                .store
                .lock()
                .unwrap()
                .get(&offer.device)
                .map(|d| crate::listen::clean_name(&d.name))
                .unwrap_or_default();
            crate::listen::notify_text(
                &app,
                &format!(
                    "{who} sent you {} ({})",
                    crate::files::display_name(&name),
                    crate::files::human_size(size)
                ),
            )
            .await;
            Json(json!({ "ok": true, "name": name, "size": size })).into_response()
        }
        Err(UploadError::TooLarge) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "more data than offered").into_response()
        }
        Err(UploadError::Short) | Err(UploadError::Aborted) => {
            (StatusCode::BAD_REQUEST, "the upload was not complete").into_response()
        }
        Err(UploadError::Io(_)) => (
            StatusCode::INSUFFICIENT_STORAGE,
            "the PC could not store the file",
        )
            .into_response(),
    }
}

/// A phone downloads a file the person at the PC offered and the phone accepted. Once.
async fn download(State(app): State<Arc<App>>, Path(ticket): Path<String>) -> Response {
    let Some(files) = app.cfg.files.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if app.is_locked() {
        return (StatusCode::LOCKED, "the PC is locked").into_response();
    }
    let Some(t) = files.take_download(&ticket) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(file) = tokio::fs::File::open(&t.path).await else {
        return StatusCode::GONE.into_response();
    };
    match file.metadata().await {
        Ok(m) if m.is_file() && m.len() == t.size => {}
        _ => return StatusCode::GONE.into_response(), // changed since it was offered
    }
    app.audit.record(
        "file_sent",
        Some(&t.device),
        &crate::files::human_size(t.size),
    );
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
    let mut res = Response::new(Body::from_stream(stream));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(t.size));
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&t.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/audio-worklet.js", get(worklet))
        .route("/api/hello", get(hello))
        .route("/api/pair", post(pair).layer(DefaultBodyLimit::max(4096)))
        .route("/ws", get(ws_handler))
        .route(
            "/upload/{ticket}",
            post(upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/download/{ticket}", get(download))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// How long a client may take to send the request line and headers.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

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
    let mut server = axum_server::bind_rustls(addr, cfg)
        .map(|a| a.acceptor(axum_server::accept::NoDelayAcceptor::new()))
        .handle(handle);
    // a client that opens a connection and never finishes its request headers is dropped (slow-loris)
    server
        .http_builder()
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    server
        .serve(router(app).into_make_service_with_connect_info::<SocketAddr>())
        .await
}
