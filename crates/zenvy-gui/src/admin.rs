//! The agent's local admin socket (`$XDG_RUNTIME_DIR/zenvy/admin.sock`, mode 0600, never on the network): the same
//! interface the command line uses. One JSON object per line in each direction.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// `$XDG_RUNTIME_DIR/zenvy/admin.sock`, or the path in `ZENVY_ADMIN_SOCKET` (for tests and demos).
pub fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZENVY_ADMIN_SOCKET") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("zenvy/admin.sock"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub version: String,
    pub address: String,
    pub locked: bool,
    pub phones_online: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: Option<u64>,
    pub last_seen: Option<u64>,
    pub online: bool,
}

const NOT_RUNNING: &str = "The Zenvy agent is not running";

fn connect() -> Result<UnixStream, String> {
    let path = socket_path().ok_or("XDG_RUNTIME_DIR is not set")?;
    let s = UnixStream::connect(path).map_err(|_| NOT_RUNNING.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(4))).ok();
    s.set_write_timeout(Some(Duration::from_secs(4))).ok();
    Ok(s)
}

fn request(op: Value) -> Result<Value, String> {
    let mut s = connect()?;
    writeln!(s, "{op}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s)
        .read_line(&mut line)
        .map_err(|_| NOT_RUNNING.to_string())?;
    serde_json::from_str(&line).map_err(|_| "The agent sent something unexpected".to_string())
}

pub fn parse_status(v: &Value) -> Option<Status> {
    if v["type"] != "status" {
        return None;
    }
    Some(Status {
        version: v["version"].as_str()?.to_string(),
        address: v["address"].as_str().unwrap_or("").to_string(),
        locked: v["locked"].as_bool().unwrap_or(false),
        phones_online: v["phones_online"].as_u64().unwrap_or(0),
    })
}

pub fn parse_devices(v: &Value) -> Option<Vec<Device>> {
    if v["type"] != "devices" {
        return None;
    }
    Some(
        v["devices"]
            .as_array()?
            .iter()
            .filter_map(|d| {
                Some(Device {
                    id: d["id"].as_str()?.to_string(),
                    name: d["name"].as_str().unwrap_or("Phone").to_string(),
                    scopes: d["scopes"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    created_at: d["created_at"].as_u64(),
                    last_seen: d["last_seen"].as_u64(),
                    online: d["online"].as_bool().unwrap_or(false),
                })
            })
            .collect(),
    )
}

pub fn poll() -> Result<(Status, Vec<Device>), String> {
    let st = parse_status(&request(json!({ "op": "status" }))?)
        .ok_or("The agent is older than this window: update it")?;
    let devs = parse_devices(&request(json!({ "op": "devices" }))?)
        .ok_or("The agent sent something unexpected")?;
    Ok((st, devs))
}

fn expect_ok(v: Value) -> Result<(), String> {
    if v["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(v["message"]
            .as_str()
            .unwrap_or("That did not work")
            .to_string())
    }
}

pub fn set_scope(id: &str, scope: &str, on: bool) -> Result<(), String> {
    expect_ok(request(
        json!({ "op": if on { "grant" } else { "ungrant" }, "id": id, "scope": scope }),
    )?)
}

pub fn revoke(id: &str) -> Result<(), String> {
    expect_ok(request(json!({ "op": "revoke", "id": id }))?)
}

/// What happens during pairing, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum PairEvent {
    /// The QR link to show, and how long it stays valid.
    Link {
        url: String,
        ttl: u64,
    },
    /// A phone scanned it and asks to be approved; `code` must match the one on the phone.
    Approval {
        name: String,
        code: String,
    },
    /// approved | denied | cancelled | expired
    Done(String),
    Error(String),
}

/// A running pairing session. Dropping it (or `cancel`) closes the socket, which cancels pairing on the agent.
pub struct Pairing {
    pub events: Receiver<PairEvent>,
    answer: Sender<bool>,
    stream: UnixStream,
}

impl Pairing {
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Result<Pairing, String> {
        let mut s = connect()?;
        s.set_read_timeout(None).ok();
        writeln!(s, "{}", json!({ "op": "pair" })).map_err(|e| e.to_string())?;
        let (tx, events) = channel();
        let (answer, answers) = channel::<bool>();
        let reader = s.try_clone().map_err(|e| e.to_string())?;
        let mut writer = s.try_clone().map_err(|e| e.to_string())?;
        let wake = std::sync::Arc::new(wake);
        let w2 = wake.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let ev = match v["type"].as_str() {
                    Some("pairing") => PairEvent::Link {
                        url: v["url"].as_str().unwrap_or("").to_string(),
                        ttl: v["ttl"].as_u64().unwrap_or(300),
                    },
                    Some("approval") => PairEvent::Approval {
                        name: v["name"].as_str().unwrap_or("A phone").to_string(),
                        code: v["code"].as_str().unwrap_or("").to_string(),
                    },
                    Some("done") => {
                        PairEvent::Done(v["result"].as_str().unwrap_or("cancelled").to_string())
                    }
                    Some("error") => PairEvent::Error(
                        v["message"]
                            .as_str()
                            .unwrap_or("Pairing failed")
                            .to_string(),
                    ),
                    _ => continue,
                };
                let last = matches!(ev, PairEvent::Done(_) | PairEvent::Error(_));
                if tx.send(ev).is_err() {
                    break;
                }
                w2();
                if last {
                    return;
                }
            }
            let _ = tx.send(PairEvent::Done("cancelled".into()));
            w2();
        });
        std::thread::spawn(move || {
            while let Ok(a) = answers.recv() {
                if writeln!(writer, "{}", json!({ "approve": a })).is_err() {
                    break;
                }
            }
        });
        Ok(Pairing {
            events,
            answer,
            stream: s,
        })
    }

    pub fn answer(&self, approve: bool) {
        let _ = self.answer.send(approve);
    }
}

impl Drop for Pairing {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_devices_are_parsed_and_junk_is_refused() {
        let s = parse_status(&json!({ "type": "status", "version": "0.1.0", "address": "https://192.168.1.20:7878", "locked": true, "phones_online": 2 })).unwrap();
        assert_eq!(
            (s.version.as_str(), s.locked, s.phones_online),
            ("0.1.0", true, 2)
        );
        assert!(parse_status(&json!({ "type": "error" })).is_none());
        assert!(
            parse_status(&json!({ "type": "status" })).is_none(),
            "a status without a version is not trusted"
        );
        let d = parse_devices(&json!({ "type": "devices", "devices": [
            { "id": "abc", "name": "Pixel", "scopes": ["view", "screen"], "last_seen": 1700000000, "online": true },
            { "id": "def" }, { "name": "no id" }, 5 ] })).unwrap();
        assert_eq!(d.len(), 2, "entries without an id are dropped");
        assert_eq!(d[0].scopes, ["view", "screen"]);
        assert!(d[0].online && !d[1].online && d[1].last_seen.is_none() && d[1].name == "Phone");
        assert!(parse_devices(&json!({ "type": "devices" })).is_none());
    }

    #[test]
    fn pairing_follows_the_agent_through_link_approval_and_result() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("zenvy-gui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("admin.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::env::set_var("ZENVY_ADMIN_SOCKET", &sock);
        let agent = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            assert!(line.contains("\"pair\""), "{line}");
            writeln!(
                s,
                "{}",
                json!({ "type": "pairing", "url": "https://192.168.1.20:7878/#s=x", "ttl": 120 })
            )
            .unwrap();
            writeln!(
                s,
                "{}",
                json!({ "type": "approval", "name": "Pixel", "code": "4815" })
            )
            .unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            let approved = serde_json::from_str::<Value>(&line).unwrap()["approve"] == true;
            writeln!(
                s,
                "{}",
                json!({ "type": "done", "result": if approved { "approved" } else { "denied" } })
            )
            .unwrap();
        });
        let p = Pairing::start(|| {}).unwrap();
        let next = |p: &Pairing| p.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            next(&p),
            PairEvent::Link {
                url: "https://192.168.1.20:7878/#s=x".into(),
                ttl: 120
            }
        );
        assert_eq!(
            next(&p),
            PairEvent::Approval {
                name: "Pixel".into(),
                code: "4815".into()
            }
        );
        p.answer(true);
        assert_eq!(next(&p), PairEvent::Done("approved".into()));
        agent.join().unwrap();
        drop(p);
        std::env::remove_var("ZENVY_ADMIN_SOCKET");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
