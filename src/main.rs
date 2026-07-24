mod config;
mod ipc;
mod render;

use config::Config;
use render::Thumbnail;
use ipc::WorkspaceInfo;

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use wayland_client::{
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

// Generated Hyprland protocol bindings
#[allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
#[allow(non_upper_case_globals, non_snake_case, unused_imports, missing_docs)]
#[allow(clippy::all)]
pub mod hyprland_toplevel_export {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-export-v1.xml");
}
use hyprland_toplevel_export::{
    hyprland_toplevel_export_frame_v1, hyprland_toplevel_export_manager_v1,
};
use hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1;
use hyprland_toplevel_export_frame_v1::HyprlandToplevelExportFrameV1;

// ── signal handling ─────────────────────────────────────────────────────────

static G_QUIT: AtomicBool = AtomicBool::new(false);
static G_TOGGLE: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn sig_handler(sig: libc::c_int) {
    if sig == libc::SIGUSR1 {
        G_TOGGLE.store(true, Ordering::Relaxed);
    } else {
        G_QUIT.store(true, Ordering::Relaxed);
    }
}

// ── key mapping (evdev keycodes → XKB keysyms) ──────────────────────────────

const XKB_KEY_ESCAPE: u32 = 0xff1b;
const XKB_KEY_RETURN: u32 = 0xff0d;
const XKB_KEY_LEFT:   u32 = 0xff51;
const XKB_KEY_UP:     u32 = 0xff52;
const XKB_KEY_RIGHT:  u32 = 0xff53;
const XKB_KEY_DOWN:   u32 = 0xff54;
const XKB_KEY_H: u32 = 0x0068;
const XKB_KEY_J: u32 = 0x006a;
const XKB_KEY_K: u32 = 0x006b;
const XKB_KEY_L: u32 = 0x006c;
const XKB_KEY_M: u32 = 0x006d;
const XKB_KEY_1: u32 = 0x0031;
const XKB_KEY_2: u32 = 0x0032;
const XKB_KEY_3: u32 = 0x0033;
const XKB_KEY_4: u32 = 0x0034;
const XKB_KEY_5: u32 = 0x0035;
const XKB_KEY_6: u32 = 0x0036;
const XKB_KEY_7: u32 = 0x0037;
const XKB_KEY_8: u32 = 0x0038;
const XKB_KEY_9: u32 = 0x0039;

fn keycode_to_keysym(keycode: u32) -> u32 {
    // Compositors send raw evdev keycodes via wl_keyboard.
    match keycode {
        1   => XKB_KEY_ESCAPE,
        28  => XKB_KEY_RETURN,
        105 => XKB_KEY_LEFT,
        103 => XKB_KEY_UP,
        106 => XKB_KEY_RIGHT,
        108 => XKB_KEY_DOWN,
        35  => XKB_KEY_H,
        36  => XKB_KEY_J,
        37  => XKB_KEY_K,
        38  => XKB_KEY_L,
        50  => XKB_KEY_M,
        2   => XKB_KEY_1,
        3   => XKB_KEY_2,
        4   => XKB_KEY_3,
        5   => XKB_KEY_4,
        6   => XKB_KEY_5,
        7   => XKB_KEY_6,
        8   => XKB_KEY_7,
        9   => XKB_KEY_8,
        10  => XKB_KEY_9,
        _   => 0,
    }
}

// ── capture frame state ──────────────────────────────────────────────────────

#[derive(Default)]
struct CaptureFrameData {
    format: u32,
    width: u32,
    height: u32,
    stride: u32,
    buffer_done: bool,
    frame_ready: bool,
    frame_failed: bool,
}

// ── application state ────────────────────────────────────────────────────────

struct AppState {
    // Wayland globals
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    toplevel_export: Option<HyprlandToplevelExportManagerV1>,

    // Per-show Wayland objects (recreated on each show)
    wl_surf: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    slab: Option<ShmSlab>,

    // Surface / overlay state
    width: u32,
    height: u32,
    configured: bool,
    visible: bool,
    should_close: bool,
    needs_redraw: bool,

    // App state
    workspaces: Vec<WorkspaceInfo>,
    thumbnails: Vec<Thumbnail>,
    /// Pre-rendered selection-independent scene; rebuilt on show/resize.
    scene: Option<render::SceneCache>,
    selected: usize,
    no_preview: bool,
    active_window_address: u64,
    config: Config,

    // Mouse support (only active when --allow-mouse is set).
    allow_mouse: bool,
    pointer_x: f64,
    pointer_y: f64,

    // In-flight capture state (sequential, one at a time)
    pending_capture: Option<CaptureFrameData>,
}

impl AppState {
    fn new(no_preview: bool, allow_mouse: bool, config: Config) -> Self {
        // CLI flags override config values (OR semantics: flag=true forces on)
        let no_preview = no_preview || config.behavior.no_preview;
        let allow_mouse = allow_mouse || config.behavior.allow_mouse;
        Self {
            compositor: None,
            shm: None,
            seat: None,
            layer_shell: None,
            toplevel_export: None,
            wl_surf: None,
            layer_surface: None,
            slab: None,
            width: 0,
            height: 0,
            configured: false,
            visible: false,
            should_close: false,
            needs_redraw: false,
            workspaces: Vec::new(),
            thumbnails: Vec::new(),
            scene: None,
            selected: 0,
            no_preview,
            active_window_address: 0,
            config,
            allow_mouse,
            pointer_x: 0.0,
            pointer_y: 0.0,
            pending_capture: None,
        }
    }

    /// Returns true if the overlay should be closed.
    fn handle_key(&mut self, keysym: u32) -> bool {
        let n = self.workspaces.len();
        if n == 0 {
            return true;
        }
        let cols = ((n as f64).sqrt().ceil() as usize).max(1);

        match keysym {
            XKB_KEY_ESCAPE => return true,
            XKB_KEY_1 ..= XKB_KEY_9 => {
                let idx = (keysym - XKB_KEY_1) as usize;
                if idx < n {
                    ipc::switch_workspace(&self.workspaces[idx]);
                }
                return true;
            }
            XKB_KEY_RETURN => {
                self.activate_selected_workspace();
                return true;
            }
            XKB_KEY_RIGHT | XKB_KEY_L => {
                if self.selected + 1 < n {
                    self.selected += 1;
                    self.needs_redraw = true;
                }
            }
            XKB_KEY_LEFT | XKB_KEY_H => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.needs_redraw = true;
                }
            }
            XKB_KEY_DOWN | XKB_KEY_J => {
                if self.selected + cols < n {
                    self.selected += cols;
                    self.needs_redraw = true;
                }
            }
            XKB_KEY_UP | XKB_KEY_K => {
                if self.selected >= cols {
                    self.selected -= cols;
                    self.needs_redraw = true;
                }
            }
            XKB_KEY_M => {
                if self.move_active_window_to_selected() {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn activate_selected_workspace(&self) {
        if self.selected < self.workspaces.len() {
            ipc::switch_workspace(&self.workspaces[self.selected]);
        }
    }

    /// Move the active window to the selected workspace. Returns true on success
    /// (overlay should close).
    fn move_active_window_to_selected(&self) -> bool {
        if self.active_window_address == 0 || self.selected >= self.workspaces.len() {
            return false;
        }
        let target = &self.workspaces[self.selected];
        ipc::move_window_to_workspace(self.active_window_address, target);
        if self.config.behavior.switch_on_move {
            ipc::switch_workspace(target);
        }
        true
    }

    fn workspace_at(&self, x: f64, y: f64) -> Option<usize> {
        let n = self.workspaces.len();
        if n == 0 || self.width == 0 || self.height == 0 {
            return None;
        }

        let cols = ((n as f64).sqrt().ceil() as usize).max(1);
        let rows = (n + cols - 1) / cols;
        let pad = self.config.appearance.card_padding;

        let card_w = ((self.width as f64 - pad * (cols + 1) as f64) / cols as f64)
            .min(self.config.appearance.max_card_width);
        let card_h = ((self.height as f64 - pad * (rows + 1) as f64) / rows as f64)
            .min(self.config.appearance.max_card_height);

        let grid_w = cols as f64 * card_w + (cols.saturating_sub(1)) as f64 * pad;
        let grid_h = rows as f64 * card_h + (rows.saturating_sub(1)) as f64 * pad;
        let ox = (self.width as f64 - grid_w) / 2.0;
        let oy = (self.height as f64 - grid_h) / 2.0;

        for i in 0..n {
            let col = i % cols;
            let row = i / cols;
            let cx = ox + col as f64 * (card_w + pad);
            let cy = oy + row as f64 * (card_h + pad);
            if x >= cx && x <= cx + card_w && y >= cy && y <= cy + card_h {
                return Some(i);
            }
        }
        None
    }

    fn select_workspace_at(&mut self, x: f64, y: f64) {
        if let Some(i) = self.workspace_at(x, y) {
            if self.selected != i {
                self.selected = i;
                self.needs_redraw = true;
            }
        }
    }
}

// ── wayland Dispatch impls ───────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for AppState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "wl_shm" => {
                state.shm = Some(registry.bind(name, 1, qh, ()));
            }
            "wl_seat" => {
                state.seat = Some(registry.bind(name, version.min(7), qh, ()));
            }
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "hyprland_toplevel_export_manager_v1" => {
                state.toplevel_export = Some(registry.bind(name, 1, qh, ()));
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(AppState: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(AppState: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(AppState: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
wayland_client::delegate_noop!(AppState: ignore HyprlandToplevelExportManagerV1);

impl Dispatch<wl_shm::WlShm, ()> for AppState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_surface::WlSurface, ()> for AppState {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
            if state.allow_mouse && caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            let keysym = keycode_to_keysym(key);
            if keysym != 0 && state.handle_key(keysym) {
                state.should_close = true;
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if !state.allow_mouse {
            return;
        }
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. }
            | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;
                state.select_workspace_at(surface_x, surface_y);
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                if state.workspace_at(state.pointer_x, state.pointer_y).is_none() {
                    return;
                }
                match button {
                    BTN_LEFT => {
                        state.activate_selected_workspace();
                        state.should_close = true;
                    }
                    BTN_RIGHT => {
                        if state.move_active_window_to_selected() {
                            state.should_close = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for AppState {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer_surface.ack_configure(serial);
                state.width = width;
                state.height = height;
                state.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.should_close = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<HyprlandToplevelExportFrameV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &HyprlandToplevelExportFrameV1,
        event: hyprland_toplevel_export_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use hyprland_toplevel_export_frame_v1::Event;
        let Some(cap) = state.pending_capture.as_mut() else { return };
        match event {
            Event::Buffer { format, width, height, stride } => {
                // Convert WEnum<wl_shm::Format> to u32
                cap.format = format.into();
                cap.width = width;
                cap.height = height;
                cap.stride = stride;
            }
            Event::BufferDone => cap.buffer_done = true,
            Event::Ready { .. } => cap.frame_ready = true,
            Event::Failed => cap.frame_failed = true,
            _ => {}
        }
    }
}

// ── Wayland surface management ───────────────────────────────────────────────

fn create_shm_fd(size: usize) -> Option<OwnedFd> {
    let name = c"hyprexpose";
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe { libc::ftruncate(raw, size as libc::off_t) } < 0 {
        return None;
    }
    Some(fd)
}

fn show(state: &mut AppState, qh: &QueueHandle<AppState>) -> bool {
    let (compositor, layer_shell) = match (&state.compositor, &state.layer_shell) {
        (Some(c), Some(l)) => (c, l),
        _ => return false,
    };

    state.should_close = false;
    state.configured = false;

    let wl_surf = compositor.create_surface(qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &wl_surf,
        None,
        zwlr_layer_shell_v1::Layer::Top,
        "hyprexpose".to_owned(),
        qh,
        (),
    );

    use zwlr_layer_surface_v1::Anchor;
    layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer_surface.set_exclusive_zone(-1);
    layer_surface.set_keyboard_interactivity(
        zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
    );

    wl_surf.commit();

    state.wl_surf = Some(wl_surf);
    state.layer_surface = Some(layer_surface);
    true
}

fn hide(state: &mut AppState) {
    if let Some(slab) = state.slab.take() {
        slab.destroy();
    }
    if let Some(ls) = state.layer_surface.take() {
        ls.destroy();
    }
    if let Some(surf) = state.wl_surf.take() {
        surf.destroy();
    }
    state.configured = false;
    state.visible = false;
    state.workspaces.clear();
    state.thumbnails.clear();
    // Workspace contents may change while hidden; rebuild the scene next show.
    state.scene = None;
}

impl Dispatch<wl_buffer::WlBuffer, usize> for AppState {
    fn event(state: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            if let Some(slab) = state.slab.as_mut() {
                if *idx < slab.busy.len() {
                    slab.busy[*idx] = false;
                }
            }
        }
    }
}

/// A persistent, double-buffered shared-memory allocation reused across
/// frames, instead of a fresh memfd + pool + buffer per redraw. Each buffer
/// remembers which selection it currently displays so a navigation event only
/// needs to re-render the old + new highlight rectangles into it.
struct ShmSlab {
    _fd: OwnedFd,
    map: *mut u8,
    map_len: usize,
    pool: wl_shm_pool::WlShmPool,
    bufs: [wl_buffer::WlBuffer; 2],
    /// Attached and not yet released by the compositor.
    busy: [bool; 2],
    /// Selection index whose frame each buffer holds; None = no valid frame.
    shown: [Option<usize>; 2],
    last_attached: usize,
    width: u32,
    height: u32,
}

impl ShmSlab {
    fn frame_size(&self) -> usize {
        (self.width * 4 * self.height) as usize
    }

    /// Mutable view of one buffer's pixels.
    fn frame_mut(&mut self, idx: usize) -> &mut [u8] {
        let size = self.frame_size();
        unsafe { std::slice::from_raw_parts_mut(self.map.add(idx * size), size) }
    }

    fn destroy(self) {
        for b in &self.bufs {
            b.destroy();
        }
        self.pool.destroy();
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.map_len) };
    }
}

fn create_slab(state: &mut AppState, qh: &QueueHandle<AppState>) -> Option<ShmSlab> {
    let shm = state.shm.as_ref()?;
    let (width, height) = (state.width, state.height);
    let stride = width * 4;
    let frame = (stride * height) as usize;
    let total = frame * 2;

    let fd = create_shm_fd(total)?;
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            total,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_fd().as_raw_fd(),
            0,
        )
    };
    if map == libc::MAP_FAILED {
        return None;
    }

    let pool = shm.create_pool(fd.as_fd(), total as i32, qh, ());
    let mk = |i: usize| {
        pool.create_buffer(
            (i * frame) as i32,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            i,
        )
    };
    let bufs = [mk(0), mk(1)];

    Some(ShmSlab {
        _fd: fd,
        map: map as *mut u8,
        map_len: total,
        pool,
        bufs,
        busy: [false; 2],
        shown: [None; 2],
        last_attached: 1,
        width,
        height,
    })
}

fn redraw(state: &mut AppState, qh: &QueueHandle<AppState>) {
    if !state.configured || state.width == 0 || state.height == 0 {
        return;
    }

    // (Re)build the selection-independent scene only when missing or stale
    // (first draw after show, or the surface was resized). Navigation events
    // reuse it, which fixes the CPU spikes of issue #14.
    let scene_stale = state
        .scene
        .as_ref()
        .map(|s| s.width != state.width || s.height != state.height)
        .unwrap_or(true);
    if scene_stale {
        state.scene = render::build_scene(
            state.width,
            state.height,
            &state.workspaces,
            &state.thumbnails,
            &state.config,
            state.active_window_address,
        );
        if let Some(slab) = state.slab.as_mut() {
            slab.shown = [None; 2]; // cached frames no longer match the scene
        }
    }
    if state.scene.is_none() {
        return;
    }

    // (Re)create the double-buffered shm allocation when missing or resized.
    let slab_stale = state
        .slab
        .as_ref()
        .map(|s| s.width != state.width || s.height != state.height)
        .unwrap_or(true);
    if slab_stale {
        if let Some(old) = state.slab.take() {
            old.destroy();
        }
        state.slab = create_slab(state, qh);
    }
    let (Some(scene), Some(slab)) = (&state.scene, state.slab.as_mut()) else { return };

    // Prefer the buffer the compositor isn't holding; with two buffers and
    // discrete redraws the previous one is normally released by now.
    let idx = {
        let next = 1 - slab.last_attached;
        if !slab.busy[next] { next } else if !slab.busy[slab.last_attached] { slab.last_attached } else { next }
    };

    let stride = (state.width * 4) as usize;
    let selected = state.selected;

    // What is currently on screen (the other buffer's frame). Damage must be
    // computed against this, while patching must cover whatever is stale in
    // the buffer we are about to reuse — take the union of both.
    let on_screen = slab.shown[slab.last_attached];

    let damage: Vec<(i32, i32, i32, i32)> = match (slab.shown[idx], on_screen) {
        // Fast path: this buffer holds a valid frame; only highlight
        // rectangles can differ. Re-render those and report as damage the
        // same set (it covers both the buffer-relative and screen-relative
        // diffs, at worst one extra small rect).
        (Some(buf_prev), screen_prev) => {
            let mut rects = Vec::with_capacity(3);
            for s in [Some(buf_prev), screen_prev, Some(selected)].into_iter().flatten() {
                if let Some(r) = scene.highlight_rect(s, &state.config) {
                    if !rects.contains(&r) {
                        rects.push(r);
                    }
                }
            }
            let frame = slab.frame_mut(idx);
            for &rect in &rects {
                let patch = render::compose_patch(scene, selected, &state.config, rect);
                let (x, y, w, h) = rect;
                let prow = (w * 4) as usize;
                for row in 0..h as usize {
                    let dst = (y as usize + row) * stride + x as usize * 4;
                    frame[dst..dst + prow].copy_from_slice(&patch[row * prow..(row + 1) * prow]);
                }
            }
            rects
        }
        // Full frame (first draw into this buffer, or scene was rebuilt).
        (None, _) => {
            let pixels = render::compose(scene, selected, &state.config);
            slab.frame_mut(idx).copy_from_slice(&pixels);
            vec![(0, 0, state.width as i32, state.height as i32)]
        }
    };

    if let Some(surf) = &state.wl_surf {
        surf.attach(Some(&slab.bufs[idx]), 0, 0);
        for (x, y, w, h) in damage {
            surf.damage_buffer(x, y, w, h);
        }
        surf.commit();
        slab.busy[idx] = true;
        slab.shown[idx] = Some(selected);
        slab.last_attached = idx;
    }
}

// ── window capture ───────────────────────────────────────────────────────────

fn capture_toplevel(
    state: &mut AppState,
    eq: &mut EventQueue<AppState>,
    qh: &QueueHandle<AppState>,
    address: u64,
) -> Option<Thumbnail> {
    let manager = state.toplevel_export.as_ref()?;
    state.pending_capture = Some(CaptureFrameData::default());
    let frame = manager.capture_toplevel(0, address as u32, qh, ());

    // Phase 1: wait for buffer format info
    loop {
        eq.blocking_dispatch(state).ok()?;
        let cap = state.pending_capture.as_ref()?;
        if cap.buffer_done || cap.frame_failed {
            break;
        }
    }

    let cap = state.pending_capture.take()?;
    if cap.frame_failed || cap.width == 0 || cap.height == 0 {
        frame.destroy();
        return None;
    }

    let size = (cap.stride * cap.height) as usize;

    let cap_name = c"hyprexpose-cap";
    let raw_fd = unsafe { libc::memfd_create(cap_name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw_fd < 0 {
        frame.destroy();
        return None;
    }
    unsafe { libc::ftruncate(raw_fd, size as libc::off_t) };

    // mmap so we can read the pixels after capture completes
    let mmap_ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            raw_fd,
            0,
        )
    };
    if mmap_ptr == libc::MAP_FAILED {
        unsafe { libc::close(raw_fd) };
        frame.destroy();
        return None;
    }

    let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let Some(shm) = &state.shm else {
        unsafe { libc::munmap(mmap_ptr, size) };
        frame.destroy();
        return None;
    };

    // Convert raw format value to wl_shm::Format
    let wl_fmt = wl_shm::Format::try_from(cap.format)
        .unwrap_or(wl_shm::Format::Argb8888);

    let pool = shm.create_pool(owned_fd.as_fd(), size as i32, qh, ());
    let wl_buf = pool.create_buffer(
        0,
        cap.width as i32,
        cap.height as i32,
        cap.stride as i32,
        wl_fmt,
        qh,
        usize::MAX, // capture scratch buffer; not part of the display slab
    );
    pool.destroy();
    drop(owned_fd); // compositor has the mapping; safe to close our fd end

    // Phase 2: request frame copy and wait for ready/failed
    state.pending_capture = Some(CaptureFrameData {
        width: cap.width,
        height: cap.height,
        stride: cap.stride,
        format: cap.format,
        ..Default::default()
    });

    frame.copy(&wl_buf, 1);

    loop {
        eq.blocking_dispatch(state).ok()?;
        let c = state.pending_capture.as_ref()?;
        if c.frame_ready || c.frame_failed {
            break;
        }
    }

    let cap2 = state.pending_capture.take().unwrap_or_default();

    let thumbnail = if cap2.frame_ready {
        let pixels = unsafe {
            std::slice::from_raw_parts(mmap_ptr as *const u8, size).to_vec()
        };
        Some(Thumbnail {
            address,
            data: pixels,
            width: cap.width,
            height: cap.height,
            stride: cap.stride,
        })
    } else {
        None
    };

    unsafe { libc::munmap(mmap_ptr, size) };
    wl_buf.destroy();
    frame.destroy();

    thumbnail
}

// ── data refresh ─────────────────────────────────────────────────────────────

fn refresh_data(
    state: &mut AppState,
    eq: &mut EventQueue<AppState>,
    qh: &QueueHandle<AppState>,
) {
    // Capture the focused window before the overlay steals keyboard focus.
    state.active_window_address = ipc::get_active_window_address();
    state.workspaces = ipc::get_workspaces();
    state.thumbnails.clear();

    // Thumbnails need the hyprland-toplevel-export protocol; on other
    // compositors (e.g. Sway) the manager is absent and we fall back to
    // colored rectangles.
    if !state.no_preview && state.toplevel_export.is_some() {
        let addrs: Vec<u64> = state
            .workspaces
            .iter()
            .flat_map(|ws| ws.clients.iter().map(|c| c.address))
            .collect();

        for addr in addrs {
            if let Some(thumb) = capture_toplevel(state, eq, qh, addr) {
                state.thumbnails.push(thumb);
            }
        }
    }

    let active = ipc::get_active_workspace_name();
    state.selected = state
        .workspaces
        .iter()
        .position(|ws| ws.name == active)
        .unwrap_or(0);
}

// ── main ─────────────────────────────────────────────────────────────────────

fn main() {
    let mut no_preview = false;
    let mut allow_mouse = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--no-preview" => no_preview = true,
            "--allow-mouse" => allow_mouse = true,
            _ => {
                eprintln!("Usage: hyprexpose [--no-preview] [--allow-mouse]");
                std::process::exit(1);
            }
        }
    }

    // Install signal handlers
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sig_handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }

    let conn = Connection::connect_to_env().expect("failed to connect to Wayland display");
    let mut eq: EventQueue<AppState> = conn.new_event_queue();
    let qh = eq.handle();

    let config = Config::load();
    let mut state = AppState::new(no_preview, allow_mouse, config);

    // Register registry listener and perform two roundtrips to discover all globals
    conn.display().get_registry(&qh, ());
    eq.roundtrip(&mut state).expect("Wayland roundtrip failed");
    eq.roundtrip(&mut state).expect("Wayland roundtrip failed");

    if state.compositor.is_none() || state.shm.is_none() || state.layer_shell.is_none() {
        eprintln!("hyprexpose: missing required Wayland globals");
        std::process::exit(1);
    }

    if state.toplevel_export.is_none() && !state.no_preview {
        eprintln!(
            "hyprexpose: hyprland-toplevel-export protocol unavailable; \
             window previews disabled (colored rectangles will be used)"
        );
    }

    eprintln!(
        "hyprexpose: daemon running on {} (send SIGUSR1 to toggle)",
        ipc::compositor().name()
    );

    let display_fd = conn.as_fd().as_raw_fd();

    'main: loop {
        if G_QUIT.load(Ordering::Relaxed) {
            break;
        }

        if G_TOGGLE.swap(false, Ordering::Relaxed) {
            if state.visible {
                hide(&mut state);
                conn.flush().ok();
            } else {
                if show(&mut state, &qh) {
                    conn.flush().ok();
                    eq.roundtrip(&mut state).ok();
                    if state.configured {
                        state.visible = true;
                        refresh_data(&mut state, &mut eq, &qh);
                        redraw(&mut state, &qh);
                        conn.flush().ok();
                    }
                }
            }
        }

        if state.should_close && state.visible {
            hide(&mut state);
            conn.flush().ok();
            state.should_close = false;
        }

        if state.needs_redraw && state.visible {
            state.needs_redraw = false;
            redraw(&mut state, &qh);
            conn.flush().ok();
        }

        conn.flush().ok();

        // Poll for events using prepare_read
        let guard = match eq.prepare_read() {
            Some(g) => g,
            None => {
                eq.dispatch_pending(&mut state).ok();
                continue;
            }
        };

        let mut pollfd = libc::pollfd {
            fd: display_fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut pollfd, 1, -1) };

        if ret < 0 {
            drop(guard);
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                // signal interrupted poll — loop back to check flags
                eq.dispatch_pending(&mut state).ok();
                continue;
            }
            break 'main;
        }

        if pollfd.revents & libc::POLLIN != 0 {
            guard.read().ok();
        } else {
            drop(guard);
        }

        eq.dispatch_pending(&mut state).ok();
    }

    hide(&mut state);
}
