//! Where a result goes and how it gets there safely.
//!
//! QPDF writes into a temporary file in the destination folder; that file is
//! verified with `qpdf --check --show-npages` (one process: the page count is
//! the last stdout line — measured) and only then renamed to its final name,
//! never replacing an existing file. Any failure, a cancel included, deletes
//! the temporary file, so a half-written result never sits under the final
//! name. The original is never touched.
//!
//! The temporary file is visible, `~<name>.qpdfdesk-<pid>-<n>.tmp`:
//! after a crash an orphan is easy to recognise; the app
//! deletes only its own files from the current session.

use crate::t;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::outcome::{self, Failure, Verdict};
use super::runner::{self, Arg, Context};
use super::secret::Secret;
use crate::platform;

/// `<stem> (<tag>).pdf` next to `input`, or in `folder` when one is chosen.
/// `None` for a path without a file name.
pub fn output_name(input: &Path, tag: &str, folder: Option<&Path>) -> Option<PathBuf> {
    let stem = input.file_stem()?.to_string_lossy();
    let dir = folder.or_else(|| input.parent())?;
    Some(dir.join(format!("{stem} ({tag}).pdf")))
}

/// `path` with ` (n)` before the extension — at the end for a `folder` — for
/// the `n`-th attempt at a free name; the first attempt is `path` itself.
/// (A folder has no extension: `a.v2 (split)` must not become
/// `a (2).v2 (split)`.)
pub fn numbered(path: &Path, n: u32, folder: bool) -> PathBuf {
    if n <= 1 {
        return path.to_path_buf();
    }
    if folder {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(" ({n})"));
        return PathBuf::from(name);
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
        None => format!("{stem} ({n})"),
    };
    path.with_file_name(name)
}

/// Free space needed to write a result from inputs of `input_bytes`:
/// the input size plus 10 % plus 16 MB — the temporary file and the final one
/// are the same file, renamed, so one copy is enough.
pub fn required_space(input_bytes: u64) -> u64 {
    input_bytes.saturating_add(input_bytes / 10).saturating_add(16 << 20)
}

/// Whether the folder `dir` has room for `needed` bytes. When Windows cannot
/// say, the answer is yes: a wrong refusal is worse than QPDF's own error.
pub fn has_room(dir: &Path, needed: u64) -> bool {
    platform::free_disk_space(dir).is_none_or(|free| free >= needed)
}

/// A temporary file — or folder, for a split result — that deletes itself
/// unless committed.
pub struct TempFile {
    path: Option<PathBuf>,
    folder: bool,
}

/// Longest part of the input's name kept in the temporary name, so the
/// temporary path stays within QPDF's limit when the final one does.
const TEMP_STEM_CHARS: usize = 40;

impl TempFile {
    /// Reserve a new, empty `~<stem>.qpdfdesk-<pid>-<n>.tmp` file in `dir` —
    /// or folder, for the parts of a split result.
    pub fn create(dir: &Path, stem: &str, folder: bool) -> io::Result<TempFile> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let stem: String = stem.chars().take(TEMP_STEM_CHARS).collect();
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("~{stem}.qpdfdesk-{}-{n}.tmp", std::process::id()));
            // `create_new` and `create_dir`: never take over what is there.
            let made = if folder {
                std::fs::create_dir(&path)
            } else {
                std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(drop)
            };
            match made {
                Ok(()) => return Ok(TempFile { path: Some(path), folder }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().unwrap_or(Path::new(""))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else { return };
        // An antivirus often holds a freshly written file for a moment, so a
        // failed delete is retried briefly. (A folder is only ever our own,
        // made by `create`, holding what QPDF wrote into it.)
        for attempt in 0..4u64 {
            let removed = if self.folder { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            match removed {
                Err(e) if e.kind() != io::ErrorKind::NotFound && attempt < 3 => {
                    std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
                }
                _ => return,
            }
        }
    }
}

/// Give `temp` its final name: `desired`, or `desired (2)`, `(3)`… when taken
/// — also when another file appears in the meantime.
pub fn commit(mut temp: TempFile, desired: &Path) -> io::Result<PathBuf> {
    let target = first_free(desired, temp.folder, |target| platform::move_no_replace(temp.path(), target))?;
    temp.path = None;
    Ok(target)
}

/// The first of `desired`, `desired (2)`, `(3)`… that `take` gets — it fails
/// with `AlreadyExists` for a name that is taken, and never takes one over.
pub fn first_free(desired: &Path, folder: bool, mut take: impl FnMut(&Path) -> io::Result<()>) -> io::Result<PathBuf> {
    for n in 1..=9999 {
        let target = numbered(desired, n, folder);
        match take(&target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, t!(FailureNoFreeName)))
}

/// Room for the numbers QPDF adds to the name of a part: `-0001-0050` for
/// pages 1–50 of a file of thousands (measured: zero-padded to the digits of
/// the page count).
const PART_NUMBERS_CHARS: usize = 14;

/// Most lines of QPDF messages kept for one result — as many as the runner
/// keeps of one run (180 first + 20 last).
const MAX_DETAILS: usize = 200;

/// One result to produce.
pub struct Target<'a> {
    /// The full path of `qpdf.exe`.
    pub qpdf: &'a Path,
    /// QPDF's arguments without the output file — it is added last.
    pub args: Vec<Arg>,
    /// Where the result should land; ` (2)`… is added when the name is taken.
    pub desired: PathBuf,
    /// A split result (`--split-pages`): `desired` is a folder, and QPDF names
    /// the parts in it `<this>-1-5.pdf`… (measured).
    pub split_parts: Option<String>,
    /// Total size of the inputs, for the disk space check.
    pub input_bytes: u64,
    /// Open password of the result, when it is encrypted with one — needed
    /// to verify it.
    pub result_password: Option<Secret>,
    /// The page count the result must have, when known — all parts
    /// together for a split one.
    pub expected_pages: Option<u64>,
    /// "Keep original if not smaller": the original's size; a
    /// result at least this big is discarded unverified.
    pub keep_if_not_smaller: Option<u64>,
}

/// What came of a [`Target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Produced {
    pub verdict: Verdict,
    /// The final file (a folder when split) — only when the verdict is a
    /// success.
    pub output: Option<PathBuf>,
    /// QPDF's warnings on success, its messages on failure — shared: a
    /// merge gives the one result to each of up to 10 000 inputs.
    pub details: Arc<[String]>,
}

impl Produced {
    /// A verdict without a result file.
    pub fn ended(verdict: Verdict) -> Produced {
        Produced::ended_with(verdict, Vec::new())
    }

    fn ended_with(verdict: Verdict, details: Vec<String>) -> Produced {
        Produced { verdict, output: None, details: details.into() }
    }

    pub fn failed(failure: Failure) -> Produced {
        Produced::ended(Verdict::Failed(failure))
    }

    fn failed_with(failure: Failure, details: Vec<String>) -> Produced {
        Produced::ended_with(Verdict::Failed(failure), details)
    }
}

/// Run QPDF into a temporary file (or folder), verify what it wrote, and
/// give it its final name.
pub fn produce(target: Target, ctx: &Context) -> Produced {
    let Target { qpdf, mut args, desired, split_parts, input_bytes, result_password, expected_pages, keep_if_not_smaller } =
        target;
    // For a split result, the longest path a part may get.
    let longest = match &split_parts {
        Some(name) => desired.join(format!("{name}{}.pdf", "0".repeat(PART_NUMBERS_CHARS))),
        None => desired.clone(),
    };
    if let Err(e) = runner::check_path(&longest) {
        return Produced::failed(outcome::from_arg_error(&e));
    }
    let (Some(dir), Some(stem)) = (desired.parent(), desired.file_stem()) else {
        return Produced::failed(Failure::Other(t!(FailureNoOutputName).into()));
    };
    if !has_room(dir, required_space(input_bytes)) {
        return Produced::failed(Failure::NoDiskSpace);
    }
    let stem = stem.to_string_lossy();
    let temp = match TempFile::create(dir, &stem, split_parts.is_some()) {
        Ok(temp) => temp,
        Err(e) => return Produced::failed(outcome::from_create_error(&e)),
    };
    args.push(Arg::path(match &split_parts {
        Some(name) => temp.path().join(format!("{name}.pdf")),
        None => temp.path().to_path_buf(),
    }));
    let done = match runner::run(qpdf, &args, ctx) {
        Ok(done) => done,
        Err(e) => return Produced::failed(outcome::from_run_error(&e)),
    };
    let mut verdict = outcome::judge(&done);
    if !verdict.succeeded() {
        return Produced::ended_with(verdict, done.stderr.lines);
    }
    let mut details: Vec<String> = outcome::warnings(&done.stderr).map(String::from).collect();
    // Not smaller: nothing to keep, so nothing to verify either.
    if let Some(original) = keep_if_not_smaller {
        let written = std::fs::metadata(temp.path()).map(|m| m.len()).unwrap_or(u64::MAX);
        if written >= original {
            return Produced::ended_with(Verdict::NotSmaller, details);
        }
    }
    // Verification: every file written must be a sound PDF, with the
    // expected number of pages in all.
    let files = match &split_parts {
        Some(_) => match parts(temp.path()) {
            Ok(files) => files,
            Err(e) => return Produced::failed(outcome::from_io_error(&e)),
        },
        None => vec![temp.path().to_path_buf()],
    };
    let mut pages = (!files.is_empty()).then_some(0u64);
    for file in &files {
        let checked = match verify(qpdf, file, result_password.as_ref(), ctx) {
            Ok(checked) => checked,
            Err(failed) => return failed,
        };
        if checked.warnings {
            verdict = Verdict::DoneWithWarnings;
        }
        // A split into hundreds of parts must not gather hundreds of reports.
        details.extend(checked.details.into_iter().take(MAX_DETAILS.saturating_sub(details.len())));
        pages = pages.zip(checked.pages).map(|(sum, more)| sum.saturating_add(more));
    }
    if pages.is_none() || expected_pages.is_some_and(|expected| Some(expected) != pages) {
        let count = |pages: Option<u64>| pages.map_or(t!(VerificationUnknown).to_string(), |n| n.to_string());
        details.push(t!(VerificationPages, expected = count(expected_pages), found = count(pages)));
        return Produced::failed_with(Failure::VerificationFailed, details);
    }
    match commit(temp, &desired) {
        Ok(path) => Produced { verdict, output: Some(path), details: details.into() },
        Err(e) => Produced::failed(outcome::from_io_error(&e)),
    }
}

/// The parts QPDF wrote into the folder `dir`, in name order.
fn parts(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = std::fs::read_dir(dir)?.map(|entry| entry.map(|e| e.path())).collect::<io::Result<Vec<_>>>()?;
    files.sort();
    Ok(files)
}

/// What the verification said about one written file.
struct Checked {
    /// QPDF found something to warn about (exit code 3).
    warnings: bool,
    pages: Option<u64>,
    details: Vec<String>,
}

/// `qpdf --check --show-npages` on a written file (one process: the page
/// count is the last stdout line — measured); `Err` is the result to report.
fn verify(qpdf: &Path, file: &Path, password: Option<&Secret>, ctx: &Context) -> Result<Checked, Produced> {
    let check = Arg::on_file(password, &["--check", "--show-npages"], file);
    let checked = runner::run(qpdf, &check, ctx).map_err(|e| Produced::failed(outcome::from_run_error(&e)))?;
    let pages = checked.stdout.last_line().and_then(|line| line.trim().parse::<u64>().ok());
    match outcome::judge(&checked) {
        verdict @ (Verdict::Done | Verdict::DoneWithWarnings) => Ok(Checked {
            warnings: verdict == Verdict::DoneWithWarnings,
            pages,
            details: outcome::warnings(&checked.stderr).map(String::from).collect(),
        }),
        Verdict::Cancelled => Err(Produced::ended(Verdict::Cancelled)),
        Verdict::Failed(failure @ (Failure::MemoryLimit | Failure::Hung)) => {
            Err(Produced::failed_with(failure, checked.stderr.lines))
        }
        Verdict::Failed(_) | Verdict::PasswordRequired | Verdict::NotSmaller => {
            Err(Produced::failed_with(Failure::VerificationFailed, checked.stderr.lines))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_named_after_the_input_and_the_step() {
        let input = Path::new(r"C:\Docs\umowa.v2.pdf");
        assert_eq!(output_name(input, "optimized", None), Some(PathBuf::from(r"C:\Docs\umowa.v2 (optimized).pdf")));
        assert_eq!(
            output_name(input, "processed", Some(Path::new(r"D:\Out"))),
            Some(PathBuf::from(r"D:\Out\umowa.v2 (processed).pdf"))
        );
        assert_eq!(output_name(Path::new(r"C:\"), "x", None), None);
    }

    #[test]
    fn taken_names_get_a_number() {
        let path = Path::new(r"C:\Docs\a (merged).pdf");
        assert_eq!(numbered(path, 1, false), path);
        assert_eq!(numbered(path, 2, false), Path::new(r"C:\Docs\a (merged) (2).pdf"));
        assert_eq!(numbered(Path::new(r"C:\Docs\noext"), 3, false), Path::new(r"C:\Docs\noext (3)"));
        // A split result's folder: its dots are not an extension (once the
        // second split of `a.v2.pdf` became `a (2).v2 (split)`).
        assert_eq!(numbered(Path::new(r"C:\Docs\a.v2 (split)"), 2, true), Path::new(r"C:\Docs\a.v2 (split) (2)"));
    }

    #[test]
    fn space_needed_has_a_margin_and_never_overflows() {
        assert_eq!(required_space(0), 16 << 20);
        assert_eq!(required_space(100 << 20), (110 << 20) + (16 << 20));
        assert_eq!(required_space(u64::MAX), u64::MAX);
        assert!(has_room(&std::env::temp_dir(), 1));
        assert!(!has_room(&std::env::temp_dir(), u64::MAX));
        assert!(has_room(Path::new(r"Q:\no\such\drive"), u64::MAX), "unknown means yes");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qd_output_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn a_temp_file_is_reserved_and_deleted_on_drop() {
        let dir = scratch("temp");
        let long_stem = "x".repeat(200);
        let temp = TempFile::create(&dir, &long_stem, false).expect("temp");
        let path = temp.path().to_path_buf();
        assert!(path.is_file());
        let name = path.file_name().expect("name").to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("~{}.qpdfdesk-{}-", "x".repeat(TEMP_STEM_CHARS), std::process::id())), "{name}");
        assert!(name.ends_with(".tmp"));
        let other = TempFile::create(&dir, &long_stem, false).expect("second");
        assert_ne!(other.path(), path, "never the same file twice");
        drop(temp);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_takes_the_next_free_name_and_never_overwrites() {
        let dir = scratch("commit");
        let desired = dir.join("a (optimized).pdf");
        std::fs::write(&desired, b"older result").expect("write");
        std::fs::write(numbered(&desired, 2, false), b"another").expect("write");
        let temp = TempFile::create(&dir, "a", false).expect("temp");
        std::fs::write(temp.path(), b"new").expect("write");
        let temp_path = temp.path().to_path_buf();
        let final_path = commit(temp, &desired).expect("commit");
        assert_eq!(final_path, numbered(&desired, 3, false));
        assert_eq!(std::fs::read(&final_path).expect("read"), b"new");
        assert_eq!(std::fs::read(&desired).expect("read"), b"older result");
        assert!(!temp_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_commit_still_cleans_up() {
        let dir = scratch("commit_fail");
        let temp = TempFile::create(&dir, "a", false).expect("temp");
        let temp_path = temp.path().to_path_buf();
        let impossible = dir.join("missing folder").join("a.pdf");
        assert!(commit(temp, &impossible).is_err());
        assert!(!temp_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
