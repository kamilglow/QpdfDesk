//! Finding `qpdf.exe` and checking its version.
//!
//! Only full paths, and never the current folder: the places below are listed
//! by us, in a fixed order, and `PATH` is searched by us too — `CreateProcess`
//! would try the app's own folder and the current one first.
//! The version comes from `qpdf --version`, not from a folder name.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::runner::{self, Arg, Context, Ending};
use super::scandir::natural_path_cmp;
use crate::platform::JobObject;

/// A QPDF version `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

/// The oldest QPDF the app works with: the
/// security limits and every option of 1.0 are there from 12.4 on.
pub const MIN_VERSION: Version = Version(12, 4, 0);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The version from `qpdf --version`, whose first line reads
/// `qpdf version 12.4.2` (measured).
pub fn parse_version(stdout: &str) -> Option<Version> {
    let rest = stdout.lines().next()?.trim().strip_prefix("qpdf version ")?;
    // A suffix such as `12.5.0-rc1` is allowed; the numbers are what counts.
    let numbers = rest.split(|c: char| !c.is_ascii_digit() && c != '.').next()?;
    let mut parts = numbers.split('.').map(str::parse::<u32>);
    let version = Version(parts.next()?.ok()?, parts.next()?.ok()?, parts.next().unwrap_or(Ok(0)).ok()?);
    Some(version)
}

/// A usable QPDF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qpdf {
    pub exe: PathBuf,
    pub version: Version,
}

/// Why there is no usable QPDF.
#[derive(Debug, PartialEq, Eq)]
pub enum Missing {
    /// No `qpdf.exe` in any of the searched places.
    NotFound,
    /// Found, but older than [`MIN_VERSION`] (the first such one).
    TooOld(Qpdf),
    /// Found, but it did not run or did not say its version (e.g. its DLLs
    /// are missing).
    Unusable(PathBuf),
}

/// Where to look, in order: next to the app — `qpdf\bin`, an unpacked
/// official ZIP `qpdf-*\bin` (newest first), `bin` — then an installed QPDF
/// in `program_files\qpdf*\bin` (newest first), then the absolute folders of
/// `path_var`. Only files that exist are returned, each once.
pub fn candidates(app_dir: &Path, program_files: Option<&Path>, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut add = |exe: PathBuf| {
        if exe.is_file() && !found.iter().any(|f| f.as_os_str().eq_ignore_ascii_case(exe.as_os_str())) {
            found.push(exe);
        }
    };
    add(app_dir.join(r"qpdf\bin\qpdf.exe"));
    for dir in matching_dirs(app_dir, "qpdf-") {
        add(dir.join(r"bin\qpdf.exe"));
    }
    add(app_dir.join(r"bin\qpdf.exe"));
    if let Some(program_files) = program_files {
        for dir in matching_dirs(program_files, "qpdf") {
            add(dir.join(r"bin\qpdf.exe"));
        }
    }
    if let Some(path_var) = path_var {
        // A relative entry (`.`, `bin`) would mean "the current folder".
        for dir in std::env::split_paths(path_var).filter(|d| d.is_absolute()) {
            add(dir.join("qpdf.exe"));
        }
    }
    // A development build runs from `target\debug`, while the test copies of
    // QPDF live in the project's `bin\`. Not in release builds.
    #[cfg(debug_assertions)]
    for dir in matching_dirs(&Path::new(env!("CARGO_MANIFEST_DIR")).join("bin"), "qpdf-") {
        add(dir.join(r"bin\qpdf.exe"));
    }
    found
}

/// Subfolders of `parent` whose name starts with `prefix` (any letter case),
/// newest version first: `qpdf-12.10.0` before `qpdf-12.4.2`.
fn matching_dirs(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            })
        })
        .map(|e| e.path())
        .collect();
    dirs.sort_by(|a, b| natural_path_cmp(b, a));
    dirs
}

/// The standard search: [`candidates`] for this executable's folder,
/// `%ProgramFiles%` and `%PATH%`.
pub fn default_candidates() -> Vec<PathBuf> {
    let app_dir = crate::settings::data_dir().unwrap_or_default();
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    candidates(&app_dir, program_files.as_deref(), std::env::var_os("PATH").as_deref())
}

/// The first candidate that runs and is at least [`MIN_VERSION`].
pub fn locate(candidates: &[PathBuf], job: &JobObject) -> Result<Qpdf, Missing> {
    let mut missing = Missing::NotFound;
    for exe in candidates {
        match version_of(exe, job) {
            Some(version) if version >= MIN_VERSION => return Ok(Qpdf { exe: exe.clone(), version }),
            Some(version) => {
                if !matches!(missing, Missing::TooOld(_)) {
                    missing = Missing::TooOld(Qpdf { exe: exe.clone(), version });
                }
            }
            None => {
                if missing == Missing::NotFound {
                    missing = Missing::Unusable(exe.clone());
                }
            }
        }
    }
    Err(missing)
}

/// Run `exe --version`; `None` if it does not answer like QPDF.
fn version_of(exe: &Path, job: &JobObject) -> Option<Version> {
    let cancel = AtomicBool::new(false);
    let done = runner::run(exe, &[Arg::plain("--version")], &Context::new(job, &cancel)).ok()?;
    if done.ending != Ending::Exited(0) {
        return None;
    }
    parse_version(done.stdout.lines.first()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_is_parsed() {
        let out = "qpdf version 12.4.2\nRun qpdf --copyright to see copyright and license information.\n";
        assert_eq!(parse_version(out), Some(Version(12, 4, 2)));
        assert_eq!(parse_version("qpdf version 12.10.0-rc1"), Some(Version(12, 10, 0)));
        assert_eq!(parse_version("qpdf version 13.0"), Some(Version(13, 0, 0)));
        for junk in ["", "qpdf version", "qpdf version x.y", "version 12.4.2", "QPDF 12.4.2", "qpdf version 99999999999.1.1"] {
            assert_eq!(parse_version(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(Version(12, 4, 0) >= MIN_VERSION);
        assert!(Version(12, 10, 0) > Version(12, 4, 2));
        assert!(Version(13, 0, 0) > Version(12, 99, 99));
        assert!(Version(12, 3, 9) < MIN_VERSION);
    }

    #[test]
    fn random_version_output_never_panics() {
        let alphabet = crate::fuzz::chars("qpdf version 0123456789.-rc\n");
        let mut rng = crate::fuzz::Rng::new(0x5EED_0008);
        for _ in 0..20_000 {
            let text = format!("{}{}", if rng.chance(2) { "qpdf version " } else { "" }, rng.text(&alphabet, 30));
            if let Some(version) = parse_version(&text) {
                assert!(text.starts_with("qpdf version "), "{text:?} gave {version}");
            }
        }
        assert!(Version(11, 9, 1) < MIN_VERSION);
        assert_eq!(Version(12, 4, 2).to_string(), "12.4.2");
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, b"").expect("file");
    }

    #[test]
    fn candidates_come_in_the_documented_order() {
        let root = std::env::temp_dir().join(format!("qd_locate_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (app, pf, on_path) = (root.join("app"), root.join("pf"), root.join("tools"));
        for exe in [
            r"app\bin\qpdf.exe",
            r"app\qpdf-12.4.2-msvc64\bin\qpdf.exe",
            r"app\qpdf-12.10.0-msvc64\bin\qpdf.exe",
            r"app\qpdf\bin\qpdf.exe",
            r"app\other\bin\qpdf.exe",
            r"pf\qpdf 12.4.2\bin\qpdf.exe",
            r"pf\QPDF\bin\qpdf.exe",
            r"tools\qpdf.exe",
        ] {
            touch(&root.join(exe));
        }
        let path_var = std::env::join_paths([on_path.clone(), PathBuf::from("."), on_path.clone()]).expect("PATH");
        let found = candidates(&app, Some(&pf), Some(&path_var));
        let expected: Vec<PathBuf> = [
            r"app\qpdf\bin\qpdf.exe",
            r"app\qpdf-12.10.0-msvc64\bin\qpdf.exe",
            r"app\qpdf-12.4.2-msvc64\bin\qpdf.exe",
            r"app\bin\qpdf.exe",
            r"pf\qpdf 12.4.2\bin\qpdf.exe",
            r"pf\QPDF\bin\qpdf.exe",
            r"tools\qpdf.exe",
        ]
        .iter()
        .map(|p| root.join(p))
        .collect();
        // Release builds stop here; debug ones add the project's `bin\`.
        assert_eq!(found[..expected.len()], expected[..]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_found_in_empty_places() {
        let empty = std::env::temp_dir().join("qd_locate_nothing_here");
        let found = candidates(&empty, Some(&empty), None);
        assert!(found.iter().all(|p| p.starts_with(env!("CARGO_MANIFEST_DIR"))));
    }
}
