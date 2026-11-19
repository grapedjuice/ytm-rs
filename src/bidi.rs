//! Right-to-left text (Arabic, Hebrew, …) in display order.
//!
//! egui shapes text a font at a time: harfrust sets the letters of an Arabic or Hebrew
//! stretch right to left, but the stretches themselves go left to right. Those scripts
//! come from a system fallback font while spaces stay in the primary one, so every word
//! is its own stretch and a sentence came out with its words reversed (#29). Here the
//! Unicode bidi algorithm puts the words in display order; each word's letters are
//! still left for harfrust to set.

use std::borrow::Cow;

use unicode_bidi::{Level, ParagraphBidiInfo};

/// Letters (and their marks) of the right-to-left scripts, which fonts set as one
/// right-to-left stretch.
pub fn rtl_letter(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

/// Whether the text has any right-to-left letters.
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(rtl_letter)
}

/// Whether a line reads right to left: its first letter with a direction is RTL.
pub fn rtl_base(text: &str) -> bool {
    has_rtl(text) && ParagraphBidiInfo::new(text, None).paragraph_level.is_rtl()
}

/// Brackets and the like swap in right-to-left text.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        c => c,
    }
}

/// `text` in display order, for egui to lay out left to right. Text without RTL letters
/// is returned as it is. Each line (`\n`) is ordered on its own.
pub fn visual(text: &str) -> Cow<'_, str> {
    if !has_rtl(text) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.split('\n').map(|l| visual_pieces(l, rtl_base(l)).concat()).collect::<Vec<_>>().join("\n"))
}

/// A layout job showing `text` in display order. Each RTL word is a section of its own:
/// egui shapes sections apart, so the font sets each word's letters right to left and
/// nothing else, whichever fonts the words and spaces come from. (Given the reordered
/// text in one piece, a font holding both the letters and the spaces would turn the
/// whole sentence around again.)
pub fn job(text: &str, font: egui::FontId, color: egui::Color32) -> egui::text::LayoutJob {
    job_based(text, font, color, None)
}

/// [`job`], with every line read right to left (`Some(true)`) or left to right
/// (`Some(false)`) whatever it starts with: the interface's own sentences go its
/// language's way, and a wrapped paragraph's rows go the paragraph's. `None`: each
/// line by its first letter with a direction.
pub fn job_based(text: &str, font: egui::FontId, color: egui::Color32, base: Option<bool>) -> egui::text::LayoutJob {
    use egui::epaint::text::{ByteIndex, LayoutSection};
    let mut job = egui::text::LayoutJob::default();
    let format = egui::TextFormat::simple(font, color);
    // Pushed directly: `LayoutJob::append` merges sections that share a format.
    let push = |job: &mut egui::text::LayoutJob, s: &str| {
        let start = job.text.len();
        job.text.push_str(s);
        job.sections.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(start)..ByteIndex(job.text.len()), format: format.clone() });
    };
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            push(&mut job, "\n");
        }
        for piece in visual_pieces(line, base.unwrap_or_else(|| rtl_base(line))) {
            push(&mut job, &piece);
        }
    }
    job
}

/// A line in display order, in pieces: whole left-to-right runs, single RTL words (or
/// stretches of RTL letters), and single other characters between them. `rtl`: the
/// line reads right to left.
fn visual_pieces(line: &str, rtl: bool) -> Vec<String> {
    if !has_rtl(line) {
        return vec![line.to_owned()];
    }
    let info = ParagraphBidiInfo::new(line, Some(if rtl { Level::rtl() } else { Level::ltr() }));
    let (levels, runs) = info.visual_runs(0..line.len());
    let mut out = Vec::new();
    for run in runs {
        let text = &line[run.clone()];
        if !levels[run.start].is_rtl() {
            out.push(text.to_owned());
            continue;
        }
        // Right to left: the pieces in reverse, where a piece is a stretch of RTL
        // letters (which the font sets right to left itself) or one other character.
        let mut pieces: Vec<Cow<str>> = Vec::new();
        let mut start: Option<usize> = None;
        for (i, c) in text.char_indices() {
            if rtl_letter(c) {
                start.get_or_insert(i);
                continue;
            }
            if let Some(s) = start.take() {
                pieces.push(Cow::Borrowed(&text[s..i]));
            }
            pieces.push(Cow::Owned(mirror(c).to_string()));
        }
        if let Some(s) = start {
            pieces.push(Cow::Borrowed(&text[s..]));
        }
        out.extend(pieces.into_iter().rev().map(Cow::into_owned));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ltr_text_is_untouched() {
        assert!(matches!(visual("Hello (world)"), Cow::Borrowed(_)));
    }

    #[test]
    fn rtl_words_go_right_to_left() {
        // Each word keeps its letters in logical order (the font sets them); the words swap.
        assert_eq!(visual("שלום עולם"), "עולם שלום");
        assert_eq!(visual("مرحبا بالعالم"), "بالعالم مرحبا");
        // Punctuation at the end of an RTL sentence lands on the left.
        assert_eq!(visual("שלום עולם."), ".עולם שלום");
    }

    #[test]
    fn numbers_and_latin_stay_left_to_right() {
        assert_eq!(visual("שיר 123"), "123 שיר");
        assert_eq!(visual("אני אוהב Taylor Swift"), "Taylor Swift אוהב אני");
        // A Latin title with an RTL word inside keeps its order around it.
        assert_eq!(visual("Song (שלום עולם) remix"), "Song (עולם שלום) remix");
    }

    #[test]
    fn brackets_mirror_in_rtl() {
        assert_eq!(visual("(שלום)"), "(שלום)");
        assert!(rtl_base("שלום world"));
        assert!(!rtl_base("hello שלום"));
    }

    fn display(text: &str, rtl: bool) -> String {
        job_based(text, egui::FontId::default(), egui::Color32::WHITE, Some(rtl)).text
    }

    #[test]
    fn a_right_to_left_sentence_starting_in_latin_can_still_read_right_to_left() {
        let line = "256 kbps بدلاً من 128.";
        // Started by a Latin name, the line reads left to right on its own: the name on
        // the left, the stop on the right…
        let ltr = display(line, false);
        assert!(ltr.starts_with("256 kbps") && ltr.ends_with('.'), "{ltr}");
        // …but in a right-to-left interface, the name goes right and the stop left (with
        // the number and unit each read right to left too: "kbps 256").
        let rtl = display(line, true);
        assert!(rtl.starts_with('.') && rtl.ends_with("kbps 256"), "{rtl}");
        assert_eq!(display("الصق الرمز في ListenBrainz", true), "ListenBrainz في الرمز الصق");
    }
}
