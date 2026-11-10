//! A theme's own icons: SVG path data, filled or stroked and drawn into a texture once per size.

use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2};

/// Paths as closed polygons in the path's own units.
#[cfg(test)]
fn flatten(d: &str) -> Vec<Vec<[f32; 2]>> {
    flatten_paths(d, false)
}

/// `close_ends`: a path that closes ends on its first point, for a stroke to go all the way round.
fn flatten_paths(d: &str, close_ends: bool) -> Vec<Vec<[f32; 2]>> {
    let mut tokens = Tokens { s: d.as_bytes(), i: 0 };
    let mut polys: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut start_x, mut start_y) = (0.0f32, 0.0f32);
    // The last control point, for S and T.
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let mut cmd = b'M';
    let mut last = b' ';
    while let Some(next) = tokens.command_or_number() {
        if let Some(c) = next {
            cmd = c;
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            b'Z' => {
                // Z takes no numbers: one after it belongs to nothing, and would be read here for ever.
                if next.is_none() {
                    break;
                }
                if !cur.is_empty() {
                    if close_ends {
                        cur.push([start_x, start_y]);
                    }
                    polys.push(std::mem::take(&mut cur));
                }
                x = start_x;
                y = start_y;
            }
            b'M' => {
                let (Some(a), Some(b)) = (tokens.number(), tokens.number()) else { break };
                if !cur.is_empty() {
                    polys.push(std::mem::take(&mut cur));
                }
                x = a + ox;
                y = b + oy;
                start_x = x;
                start_y = y;
                cur.push([x, y]);
                // More pairs after a move are lines.
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let (Some(a), Some(b)) = (tokens.number(), tokens.number()) else { break };
                x = a + ox;
                y = b + oy;
                cur.push([x, y]);
            }
            b'H' => {
                let Some(a) = tokens.number() else { break };
                x = a + ox;
                cur.push([x, y]);
            }
            b'V' => {
                let Some(a) = tokens.number() else { break };
                y = a + oy;
                cur.push([x, y]);
            }
            b'C' | b'S' => {
                let smooth = cmd.eq_ignore_ascii_case(&b'S');
                let (x1, y1) = if smooth {
                    if matches!(last.to_ascii_uppercase(), b'C' | b'S') { (2.0 * x - cx, 2.0 * y - cy) } else { (x, y) }
                } else {
                    let (Some(a), Some(b)) = (tokens.number(), tokens.number()) else { break };
                    (a + ox, b + oy)
                };
                let (Some(a), Some(b), Some(e), Some(f)) = (tokens.number(), tokens.number(), tokens.number(), tokens.number()) else { break };
                let (x2, y2, ex, ey) = (a + ox, b + oy, e + ox, f + oy);
                for k in 1..=12 {
                    let t = k as f32 / 12.0;
                    let u = 1.0 - t;
                    cur.push([u * u * u * x + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * ex, u * u * u * y + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * ey]);
                }
                cx = x2;
                cy = y2;
                x = ex;
                y = ey;
            }
            b'Q' | b'T' => {
                let smooth = cmd.eq_ignore_ascii_case(&b'T');
                let (x1, y1) = if smooth {
                    if matches!(last.to_ascii_uppercase(), b'Q' | b'T') { (2.0 * x - cx, 2.0 * y - cy) } else { (x, y) }
                } else {
                    let (Some(a), Some(b)) = (tokens.number(), tokens.number()) else { break };
                    (a + ox, b + oy)
                };
                let (Some(e), Some(f)) = (tokens.number(), tokens.number()) else { break };
                let (ex, ey) = (e + ox, f + oy);
                for k in 1..=10 {
                    let t = k as f32 / 10.0;
                    let u = 1.0 - t;
                    cur.push([u * u * x + 2.0 * u * t * x1 + t * t * ex, u * u * y + 2.0 * u * t * y1 + t * t * ey]);
                }
                cx = x1;
                cy = y1;
                x = ex;
                y = ey;
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(e), Some(f)) = (tokens.number(), tokens.number(), tokens.number(), tokens.flag(), tokens.flag(), tokens.number(), tokens.number()) else { break };
                let (ex, ey) = (e + ox, f + oy);
                arc(&mut cur, [x, y], [ex, ey], rx, ry, rot, large, sweep);
                x = ex;
                y = ey;
            }
            _ => break,
        }
        last = cmd;
    }
    if !cur.is_empty() {
        polys.push(cur);
    }
    polys
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tokens<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }
    /// `Some(Some(c))` a command, `Some(None)` a number comes next (the command repeats), `None` the end.
    fn command_or_number(&mut self) -> Option<Option<u8>> {
        self.skip();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(Some(c))
        } else {
            Some(None)
        }
    }
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        Some(c == b'1')
    }
    fn number(&mut self) -> Option<f32> {
        self.skip();
        let begin = self.i;
        let (mut dot, mut digits) = (false, false);
        while let Some(&c) = self.s.get(self.i) {
            let at_start = self.i == begin;
            match c {
                b'+' | b'-' if at_start => {}
                b'+' | b'-' if matches!(self.s[self.i - 1], b'e' | b'E') => {}
                b'0'..=b'9' => digits = true,
                b'.' if !dot => dot = true,
                b'e' | b'E' if digits => {}
                _ => break,
            }
            self.i += 1;
        }
        std::str::from_utf8(&self.s[begin..self.i]).ok()?.parse().ok()
    }
}

/// An elliptical arc as line segments (the SVG spec's endpoint-to-centre conversion).
#[allow(clippy::too_many_arguments)]
fn arc(out: &mut Vec<[f32; 2]>, from: [f32; 2], to: [f32; 2], rx: f32, ry: f32, rot: f32, large: bool, sweep: bool) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || from == to {
        out.push(to);
        return;
    }
    let (sin, cos) = rot.to_radians().sin_cos();
    let (dx, dy) = ((from[0] - to[0]) / 2.0, (from[1] - to[1]) / 2.0);
    let (x1, y1) = (cos * dx + sin * dy, -sin * dx + cos * dy);
    let k = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if k > 1.0 {
        rx *= k.sqrt();
        ry *= k.sqrt();
    }
    let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let co = (num / den).sqrt() * if large == sweep { -1.0 } else { 1.0 };
    let (cxp, cyp) = (co * rx * y1 / ry, -co * ry * x1 / rx);
    let (cx, cy) = (cos * cxp - sin * cyp + (from[0] + to[0]) / 2.0, sin * cxp + cos * cyp + (from[1] + to[1]) / 2.0);
    let a0 = ((y1 - cyp) / ry).atan2((x1 - cxp) / rx);
    let a1 = ((-y1 - cyp) / ry).atan2((-x1 - cxp) / rx);
    let mut delta = a1 - a0;
    if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    } else if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    }
    let steps = ((delta.abs() / 0.2).ceil() as usize).clamp(2, 40);
    for i in 1..=steps {
        let a = a0 + delta * i as f32 / steps as f32;
        let (s, c) = a.sin_cos();
        out.push([cos * rx * c - sin * ry * s + cx, sin * rx * c + cos * ry * s + cy]);
    }
}

/// The outline of lines this wide with round ends and joins, as polygons all wound the same
/// way, so where they overlap the non-zero rule keeps them one shape.
fn stroke_outline(lines: &[Vec<[f32; 2]>], width: f32) -> Vec<Vec<[f32; 2]>> {
    const ROUND: usize = 16;
    let r = width / 2.0;
    let mut out = Vec::new();
    for line in lines {
        for (i, a) in line.iter().enumerate() {
            out.push((0..ROUND).map(|k| {
                let (s, c) = (-(k as f32) / ROUND as f32 * std::f32::consts::TAU).sin_cos();
                [a[0] + c * r, a[1] + s * r]
            }).collect());
            let Some(b) = line.get(i + 1) else { continue };
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dy);
            if len > 0.0 {
                let (nx, ny) = (-dy / len * r, dx / len * r);
                out.push(vec![[a[0] + nx, a[1] + ny], [b[0] + nx, b[1] + ny], [b[0] - nx, b[1] - ny], [a[0] - nx, a[1] - ny]]);
            }
        }
    }
    out
}

/// How much of each pixel the polygons cover, by the non-zero rule, on four lines a pixel.
fn coverage(polys: &[Vec<[f32; 2]>], w: usize, h: usize) -> Vec<f32> {
    const LINES: usize = 4;
    let mut cover = vec![0.0f32; w * h];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for row in 0..h * LINES {
        let y = (row as f32 + 0.5) / LINES as f32;
        crossings.clear();
        for poly in polys {
            for (i, a) in poly.iter().enumerate() {
                let b = &poly[(i + 1) % poly.len()];
                if (a[1] <= y) != (b[1] <= y) {
                    crossings.push((a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]), if b[1] > a[1] { 1 } else { -1 }));
                }
            }
        }
        crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
        let line = &mut cover[(row / LINES) * w..(row / LINES + 1) * w];
        let mut winding = 0;
        for pair in crossings.windows(2) {
            winding += pair[0].1;
            if winding == 0 {
                continue;
            }
            let (from, to) = (pair[0].0.max(0.0), pair[1].0.min(w as f32));
            if to <= from {
                continue;
            }
            for (x, c) in line.iter_mut().enumerate().take(to.ceil() as usize).skip(from.floor() as usize) {
                let span = (x as f32 + 1.0).min(to) - (x as f32).max(from);
                *c += span.max(0.0) / LINES as f32;
            }
        }
    }
    cover
}

/// `px` wide and tall, the path fitted inside and centred.
fn rasterize(icon: &crate::preset::VectorIcon, px: usize) -> ColorImage {
    let [vw, vh] = icon.view;
    let [ox, oy] = icon.origin;
    let scale = px as f32 / vw.max(vh).max(1.0);
    let (dx, dy) = ((px as f32 - vw * scale) / 2.0 - ox * scale, (px as f32 - vh * scale) / 2.0 - oy * scale);
    let mut polys = flatten_paths(&icon.d, icon.stroke > 0.0);
    for p in polys.iter_mut().flatten() {
        let x = p[0] * scale + dx;
        *p = [if icon.flip { px as f32 - x } else { x }, p[1] * scale + dy];
    }
    if icon.stroke > 0.0 {
        polys = stroke_outline(&polys, icon.stroke * scale);
    }
    let cover = coverage(&polys, px, px);
    let pixels = cover.iter().map(|c| Color32::from_white_alpha((c.clamp(0.0, 1.0) * 255.0) as u8)).collect();
    ColorImage { size: [px, px], source_size: Vec2::splat(px as f32), pixels }
}

/// Draws the theme's own icon by this name, if it has one.
pub fn paint(ui: &Ui, name: &str, center: Pos2, size: f32, color: Color32) -> bool {
    let Some(icon) = crate::preset::active().app.icons.get(name) else { return false };
    let ppp = ui.ctx().pixels_per_point();
    // Sizes in steps of two pixels, so a growing icon doesn't make a texture every frame.
    let px = (((size * ppp) as usize + 1) & !1).clamp(8, 256);
    let key = egui::Id::unique(("vector-icon", icon.d.as_ptr() as usize, icon.d.len(), px));
    let tex = ui.ctx().data_mut(|d| d.get_temp::<TextureHandle>(key)).unwrap_or_else(|| {
        let tex = ui.ctx().load_texture(format!("icon-{name}-{px}"), rasterize(icon, px), TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.insert_temp(key, tex.clone()));
        tex
    });
    let rect = Rect::from_center_size(center, vec2(size, size));
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter().image(tex.id(), rect, uv, color);
    true
}

/// The name a theme gives the icon drawn for this glyph.
pub fn name_of(glyph: char) -> Option<&'static str> {
    use crate::theme::icon;
    Some(match glyph {
        icon::HOUSE => "home",
        icon::COMPASS => "explore",
        icon::LIBRARY => "library",
        icon::SKIP_BACK => "previous",
        icon::SKIP_FORWARD => "next",
        icon::LIST_MUSIC => "up_next",
        icon::MIC_VOCAL => "lyrics",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(d: &str) -> crate::preset::VectorIcon {
        crate::preset::VectorIcon { view: [10.0, 10.0], origin: [0.0, 0.0], d: d.into(), flip: false, stroke: 0.0 }
    }

    #[test]
    fn compact_numbers_and_implicit_lines_parse() {
        let polys = flatten("M1-1.5.5 2zm1 1h1v1z");
        assert_eq!(polys.len(), 2);
        assert_eq!(polys[0], vec![[1.0, -1.5], [0.5, 2.0]]);
        assert_eq!(polys[0].len(), 2);
    }

    #[test]
    fn a_filled_square_covers_what_it_should() {
        let img = rasterize(&icon("M0 0H5V10H0Z"), 10);
        assert_eq!(img.pixels[0].a(), 255);
        assert_eq!(img.pixels[4].a(), 255);
        assert_eq!(img.pixels[5].a(), 0);
    }

    #[test]
    fn an_opposite_winding_hole_stays_filled_only_when_it_should() {
        // Same direction: non-zero fills the inside. Opposite: a hole.
        let same = rasterize(&icon("M0 0H10V10H0ZM3 3H7V7H3Z"), 10);
        let hole = rasterize(&icon("M0 0H10V10H0ZM3 3V7H7V3Z"), 10);
        assert_eq!(same.pixels[55].a(), 255);
        assert_eq!(hole.pixels[55].a(), 0);
    }

    #[test]
    fn a_stroke_draws_the_line_and_keeps_its_overlaps() {
        let img = rasterize(&crate::preset::VectorIcon { stroke: 2.0, ..icon("M2 5H8M5 2V8") }, 10);
        // On each line, and where they cross (the windings add, they don't cancel).
        assert_eq!(img.pixels[5 * 10 + 3].a(), 255);
        assert_eq!(img.pixels[3 * 10 + 5].a(), 255);
        assert_eq!(img.pixels[5 * 10 + 5].a(), 255);
        // Round ends reach past the end, and the corners stay empty.
        assert!(img.pixels[5 * 10 + 1].a() > 100);
        assert_eq!(img.pixels[0].a(), 0);
    }

    #[test]
    fn a_stroke_goes_all_the_way_round_a_closed_path() {
        let img = rasterize(&crate::preset::VectorIcon { stroke: 1.0, ..icon("M2 2H8V8H2Z") }, 10);
        // The closing side, and the inside stays empty.
        assert!(img.pixels[5 * 10 + 2].a() > 100);
        assert_eq!(img.pixels[5 * 10 + 5].a(), 0);
    }

    #[test]
    fn arcs_end_where_they_say() {
        let polys = flatten("M0 5A5 5 0 0 1 10 5Z");
        let last = polys[0].last().unwrap();
        assert!((last[0] - 10.0).abs() < 0.01 && (last[1] - 5.0).abs() < 0.01);
        // Sweeps through the top.
        assert!(polys[0].iter().any(|p| p[1] < 0.5));
    }
}
