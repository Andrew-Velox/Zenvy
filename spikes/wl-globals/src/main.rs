//! Spike 3: list the Wayland globals Hyprland advertises (read-only registry query).
use wayland_client::{globals::registry_queue_init, Connection};
struct S;
impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for S {
    fn event(_: &mut Self, _: &wayland_client::protocol::wl_registry::WlRegistry, _: wayland_client::protocol::wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents, _: &Connection, _: &wayland_client::QueueHandle<Self>) {}
}
fn main() {
    let conn = Connection::connect_to_env().unwrap();
    let (globals, _q) = registry_queue_init::<S>(&conn).unwrap();
    let mut names: Vec<_> = globals.contents().clone_list().into_iter().map(|g| format!("{} v{}", g.interface, g.version)).collect();
    names.sort();
    for n in names { println!("{n}"); }
}
