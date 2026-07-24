use crate::config::Config;
use crate::ipc::{ClientInfo, WorkspaceInfo};
use cairo::{Context, Format, ImageSurface};

pub struct Thumbnail {
    pub address: u64,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

/// Everything that does not depend on the current selection, rendered once
/// per show: workspace cards, labels and (pre-scaled) window thumbnails on a
/// transparent layer. Navigating between workspaces then only needs to
/// composite this layer over the background + selection highlight instead of
/// re-rendering the whole scene (see issue #14).
pub struct SceneCache {
    layer: ImageSurface,
    /// (x, y, w, h) of each workspace card, index-aligned with `workspaces`.
    card_rects: Vec<(f64, f64, f64, f64)>,
    pub width: u32,
    pub height: u32,
}

impl SceneCache {
    /// Bounding box (x, y, w, h) of the selection highlight for card `index`,
    /// in integer pixels, suitable for `wl_surface.damage_buffer`.
    pub fn highlight_rect(&self, index: usize, cfg: &Config) -> Option<(i32, i32, i32, i32)> {
        let (cx, cy, cw, ch) = *self.card_rects.get(index)?;
        // Highlight extends select_border past the card on every side; pad a
        // couple of extra pixels for the larger corner radius / antialiasing.
        let m = cfg.appearance.select_border + 3.0;
        let x0 = (cx - m).floor().max(0.0) as i32;
        let y0 = (cy - m).floor().max(0.0) as i32;
        let x1 = ((cx + cw + m).ceil() as i32).min(self.width as i32);
        let y1 = ((cy + ch + m).ceil() as i32).min(self.height as i32);
        Some((x0, y0, (x1 - x0).max(0), (y1 - y0).max(0)))
    }
}

fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r,     r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r,  0.0,      PI / 2.0);
    cr.arc(x + r,     y + h - r, r,  PI / 2.0, PI);
    cr.arc(x + r,     y + r,     r,  PI,       3.0 * PI / 2.0);
    cr.close_path();
}

fn find_thumb<'a>(thumbnails: &'a [Thumbnail], address: u64) -> Option<&'a Thumbnail> {
    thumbnails.iter().find(|t| t.address == address)
}

fn draw_client(
    cr: &Context,
    cfg: &Config,
    client: &ClientInfo,
    min_x: i32,
    min_y: i32,
    scale: f64,
    off_x: f64,
    off_y: f64,
    thumbnails: &[Thumbnail],
    active_window_address: u64,
    window_font: &pango::FontDescription,
) {
    let rx = off_x + (client.x - min_x) as f64 * scale;
    let ry = off_y + (client.y - min_y) as f64 * scale;
    let rw = client.w as f64 * scale;
    let rh = client.h as f64 * scale;

    // Window thumbnail or fallback colored rect
    if let Some(thumb) = find_thumb(thumbnails, client.address) {
        if let Ok(img) = ImageSurface::create_for_data(
            thumb.data.clone(),
            Format::ARgb32,
            thumb.width as i32,
            thumb.height as i32,
            thumb.stride as i32,
        ) {
            cr.save().ok();
            rounded_rect(cr, rx, ry, rw, rh, 4.0);
            cr.clip();
            cr.translate(rx, ry);
            cr.scale(rw / thumb.width as f64, rh / thumb.height as f64);
            cr.set_source_surface(&img, 0.0, 0.0).ok();
            cr.paint().ok();
            cr.restore().ok();
        }
    } else {
        let hash: u32 = client
            .class_name
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        let r = 0.2 + (hash % 100) as f64 / 200.0;
        let g = 0.2 + ((hash / 100) % 100) as f64 / 200.0;
        let b = 0.3 + ((hash / 10000) % 100) as f64 / 200.0;
        rounded_rect(cr, rx, ry, rw, rh, 4.0);
        cr.set_source_rgba(r, g, b, 0.85);
        cr.fill().ok();
    }

    // Active-window indicator: coloured border so the user knows which window 'm' will move
    if active_window_address != 0 && client.address == active_window_address {
        let (ar, ag, ab, aa) = cfg.colors.active_window.rgba();
        rounded_rect(cr, rx - 2.0, ry - 2.0, rw + 4.0, rh + 4.0, 5.0);
        cr.set_source_rgba(ar, ag, ab, aa);
        cr.set_line_width(2.5);
        cr.stroke().ok();
    }

    // Window label (class name or title)
    if rw > 40.0 && rh > 20.0 {
        let layout = pangocairo::functions::create_layout(cr);
        let name = if client.class_name.is_empty() { &client.title } else { &client.class_name };
        layout.set_text(name);
        layout.set_font_description(Some(window_font));
        layout.set_width(((rw - 4.0) * pango::SCALE as f64) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        let (tw, th) = layout.pixel_size();
        let (wr, wg, wb, wa) = cfg.colors.window_label.rgba();
        cr.set_source_rgba(wr, wg, wb, wa);
        cr.move_to(rx + (rw - tw as f64) / 2.0, ry + (rh - th as f64) / 2.0);
        pangocairo::functions::show_layout(cr, &layout);
    }
}

/// Render everything that doesn't depend on the selection (cards, labels,
/// thumbnails) once. Called when the overlay is (re)shown or resized.
pub fn build_scene(
    width: u32,
    height: u32,
    workspaces: &[WorkspaceInfo],
    thumbnails: &[Thumbnail],
    cfg: &Config,
    active_window_address: u64,
) -> Option<SceneCache> {
    let surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32).ok()?;
    let cr = Context::new(&surface).ok()?;

    let mut card_rects = Vec::with_capacity(workspaces.len());

    if workspaces.is_empty() {
        drop(cr);
        return Some(SceneCache { layer: surface, card_rects, width, height });
    }

    let n = workspaces.len();
    let cols = ((n as f64).sqrt().ceil() as usize).max(1);
    let rows = (n + cols - 1) / cols;
    let pad = cfg.appearance.card_padding;

    let card_w = ((width as f64 - pad * (cols + 1) as f64) / cols as f64)
        .min(cfg.appearance.max_card_width);
    let card_h = ((height as f64 - pad * (rows + 1) as f64) / rows as f64)
        .min(cfg.appearance.max_card_height);

    let grid_w = cols as f64 * card_w + (cols - 1) as f64 * pad;
    let grid_h = rows as f64 * card_h + (rows - 1) as f64 * pad;
    let ox = (width as f64 - grid_w) / 2.0;
    let oy = (height as f64 - grid_h) / 2.0;

    let label_font = pango::FontDescription::from_string(&cfg.appearance.label_font);
    let window_font = pango::FontDescription::from_string(&cfg.appearance.font);
    let empty_font = window_font.clone();

    for (i, ws) in workspaces.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let cx = ox + col as f64 * (card_w + pad);
        let cy = oy + row as f64 * (card_h + pad);
        let r = cfg.appearance.card_radius;

        card_rects.push((cx, cy, card_w, card_h));

        // Card background
        let (cr_c, cg, cb, ca) = cfg.colors.card.rgba();
        rounded_rect(&cr, cx, cy, card_w, card_h, r);
        cr.set_source_rgba(cr_c, cg, cb, ca);
        cr.fill().ok();

        // Workspace label
        {
            let layout = pangocairo::functions::create_layout(&cr);
            // Named workspaces without a number (sway: num == -1) show just the name.
            let mut label = if ws.id >= 1 { ws.id.to_string() } else { String::new() };
            if !ws.name.is_empty() && ws.name != label {
                if !label.is_empty() {
                    label.push(' ');
                }
                label.push_str(&ws.name);
            }
            if label.is_empty() {
                label = ws.id.to_string();
            }
            layout.set_text(&label);
            layout.set_font_description(Some(&label_font));
            let (tw, _) = layout.pixel_size();
            let (lr, lg, lb, la) = cfg.colors.label.rgba();
            cr.set_source_rgba(lr, lg, lb, la);
            cr.move_to(cx + (card_w - tw as f64) / 2.0, cy + 6.0);
            pangocairo::functions::show_layout(&cr, &layout);
        }

        let lh = cfg.appearance.label_height;
        let tp = cfg.appearance.thumb_padding;
        let win_x = cx + tp;
        let win_y = cy + lh;
        let win_w = card_w - 2.0 * tp;
        let win_h = card_h - lh - tp;

        if ws.clients.is_empty() {
            let layout = pangocairo::functions::create_layout(&cr);
            layout.set_text("(empty)");
            layout.set_font_description(Some(&empty_font));
            let (tw, th) = layout.pixel_size();
            let (er, eg, eb, ea) = cfg.colors.empty_label.rgba();
            cr.set_source_rgba(er, eg, eb, ea);
            cr.move_to(cx + (card_w - tw as f64) / 2.0, win_y + (win_h - th as f64) / 2.0);
            pangocairo::functions::show_layout(&cr, &layout);
            continue;
        }

        // Scale all client rects to fit the window area
        let (min_x, min_y, max_x, max_y) = ws.clients.iter().fold(
            (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
            |(mix, miy, mxx, mxy), c| {
                (mix.min(c.x), miy.min(c.y), mxx.max(c.x + c.w), mxy.max(c.y + c.h))
            },
        );

        let ws_w = (max_x - min_x).max(1) as f64;
        let ws_h = (max_y - min_y).max(1) as f64;
        let scale = (win_w / ws_w).min(win_h / ws_h) * 0.9;

        let off_x = win_x + (win_w - ws_w * scale) / 2.0;
        let off_y = win_y + (win_h - ws_h * scale) / 2.0;

        for client in &ws.clients {
            draw_client(&cr, cfg, client, min_x, min_y, scale, off_x, off_y, thumbnails, active_window_address, &window_font);
        }
    }

    drop(cr);
    Some(SceneCache { layer: surface, card_rects, width, height })
}

/// Produce the final frame for the current selection: dimmed background,
/// selection highlight, then the cached scene layer on top. This is the only
/// work done per navigation event and is a couple of fills plus one blit.
pub fn compose(scene: &SceneCache, selected_index: usize, cfg: &Config) -> Vec<u8> {
    let width = scene.width;
    let height = scene.height;
    let stride = width * 4;
    let size = (stride * height) as usize;

    let surface = match ImageSurface::create_for_data(
        vec![0u8; size], Format::ARgb32, width as i32, height as i32, stride as i32,
    ) {
        Ok(s) => s,
        Err(_) => return vec![0u8; size],
    };
    let cr = match Context::new(&surface) {
        Ok(c) => c,
        Err(_) => return vec![0u8; size],
    };

    // Dimmed background
    let (br, bg, bb, ba) = cfg.colors.background.rgba();
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(br, bg, bb, ba);
    cr.paint().ok();
    cr.set_operator(cairo::Operator::Over);

    // Selection highlight (sits underneath the card, exactly as before)
    if let Some(&(cx, cy, cw, ch)) = scene.card_rects.get(selected_index) {
        let b = cfg.appearance.select_border;
        let r = cfg.appearance.card_radius;
        let (sr, sg, sb, sa) = cfg.colors.selection.rgba();
        rounded_rect(&cr, cx - b, cy - b, cw + 2.0 * b, ch + 2.0 * b, r + 2.0);
        cr.set_source_rgba(sr, sg, sb, sa);
        cr.fill().ok();
    }

    // Cached cards / thumbnails / labels
    cr.set_source_surface(&scene.layer, 0.0, 0.0).ok();
    cr.paint().ok();

    drop(cr);
    surface.take_data().map(|d| d.to_vec()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{ClientInfo, WorkspaceInfo};

    fn fake_thumb(address: u64, w: u32, h: u32) -> Thumbnail {
        let stride = w * 4;
        let mut data = vec![0u8; (stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * stride) + x * 4) as usize;
                data[i] = (x % 256) as u8;
                data[i + 1] = (y % 256) as u8;
                data[i + 2] = 128;
                data[i + 3] = 255;
            }
        }
        Thumbnail { address, data, width: w, height: h, stride }
    }

    fn fake_scene() -> (Vec<WorkspaceInfo>, Vec<Thumbnail>) {
        let mk = |addr: u64, class: &str, x, y, w, h, ws| ClientInfo {
            class_name: class.into(),
            title: format!("{class} title"),
            address: addr,
            workspace_id: ws,
            x, y, w, h,
        };
        let workspaces = vec![
            WorkspaceInfo {
                id: 1, name: "1".into(), monitor_id: 0,
                clients: vec![mk(0xa, "kitty", 0, 0, 960, 1080, 1), mk(0xb, "firefox", 960, 0, 960, 1080, 1)],
            },
            WorkspaceInfo { id: 2, name: "web".into(), monitor_id: 0, clients: vec![mk(0xc, "chromium", 100, 100, 1720, 880, 2)] },
            WorkspaceInfo { id: 3, name: "3".into(), monitor_id: 0, clients: vec![] },
            WorkspaceInfo { id: 4, name: "4".into(), monitor_id: 0, clients: vec![mk(0xd, "", 0, 0, 1920, 1080, 4)] },
        ];
        let thumbnails = vec![fake_thumb(0xa, 320, 200), fake_thumb(0xc, 400, 240)];
        (workspaces, thumbnails)
    }

    /// Every pixel that changes when the selection moves must fall inside
    /// the union of the two damage rects reported by `highlight_rect`,
    /// otherwise partial damage in `redraw()` would leave stale pixels.
    #[test]
    fn highlight_rect_covers_selection_change() {
        let (workspaces, thumbnails) = fake_scene();
        let cfg = Config::default();
        let (w, h) = (1280u32, 800u32);
        let scene = build_scene(w, h, &workspaces, &thumbnails, &cfg, 0xb).unwrap();

        for (s0, s1) in [(0usize, 1usize), (0, 2), (1, 3), (2, 3)] {
            let a = compose(&scene, s0, &cfg);
            let b = compose(&scene, s1, &cfg);
            let r0 = scene.highlight_rect(s0, &cfg).unwrap();
            let r1 = scene.highlight_rect(s1, &cfg).unwrap();
            let inside = |x: i32, y: i32, (rx, ry, rw, rh): (i32, i32, i32, i32)| {
                x >= rx && x < rx + rw && y >= ry && y < ry + rh
            };
            let stride = (w * 4) as usize;
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let i = y as usize * stride + x as usize * 4;
                    if a[i..i + 4] != b[i..i + 4] {
                        assert!(
                            inside(x, y, r0) || inside(x, y, r1),
                            "differing pixel ({x},{y}) outside damage rects for {s0}->{s1}"
                        );
                    }
                }
            }
        }
    }

    /// compose() must be deterministic for a fixed scene + selection, since
    /// partial damage assumes undamaged regions are identical across frames.
    #[test]
    fn compose_is_deterministic() {
        let (workspaces, thumbnails) = fake_scene();
        let cfg = Config::default();
        let scene = build_scene(1280, 800, &workspaces, &thumbnails, &cfg, 0xb).unwrap();
        assert_eq!(compose(&scene, 1, &cfg), compose(&scene, 1, &cfg));
    }
}
