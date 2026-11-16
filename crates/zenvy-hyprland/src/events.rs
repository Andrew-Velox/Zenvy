//! `.socket2.sock` event stream: parsing, title coalescing, reconnect with backoff.

use crate::client::resolve_instance_dir;
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zenvy_proto::Address;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Workspace {
        id: i64,
        name: String,
    },
    ActiveWindow {
        address: Option<Address>,
    },
    OpenWindow {
        address: Address,
        workspace: String,
        class: String,
        title: String,
    },
    CloseWindow {
        address: Address,
    },
    MoveWindow {
        address: Address,
        workspace: i64,
    },
    WindowTitle {
        address: Address,
        title: String,
    },
    FocusedMonitor {
        monitor: String,
        workspace: i64,
    },
    MonitorAdded {
        name: String,
    },
    MonitorRemoved {
        name: String,
    },
    Fullscreen {
        on: bool,
    },
    ConfigReloaded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItem {
    Event(Event),
    Connected,
    Disconnected,
}

/// Parses one `name>>data` line. Legacy v1 events that have a v2 twin return `None`
/// (we consume only the v2 forms); unknown events also return `None`.
pub fn parse_event(line: &str) -> Option<Event> {
    let (name, data) = line.trim_end().split_once(">>")?;
    match name {
        "workspacev2" => {
            let (id, name) = data.split_once(',')?;
            Some(Event::Workspace {
                id: id.parse().ok()?,
                name: name.into(),
            })
        }
        "activewindowv2" => Some(Event::ActiveWindow {
            address: Address::parse(data).ok(),
        }),
        "openwindow" => {
            let mut p = data.splitn(4, ',');
            Some(Event::OpenWindow {
                address: Address::parse(p.next()?).ok()?,
                workspace: p.next()?.into(),
                class: p.next()?.into(),
                title: p.next().unwrap_or("").into(), // titles may contain commas: 4th field is the rest
            })
        }
        "closewindow" => Some(Event::CloseWindow {
            address: Address::parse(data).ok()?,
        }),
        "movewindowv2" => {
            let mut p = data.splitn(3, ',');
            Some(Event::MoveWindow {
                address: Address::parse(p.next()?).ok()?,
                workspace: p.next()?.parse().ok()?,
            })
        }
        "windowtitlev2" => {
            let (addr, title) = data.split_once(',')?;
            Some(Event::WindowTitle {
                address: Address::parse(addr).ok()?,
                title: title.into(),
            })
        }
        "focusedmonv2" => {
            let (mon, ws) = data.rsplit_once(',')?;
            Some(Event::FocusedMonitor {
                monitor: mon.into(),
                workspace: ws.parse().ok()?,
            })
        }
        "monitoraddedv2" => Some(Event::MonitorAdded {
            name: data.split(',').nth(1)?.into(),
        }),
        "monitorremovedv2" => Some(Event::MonitorRemoved {
            name: data.split(',').nth(1)?.into(),
        }),
        "fullscreen" => Some(Event::Fullscreen { on: data == "1" }),
        "configreloaded" => Some(Event::ConfigReloaded),
        _ => None,
    }
}

/// Title changes can arrive several times per second (a terminal animating its title was measured
/// at ~15 events in 8 s). They are held back and only the latest title per window is forwarded
/// at most once per `TITLE_WINDOW`. No timer runs while nothing is pending.
const TITLE_WINDOW: Duration = Duration::from_millis(250);

/// Starts the event task. It reconnects forever (re-resolving the instance directory, because the
/// signature changes when Hyprland restarts) with exponential backoff capped at 5 s.
pub fn subscribe() -> mpsc::Receiver<StreamItem> {
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut backoff = Duration::from_millis(200);
        loop {
            if tx.is_closed() {
                return;
            }
            match run_once(&tx).await {
                Ok(()) => backoff = Duration::from_millis(200),
                Err(e) => tracing::debug!("event stream: {e}"),
            }
            if tx.send(StreamItem::Disconnected).await.is_err() {
                return;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    });
    rx
}

async fn run_once(tx: &mpsc::Sender<StreamItem>) -> std::io::Result<()> {
    let dir = resolve_instance_dir().map_err(|e| std::io::Error::other(e.to_string()))?;
    let stream = UnixStream::connect(dir.join(".socket2.sock")).await?;
    if tx.send(StreamItem::Connected).await.is_err() {
        return Ok(());
    }
    let mut lines = BufReader::new(stream).lines();
    let mut pending: HashMap<Address, String> = HashMap::new();
    let mut deadline: Option<Instant> = None;
    let mut last_active: Option<Option<Address>> = None;

    loop {
        let flush = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                match parse_event(&line) {
                    Some(Event::WindowTitle { address, title }) => {
                        pending.insert(address, title);
                        deadline.get_or_insert_with(|| Instant::now() + TITLE_WINDOW);
                    }
                    Some(ev) => {
                        // Hyprland repeats `activewindowv2` for the same window; forward changes only.
                        if let Event::ActiveWindow { address } = &ev {
                            if last_active.as_ref() == Some(address) { continue; }
                            last_active = Some(address.clone());
                        }
                        if let Event::CloseWindow { address } = &ev { pending.remove(address); }
                        if tx.send(StreamItem::Event(ev)).await.is_err() { return Ok(()); }
                    }
                    None => {}
                }
            }
            _ = flush => {
                deadline = None;
                for (address, title) in pending.drain() {
                    if tx.send(StreamItem::Event(Event::WindowTitle { address, title })).await.is_err() { return Ok(()); }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_seen_on_the_reference_machine() {
        assert_eq!(
            parse_event("activewindowv2>>55f131746e40"),
            Some(Event::ActiveWindow {
                address: Address::parse("0x55f131746e40").ok()
            })
        );
        assert_eq!(
            parse_event("windowtitlev2>>55f131746e40,◑ Adaptive Hyprland remote control"),
            Some(Event::WindowTitle {
                address: Address::parse("55f131746e40").unwrap(),
                title: "◑ Adaptive Hyprland remote control".into()
            })
        );
        assert_eq!(
            parse_event("workspacev2>>3,3"),
            Some(Event::Workspace {
                id: 3,
                name: "3".into()
            })
        );
    }

    #[test]
    fn v1_duplicates_are_ignored() {
        for l in [
            "windowtitle>>55f131746e40",
            "activewindow>>kitty,title",
            "workspace>>3",
            "focusedmon>>HDMI-A-1,3",
        ] {
            assert_eq!(parse_event(l), None, "{l}");
        }
    }

    #[test]
    fn titles_with_commas_survive() {
        let e = parse_event("openwindow>>55f1,3,kitty,a, b, c").unwrap();
        assert!(matches!(e, Event::OpenWindow { title, .. } if title == "a, b, c"));
    }

    #[test]
    fn garbage_is_none_not_panic() {
        for l in [
            "",
            ">>",
            "x",
            "workspacev2>>x,y",
            "closewindow>>zz",
            "openwindow>>",
            "movewindowv2>>1",
        ] {
            assert_eq!(parse_event(l), None, "{l:?}");
        }
    }
}
