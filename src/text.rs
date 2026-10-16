//! Every text the program shows, from a catalog per language (`lang/*.txt`,
//! compiled in): `key = text` lines, `{name}` for a value the program fills
//! in, `\n` for a line break. English is the reference: build.rs turns its
//! keys into [`Msg`], so a key the program uses but the catalog lacks does
//! not compile. Another language may leave keys out — they show in English.
//!
//! build.rs checks every catalog and turns each into its texts in one string
//! with where each ends, so a lookup is two indexes and a slice — nothing is
//! parsed or built at start. The texts QPDF prints are never translated, and
//! nothing translated ever decides anything.

use std::fmt::{Display, Write};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/messages.rs"));

/// A language's texts: one string, the text of slot `s` from `ends[s]` to
/// `ends[s + 1]`.
struct Catalog {
    text: &'static str,
    ends: &'static [u16],
}

/// A language the program speaks.
struct Language {
    /// Its code in `settings.txt` (`language=`) and its file in `lang/`.
    code: &'static str,
    /// Windows' primary language id: the low 10 bits of a LANGID.
    primary_id: u16,
    catalog: &'static Catalog,
}

/// The languages compiled in; the first, English, is the fallback.
static LANGUAGES: [Language; 1] = [Language { code: "en", primary_id: 0x09, catalog: &CATALOG_EN }];

static ACTIVE: OnceLock<&'static Catalog> = OnceLock::new();

/// Choose the language, before any text is shown: `chosen` (`settings.txt`)
/// when the program has it, else the system's (`system`, a Windows LANGID),
/// else English. Only the first call counts.
pub fn init(chosen: Option<&str>, system: u16) {
    let _ = ACTIVE.set(choose(chosen, system).catalog);
}

/// The language [`init`] takes.
fn choose(chosen: Option<&str>, system: u16) -> &'static Language {
    LANGUAGES
        .iter()
        .find(|language| Some(language.code) == chosen)
        .or_else(|| LANGUAGES.iter().find(|language| language.primary_id == system & 0x3ff))
        .unwrap_or(&LANGUAGES[0])
}

/// The text in slot `form` of `message` (a message without plural forms has
/// only one) in `catalog`.
fn text_in(catalog: &Catalog, message: Msg, form: usize) -> &'static str {
    let index = message as usize;
    let first = usize::from(FIRST_SLOT[index]);
    let slot = if usize::from(FIRST_SLOT[index + 1]) - first == 3 { first + form } else { first };
    catalog.text.get(usize::from(catalog.ends[slot])..usize::from(catalog.ends[slot + 1])).unwrap_or("")
}

/// [`text_in`] the chosen language — English before one was chosen (tests).
fn text(message: Msg, form: usize) -> &'static str {
    text_in(ACTIVE.get().copied().unwrap_or(LANGUAGES[0].catalog), message, form)
}

/// The text of `message` (of a plural one, the `many` form).
pub fn get(message: Msg) -> &'static str {
    text(message, MANY)
}

/// The text of `message` for `count` of something.
pub fn plural(message: Msg, count: u64) -> &'static str {
    text(message, form(count))
}

/// The English text of `message`, whatever the language shown: dev tools
/// (`QD_SHOW`) name things the same in every language.
#[cfg(any(debug_assertions, test))]
pub fn english(message: Msg) -> &'static str {
    text_in(LANGUAGES[0].catalog, message, MANY)
}

/// The plural form for most counts.
const MANY: usize = 2;

/// Which plural form `count` takes, by the Polish rule: 0 `one` (1), 1 `few`
/// (2–4, 22–24, … but not 12–14), 2 `many` (the rest). English, whose `few`
/// is its `many`, gets the right one too.
fn form(count: u64) -> usize {
    match (count % 10, count % 100) {
        _ if count == 1 => 0,
        (2..=4, tens) if !(12..=14).contains(&tens) => 1,
        _ => MANY,
    }
}

/// `template` with each `{name}` replaced by its value in `values`; an
/// unknown name stays as written.
pub fn fill(template: &str, values: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').and_then(|close| values.iter().find(|(name, _)| *name == &after[..close]).map(|value| (close, value))) {
            Some((close, (_, value))) => {
                let _ = write!(out, "{value}");
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of a message, by its name in [`Msg`] — with `name = value`
/// pairs filling its `{name}`s: `t!(StatusReady)`, `t!(ProfileSaved, name = name)`.
#[macro_export]
macro_rules! t {
    ($message:ident) => {
        $crate::text::get($crate::text::Msg::$message)
    };
    ($message:ident, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::text::fill($crate::text::get($crate::text::Msg::$message), &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

/// The text of a plural message for `count`, which also fills its
/// `{count}`: `tn!(AddedFiles, 3)`, `tn!(AddedLeftOut, n, limit = max)`.
#[macro_export]
macro_rules! tn {
    ($message:ident, $count:expr $(, $name:ident = $value:expr)* $(,)?) => {{
        let count = $count as u64;
        $crate::text::fill(
            $crate::text::plural($crate::text::Msg::$message, count),
            &[("count", &count as &dyn ::std::fmt::Display) $(, (stringify!($name), &$value as &dyn ::std::fmt::Display))*],
        )
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_come_from_their_slots() {
        assert_eq!(get(Msg::ItemProfileDefault), "Default");
        assert_eq!(get(Msg::PagesSome), "{chosen} of {total} pages: {pages}", "the slot after a plural message's three");
        assert_eq!(plural(Msg::AddedFiles, 1), "Added {count} file");
        assert_eq!(plural(Msg::AddedFiles, 3), "Added {count} files", "English `few` is its `many`");
        assert_eq!(get(Msg::AddedFiles), "Added {count} files");
        assert_eq!(plural(Msg::ItemProfileDefault, 1), "Default", "a message without plural forms has one text");
        assert_eq!(english(Msg::StepProtect), "Protection");
        assert_eq!(get(Msg::FailureNoFreeName), "Every name from “(2)” to “(9999)” is taken", "the last slot");
    }

    #[test]
    fn plural_forms_follow_the_polish_rule() {
        let forms: Vec<usize> = [0, 1, 2, 4, 5, 11, 12, 14, 21, 22, 24, 25, 101, 112, 122].map(form).to_vec();
        assert_eq!(forms, [2, 0, 1, 1, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 1]);
    }

    #[test]
    fn values_are_filled_in_by_name() {
        let one: &dyn Display = &1;
        assert_eq!(fill("{a} and {b}, {a}", &[("a", one), ("b", &"two")]), "1 and two, 1");
        assert_eq!(fill("{unknown} {a", &[("a", one)]), "{unknown} {a", "left as written");
        assert_eq!(fill("no values", &[]), "no values");
    }

    #[test]
    fn the_language_is_chosen_from_the_settings_then_the_system() {
        let pick = |chosen, system| choose(chosen, system).code;
        assert_eq!(pick(Some("en"), 0x0415), "en");
        assert_eq!(pick(Some("xx"), 0x0409), "en", "English: US English system");
        assert_eq!(pick(None, 0x0809), "en", "British English: the same primary language");
        assert_eq!(pick(None, 0x0411), "en", "a language the program lacks falls back to English");
    }

    #[test]
    fn every_message_is_used() {
        let mut source = String::new();
        let mut folders = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).expect("src").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    source += &std::fs::read_to_string(&path).expect("read");
                }
            }
        }
        for name in NAMES {
            let used = [format!("t!({name}"), format!("tn!({name}"), format!("Msg::{name}")].iter().any(|form| source.contains(form.as_str()));
            assert!(used, "`{name}` is in lang/en.txt but no code uses it");
        }
    }
}
