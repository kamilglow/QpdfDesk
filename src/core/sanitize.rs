//! Names from inside a PDF made safe as Windows file names. An attachment's
//! name is chosen by whoever made the file, so it may
//! try to climb out of the folder (`..\..\x`), name a drive (`C:\Windows\x`)
//! or a device (`CON`, `nul.txt`), hide a stream (`a:b`), lose its extension
//! to trailing dots, or disguise it (`invoice\u{202E}fdp.exe`).
//!
//! Only the last part of the name is kept, every character Windows forbids
//! (and every control or direction mark) becomes `_`, device names get a `_`
//! in front, and the length is capped with the extension kept. Collisions are
//! left to the caller, which never overwrites (`output::commit`). A name the
//! user types for a result meets the same rules, but is refused, not fixed.

use crate::t;

/// Longest name kept, in UTF-16 units — far below NTFS's 255, so the folder
/// and a ` (2)` still fit.
const MAX_NAME: usize = 100;

/// Longest extension kept apart when a name is cut.
const MAX_EXTENSION: usize = 16;

/// The name used when nothing usable is left.
const FALLBACK: &str = "attachment";

/// A safe file name for `raw`, a name from a PDF.
pub fn file_name(raw: &str) -> String {
    // The last part only: no folder, no `..`, no drive.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last.chars().map(|c| if forbidden(c) { '_' } else { c }).collect();
    // Windows drops dots and spaces at the end (`a.exe.` would open as
    // `a.exe`); spaces at the start are legal, but invisible.
    let trimmed = cleaned.trim_end_matches(['.', ' ']).trim_start_matches(' ');
    let name = shorten(if trimmed.is_empty() { FALLBACK } else { trimmed });
    // After the cut, which can leave one (`CON` + spaces + `y.txt`); the `_`
    // may need it to cut again.
    if is_device(&name) { shorten(&format!("_{name}")) } else { name }
}

/// Why `typed`, a name the user typed for a result, cannot be one —
/// refused, not fixed: they can type it again.
pub fn check_typed(typed: &str) -> Result<(), &'static str> {
    if typed.contains(['\\', '/']) || typed.chars().any(forbidden) {
        return Err(t!(NameCharacters));
    }
    if typed.trim().is_empty() || typed.ends_with(['.', ' ']) || typed.starts_with(' ') {
        return Err(t!(NameBlank));
    }
    if is_device(typed) {
        return Err(t!(NameDevice));
    }
    if typed.encode_utf16().count() > MAX_NAME {
        return Err(t!(NameTooLong));
    }
    Ok(())
}

/// What Windows refuses in a name, and what could mislead whoever reads it.
fn forbidden(c: char) -> bool {
    c.is_control() || super::is_bidi_control(c) || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
}

/// A device name — with any extension (`nul.txt`) and in any case — opens
/// the device, not a file. Windows also takes `COM¹`–`COM³` for ports.
fn is_device(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    match base.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let port = base.strip_prefix("COM").or_else(|| base.strip_prefix("LPT"));
            port.is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
        }
    }
}

/// `name` cut to [`MAX_NAME`], keeping a short extension.
fn shorten(name: &str) -> String {
    if name.encode_utf16().count() <= MAX_NAME {
        return name.to_string();
    }
    let extension = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.encode_utf16().count() <= MAX_EXTENSION => &name[stem.len()..],
        _ => "",
    };
    let room = MAX_NAME - extension.encode_utf16().count();
    let mut stem = String::new();
    let mut used = 0;
    for c in name.chars() {
        if used + c.len_utf16() > room {
            break;
        }
        used += c.len_utf16();
        stem.push(c);
    }
    let stem = stem.trim_end_matches(['.', ' ']);
    format!("{}{extension}", if stem.is_empty() { FALLBACK } else { stem })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_cannot_leave_its_folder() {
        assert_eq!(file_name(r"..\..\outside.txt"), "outside.txt");
        assert_eq!(file_name("../../outside2.txt"), "outside2.txt");
        assert_eq!(file_name(r"C:\Windows\evil.txt"), "evil.txt");
        assert_eq!(file_name(r"\\server\share\x.txt"), "x.txt");
        assert_eq!(file_name(r"C:evil.txt"), "C_evil.txt", "a drive-relative name");
        for nothing in ["", "..", ".", r"a\", "/", " . . ", "\u{0}"] {
            let name = file_name(nothing);
            assert!(name == FALLBACK || name == "_", "{nothing:?} → {name:?}");
        }
    }

    #[test]
    fn devices_streams_and_forbidden_characters_are_defused() {
        assert_eq!(file_name("CON"), "_CON");
        assert_eq!(file_name("nul.txt"), "_nul.txt");
        assert_eq!(file_name("Com1.log"), "_Com1.log");
        assert_eq!(file_name("COM¹"), "_COM¹");
        assert_eq!(file_name("lpt9 .txt"), "_lpt9 .txt");
        assert_eq!(file_name("CONSOLE.txt"), "CONSOLE.txt", "only the exact names");
        assert_eq!(file_name("COM10"), "COM10");
        assert_eq!(file_name("a:b.txt"), "a_b.txt", "no alternate stream");
        assert_eq!(file_name("what?<>|*\".txt"), "what______.txt");
        assert_eq!(file_name("tab\there\r\n.txt"), "tab_here__.txt");
    }

    #[test]
    fn a_name_keeps_its_true_extension() {
        assert_eq!(file_name("trailing dot. "), "trailing dot");
        assert_eq!(file_name("report.pdf.exe..."), "report.pdf.exe");
        assert_eq!(file_name("invoice\u{202E}fdp.exe"), "invoice_fdp.exe", "the right-to-left trick shows");
        assert_eq!(file_name("  spaced.txt"), "spaced.txt");
        assert_eq!(file_name("zażółć.txt"), "zażółć.txt");
    }

    #[test]
    fn a_typed_name_is_refused_rather_than_fixed() {
        for good in ["small", "Umowa komplet.pdf", "wersja 2 (final)", "zażółć", "CONSOLE"] {
            assert_eq!(check_typed(good), Ok(()), "{good}");
        }
        for bad in [r"..\up", "a/b", "a:b", "what?", "tab\there", "end.", "end ", " start", "", "  ", "nul", "COM1.pdf", "x\u{202E}y"] {
            assert!(check_typed(bad).is_err(), "{bad:?}");
        }
        assert_eq!(check_typed(&"n".repeat(MAX_NAME)), Ok(()));
        assert_eq!(check_typed(&"n".repeat(MAX_NAME + 1)), Err("at most 100 characters"));
    }

    #[test]
    fn long_names_are_cut_with_the_extension_kept() {
        let long = format!("{}.txt", "n".repeat(296));
        let name = file_name(&long);
        assert_eq!(name.encode_utf16().count(), MAX_NAME);
        assert!(name.ends_with("nnn.txt"));
        let polish = "ż".repeat(300);
        assert_eq!(file_name(&polish).chars().count(), MAX_NAME);
        // Counted in UTF-16 units, like Windows: an emoji takes two.
        let emoji = format!("{}.pdf", "😀".repeat(100));
        let name = file_name(&emoji);
        assert!(name.encode_utf16().count() <= MAX_NAME && name.ends_with(".pdf"), "{name}");
        let no_extension = "x".repeat(150);
        assert_eq!(file_name(&no_extension).len(), MAX_NAME);
        let long_extension = format!("a.{}", "e".repeat(200));
        assert_eq!(file_name(&long_extension).encode_utf16().count(), MAX_NAME);
    }

    #[test]
    fn every_name_from_a_pdf_becomes_one_windows_takes() {
        // Found by this test: the cut could leave a device name, `CON` and
        // spaces + `.txt` → `CON.txt`.
        assert_eq!(file_name(&format!("CON{}y.txt", " ".repeat(120))), "_CON.txt");
        assert_eq!(file_name(&format!("nul.{}", "e".repeat(200))).encode_utf16().count(), MAX_NAME);
        const TOKENS: [&str; 18] = ["CON", "nul", "Com1", "LPT¹", "CONIN$", " ", ".", "..", "\\", "/", ":", "a", "ż", "😀", "\u{202E}", "\u{0}", "x.pdf", "abcdefghijklmnopqrstuvwxyz0123456789"];
        let mut rng = crate::fuzz::Rng::new(0x5EED_0003);
        for _ in 0..30_000 {
            let mut raw = String::new();
            for _ in 0..rng.below(16) {
                match rng.below(8) {
                    0 => raw.push_str(&" ".repeat(rng.below(120) as usize)),
                    1 => raw.push(rng.any_char()),
                    _ => raw.push_str(rng.pick(&TOKENS)),
                }
            }
            let name = file_name(&raw);
            assert_eq!(check_typed(&name), Ok(()), "{raw:?} → {name:?}");
            assert_eq!(file_name(&name), name, "a safe name stays as it is");
        }
    }
}
