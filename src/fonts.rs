//! System font fallbacks.
//!
//! egui's bundled fonts cover Latin plus a small emoji/icon set, so CJK, Arabic, Indic,
//! Thai, most symbols and emoji in titles would render as boxes. We add the OS's own
//! fonts as fallbacks. They are memory-mapped rather than read, so a 20 MB CJK
//! collection costs only the pages whose glyphs are actually drawn.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// (file, collection index) in priority order; missing files are skipped.
#[cfg(windows)]
const CANDIDATES: &[(&str, u32)] = &[
    ("C:\\Windows\\Fonts\\segoeui.ttf", 0),   // Latin ext., Greek, Cyrillic, Arabic, Hebrew
    ("C:\\Windows\\Fonts\\seguisym.ttf", 0),  // symbols, arrows, dingbats
    ("C:\\Windows\\Fonts\\YuGothM.ttc", 0),   // Japanese
    ("C:\\Windows\\Fonts\\msyh.ttc", 0),      // Simplified Chinese
    ("C:\\Windows\\Fonts\\msjh.ttc", 0),      // Traditional Chinese
    ("C:\\Windows\\Fonts\\malgun.ttf", 0),    // Korean
    ("C:\\Windows\\Fonts\\Nirmala.ttc", 0),   // Indic scripts
    ("C:\\Windows\\Fonts\\Nirmala.ttf", 0),
    ("C:\\Windows\\Fonts\\LeelawUI.ttf", 0),  // Thai, Lao, Khmer
    ("C:\\Windows\\Fonts\\ebrima.ttf", 0),    // Ethiopic, N'Ko, etc.
    ("C:\\Windows\\Fonts\\seguiemj.ttf", 0),  // emoji (drawn monochrome)
];

#[cfg(target_os = "macos")]
const CANDIDATES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
    ("/System/Library/Fonts/Apple Symbols.ttf", 0),
];

#[cfg(all(unix, not(target_os = "macos")))]
const CANDIDATES: &[(&str, u32)] = &[
    ("/usr/share/fonts/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
];

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    for &(path, index) in CANDIDATES {
        let Some(bytes) = map(path) else { continue };
        let name = format!("sys:{path}");
        defs.font_data.insert(name.clone(), Arc::new(FontData { index, ..FontData::from_static(bytes) }));
        // Fallbacks go after egui's own fonts so the look of Latin text is unchanged.
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(family).or_default().push(name.clone());
        }
    }
    ctx.set_fonts(defs);
}

fn map(path: &str) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system font files aren't modified while in use; the mapping is
    // read-only and intentionally lives for the rest of the process.
    let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    Some(Box::leak(Box::new(mmap)))
}
