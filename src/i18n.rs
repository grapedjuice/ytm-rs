//! The app's own text in the listener's language. English (`locales/en-US`) is the
//! source; each other language is a folder of the same files beside it, built into the
//! app. A message a language hasn't translated yet shows in English. The language is
//! picked once, at start (the interface isn't laid out again for it), so a change in
//! Settings applies from the next start.
//!
//! `tr!("id")` is a message; `tr!("id", count = n, name = s)` one with variables, for
//! the language to place and to pick its plural by.
//!
//! What YouTube Music sends (titles, names, its shelves) isn't the app's own, and stays
//! as it comes.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use fluent_templates::fluent_bundle::FluentValue;
use fluent_templates::{LanguageIdentifier, Loader};

fluent_templates::static_loader! {
    static LOCALES = {
        locales: "./locales",
        fallback_language: "en-US",
        // The marks Fluent puts around a variable keep it in order in right-to-left text,
        // which the interface's text already lays out by itself; drawn, they'd show as boxes.
        customise: |bundle| bundle.set_use_isolating(false),
    };
}

/// The languages Settings offers: a language is added once it's mostly translated (what
/// it hasn't got yet shows in English).
pub const LANGUAGES: &[&str] = &["en-US"];

/// Dev hook: YTM_PSEUDO_LOCALE=1 shows every message accented, bracketed and a good deal
/// longer, the way a translation can be. Text still plain is text not yet translatable;
/// text cut off has too little room.
const PSEUDO: &str = "YTM_PSEUDO_LOCALE";

struct Active {
    lang: LanguageIdentifier,
    pseudo: bool,
}

static ACTIVE: OnceLock<Active> = OnceLock::new();

/// Picks this run's language: the one picked in Settings if it's offered, else the
/// system's nearest, else English. Only the first call counts.
pub fn init(picked: Option<&str>) {
    let lang = resolve(picked);
    log::info!("language: {lang}");
    let _ = ACTIVE.set(Active { lang: lang.parse().unwrap_or_else(|_| english()), pseudo: std::env::var_os(PSEUDO).is_some() });
}

/// The language a start with `picked` shows (see [`init`]).
pub fn resolve(picked: Option<&str>) -> &'static str {
    choose(picked, &sys_locale::get_locales().collect::<Vec<_>>())
}

fn english() -> LanguageIdentifier {
    fluent_templates::langid!("en-US")
}

fn active() -> &'static Active {
    ACTIVE.get_or_init(|| Active { lang: english(), pseudo: false })
}

/// The language this run shows (one of [`LANGUAGES`]).
pub fn current() -> String {
    active().lang.to_string()
}

/// Of [`LANGUAGES`]: `picked`, else the first of the system's languages offered (its
/// region's own if there is one, else the same language's), else English.
fn choose(picked: Option<&str>, system: &[String]) -> &'static str {
    let offered = |tag: &str| LANGUAGES.iter().copied().find(|l| l.eq_ignore_ascii_case(tag));
    if let Some(l) = picked.and_then(offered) {
        return l;
    }
    let base = |tag: &str| tag.split(['-', '_']).next().unwrap_or_default().to_ascii_lowercase();
    for want in system {
        // Some systems say pt_BR, or add an encoding (en_US.UTF-8).
        let want = want.split('.').next().unwrap_or_default().replace('_', "-");
        if let Some(l) = offered(&want).or_else(|| LANGUAGES.iter().copied().find(|l| base(l) == base(&want))) {
            return l;
        }
    }
    "en-US"
}

/// A language's own name for itself, for the picker.
pub fn name(lang: &str) -> String {
    let id: LanguageIdentifier = lang.parse().unwrap_or_else(|_| english());
    LOCALES.lookup(&id, "language-name")
}

/// The message `id`. Each is looked up once a run and kept, so it's cheap every frame.
pub fn text(id: &'static str) -> &'static str {
    static KEPT: Mutex<Option<HashMap<&'static str, &'static str>>> = Mutex::new(None);
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    let kept = kept.get_or_insert_with(HashMap::new);
    if let Some(t) = kept.get(id) {
        return t;
    }
    let t: &'static str = Box::leak(shown(LOCALES.lookup(&active().lang, id)).into_boxed_str());
    kept.insert(id, t);
    t
}

/// The message `id` with its variables filled in.
pub fn format(id: &'static str, args: &[(&'static str, FluentValue<'_>)]) -> String {
    let args: HashMap<Cow<'static, str>, FluentValue> = args.iter().map(|(k, v)| (Cow::Borrowed(*k), v.clone())).collect();
    shown(LOCALES.lookup_with_args(&active().lang, id, &args))
}

/// For `tr!`: a variable's value.
pub fn arg<'a>(v: impl Into<FluentValue<'a>>) -> FluentValue<'a> {
    v.into()
}

fn shown(t: String) -> String {
    if active().pseudo { pseudo(&t) } else { t }
}

/// "Keep on top" as "[Ķééƥ ŏñ ţŏƥ ~~~~]": a translation is often this much longer.
fn pseudo(t: &str) -> String {
    const FROM: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const TO: &str = "ȧƀçđéƒĝĥîĵķĺɱñŏƥǫŕšţûṽŵẋýžÅƁÇĐÉƑĜĤÎĴĶĹṀÑÖƤǪŔŠŢÛṼŴẊÝŽ";
    let body: String = t.chars().map(|c| FROM.find(c).and_then(|i| TO.chars().nth(i)).unwrap_or(c)).collect();
    let pad = "~".repeat((t.chars().count() * 2 / 5).max(1));
    format!("[{body} {pad}]")
}

/// A message, by id: see the module's notes.
#[macro_export]
macro_rules! tr {
    ($id:literal) => {
        $crate::i18n::text($id)
    };
    ($id:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::format($id, &[$((stringify!($name), $crate::i18n::arg($value))),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id in `src` that `tr!` asks for, with where.
    fn used_ids() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("i18n.rs") {
                    let src = std::fs::read_to_string(&p).unwrap();
                    for (i, _) in src.match_indices("tr!(\"") {
                        // Not include_str!( and the like.
                        if src[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                            continue;
                        }
                        let rest = &src[i + 5..];
                        let id = &rest[..rest.find('"').unwrap()];
                        out.push((id.to_owned(), p.display().to_string()));
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out
    }

    /// The ids a language's files define.
    fn defined_ids(lang: &str) -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales").join(lang);
        let mut ids = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())).flatten() {
            let src = std::fs::read_to_string(e.path()).unwrap();
            // A message starts a line with its id; its value's other lines are indented.
            for line in src.lines() {
                if let Some((id, _)) = line.split_once(" =")
                    && line.starts_with(|c: char| c.is_ascii_alphabetic())
                {
                    ids.push(id.trim().to_owned());
                }
            }
        }
        ids
    }

    #[test]
    fn every_id_used_is_in_english_and_every_english_id_is_used() {
        let used = used_ids();
        let english = defined_ids("en-US");
        let missing: Vec<_> = used.iter().filter(|(id, _)| !english.contains(id)).collect();
        assert!(missing.is_empty(), "not in locales/en-US: {missing:?}");
        let unused: Vec<_> = english.iter().filter(|id| *id != "language-name" && !used.iter().any(|(u, _)| u == *id)).collect();
        assert!(unused.is_empty(), "in locales/en-US but not used: {unused:?}");
        let mut seen = std::collections::HashSet::new();
        let twice: Vec<_> = english.iter().filter(|id| !seen.insert(*id)).collect();
        assert!(twice.is_empty(), "defined twice in locales/en-US: {twice:?}");
    }

    #[test]
    fn every_languages_files_read_as_written() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        for lang in std::fs::read_dir(&root).unwrap().flatten() {
            for file in std::fs::read_dir(lang.path()).unwrap().flatten() {
                let src = std::fs::read_to_string(file.path()).unwrap();
                if let Err((_, errors)) = fluent_templates::fluent_bundle::FluentResource::try_new(src) {
                    panic!("{}: {errors:?}", file.path().display());
                }
            }
        }
        // And loaded: a message with no variables, in English.
        assert_eq!(LOCALES.lookup(&english(), "settings-language"), "Language");
    }

    #[test]
    fn every_language_offered_has_its_files_and_its_name() {
        for lang in LANGUAGES {
            assert!(!defined_ids(lang).is_empty(), "{lang} has no files");
            assert_ne!(name(lang), "", "{lang} has no language-name");
        }
    }

    #[test]
    fn the_language_is_the_picked_one_else_the_systems_else_english() {
        let sys = |s: &[&str]| s.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(choose(Some("en-US"), &sys(&["fr-FR"])), "en-US");
        // Not offered (yet): the system's, then English.
        assert_eq!(choose(Some("xx-XX"), &sys(&["en_GB.UTF-8"])), "en-US");
        assert_eq!(choose(None, &sys(&["fr-FR"])), "en-US");
    }

    #[test]
    fn variables_and_plurals_fill_in() {
        assert_eq!(format("likes-removed", &[("count", arg(1))]), "Removed 1 like");
        assert_eq!(format("likes-removed", &[("count", arg(3))]), "Removed 3 likes");
    }

    #[test]
    fn pseudo_is_longer_and_bracketed() {
        let p = pseudo("Keep on top");
        assert!(p.starts_with('[') && p.ends_with(']') && p.chars().count() > "Keep on top".len() + 4, "{p}");
        assert!(!p.contains('K'));
    }
}
