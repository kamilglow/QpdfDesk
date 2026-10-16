//! Shared helpers of the integration tests: where the test copies of QPDF are,
//! and the generated test corpus.

#![allow(dead_code)] // each test binary uses a different part of this module

pub mod corpus;
/// The fixed-seed random inputs of the unit tests, shared.
#[path = "../../src/fuzz.rs"]
pub mod fuzz;

use std::path::{Path, PathBuf};

use qpdf_desk::core::filelist::Probe;
use qpdf_desk::core::ranges::PageRange;
use qpdf_desk::core::secret::Secret;
use qpdf_desk::platform::JobObject;

/// The project folder.
pub fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The unpacked official QPDF builds in `bin\` (outside git); empty when
/// none is there — then the tests that need QPDF skip themselves.
pub fn qpdf_builds() -> Vec<PathBuf> {
    ["qpdf-12.4.2-msvc64", "qpdf-12.4.2-mingw64"]
        .iter()
        .map(|build| project_dir().join("bin").join(build).join("bin").join("qpdf.exe"))
        .filter(|exe| exe.is_file())
        .collect()
}

/// The first available QPDF build, or `None` (and a note in the test output).
pub fn qpdf() -> Option<PathBuf> {
    let found = qpdf_builds().into_iter().next();
    if found.is_none() {
        eprintln!("skipped: no QPDF build in bin\\");
    }
    found
}

/// A fresh, empty scratch folder for one test, inside `target\`.
pub fn scratch(name: &str) -> PathBuf {
    let dir = scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch folder");
    dir
}

/// Where [`scratch`] puts the folder `name` (left as it is).
pub fn scratch_path(name: &str) -> PathBuf {
    project_dir().join("target").join("scratch").join(name)
}

/// A job for the tests: room for several QPDF at once, 4 GB together.
pub fn job_object() -> JobObject {
    JobObject::new(4 << 30, 8).expect("job object")
}

/// A copy of `item` in `dir`, named `name`; returns its path and size.
pub fn copy_item(item: corpus::Item, dir: &Path, name: &str) -> (PathBuf, u64) {
    let path = dir.join(name);
    std::fs::copy(corpus::get(item).expect("corpus item"), &path).expect("copy");
    let size = std::fs::metadata(&path).expect("size").len();
    (path, size)
}

/// Names of the files in `dir`, other than `keep`.
pub fn others(dir: &Path, keep: &[&Path]) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| !keep.contains(&p.as_path()))
        .map(|p| p.file_name().expect("name").to_string_lossy().into_owned())
        .collect()
}

/// `qpdf <args> <file>` run directly (not through the app's runner) →
/// (exit code, stdout, stderr): the tests' independent look at a file.
pub fn run_qpdf(exe: &Path, args: &[&str], file: &Path) -> (i32, String, String) {
    let out = std::process::Command::new(exe).args(args).arg(file).output().expect("run qpdf");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// What the probe would have said about a readable, unencrypted file of
/// `pages` pages — or nothing yet.
pub fn probe(pages: Option<u64>) -> Probe {
    pages.map_or(Probe::Pending, |pages| Probe::Ready { pages, restricted: false })
}

/// What the probe says of `Item::Base`.
pub const BASE: Probe = Probe::Ready { pages: 3, restricted: false };

pub fn secret(text: &str) -> Secret {
    Secret::new(text.to_string()).expect("a valid password")
}

pub fn range(text: &str) -> PageRange {
    PageRange::parse(text).expect("a valid range")
}

/// `qpdf <args> <input> <output>` run directly; its exit code.
pub fn qpdf_write(exe: &Path, args: &[&str], input: &Path, output: &Path) -> Option<i32> {
    std::process::Command::new(exe).args(args).arg(input).arg(output).status().expect("run qpdf").code()
}
