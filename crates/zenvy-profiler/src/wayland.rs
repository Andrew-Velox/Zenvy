use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{Event, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

struct S;
impl Dispatch<WlRegistry, GlobalListContents> for S {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Names of the Wayland globals the compositor advertises (read-only registry query),
/// or `None` when there is no reachable Wayland session.
pub fn wayland_globals() -> Option<Vec<String>> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, _q) = registry_queue_init::<S>(&conn).ok()?;
    let mut v: Vec<String> = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|g| g.interface)
        .collect();
    v.sort();
    v.dedup();
    Some(v)
}
