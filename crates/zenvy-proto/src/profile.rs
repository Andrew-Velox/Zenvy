use crate::Monitor;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Local-only device profile (PLAN.md §6). Contains **no** serials, machine ids, MAC addresses
/// or hostnames. Never transmitted as a whole; the phone gets a filtered subset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub schema_version: u32,
    pub os: OsInfo,
    pub session: SessionInfo,
    pub hardware: HardwareInfo,
    pub displays: Vec<Monitor>,
    pub peripherals: Peripherals,
    /// Tool presence only (path or null). NOT proof that a capability works.
    pub software_hints: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Pacman,
    Dnf,
    Apt,
    Zypper,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsInfo {
    pub distro_id: String,
    pub distro_like: Vec<String>,
    pub version_id: Option<String>,
    pub kernel: String,
    pub arch: String,
    pub package_manager: PackageManager,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_type: String,
    pub compositor: String,
    pub wayland_display: bool,
    pub wayland_protocols: Vec<String>,
    /// Allow-listed variables only.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormFactor {
    Laptop,
    Desktop,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub vendor: String,
    pub driver: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub cpu_model: String,
    pub cpu_threads: u32,
    pub ram_mb: u64,
    pub gpus: Vec<Gpu>,
    pub form_factor: FormFactor,
    pub battery_present: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peripherals {
    pub keyboards: u32,
    pub pointers: u32,
    pub touchpads: u32,
    pub gamepads: u32,
    pub audio_outputs: u32,
    pub audio_inputs: u32,
}
