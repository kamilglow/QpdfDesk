//! `core::output::produce` with the real QPDF: the result is written to a
//! temporary file, verified and renamed without overwriting; every failure and
//! a cancel leave nothing behind; the original is never touched.

mod common;

use common::corpus::{self, Item, USER_PASSWORD};
use common::{job_object as job, others, secret};
use qpdf_desk::core::job::{Input, Job, Optimize};
use qpdf_desk::core::outcome::{Failure, Verdict};
use qpdf_desk::core::output::{output_name, produce, Produced, Target};
use qpdf_desk::core::runner::{Arg, Context};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A copy of `item` in a fresh scratch folder `name`.
fn input(name: &str, item: Item) -> PathBuf {
    let file_name = corpus::item(item).file_name().expect("name").to_string_lossy().into_owned();
    common::copy_item(item, &common::scratch(name), &file_name).0
}

/// A result of `args` to be written as `desired`, nothing else known about it.
fn target(exe: &Path, args: Vec<Arg>, desired: PathBuf) -> Target<'_> {
    Target {
        qpdf: exe,
        args,
        desired,
        split_parts: None,
        input_bytes: 0,
        result_password: None,
        expected_pages: None,
        keep_if_not_smaller: None,
    }
}

/// Run the Optimize step on `input` (every result kept, even a bigger one),
/// as if the probe had found `pages` pages.
fn rewrite(exe: &Path, input: &Path, ctx: &Context, pages: Option<u64>) -> Produced {
    let size = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    let job = Job { optimize: Some(Optimize { keep_if_not_smaller: false, ..Optimize::default() }), ..Job::default() };
    let probe = common::probe(pages);
    job.produce(exe, &[Input { path: input, size, probe: &probe, range: None, attachments: None }], ctx).remove(0)
}

#[test]
fn a_result_is_verified_and_lands_next_to_the_original() {
    for exe in common::qpdf_builds() {
        let (job, cancel) = (job(), AtomicBool::new(false));
        let ctx = Context::new(&job, &cancel);
        let original = input("result", Item::Manual);
        let before = std::fs::read(&original).expect("read");
        let done = rewrite(&exe, &original, &ctx, Some(178));
        assert_eq!(done.verdict, Verdict::Done, "{:?}", done.details);
        let output = done.output.expect("output");
        assert_eq!(output, original.with_file_name("qpdf-manual (optimized).pdf"));
        assert!(output.is_file());
        assert_eq!(std::fs::read(&original).expect("read"), before, "the original is untouched");
        let dir = original.parent().expect("dir");
        assert!(others(dir, &[&original, &output]).is_empty(), "no temporary file is left");

        // Again: the name is taken, so the new result gets " (2)" and the
        // first one stays as it was.
        let first = std::fs::read(&output).expect("read");
        let again = rewrite(&exe, &original, &ctx, Some(178));
        assert_eq!(again.output, Some(original.with_file_name("qpdf-manual (optimized) (2).pdf")));
        assert_eq!(std::fs::read(&output).expect("read"), first);
    }
}

#[test]
fn a_wrong_page_count_fails_verification_and_leaves_nothing() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("page-count", Item::Base);
    let done = rewrite(&exe, &original, &ctx, Some(4));
    assert_eq!(done.verdict, Verdict::Failed(Failure::VerificationFailed));
    assert_eq!(done.output, None);
    assert!(others(original.parent().expect("dir"), &[&original]).is_empty());
}

#[test]
fn cancel_in_the_middle_leaves_nothing_behind() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("cancel", Item::Heavy);
    let args = vec![Arg::plain("--recompress-flate"), Arg::plain("--compression-level=9"), Arg::path(&original)];
    let target = target(&exe, args, output_name(&original, "optimized", None).expect("name"));
    let done = std::thread::scope(|s| {
        s.spawn(|| {
            // Long enough for QPDF to be writing the temporary file.
            std::thread::sleep(Duration::from_millis(1200));
            cancel.store(true, Ordering::Relaxed);
        });
        produce(target, &ctx)
    });
    assert_eq!(done.verdict, Verdict::Cancelled);
    assert!(others(original.parent().expect("dir"), &[&original]).is_empty());
}

#[test]
fn an_encrypted_result_is_verified_with_its_new_password() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("encrypt", Item::Base);
    let encrypted = |result_password| {
        let args = vec![
            Arg::plain("--encrypt"),
            Arg::Secret("--user-password", secret("new pass")),
            Arg::Secret("--owner-password", secret("new owner")),
            Arg::plain("--bits=256"),
            Arg::plain("--"),
            Arg::path(&original),
        ];
        Target { result_password, expected_pages: Some(3), ..target(&exe, args, output_name(&original, "protected", None).expect("name")) }
    };
    let done = produce(encrypted(Some(secret("new pass"))), &ctx);
    assert_eq!(done.verdict, Verdict::Done, "{:?}", done.details);
    assert!(done.output.is_some());
    // Without the password the result cannot be checked — and is not kept.
    let done = produce(encrypted(None), &ctx);
    assert_eq!(done.verdict, Verdict::Failed(Failure::VerificationFailed));
    assert_eq!(done.output, None);
}

#[test]
fn a_repaired_file_is_done_with_warnings() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("repair", Item::Damaged);
    let done = rewrite(&exe, &original, &ctx, Some(3));
    assert_eq!(done.verdict, Verdict::DoneWithWarnings);
    assert!(done.output.is_some_and(|p| p.is_file()));
    assert!(done.details.iter().any(|l| l.starts_with("WARNING: ")), "{:?}", done.details);
}

#[test]
fn inputs_qpdf_cannot_take_fail_cleanly() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let cases = [
        (Item::NotPdf, Verdict::Failed(Failure::NotPdf)),
        (Item::Empty, Verdict::Failed(Failure::NotPdf)),
        (Item::Truncated, Verdict::Failed(Failure::Damaged)),
        (Item::EncryptedOpen, Verdict::PasswordRequired),
    ];
    for (item, expected) in cases {
        let original = input("bad-inputs", item);
        let done = rewrite(&exe, &original, &ctx, None);
        assert_eq!(done.verdict, expected, "{item:?}: {:?}", done.details);
        assert!(others(original.parent().expect("dir"), &[&original]).is_empty(), "{item:?}");
    }
}

#[test]
fn a_file_open_in_another_program_is_reported() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("locked", Item::Base);
    let lock = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).share_mode(0).open(&original).expect("lock")
    };
    let done = rewrite(&exe, &original, &ctx, None);
    drop(lock);
    assert_eq!(done.verdict, Verdict::Failed(Failure::AccessDenied));
    assert!(others(original.parent().expect("dir"), &[&original]).is_empty());
}

#[test]
fn a_path_too_long_for_qpdf_is_refused_before_it_starts() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let long = corpus::item(Item::LongPath);
    // Output next to it: the desired name is already too long.
    let done = rewrite(&exe, &long, &ctx, None);
    assert_eq!(done.verdict, Verdict::Failed(Failure::PathTooLong));
    // Output somewhere short: the INPUT is still too long.
    let short = common::scratch("long-input");
    let target = target(&exe, vec![Arg::path(&long)], short.join("out.pdf"));
    assert_eq!(produce(target, &ctx).verdict, Verdict::Failed(Failure::PathTooLong));
    assert!(others(&short, &[]).is_empty());
}

#[test]
fn passwords_open_encrypted_inputs() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("decrypt", Item::EncryptedOpen);
    let args = vec![Arg::Secret("--password", secret(USER_PASSWORD)), Arg::plain("--decrypt"), Arg::path(&original)];
    let target = Target { expected_pages: Some(3), ..target(&exe, args, output_name(&original, "unlocked", None).expect("name")) };
    let done = produce(target, &ctx);
    assert_eq!(done.verdict, Verdict::Done, "{:?}", done.details);
}

/// A folder where nobody may create files (an ACL denying it to everyone),
/// given back when dropped — also when the test fails.
struct ReadOnlyFolder(PathBuf);

impl ReadOnlyFolder {
    fn icacls(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new(qpdf_desk::platform::system32("icacls.exe")).arg(dir).args(args).stdout(std::process::Stdio::null()).status();
        assert!(status.expect("icacls").success(), "icacls {args:?}");
    }

    fn new(dir: &Path) -> ReadOnlyFolder {
        // S-1-1-0: everyone. WD, AD: create files, create folders.
        Self::icacls(dir, &["/deny", "*S-1-1-0:(WD,AD)"]);
        ReadOnlyFolder(dir.to_path_buf())
    }
}

impl Drop for ReadOnlyFolder {
    fn drop(&mut self) {
        Self::icacls(&self.0, &["/remove:d", "*S-1-1-0"]);
    }
}

#[test]
fn a_folder_that_cannot_be_written_is_named_and_left_as_it_was() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("read-only", Item::Base);
    let dir = original.parent().expect("dir").to_path_buf();
    let before = std::fs::read(&original).expect("read");
    let done = {
        let _read_only = ReadOnlyFolder::new(&dir);
        rewrite(&exe, &original, &ctx, Some(3))
    };
    assert_eq!(done.verdict, Verdict::Failed(Failure::FolderNotWritable), "{:?}", done.details);
    assert_eq!(std::fs::read(&original).expect("read"), before);
    assert!(others(&dir, &[&original]).is_empty());
}

#[test]
fn a_folder_that_is_gone_is_named_not_taken_for_a_missing_file() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let original = input("gone-folder", Item::Base);
    let gone = original.parent().expect("dir").join("deleted meanwhile");
    let done = produce(target(&exe, vec![Arg::path(&original)], gone.join("result.pdf")), &ctx);
    assert_eq!(done.verdict, Verdict::Failed(Failure::FolderMissing), "{:?}", done.details);
    assert!(!gone.exists());
}
