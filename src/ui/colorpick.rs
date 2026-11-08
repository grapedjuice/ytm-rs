//! A colour picker drawn like the rest of the app (egui's own has RGB/alpha rows and
//! blending options an accent doesn't need): a round swatch that opens a saturation and
//! brightness square, a hue bar and a hex field.

use egui::{Align2, Color32, ColorImage, Id, Sense, Stroke, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2};

use super::widgets;
use crate::theme;

const WIDTH: f32 = 216.0;

/// A round swatch of `c` that opens the picker.
pub fn button(ui: &mut Ui, c: &mut Color32, name: &str) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let resp = widgets::named(resp, name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.has_focus() || open, 0.12);
    ui.painter().circle_filled(rect.center(), 11.0, *c);
    ui.painter().circle_stroke(rect.center(), 14.0, Stroke::new(1.5, theme::glass(0.18 + 0.27 * h)));
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| picker(ui, resp.id, c));
}

fn picker(ui: &mut Ui, id: Id, c: &mut Color32) {
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    // Hue and saturation are kept between frames: dragging through grey or black loses
    // them from the colour itself, and the marker would jump.
    let key = id.with("hsv");
    let mut hsv = ui.data(|d| d.get_temp::<(Color32, [f32; 3])>(key)).filter(|(was, _)| was == c).map_or_else(|| to_hsv(*c), |(_, hsv)| hsv);
    let mut changed = false;
    ui.add_space(6.0);

    // Saturation across, brightness down.
    let (sv_rect, sv) = ui.allocate_exact_size(vec2(WIDTH, 150.0), Sense::click_and_drag());
    if let Some(p) = sv.interact_pointer_pos() {
        hsv[1] = ((p.x - sv_rect.left()) / sv_rect.width()).clamp(0.0, 1.0);
        hsv[2] = 1.0 - ((p.y - sv_rect.top()) / sv_rect.height()).clamp(0.0, 1.0);
        changed = true;
    }
    ui.add_space(12.0);
    let (hue_rect, hue) = ui.allocate_exact_size(vec2(WIDTH, 14.0), Sense::click_and_drag());
    if let Some(p) = hue.interact_pointer_pos() {
        hsv[0] = ((p.x - hue_rect.left()) / hue_rect.width()).clamp(0.0, 1.0);
        changed = true;
    }
    ui.add_space(12.0);

    // Hex: shows the colour, and takes a typed or pasted one as soon as it's whole.
    let (field, _) = ui.allocate_exact_size(vec2(WIDTH, 36.0), Sense::hover());
    ui.painter().rect_filled(field, 10.0, theme::glass(0.08));
    let edit_id = id.with("hex");
    let mut text = if ui.memory(|m| m.has_focus(edit_id)) { ui.data(|d| d.get_temp::<String>(edit_id)).unwrap_or_default() } else { hex(*c) };
    let inner = field.shrink2(vec2(12.0, 4.0));
    let edit = ui.put(
        inner,
        egui::TextEdit::singleline(&mut text)
            .id(edit_id)
            .font(theme::regular(14.0))
            .frame(egui::Frame::NONE)
            .align(Align2::LEFT_CENTER)
            .desired_width(inner.width()),
    );
    if edit.changed()
        && let Some(typed) = crate::preset::hex(&format!("#{}", text.trim().trim_start_matches('#')))
    {
        hsv = to_hsv(typed);
        changed = true;
    }
    ui.data_mut(|d| d.insert_temp(edit_id, text));
    ui.add_space(6.0);

    if changed {
        *c = from_hsv(hsv);
    }
    ui.data_mut(|d| d.insert_temp(key, (*c, hsv)));

    let [h, s, v] = hsv;
    let sv_tex = gradient(ui, id.with("sv-tex"), h, [48, 48], |x, y| from_hsv([h, x, 1.0 - y]));
    egui::Image::from_texture(&sv_tex).corner_radius(10.0).paint_at(ui, sv_rect);
    let hue_tex = gradient(ui, id.with("hue-tex"), 0.0, [192, 1], |x, _| from_hsv([x, 1.0, 1.0]));
    egui::Image::from_texture(&hue_tex).corner_radius(7.0).paint_at(ui, hue_rect);
    knob(ui, pos2(sv_rect.left() + s * sv_rect.width(), sv_rect.bottom() - v * sv_rect.height()), *c);
    knob(ui, pos2(hue_rect.left() + h * hue_rect.width(), hue_rect.center().y), from_hsv([h, 1.0, 1.0]));
}

/// A white-ringed marker filled with the colour under it.
fn knob(ui: &Ui, at: egui::Pos2, fill: Color32) {
    let p = ui.painter();
    p.circle_stroke(at, 9.5, Stroke::new(1.0, Color32::from_black_alpha(90)));
    p.circle(at, 8.0, fill, Stroke::new(2.0, Color32::WHITE));
}

/// A gradient texture, made again only when `tag` (the hue it shows) changes.
fn gradient(ui: &Ui, key: Id, tag: f32, size: [usize; 2], at: impl Fn(f32, f32) -> Color32) -> TextureHandle {
    if let Some((was, tex)) = ui.data(|d| d.get_temp::<(f32, TextureHandle)>(key))
        && was == tag
    {
        return tex;
    }
    let [w, h] = size;
    let t = |i: usize, n: usize| i as f32 / (n - 1).max(1) as f32;
    let pixels = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| at(t(x, w), t(y, h))).collect();
    let tex = ui.ctx().load_texture("color-picker", ColorImage::new(size, pixels), TextureOptions::LINEAR);
    ui.data_mut(|d| d.insert_temp(key, (tag, tex.clone())));
    tex
}

fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Hue, saturation and value, each 0–1, from an sRGB colour.
fn to_hsv(c: Color32) -> [f32; 3] {
    let [r, g, b] = [c.r(), c.g(), c.b()].map(|x| x as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, if max == 0.0 { 0.0 } else { d / max }, max]
}

fn from_hsv([h, s, v]: [f32; 3]) -> Color32 {
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        let x = v - v * s * k.min(4.0 - k).clamp(0.0, 1.0);
        (x * 255.0).round() as u8
    };
    Color32::from_rgb(f(5.0), f(3.0), f(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips() {
        for c in [Color32::from_rgb(78, 165, 220), Color32::from_rgb(255, 48, 72), Color32::BLACK, Color32::WHITE, Color32::from_rgb(10, 200, 30)] {
            assert_eq!(from_hsv(to_hsv(c)), c);
        }
    }
}
