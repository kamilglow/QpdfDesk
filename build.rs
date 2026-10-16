//! Stamps the Windows resources into the executable: the application icon and
//! the VERSIONINFO block behind Explorer's Properties → Details tab.
//!
//! Both are generated here rather than committed as files, so each fact keeps a
//! single source of truth: the version and author in `Cargo.toml`, the
//! copyright notice in `LICENSE`, the drawing in `src/icon.rs`. Change any of
//! them and the executable follows on the next build.
//!
//! Non-Windows targets are skipped entirely; there is nothing to attach to.

// The icon is pulled in as a real module (not `include!`) so it keeps its own
// module documentation. It is plain `std`, with nothing to link against.
#[path = "src/icon.rs"]
mod icon;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Product and file name. Cargo exposes no bin-target name to build scripts,
/// and the package name is `qpdf-desk`, so it has to be spelled out.
const PRODUCT: &str = "QpdfDesk";

/// Shown by Task Manager and the UAC prompt, so it stays short — the full
/// sentence from `Cargo.toml` would be truncated in both.
const FILE_DESCRIPTION: &str = "PDF toolbox (GUI for QPDF)";

fn main() {
    // Cargo honours ONLY the paths listed here. The manifest carries version and
    // authors, LICENSE the copyright line; leave either out and an edit to it
    // would keep the previous VERSIONINFO baked into the executable.
    for path in ["build.rs", "src/icon.rs", "Cargo.toml", "LICENSE", "CHANGELOG.md", "lang"] {
        println!("cargo:rerun-if-changed={path}");
    }
    // The About window's release date, author and copyright, from their one
    // source.
    println!("cargo:rustc-env=QD_RELEASE_DATE={}", release_date());
    println!("cargo:rustc-env=QD_AUTHOR={}", author());
    println!("cargo:rustc-env=QD_COPYRIGHT={}", license_notice());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"));
    // Every text the program shows, from the catalogs in `lang/`.
    fs::write(out_dir.join("messages.rs"), messages_source()).expect("failed to write the message list");

    // `CARGO_CFG_TARGET_OS` describes the target, unlike `cfg!` which would
    // describe the machine running this script.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // DLL planting: Windows looks for an executable's DLLs next to it first, and
    // a portable app typically runs from Downloads, where anything can land.
    // Measured: a stray `dwmapi.dll` beside the EXE was loaded instead of the
    // system one. 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32 — resolve our imports from
    // System32 only (Windows 10 1607+ honours it; older ones ignore the flag).
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        // The C runtime: `vcruntime140.dll` is not part of Windows (a clean
        // install has none), so `.cargo/config.toml` links it in statically
        // (`crt-static`). The prebuilt FLTK of `fltk-bundled` calls the
        // Universal CRT through its DLL, which Windows 10 and 11 carry, so
        // that part stays dynamic ("hybrid CRT"; measured: +21 KB, and the
        // EXE imports only Windows' own DLLs). Without `crt-static` — a
        // build with its own RUSTFLAGS — the defaults apply.
        let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
        if features.split(',').any(|feature| feature == "crt-static") {
            println!("cargo:rustc-link-arg=/NODEFAULTLIB:libucrt.lib");
            println!("cargo:rustc-link-arg=/DEFAULTLIB:ucrt.lib");
        }
    }

    let ico_path = out_dir.join(format!("{PRODUCT}.ico"));
    let rc_path = out_dir.join(format!("{PRODUCT}.rc"));

    fs::write(&ico_path, icon::ico_bytes()).expect("failed to write the generated icon");
    fs::write(&rc_path, rc_source(&ico_path)).expect("failed to write the resource script");

    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("failed to compile the Windows resources");
}

/// The first entry of `authors`, without the `<email>` part — the address
/// belongs in the manifest, not in a field Explorer shows to everyone.
fn author() -> &'static str {
    env!("CARGO_PKG_AUTHORS").split([':', '<']).next().unwrap_or("").trim()
}

/// Build the resource script: one icon plus the version block.
///
/// `#pragma code_page(65001)` lets the file be UTF-8, which matters as soon as
/// the author's name carries diacritics.
fn rc_source(ico_path: &Path) -> String {
    let version = env!("CARGO_PKG_VERSION");
    // VERSIONINFO wants four numeric fields; the crate only has three.
    let numeric = concat!(
        env!("CARGO_PKG_VERSION_MAJOR"),
        ",",
        env!("CARGO_PKG_VERSION_MINOR"),
        ",",
        env!("CARGO_PKG_VERSION_PATCH"),
        ",0"
    );
    let author = author();
    // Trimming a final period first keeps a notice that already ends with one
    // (e.g. initials) from rendering as "G.. MIT License."
    let copyright = format!("{}. MIT License.", license_notice().trim_end_matches('.'));

    // Language block 040904B0 = US English, Unicode; the VarFileInfo entry
    // below must repeat the same pair (0x409, 1200) or Explorer ignores it.
    format!(
        r#"#pragma code_page(65001)
#include <winver.h>

1 ICON "{ico}"

1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK VS_FFI_FILEFLAGSMASK
FILEFLAGS 0x0L
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
FILESUBTYPE VFT2_UNKNOWN
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "{author}"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "{product}"
            VALUE "LegalCopyright", "{copyright}"
            VALUE "OriginalFilename", "{product}.exe"
            VALUE "ProductName", "{product}"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        ico = escape(&ico_path.display().to_string()),
        description = escape(FILE_DESCRIPTION),
        author = escape(author),
        copyright = escape(&copyright),
        version = escape(version),
        product = PRODUCT,
    )
}

/// The date of this version's entry in `CHANGELOG.md` (`## [1.0.0] - 2026-10-16`),
/// or empty for a version without one (About then says so in its language).
fn release_date() -> String {
    let heading = format!("## [{}] - ", env!("CARGO_PKG_VERSION"));
    fs::read_to_string("CHANGELOG.md")
        .ok()
        .and_then(|log| log.lines().find_map(|line| line.strip_prefix(&heading).map(|date| date.trim().to_string())))
        .unwrap_or_default()
}

/// The `Copyright (c) …` line of `LICENSE`, verbatim. Reading it — rather than
/// rebuilding it from constants — means the year, name and URL exist once, and
/// the notice cannot drift with the build clock either.
fn license_notice() -> String {
    let license = fs::read_to_string("LICENSE").expect("LICENSE must sit next to Cargo.toml");
    license
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("Copyright"))
        .expect("LICENSE must contain a Copyright line")
        .to_string()
}

/// Escape a value for an RC string literal: backslashes double (paths!) and a
/// quote is written twice.
fn escape(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', "\"\"")
}

/// The plural forms a message may have, in slot order: the Polish ones —
/// `one` (1), `few` (2–4, 22–24…) and `many` (the rest). English has no
/// `few`: it takes `many`.
const PLURAL_FORMS: [&str; 3] = ["one", "few", "many"];

/// The `key = text` lines of `lang/<code>.txt` with their line numbers, `\n`
/// made a line break.
fn read_catalog(code: &str) -> Vec<(usize, String, String)> {
    let path = format!("lang/{code}.txt");
    let catalog = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut lines = Vec::new();
    for (index, line) in catalog.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, text) = line.split_once('=').unwrap_or_else(|| panic!("{path}:{}: no `=`", index + 1));
        lines.push((index + 1, key.trim().to_string(), text.trim().replace("\\n", "\n")));
    }
    lines
}

/// The `{name}`s of a text, sorted.
fn placeholders(text: &str) -> Vec<&str> {
    let mut names: Vec<&str> = text.split('{').skip(1).filter_map(|part| part.split_once('}').map(|(name, _)| name)).collect();
    names.sort_unstable();
    names
}

/// Every catalog in `lang/` as Rust (`messages.rs`). English, the reference,
/// gives one `Msg` variant per message — a key the program uses but English
/// lacks does not compile. Each language becomes its texts in one string and
/// where each ends, a key it leaves out taking the English text: no parsing
/// and no table to build at start. A catalog that would show wrong does not
/// build either: a key the program does not have, a key twice, an `@`,
/// another `{name}` than English, a character a drop-down cannot show.
fn messages_source() -> String {
    let english = read_catalog("en");
    // (message key, plural) in catalog order; a plural one takes three slots.
    let mut messages: Vec<(String, bool)> = Vec::new();
    for (number, key, _) in &english {
        assert!(
            !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_'),
            "lang/en.txt:{number}: a key is lowercase letters, digits, `.` and `_`"
        );
        let (base, plural) = match key.rsplit_once('.') {
            Some((base, form)) if PLURAL_FORMS.contains(&form) => (base, true),
            _ => (key.as_str(), false),
        };
        if !messages.iter().any(|(known, was_plural)| known == base && plural && *was_plural) {
            messages.push((base.to_string(), plural));
        }
    }
    let mut slot_keys: Vec<String> = Vec::new();
    let mut first_slot = Vec::new();
    for (key, plural) in &messages {
        first_slot.push(slot_keys.len());
        match plural {
            true => slot_keys.extend(PLURAL_FORMS.map(|form| format!("{key}.{form}"))),
            false => slot_keys.push(key.clone()),
        }
    }
    first_slot.push(slot_keys.len());

    // Every language in `lang/`, English first: the others fall back to it.
    let mut codes: Vec<String> = fs::read_dir("lang")
        .expect("lang/ must sit next to Cargo.toml")
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".txt").map(str::to_string))
        .filter(|code| code != "en")
        .collect();
    codes.sort();
    codes.insert(0, "en".to_string());
    let mut reference: Vec<String> = Vec::new();
    let mut catalogs = String::new();
    for code in &codes {
        let path = format!("lang/{code}.txt");
        let mut slots: Vec<Option<String>> = vec![None; slot_keys.len()];
        for (number, key, text) in if code == "en" { english.clone() } else { read_catalog(code) } {
            let slot = slot_keys.iter().position(|k| *k == key).unwrap_or_else(|| panic!("{path}:{number}: `{key}` is no key of the program"));
            assert!(slots[slot].is_none(), "{path}:{number}: `{key}` twice");
            // `@` starts a symbol in an FLTK label.
            assert!(!text.contains('@'), "{path}:{number}: `{key}` has an `@`");
            // `&` marks a button's shortcut — only a step name doubles it.
            assert!(!text.contains('&') || key.starts_with("step."), "{path}:{number}: `{key}` has an `&`");
            // A drop-down or menu item: `|` separates items, `/` makes a
            // submenu, `_` a divider.
            assert!(!key.starts_with("item.") || !text.contains(['|', '/', '_']), "{path}:{number}: `{key}` has | / or _");
            slots[slot] = Some(text);
        }
        // A plural message without its `few` form takes `many`.
        for (first, (_, plural)) in first_slot.iter().zip(&messages) {
            if *plural && slots[first + 1].is_none() {
                slots[first + 1] = slots[first + 2].clone();
            }
        }
        let texts: Vec<String> = if code == "en" {
            let texts: Vec<String> = slots.into_iter().zip(&slot_keys).map(|(text, key)| text.unwrap_or_else(|| panic!("lang/en.txt has no `{key}`"))).collect();
            reference = texts.clone();
            texts
        } else {
            for (slot, text) in slots.iter().enumerate() {
                if let Some(text) = text {
                    assert_eq!(placeholders(text), placeholders(&reference[slot]), "{path}: `{}` fills in other values than English", slot_keys[slot]);
                }
            }
            slots.into_iter().zip(&reference).map(|(text, english)| text.unwrap_or_else(|| english.clone())).collect()
        };
        let mut ends = vec![0];
        let mut all = String::new();
        for text in &texts {
            all += text;
            ends.push(u16::try_from(all.len()).unwrap_or_else(|_| panic!("{path}: over 64 KB of text")));
        }
        catalogs += &format!("static CATALOG_{}: Catalog = Catalog {{ text: {all:?}, ends: &{ends:?} }};\n", code.to_uppercase());
    }

    let variant = |key: &str| -> String { key.split(['.', '_']).map(|part| part[..1].to_ascii_uppercase() + &part[1..]).collect() };
    let mut names: Vec<String> = messages.iter().map(|(key, _)| variant(key)).collect();
    let mut out = String::from("// Generated by build.rs from lang/*.txt — do not edit.\n\n");
    out += "/// A message of the catalog: its text comes from `text::get`.\n";
    out += "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum Msg {\n";
    for name in &names {
        out += &format!("    {name},\n");
    }
    out += "}\n\n";
    out += &format!("/// Each message's first slot, then the end: a plural one has three — one, few, many.\nconst FIRST_SLOT: [u16; {}] = {first_slot:?};\n", first_slot.len());
    out += &format!("/// Each message's variant name, for the tests.\n#[cfg(test)]\nconst NAMES: [&str; {}] = {names:?};\n", names.len());
    out += &catalogs;
    names.sort();
    names.dedup();
    assert_eq!(names.len(), messages.len(), "lang/en.txt: two keys make the same name");
    out
}
