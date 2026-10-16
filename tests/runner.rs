//! `core::runner` against the real QPDF: arguments through `@-`, progress,
//! cancel, the watchdog, the job's limits (the command line itself:
//! command_line.rs).

mod common;

use common::corpus::{self, Item, USER_PASSWORD};
use common::job_object as job;
use qpdf_desk::core::runner::{run, Arg, Context, Ending};
use qpdf_desk::platform::JobObject;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

const GB: u64 = 1 << 30;

#[test]
fn arguments_arrive_through_stdin() {
    for exe in common::qpdf_builds() {
        let (job, cancel) = (job(), AtomicBool::new(false));
        let ctx = Context::new(&job, &cancel);
        // A file named like an option, given as an absolute path.
        let file = corpus::item(Item::DashName);
        let done = run(&exe, &[Arg::plain("--show-npages"), Arg::path(&file)], &ctx).expect("run");
        assert_eq!(done.ending, Ending::Exited(0), "{:?}", done.stderr);
        assert_eq!(done.stdout.lines, ["3"]);
    }
}

#[test]
fn a_password_with_spaces_and_polish_letters_opens_the_file() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let file = corpus::item(Item::EncryptedOpen);
    let password = common::secret(USER_PASSWORD);
    let args = [Arg::Secret("--password", password), Arg::plain("--show-npages"), Arg::path(&file)];
    let done = run(&exe, &args, &ctx).expect("run");
    assert_eq!((done.ending, done.stdout.lines.as_slice()), (Ending::Exited(0), &["3".to_string()][..]));

    let wrong = [
        Arg::Secret("--password", common::secret("nope")),
        Arg::plain("--check"),
        Arg::path(&file),
    ];
    let done = run(&exe, &wrong, &ctx).expect("run");
    assert_eq!(done.ending, Ending::Exited(2));
    assert!(done.stderr.last_line().is_some_and(|l| l.ends_with(": invalid password")), "{:?}", done.stderr);
}

#[test]
fn progress_is_reported_while_writing() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let last = AtomicU8::new(0);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let progress = |p: u8| {
        last.store(p, Ordering::Relaxed);
        calls.fetch_add(1, Ordering::Relaxed);
    };
    let ctx = Context { progress: &progress, ..Context::new(&job, &cancel) };
    let out = common::scratch("progress").join("out.pdf");
    let args = [Arg::plain("--progress"), Arg::path(corpus::item(Item::Manual)), Arg::path(&out)];
    let done = run(&exe, &args, &ctx).expect("run");
    assert_eq!(done.ending, Ending::Exited(0), "{:?}", done.stderr);
    assert_eq!(last.load(Ordering::Relaxed), 100);
    assert!(calls.load(Ordering::Relaxed) > 1);
    assert!(done.stdout.lines.is_empty(), "progress lines are consumed: {:?}", done.stdout);
}

#[test]
fn a_relative_path_never_reaches_qpdf() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    assert!(run(&exe, &[Arg::path("-x.pdf")], &ctx).is_err());
}

#[test]
fn cancel_kills_qpdf_quickly() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let out = common::scratch("cancel").join("out.pdf");
    let args = [
        Arg::plain("--compress-streams=y"),
        Arg::plain("--recompress-flate"),
        Arg::plain("--compression-level=9"),
        Arg::path(corpus::item(Item::Heavy)),
        Arg::path(&out),
    ];
    let started = Instant::now();
    let done = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(700));
            cancel.store(true, Ordering::Relaxed);
        });
        run(&exe, &args, &ctx).expect("run")
    });
    let took = started.elapsed();
    assert_eq!(done.ending, Ending::Cancelled);
    assert!(took < Duration::from_secs(3), "cancel took {took:?}");
}

#[test]
fn the_watchdog_kills_a_process_that_does_nothing() {
    // QPDF cannot be made to hang on purpose, so the watchdog is shown on
    // `cmd.exe`: it reads its commands from stdin, as QPDF reads `@-`, and
    // waits for `waitfor` without using any processor time.
    let cmd = &qpdf_desk::platform::system32("cmd.exe");
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context { hang_after: Duration::from_secs(1), ..Context::new(&job, &cancel) };
    let started = Instant::now();
    let done = run(cmd, &[Arg::plain("waitfor /t 4 QpdfDeskTestNeverSent >nul 2>nul")], &ctx).expect("run");
    assert_eq!(done.ending, Ending::Hung);
    // `waitfor` keeps the inherited pipes open until its own timeout, so the
    // run ends with it (≈4 s), not with the kill — but well before the
    // timeout would end it by itself...
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn the_job_refuses_more_processes_than_allowed() {
    let cmd = &qpdf_desk::platform::system32("cmd.exe");
    let job = JobObject::new(GB, 1).expect("job");
    let cancel = AtomicBool::new(false);
    let ctx = Context::new(&job, &cancel);
    // The first `cmd` starts `waitfor` — a second process in a job of one.
    let done = run(cmd, &[Arg::plain("waitfor /t 3 QpdfDeskTestNeverSent"), Arg::plain("exit 7")], &ctx)
        .expect("run");
    // `waitfor` was refused at once, so cmd went straight on to `exit 7`.
    assert_eq!(done.ending, Ending::Exited(7));
}

#[test]
fn running_out_of_the_memory_limit_is_reported() {
    // Measured on 12.4.2: QPDF itself never says it ran out of memory.
    // Many small objects exhaust the STACK first → crash 0xC00000FD; one huge
    // object fails a HEAP allocation → "unable to find trailer dictionary
    // while recovering damaged file", exit 2 — as if the file were damaged.
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("memory");
    let huge_array = dir.join("huge-array.pdf");
    let base = std::fs::read(corpus::item(Item::Base)).expect("read");
    let text = String::from_utf8_lossy(&base)
        .replace("/Type /Catalog", &format!("/Type /Catalog /Big [{}]", "1 ".repeat(2_000_000)));
    std::fs::write(&huge_array, text).expect("write");
    let many_objects = corpus::item(Item::Pages10k);
    let cancel = AtomicBool::new(false);
    for (input, limit_mb, expected) in [
        (&many_objects, 20, Ending::Exited(0xC00000FDu32 as i32)),
        (&huge_array, 64, Ending::Exited(2)),
    ] {
        let job = JobObject::new(limit_mb << 20, 4).expect("job");
        let ctx = Context::new(&job, &cancel);
        let args = [Arg::plain("--object-streams=generate"), Arg::path(input), Arg::path(dir.join("out.pdf"))];
        let done = run(&exe, &args, &ctx).expect("run");
        assert_eq!(done.ending, expected, "{input:?}: {:?}", done.stderr);
        assert!(done.memory_limit_hit, "{input:?}");
    }
    // With room enough, the same files go through and nothing is reported.
    let roomy = job();
    let ctx = Context::new(&roomy, &cancel);
    let args = [Arg::plain("--object-streams=generate"), Arg::path(&many_objects), Arg::path(dir.join("out.pdf"))];
    let done = run(&exe, &args, &ctx).expect("run");
    assert_eq!((done.ending, done.memory_limit_hit), (Ending::Exited(0), false));
}

