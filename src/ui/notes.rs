//! Release notes: a small renderer for GitHub release bodies (headings, paragraphs,
//! bullets, **bold**, `code`, [links](…)), the dialog shown when an update is ready
//! (or to read a version's notes), and the card shown after updating.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Margin, Stroke, Ui, vec2};

use super::{App, widgets};
use crate::theme::{self, icon};

pub enum Block {
    Heading(String),
    Para(String),
    Bullet(String),
}

pub fn parse(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut Vec<Block>| {
        if !para.trim().is_empty() {
            out.push(Block::Para(para.trim().to_owned()));
        }
        para.clear();
    };
    for line in md.lines() {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut para, &mut out);
        } else if t.starts_with('#') {
            flush(&mut para, &mut out);
            out.push(Block::Heading(t.trim_start_matches('#').trim().to_owned()));
        } else if let Some(b) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            flush(&mut para, &mut out);
            out.push(Block::Bullet(b.trim().to_owned()));
        } else if let (true, Some(Block::Bullet(b))) = (para.is_empty(), out.last_mut()) {
            // A wrapped bullet continues on the next line.
            b.push(' ');
            b.push_str(t);
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t);
        }
    }
    flush(&mut para, &mut out);
    out
}

/// Inline spans: (text, bold). Links keep their text; code loses its backticks.
fn spans(s: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut push = |t: &str, bold: bool| match out.last_mut() {
        Some((prev, b)) if *b == bold => prev.push_str(t),
        _ => out.push((t.to_owned(), bold)),
    };
    let mut rest = s;
    let mut bold = false;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("**") {
            bold = !bold;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('`') {
            let end = r.find('`').unwrap_or(r.len());
            push(&r[..end], bold);
            rest = r.get(end + 1..).unwrap_or("");
        } else if let Some(r) = rest.strip_prefix('[')
            && let Some(close) = r.find("](")
            && let Some(end) = r[close..].find(')')
        {
            push(&r[..close], bold);
            rest = &r[close + end + 1..];
        } else {
            let next = rest.char_indices().skip(1).find(|(_, c)| matches!(c, '*' | '`' | '[')).map_or(rest.len(), |(i, _)| i);
            push(&rest[..next], bold);
            rest = &rest[next..];
        }
    }
    out
}

fn plain(s: &str) -> String {
    spans(s).into_iter().map(|(t, _)| t).collect()
}

fn job(text: &str, size: f32, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    for (t, bold) in spans(text) {
        let font = if bold { theme::semibold(size) } else { theme::regular(size) };
        let color = if bold { theme::text() } else { color };
        job.append(&t, 0.0, TextFormat { font_id: font, color, line_height: Some(size * 1.5), ..Default::default() });
    }
    job
}

/// The notes laid out like the release page: section headings, paragraphs, bullets.
pub fn show(ui: &mut Ui, blocks: &[Block], size: f32) {
    let width = ui.available_width();
    let body = body_color();
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Heading(t) => {
                if i > 0 {
                    ui.add_space(12.0);
                }
                let g = widgets::galley(ui, &plain(t), theme::semibold(size + 1.0), theme::text(), width);
                ui.label(g);
                ui.add_space(4.0);
            }
            Block::Para(t) => {
                let g = ui.fonts_mut(|f| f.layout_job(job(t, size, body, width)));
                ui.label(g);
                ui.add_space(6.0);
            }
            Block::Bullet(t) => {
                let g = ui.fonts_mut(|f| f.layout_job(job(t, size, body, width - 18.0)));
                let (rect, _) = ui.allocate_exact_size(vec2(width, g.size().y), egui::Sense::hover());
                let dot = egui::pos2(rect.left() + 6.0, rect.top() + size * 0.75);
                ui.painter().circle_filled(dot, 2.2, body);
                ui.painter().galley(rect.min + vec2(18.0, 0.0), g, body);
                ui.add_space(4.0);
            }
        }
    }
}

/// The card's short list: feature headings and bullets (first sentence), up to
/// three, plus how many more there are. A "Fixes" heading isn't an item itself.
pub fn summary(blocks: &[Block]) -> (Vec<String>, usize) {
    let mut items = Vec::new();
    for b in blocks {
        match b {
            Block::Heading(t) if !t.to_lowercase().starts_with("fix") => items.push(plain(t)),
            Block::Bullet(t) => {
                let t = plain(t);
                let first = t.find(". ").map_or(t.as_str(), |i| &t[..i]);
                items.push(first.trim_end_matches('.').to_owned());
            }
            _ => {}
        }
    }
    let more = items.len().saturating_sub(3);
    items.truncate(3);
    (items, more)
}

/// The notes' paragraphs and bullets.
fn body_color() -> Color32 {
    if theme::light() { theme::text() } else { Color32::from_gray(225) }
}

/// Lines between parts of the card.
fn rule_color() -> Color32 {
    if theme::light() { Color32::from_gray(222) } else { Color32::from_gray(44) }
}

pub(super) fn surface() -> egui::Frame {
    let look = &crate::preset::active().app;
    // A theme's own dialog fill; see-through only when what's behind is really frosted.
    let wanted = look.menu_look.dialog_alpha.unwrap_or(look.menu_look.alpha);
    let alpha = if look.glass.dialog.is_some() && crate::shader::glass_ready() { wanted } else { wanted.max(0.85) };
    let dark = crate::preset::hex(&look.menu_color).map_or(Color32::from_gray(28), |c| theme::with_alpha(c, alpha));
    egui::Frame::new()
        .fill(if theme::light() { theme::surface_fill() } else { dark })
        .stroke(if look.dialog_bottom_line { Stroke::NONE } else { Stroke::new(1.0, rule_color()) })
        .corner_radius(look.menu_radius.unwrap_or(16.0).clamp(0.0, 32.0) as u8)
        .shadow(egui::epaint::Shadow { offset: [0, 24], blur: 70, spread: 0, color: theme::shade(0.55) })
}

fn hline(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, rule_color());
}

pub fn github_link(ui: &mut Ui) {
    ui.hyperlink_to(egui::RichText::new("Full release notes on GitHub").font(theme::regular(13.0)).color(if theme::light() { Color32::from_rgb(0, 102, 204) } else { Color32::from_rgb(62, 166, 255) }), crate::update::RELEASES);
}

/// What the notes dialog shows: a downloaded update (with Restart), or a version's notes.
#[derive(Clone)]
pub struct NotesDialog {
    pub version: String,
    pub ready: bool,
}

pub fn dialog(app: &mut App, ctx: &egui::Context) {
    let Some(d) = app.notes_dialog.clone() else { return };
    let body = app.updater.as_ref().and_then(|u| u.notes(&d.version));
    // For the .deb, which can't install it itself: the new .deb to download.
    let deb_url = app.updater.as_ref().and_then(|u| match u.state() {
        crate::update::State::Available { version, url } if d.ready && version == d.version => Some(url),
        _ => None,
    });
    let (mut close, mut restart) = (false, false);
    let accent = app.accent_color();
    widgets::modal_extras(ctx, egui::Id::unique("release-notes"));
    let modal = egui::Modal::new(egui::Id::unique("release-notes")).frame(surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
        let w = 540.0_f32.min(ctx.content_rect().width() - 48.0);
        ui.set_width(w);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 18 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::icon_at(ui, ui.cursor().min + vec2(8.0, 8.0), icon::SPARKLES, 15.0, accent);
                ui.add_space(22.0);
                let label = if deb_url.is_some() { "UPDATE AVAILABLE" } else if d.ready { "UPDATE READY" } else { "WHAT'S NEW" };
                let mut j = LayoutJob::default();
                j.append(label, 0.0, TextFormat { font_id: theme::semibold(12.0), color: theme::text_dim(), extra_letter_spacing: 1.0, ..Default::default() });
                ui.label(j);
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new(format!("ytm-rs {}", d.version)).font(theme::bold(26.0)).color(theme::text()));
            ui.add_space(6.0);
            let sub = if deb_url.is_some() {
                format!("You're on {}. {}", crate::update::current_version(), crate::update::package_step())
            } else if d.ready {
                format!("You're on {}. It's downloaded and installs when you close the app.", crate::update::current_version())
            } else {
                "The version you're running.".to_owned()
            };
            ui.label(egui::RichText::new(sub).font(theme::regular(13.5)).color(theme::text_dim()));
        });
        hline(ui);
        let max_h = (ctx.content_rect().height() - 330.0).clamp(160.0, 460.0);
        egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            egui::Frame::new().inner_margin(Margin { left: 28, right: 28, top: 18, bottom: 18 }).show(ui, |ui| {
                match &body {
                    Some(md) => show(ui, &parse(md), 14.0),
                    None => {
                        ui.horizontal(|ui| {
                            widgets::loader(ui);
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Loading release notes…").color(theme::text_dim()));
                        });
                    }
                }
                ui.add_space(8.0);
                github_link(ui);
            });
        });
        hline(ui);
        egui::Frame::new().inner_margin(Margin { left: 28, right: 28, top: 16, bottom: 16 }).show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if let Some(url) = &deb_url {
                    if widgets::pill(ui, "Download", None, true).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        close = true;
                    }
                    close |= widgets::pill(ui, "Later", None, false).clicked();
                } else if d.ready {
                    restart = widgets::pill(ui, "Restart to update", None, true).clicked();
                    close = widgets::pill(ui, "Later", None, false).clicked();
                } else {
                    close = widgets::pill(ui, "Close", None, true).clicked();
                }
            });
        });
    });
    if close || modal.should_close() {
        app.notes_dialog = None;
    }
    if restart {
        app.notes_dialog = None;
        app.restart_to_update(ctx);
    }
}

/// "Updated to X" in the corner on the first launch of a new version.
pub fn card(app: &mut App, ctx: &egui::Context) {
    let Some(version) = app.whats_new.clone() else { return };
    let Some(md) = app.updater.as_ref().and_then(|u| u.notes(&version)) else { return };
    let (items, more) = summary(&parse(&md));
    if items.is_empty() {
        app.whats_new = None;
        return;
    }
    let screen = ctx.content_rect();
    let at = egui::pos2(screen.right() - 24.0, screen.bottom() - app.bar_height() - 20.0);
    let t = theme::anim_bool(ctx, "whats-new-in", true, 0.35);
    // A see-through dialog fill needs its frost here too.
    widgets::modal_extras(ctx, egui::Id::unique("whats-new"));
    let (mut dismiss, mut all) = (false, false);
    egui::Area::new(egui::Id::unique("whats-new"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(at + vec2(0.0, 16.0 * (1.0 - t)))
        .show(ctx, |ui| {
            ui.multiply_opacity(t);
            let radius = crate::preset::active().app.menu_radius.unwrap_or(14.0).clamp(0.0, 32.0) as u8;
            surface().corner_radius(radius).inner_margin(Margin { left: 18, right: 12, top: 14, bottom: 16 }).show(ui, |ui| {
                ui.set_width(330.0);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(format!("Updated to {version}")).font(theme::semibold(15.0)).color(theme::text()));
                        ui.add_space(3.0);
                        ui.label(egui::RichText::new("Here's what changed.").font(theme::regular(13.0)).color(theme::text_dim()));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        dismiss = widgets::icon_button(ui, icon::X, 16.0, "Dismiss", false).clicked();
                    });
                });
                ui.add_space(10.0);
                let mut lines: Vec<String> = items.clone();
                if more > 0 {
                    lines.push(if more == 1 { "+ 1 more change".to_owned() } else { format!("+ {more} more changes") });
                }
                for (i, l) in lines.iter().enumerate() {
                    let faint = more > 0 && i == lines.len() - 1;
                    let color = if faint { theme::text_dim() } else { body_color() };
                    let mut j = LayoutJob::single_section(l.clone(), TextFormat { font_id: theme::regular(13.5), color, ..Default::default() });
                    j.wrap.max_width = 300.0;
                    j.wrap.max_rows = 1;
                    let g = ui.fonts_mut(|f| f.layout_job(j));
                    let (rect, _) = ui.allocate_exact_size(vec2(318.0, 22.0), egui::Sense::hover());
                    if !faint {
                        ui.painter().circle_filled(egui::pos2(rect.left() + 5.0, rect.center().y), 2.2, color);
                    }
                    ui.painter().galley(egui::pos2(rect.left() + 16.0, rect.center().y - g.size().y / 2.0), g, color);
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    all = widgets::pill(ui, "See all notes", None, true).clicked();
                    dismiss |= widgets::pill(ui, "Got it", None, false).clicked();
                });
            });
        });
    if all {
        app.notes_dialog = Some(NotesDialog { version: version.clone(), ready: false });
    }
    if all || dismiss {
        app.whats_new = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "## Uploads\n\nPick **files** or drop them.\n\n## Fixes\n\n- Mixes play again. They broke in 0.2.9.\n- Icons are `centred`\n  in their buttons.\n- See [the notes](https://example.com).\n";

    #[test]
    fn parses_release_markdown() {
        let b = parse(MD);
        assert!(matches!(&b[0], Block::Heading(t) if t == "Uploads"));
        assert!(matches!(&b[1], Block::Para(t) if t == "Pick **files** or drop them."));
        assert!(matches!(&b[4], Block::Bullet(t) if t == "Icons are `centred` in their buttons."));
        assert_eq!(spans("Pick **files** now"), vec![("Pick ".into(), false), ("files".into(), true), (" now".into(), false)]);
        assert_eq!(plain("See [the notes](https://example.com)."), "See the notes.");
    }

    #[test]
    fn summary_takes_features_then_first_sentences() {
        let (items, more) = summary(&parse(MD));
        assert_eq!(items, vec!["Uploads", "Mixes play again", "Icons are centred in their buttons"]);
        assert_eq!(more, 1);
    }
}
