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

pub fn draw(
    width: u32,
    height: u32,
    workspaces: &[WorkspaceInfo],
    selected_index: usize,
    thumbnails: &[Thumbnail],
    cfg: &Config,
    active_window_address: u64,
) -> Vec<u8> {
    let stride = width * 4;
    let size = (stride * height) as usize;
    let buf = vec![0u8; size];

    let surface = match ImageSurface::create_for_data(
        buf, Format::ARgb32, width as i32, height as i32, stride as i32,
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

    if workspaces.is_empty() {
        drop(cr);
        return surface.take_data().map(|d| d.to_vec()).unwrap_or_default();
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
        let select_border_w = cfg.appearance.select_border;

        // Selection highlight
        if i == selected_index {
            let (sr, sg, sb, sa) = cfg.colors.selection.rgba();
            rounded_rect(&cr, cx - select_border_w, cy - select_border_w, card_w + 2.0 * select_border_w, card_h + 2.0 * select_border_w, r + 2.0);
            cr.set_source_rgba(sr, sg, sb, sa);
            cr.fill().ok();
        }

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
    surface.take_data().map(|d| d.to_vec()).unwrap_or_default()
}
