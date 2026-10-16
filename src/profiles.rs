//! Profiles: named sets of steps and options, kept in `profiles.txt`
//! next to the executable as `[name]` sections of `key=value` lines.
//!
//! What a key means is the steps panel's business (`app::steps`); this module
//! only keeps the pairs, in their order. Passwords and the files' own pages
//! are never among them. A damaged entry — a line without `=`,
//! a bad key, a section with a bad or repeated name — is skipped without a
//! word; the rest of the file still counts.

use crate::t;
use std::io;
use std::path::{Path, PathBuf};

/// One named set of the panel's values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// `(key, value)` in the panel's order; a key may repeat (a list).
    pub values: Vec<(String, String)>,
}

/// The panel as it starts, in English — not a stored profile, so no profile
/// may take this name. (The window also refuses its name in the language
/// shown when one is saved; which profiles load never depends on it.)
pub const DEFAULT: &str = "Default";

/// Most profiles kept, and most characters of a name — a menu, not a database.
const MAX_PROFILES: usize = 100;
const MAX_NAME_CHARS: usize = 40;

/// The most of `profiles.txt` read: far beyond any real one.
const MAX_FILE_BYTES: u64 = 1 << 20;

fn profiles_file(dir: &Path) -> PathBuf {
    dir.join("profiles.txt")
}

/// `typed` as a profile name, trimmed — or why it cannot be one. Only
/// letters, digits, spaces and `- . , ( ) + # '`: a name is a menu item, and
/// FLTK reads `/`, `&`, `_` and `|` in one as commands.
pub fn check_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err(t!(ProfileNameEmpty).into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(t!(ProfileNameTooLong).into());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '.' | ',' | '(' | ')' | '+' | '#' | '\'')) {
        return Err(t!(ProfileNameCharacters).into());
    }
    if name.eq_ignore_ascii_case(DEFAULT) {
        return Err(t!(ProfileNameDefault, name = name));
    }
    Ok(name.to_string())
}

/// A key the panel could have written: lowercase words joined by `.` or `_`.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_'))
}

/// A `key=value` line as a pair: the key trimmed and valid, the value as it
/// was, spaces too.
fn pair(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    valid_key(key.trim()).then(|| (key.trim().to_string(), value.to_string()))
}

/// Whether `value` can be written as a line: one with a line break (pasted
/// into a field) would split into a line of its own — even a `[section]` —
/// so it is left out.
fn storable(value: &str) -> bool {
    !value.contains(['\r', '\n'])
}

/// The profiles in `text`, skipping whatever is damaged.
pub fn parse(text: &str) -> Vec<Profile> {
    let mut profiles: Vec<Profile> = Vec::new();
    // Lines go to the last good section; after a bad header, to none.
    let mut open = false;
    for line in text.lines() {
        if let Some(header) = line.trim().strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            open = match check_name(header) {
                Ok(name) if profiles.len() < MAX_PROFILES && !profiles.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) => {
                    profiles.push(Profile { name, values: Vec::new() });
                    true
                }
                _ => false,
            };
            continue;
        }
        if let (Some(profile), Some(pair)) = (profiles.last_mut().filter(|_| open), pair(line)) {
            profile.values.push(pair);
        }
    }
    profiles
}

/// `profiles` as the text of `profiles.txt` (the inverse of [`parse`]), but
/// for the values not [`storable`].
pub fn serialize(profiles: &[Profile]) -> String {
    let mut text = String::new();
    for profile in profiles {
        text.push_str(&format!("[{}]\r\n", profile.name));
        for (key, value) in profile.values.iter().filter(|(_, value)| storable(value)) {
            text.push_str(&format!("{key}={value}\r\n"));
        }
        text.push_str("\r\n");
    }
    text
}

/// The profiles saved in `dir`; none when there are none or the file cannot
/// be read.
pub fn load(dir: &Path) -> Vec<Profile> {
    crate::settings::read_text(&profiles_file(dir), MAX_FILE_BYTES).map(|text| parse(&text)).unwrap_or_default()
}

/// Save `profiles` in `dir`, never half-written.
pub fn save(dir: &Path, profiles: &[Profile]) -> io::Result<()> {
    crate::settings::write_text(&profiles_file(dir), &serialize(profiles))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, values: &[(&str, &str)]) -> Profile {
        Profile { name: name.into(), values: values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    #[test]
    fn profiles_survive_the_round_trip() {
        let profiles = [
            profile("Archive", &[("step.optimize", "yes"), ("attachments.file", r"C:\a.txt"), ("attachments.file", r"C:\b=c.txt")]),
            profile("Web (small)", &[("output.name", " web = small ")]),
            profile("Empty", &[]),
        ];
        assert_eq!(parse(&serialize(&profiles)), profiles);
    }

    #[test]
    fn damaged_entries_are_skipped_and_the_rest_kept() {
        let text = "stray=line before any section\n\
                    [Good]\nstep.pages=yes\nno equals sign\nBad Key=x\n\
                    [bad/name]\nstep.pages=no\n\
                    [good]\nstep.pages=no\n\
                    [Default]\nstep.pages=no\n\
                    [Second]\r\n output.name =x\r\n";
        let parsed = parse(text);
        assert_eq!(parsed, [profile("Good", &[("step.pages", "yes")]), profile("Second", &[("output.name", "x")])]);
        assert_eq!(parse(""), []);
        assert_eq!(parse("\u{0}\u{1}garbage]["), []);
    }

    #[test]
    fn names_are_plain_menu_text() {
        assert_eq!(check_name("  Web (small), v2 ").as_deref(), Ok("Web (small), v2"));
        assert_eq!(check_name("Skany 300 dpi #2").as_deref(), Ok("Skany 300 dpi #2"));
        assert_eq!(check_name("zażółć"), Ok("zażółć".into()));
        for bad in ["", "   ", "a/b", "a&b", "a_b", "a|b", "[x]", "a\tb", "default", "DEFAULT"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name(&"x".repeat(41)).is_err());
    }

    #[test]
    fn a_hundred_profiles_at_most() {
        let text: String = (0..150).map(|n| format!("[P{n}]\nk=v\n")).collect();
        assert_eq!(parse(&text).len(), MAX_PROFILES);
    }

    #[test]
    fn saved_profiles_load_back_and_a_missing_file_is_none() {
        let dir = std::env::temp_dir().join(format!("qd_profiles_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        assert_eq!(load(&dir), []);
        let profiles = [profile("One", &[("step.privacy", "yes")])];
        save(&dir, &profiles).expect("saved");
        save(&dir, &profiles).expect("saved over the old file");
        assert_eq!(load(&dir), profiles);
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 1, "no temporary file left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn random_files_and_values_round_trip_without_breaking_a_line() {
        let alphabet = crate::fuzz::chars("[]=ab. _\r\n#Default");
        let mut rng = crate::fuzz::Rng::new(0x5EED_0007);
        for _ in 0..20_000 {
            let mut profiles = parse(&rng.text(&alphabet, 60));
            // Values as a text field may hold them: a pasted line break, a
            // `[section]` after it.
            for profile in &mut profiles {
                profile.values.push(("output.name".into(), rng.text(&alphabet, 20)));
            }
            let kept: Vec<Profile> = profiles
                .iter()
                .map(|p| Profile { name: p.name.clone(), values: p.values.iter().filter(|(_, v)| !v.contains(['\r', '\n'])).cloned().collect() })                .collect();
            assert_eq!(parse(&serialize(&profiles)), kept, "a value with a line break is left out, never split");
        }
    }
}
