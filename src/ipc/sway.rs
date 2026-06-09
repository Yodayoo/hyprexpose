use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use serde::Deserialize;

use super::{ClientInfo, WorkspaceInfo};

// i3/sway IPC message types
const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const GET_TREE: u32 = 4;

const IPC_MAGIC: &[u8; 6] = b"i3-ipc";

fn socket_path() -> Option<String> {
    std::env::var("SWAYSOCK")
        .ok()
        .or_else(|| std::env::var("I3SOCK").ok())
}

fn ipc_request(msg_type: u32, payload: &str) -> Option<Vec<u8>> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).ok()?;

    let mut msg = Vec::with_capacity(14 + payload.len());
    msg.extend_from_slice(IPC_MAGIC);
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&msg_type.to_ne_bytes());
    msg.extend_from_slice(payload.as_bytes());
    stream.write_all(&msg).ok()?;

    let mut header = [0u8; 14];
    stream.read_exact(&mut header).ok()?;
    if &header[0..6] != IPC_MAGIC {
        return None;
    }
    let len = u32::from_ne_bytes(header[6..10].try_into().ok()?) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn run_command(cmd: &str) {
    let _ = ipc_request(RUN_COMMAND, cmd);
}

/// Escape a workspace name for use inside a double-quoted sway command argument.
fn quote(name: &str) -> String {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

// ── IPC reply types ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SwayRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Deserialize)]
struct SwayWorkspace {
    num: i32,
    name: String,
    #[serde(default)]
    focused: bool,
}

#[derive(Deserialize)]
struct WindowProperties {
    #[serde(default)]
    class: Option<String>,
}

#[derive(Deserialize)]
struct SwayNode {
    id: u64,
    #[serde(rename = "type")]
    node_type: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    window_properties: Option<WindowProperties>,
    #[serde(default)]
    pid: Option<i32>,
    rect: SwayRect,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    nodes: Vec<SwayNode>,
    #[serde(default)]
    floating_nodes: Vec<SwayNode>,
}

impl SwayNode {
    /// Views (actual windows) are the only tree nodes carrying a pid.
    fn is_view(&self) -> bool {
        self.pid.is_some()
    }

    fn children(&self) -> impl Iterator<Item = &SwayNode> {
        self.nodes.iter().chain(self.floating_nodes.iter())
    }
}

fn get_tree() -> Option<SwayNode> {
    let raw = ipc_request(GET_TREE, "")?;
    serde_json::from_slice(&raw).ok()
}

fn collect_views(node: &SwayNode, workspace_id: i32, out: &mut Vec<ClientInfo>) {
    if node.is_view() {
        let class_name = node
            .app_id
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| node.window_properties.as_ref().and_then(|p| p.class.clone()))
            .unwrap_or_default();
        out.push(ClientInfo {
            class_name,
            title: node.name.clone().unwrap_or_default(),
            address: node.id,
            workspace_id,
            x: node.rect.x,
            y: node.rect.y,
            w: node.rect.width,
            h: node.rect.height,
        });
    }
    for child in node.children() {
        collect_views(child, workspace_id, out);
    }
}

fn fill_workspace_clients(node: &SwayNode, workspaces: &mut [WorkspaceInfo]) {
    if node.node_type == "workspace" {
        let Some(name) = &node.name else { return };
        if let Some(ws) = workspaces.iter_mut().find(|w| &w.name == name) {
            let id = ws.id;
            for child in node.children() {
                collect_views(child, id, &mut ws.clients);
            }
        }
        return;
    }
    for child in node.children() {
        fill_workspace_clients(child, workspaces);
    }
}

pub fn get_workspaces() -> Vec<WorkspaceInfo> {
    let raw = ipc_request(GET_WORKSPACES, "").unwrap_or_default();
    let ws_list: Vec<SwayWorkspace> = serde_json::from_slice(&raw).unwrap_or_default();

    // Keep sway's own ordering (per output, numbered first); skip internal
    // workspaces like __i3_scratch.
    let mut workspaces: Vec<WorkspaceInfo> = ws_list
        .iter()
        .filter(|w| !w.name.starts_with("__"))
        .map(|w| WorkspaceInfo {
            id: w.num,
            name: w.name.clone(),
            monitor_id: 0,
            clients: Vec::new(),
        })
        .collect();

    if let Some(tree) = get_tree() {
        fill_workspace_clients(&tree, &mut workspaces);
    }

    workspaces
}

pub fn switch_workspace(name: &str) {
    run_command(&format!("workspace {}", quote(name)));
}

pub fn move_window_to_workspace(con_id: u64, name: &str) {
    run_command(&format!(
        "[con_id={con_id}] move container to workspace {}",
        quote(name)
    ));
}

pub fn get_active_workspace_name() -> String {
    let raw = ipc_request(GET_WORKSPACES, "").unwrap_or_default();
    let ws_list: Vec<SwayWorkspace> = serde_json::from_slice(&raw).unwrap_or_default();
    ws_list
        .into_iter()
        .find(|w| w.focused)
        .map(|w| w.name)
        .unwrap_or_default()
}

fn find_focused_view(node: &SwayNode) -> Option<u64> {
    if node.focused && node.is_view() {
        return Some(node.id);
    }
    node.children().find_map(find_focused_view)
}

pub fn get_active_window_address() -> u64 {
    get_tree()
        .as_ref()
        .and_then(find_focused_view)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    const WORKSPACES_JSON: &str = r#"[
        {"num": 1, "name": "1", "focused": false, "rect": {"x":0,"y":0,"width":1920,"height":1080}},
        {"num": -1, "name": "web", "focused": true, "rect": {"x":0,"y":0,"width":1920,"height":1080}},
        {"num": -1, "name": "__i3_scratch", "focused": false, "rect": {"x":0,"y":0,"width":0,"height":0}}
    ]"#;

    const TREE_JSON: &str = r#"{
        "id": 1, "type": "root", "name": "root",
        "rect": {"x":0,"y":0,"width":1920,"height":1080},
        "nodes": [{
            "id": 2, "type": "output", "name": "eDP-1",
            "rect": {"x":0,"y":0,"width":1920,"height":1080},
            "nodes": [
                {
                    "id": 3, "type": "workspace", "name": "1", "num": 1,
                    "rect": {"x":0,"y":0,"width":1920,"height":1080},
                    "nodes": [{
                        "id": 10, "type": "con", "name": "vim", "app_id": "foot", "pid": 100,
                        "rect": {"x":0,"y":0,"width":960,"height":1080},
                        "focused": true
                    }],
                    "floating_nodes": [{
                        "id": 11, "type": "floating_con", "name": "calc", "app_id": null, "pid": 101,
                        "window_properties": {"class": "Gnome-calculator"},
                        "rect": {"x":100,"y":100,"width":400,"height":300}
                    }]
                },
                {
                    "id": 4, "type": "workspace", "name": "web", "num": -1,
                    "rect": {"x":0,"y":0,"width":1920,"height":1080},
                    "nodes": []
                }
            ]
        }]
    }"#;

    fn serve_one(listener: &UnixListener) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut header = [0u8; 14];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..6], IPC_MAGIC);
        let len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
        let msg_type = u32::from_ne_bytes(header[10..14].try_into().unwrap());
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).unwrap();

        let body = match msg_type {
            GET_WORKSPACES => WORKSPACES_JSON,
            GET_TREE => TREE_JSON,
            _ => "[]",
        };
        let mut reply = Vec::new();
        reply.extend_from_slice(IPC_MAGIC);
        reply.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        reply.extend_from_slice(&msg_type.to_ne_bytes());
        reply.extend_from_slice(body.as_bytes());
        stream.write_all(&reply).unwrap();
    }

    #[test]
    fn parses_workspaces_and_tree_from_mock_socket() {
        let dir = std::env::temp_dir().join(format!("hyprexpose-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("sway.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::env::set_var("SWAYSOCK", &sock);

        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                serve_one(&listener);
            }
        });

        let workspaces = get_workspaces();
        assert_eq!(workspaces.len(), 2, "scratchpad workspace must be skipped");

        assert_eq!(workspaces[0].id, 1);
        assert_eq!(workspaces[0].name, "1");
        assert_eq!(workspaces[0].clients.len(), 2);
        assert_eq!(workspaces[0].clients[0].class_name, "foot");
        assert_eq!(workspaces[0].clients[0].address, 10);
        assert_eq!(workspaces[0].clients[1].class_name, "Gnome-calculator");

        assert_eq!(workspaces[1].name, "web");
        assert!(workspaces[1].clients.is_empty());

        assert_eq!(get_active_window_address(), 10);

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn quote_escapes_specials() {
        assert_eq!(quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }
}
