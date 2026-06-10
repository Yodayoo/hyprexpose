use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use super::{ClientInfo, WorkspaceInfo};


fn socket_path() -> Option<String> {
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(format!("{xdg}/hypr/{sig}/.socket.sock"))
}

fn hypr_request(cmd: &str) -> Option<String> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).ok()?;
    let msg = format!("j/{cmd}");
    stream.write_all(msg.as_bytes()).ok()?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).ok()?;
    Some(resp)
}

// Minimal JSON helpers — avoids pulling in a JSON library for Hyprland's flat JSON.

fn json_string<'a>(json: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":");
    let Some(pos) = json.find(&needle) else { return "" };
    let rest = json[pos + needle.len()..].trim_start_matches([' ', '\t']);
    if rest.starts_with('"') {
        let inner = &rest[1..];
        let end = inner.find('"').unwrap_or(0);
        &inner[..end]
    } else {
        ""
    }
}

fn json_int(json: &str, key: &str) -> i64 {
    let needle = format!("\"{key}\":");
    let Some(pos) = json.find(&needle) else { return 0 };
    let rest = json[pos + needle.len()..].trim_start_matches([' ', '\t']);
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    num.parse().unwrap_or(0)
}

fn json_array_objects(json: &str) -> Vec<&str> {
    let mut objs = Vec::new();
    let bytes = json.as_bytes();
    let mut depth = 0i32;
    let mut start = 0;
    let mut in_string = false;

    for (i, &c) in bytes.iter().enumerate() {
        if c == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
        }
        if in_string {
            continue;
        }
        match c {
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    objs.push(&json[start..=i]);
                }
            }
            _ => {}
        }
    }
    objs
}

fn parse_address(json: &str) -> u64 {
    let s = json_string(json, "address");
    let hex = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(hex, 16).unwrap_or(0)
}

fn parse_array_pair(json: &str, key: &str) -> (i32, i32) {
    let needle = format!("\"{key}\":");
    let Some(pos) = json.find(&needle) else { return (0, 0) };
    let rest = &json[pos + needle.len()..];
    let Some(bracket) = rest.find('[') else { return (0, 0) };
    let inner = &rest[bracket + 1..];
    let a: i32 = inner
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    let Some(comma) = inner.find(',') else { return (a, 0) };
    let b: i32 = inner[comma + 1..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    (a, b)
}

pub fn get_workspaces() -> Vec<WorkspaceInfo> {
    let ws_json = hypr_request("workspaces").unwrap_or_default();
    let cl_json = hypr_request("clients").unwrap_or_default();

    let ws_objs = json_array_objects(&ws_json);
    let cl_objs = json_array_objects(&cl_json);

    let mut workspaces: Vec<WorkspaceInfo> = ws_objs
        .iter()
        .filter_map(|wj| {
            let id = json_int(wj, "id") as i32;
            if id < 1 {
                return None; // skip special workspaces
            }
            Some(WorkspaceInfo {
                id,
                name: json_string(wj, "name").to_owned(),
                monitor_id: json_int(wj, "monitorID") as i32,
                clients: Vec::new(),
            })
        })
        .collect();

    workspaces.sort_by_key(|w| w.id);

    for cj in &cl_objs {
        let mut workspace_id = json_int(cj, "workspace") as i32;

        // Hyprland may use nested "workspace":{"id":1,"name":"1"}
        if workspace_id == 0 {
            if let Some(pos) = cj.find("\"workspace\":") {
                if let Some(brace) = cj[pos..].find('{') {
                    let inner_start = pos + brace;
                    if let Some(end) = cj[inner_start..].find('}') {
                        let inner = &cj[inner_start..inner_start + end + 1];
                        workspace_id = json_int(inner, "id") as i32;
                    }
                }
            }
        }

        let (x, y) = parse_array_pair(cj, "at");
        let (w, h) = parse_array_pair(cj, "size");

        let ci = ClientInfo {
            class_name: json_string(cj, "class").to_owned(),
            title: json_string(cj, "title").to_owned(),
            address: parse_address(cj),
            workspace_id,
            x,
            y,
            w,
            h,
        };

        if let Some(ws) = workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.clients.push(ci);
        }
    }

    workspaces
}

fn dispatch(cmd: &str) {
    let Some(path) = socket_path() else { return };
    let Ok(mut stream) = UnixStream::connect(&path) else { return };
    let msg = format!("/dispatch {cmd}");
    let _ = stream.write_all(msg.as_bytes());
    let mut buf = [0u8; 256];
    let _ = stream.read(&mut buf);
}

pub fn switch_workspace(id: i32) {
    dispatch(&format!("hl.dsp.focus({{ workspace = \"{id}\" }})"));
}

pub fn move_window_to_workspace(window_address: u64, workspace_id: i32) {
    dispatch(&format!(
        "hl.dsp.window.move({{ workspace = {workspace_id}, window = \"address:0x{window_address:x}\", silent = true }})"
    ));
}

pub fn get_active_workspace_name() -> String {
    let json = hypr_request("activeworkspace").unwrap_or_default();
    json_string(&json, "name").to_owned()
}

/// Returns the address of the currently focused window, or 0 if none.
pub fn get_active_window_address() -> u64 {
    let json = hypr_request("activewindow").unwrap_or_default();
    // Hyprland returns `{}` when no window is focused
    if json.trim() == "{}" {
        return 0;
    }
    parse_address(&json)
}
