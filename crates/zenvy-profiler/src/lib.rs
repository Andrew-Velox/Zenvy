//! Device profiler + system capability probes (PLAN.md §6–7).
//!
//! Everything reads from a `root` directory (`/` in production, a temp tree in tests) so
//! distro/hardware fixtures can be tested without touching the real machine. Nothing here
//! reads serials, machine-ids, MAC addresses or hostnames.

mod facts;
mod probes;
mod remediation;
mod wayland;

pub use facts::{collect, parse_os_release, Env};
pub use probes::{probe_system, Access};
pub use remediation::attach_remediation;
pub use wayland::wayland_globals;
