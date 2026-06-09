mod hyprland;
mod sway;

use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub class_name: String,
    pub title: String,
    pub address: u64,
    #[allow(dead_code)]
    pub workspace_id: i32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub id: i32,
    pub name: String,
    #[allow(dead_code)]
    pub monitor_id: i32,
    pub clients: Vec<ClientInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Hyprland,
    Sway,
}

impl Compositor {
    pub fn name(self) -> &'static str {
        match self {
            Compositor::Hyprland => "Hyprland",
            Compositor::Sway => "Sway",
        }
    }
}

pub fn compositor() -> Compositor {
    static COMP: OnceLock<Compositor> = OnceLock::new();
    *COMP.get_or_init(|| {
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
            Compositor::Hyprland
        } else if std::env::var_os("SWAYSOCK").is_some() || std::env::var_os("I3SOCK").is_some() {
            Compositor::Sway
        } else {
            Compositor::Hyprland
        }
    })
}

pub fn get_workspaces() -> Vec<WorkspaceInfo> {
    match compositor() {
        Compositor::Hyprland => hyprland::get_workspaces(),
        Compositor::Sway => sway::get_workspaces(),
    }
}

pub fn switch_workspace(ws: &WorkspaceInfo) {
    match compositor() {
        Compositor::Hyprland => hyprland::switch_workspace(ws.id),
        Compositor::Sway => sway::switch_workspace(&ws.name),
    }
}

pub fn move_window_to_workspace(window_address: u64, ws: &WorkspaceInfo) {
    match compositor() {
        Compositor::Hyprland => hyprland::move_window_to_workspace(window_address, ws.id),
        Compositor::Sway => sway::move_window_to_workspace(window_address, &ws.name),
    }
}

/// Name of the currently focused workspace, or empty string if unknown.
pub fn get_active_workspace_name() -> String {
    match compositor() {
        Compositor::Hyprland => hyprland::get_active_workspace_name(),
        Compositor::Sway => sway::get_active_workspace_name(),
    }
}

/// Returns the address (Hyprland) or container id (Sway) of the currently
/// focused window, or 0 if none.
pub fn get_active_window_address() -> u64 {
    match compositor() {
        Compositor::Hyprland => hyprland::get_active_window_address(),
        Compositor::Sway => sway::get_active_window_address(),
    }
}
