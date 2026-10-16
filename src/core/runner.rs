//! Running one QPDF process: the rules every run follows.
//!
//! - The command line is just `qpdf.exe @-`; every argument, passwords
//!   included, goes through stdin, one per line, and stdin is closed at once.
//!   Nothing secret is ever visible to other processes.
//! - Each path must be absolute: QPDF takes a relative `-x.pdf` for an option
//!   (measured). Values with a line break or NUL are refused — the `@-` list is
//!   newline-separated. An empty line is refused too: QPDF reads it as an
//!   empty argument (measured). A nested `@file` line is NOT expanded by QPDF
//!   (measured), so it needs no special care. The text is UTF-8 WITHOUT a
//!   byte order mark: with one, the first option is taken for a file name
//!   (measured with .NET's stdin writer).
//! - The process runs below normal priority, without a window, inside the
//!   app's shared [`JobObject`]. It is put there right after it starts:
//!   `std::process` cannot start it suspended (it hides the thread handle) nor
//!   create it inside the job (`raw_attribute` is unstable as of Rust 1.99).
//!   The gap is harmless: until its arguments arrive on stdin — written only
//!   after the assignment — QPDF reads nothing (measured).
//! - stdout carries the `write progress: N%` lines; everything else on
//!   stdout and stderr is captured as bytes with a size limit, keeping both the
//!   beginning and the end, because a hostile file can make QPDF print millions
//!   of warnings while its final verdict is the last line.
//!   [`run_into`] hands stdout to a writer instead — a JSON report, the bytes
//!   of an attachment — stopping QPDF past a size limit.
//! - A watchdog kills a process whose processor time has not moved for
//!   [`HANG_AFTER`]; a cancel request kills it at once. Whatever
//!   happens, [`ChildGuard`] reaps it.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf, Prefix};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::secret::{wipe, Secret};
use crate::platform::{self, ChildGuard, JobObject};

/// The longest path QPDF 12.4 can open: classic `MAX_PATH` without the
/// terminating NUL. Longer ones fail even with the `\\?\` prefix.
pub const MAX_PATH_CHARS: usize = 259;

/// How long a process may use no processor time before it counts as hung.
/// There is no limit on the total time — big files take long.
pub const HANG_AFTER: Duration = Duration::from_secs(60);

/// How often the cancel flag and the watchdog are checked while QPDF runs.
const POLL_MS: u32 = 100;

/// One argument for QPDF.
pub enum Arg {
    /// An option or a value that is not a path, e.g. `--check`, `1-5,8`.
    Plain(String),
    /// A file path; must be absolute and short enough for QPDF.
    Path(PathBuf),
    /// `option=password`, e.g. `--password=…`; the password never leaves
    /// memory except through QPDF's stdin.
    Secret(&'static str, Secret),
}

impl Arg {
    pub fn plain(text: impl Into<String>) -> Arg {
        Arg::Plain(text.into())
    }

    pub fn path(path: impl Into<PathBuf>) -> Arg {
        Arg::Path(path.into())
    }

    /// `--password=…`, when there is a password to give.
    pub fn password(password: Option<&Secret>) -> Option<Arg> {
        password.map(|p| Arg::Secret("--password", p.duplicate()))
    }

    /// `--password` (when given), the `options`, and `path` last: one look
    /// at one file.
    pub fn on_file(password: Option<&Secret>, options: &[&str], path: &Path) -> Vec<Arg> {
        let mut args: Vec<Arg> = Arg::password(password).into_iter().collect();
        args.extend(options.iter().map(|option| Arg::plain(*option)));
        args.push(Arg::path(path));
        args
    }
}

/// Why an argument list was refused before QPDF was started.
#[derive(Debug, PartialEq, Eq)]
pub enum ArgError {
    /// A line break or NUL in a value (it would split the argument).
    ForbiddenCharacter,
    /// An empty argument (QPDF would read the empty line as one).
    Empty,
    /// A path that is not absolute, or uses the `\\?\` form QPDF breaks.
    NotAbsolute(PathBuf),
    /// A path longer than [`MAX_PATH_CHARS`].
    PathTooLong(PathBuf),
    /// A path that cannot be written as UTF-8 (an unpaired surrogate).
    NotUnicode(PathBuf),
}

/// Check that QPDF can be given `path`; returns it as
/// text for the argument list.
pub fn check_path(path: &Path) -> Result<&str, ArgError> {
    let text = path.to_str().ok_or_else(|| ArgError::NotUnicode(path.into()))?;
    // A drive (`C:\…`) or a share (`\\server\share\…`), with a root. The
    // verbatim `\\?\` forms are refused: QPDF mangles them.
    let usable = matches!(
        path.components().next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::UNC(..))
    ) && path.is_absolute();
    if !usable {
        return Err(ArgError::NotAbsolute(path.into()));
    }
    if text.encode_utf16().count() > MAX_PATH_CHARS {
        return Err(ArgError::PathTooLong(path.into()));
    }
    check_value(text)?;
    Ok(text)
}

fn check_value(text: &str) -> Result<(), ArgError> {
    if text.contains(['\n', '\r', '\0']) {
        Err(ArgError::ForbiddenCharacter)
    } else {
        Ok(())
    }
}

/// Bytes that are wiped when dropped — the stdin buffer holds passwords.
struct Wiped(Vec<u8>);

impl Drop for Wiped {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// The `@-` stdin text: one argument per line, UTF-8.
///
/// The buffer is allocated once at its exact final size, so no copy of a
/// password is left behind in a freed, outgrown allocation.
fn stdin_text(args: &[Arg]) -> Result<Wiped, ArgError> {
    // Each line in up to three pieces: `text`, or `option` `=` `secret`.
    let mut lines: Vec<[&str; 3]> = Vec::with_capacity(args.len());
    for arg in args {
        lines.push(match arg {
            Arg::Plain(text) if text.is_empty() => return Err(ArgError::Empty),
            Arg::Plain(text) => {
                check_value(text)?;
                [text.as_str(), "", ""]
            }
            Arg::Path(path) => [check_path(path)?, "", ""],
            // `Secret` already refuses line breaks; the option is ours.
            Arg::Secret(option, secret) => {
                check_value(option)?;
                [*option, "=", secret.expose()]
            }
        });
    }
    let len = lines.iter().flatten().map(|piece| piece.len()).sum::<usize>() + lines.len();
    let mut out = Wiped(Vec::with_capacity(len));
    for line in &lines {
        for piece in line {
            out.0.extend_from_slice(piece.as_bytes());
        }
        out.0.push(b'\n');
    }
    Ok(out)
}

/// Text captured from one of QPDF's output streams, cleaned for display:
/// control characters removed, each line at most [`MAX_LINE_BYTES`].
///
/// At most [`HEAD_LINES`] lines from the start and [`TAIL_LINES`] from the end
/// are kept; `omitted` counts the lines dropped in between.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Captured {
    pub lines: Vec<String>,
    pub omitted: usize,
}

pub const HEAD_LINES: usize = 180;
pub const TAIL_LINES: usize = 20;
pub const MAX_LINE_BYTES: usize = 320;

impl Captured {
    /// The last non-empty line — where QPDF puts its verdict.
    pub fn last_line(&self) -> Option<&str> {
        self.lines.iter().rev().map(String::as_str).find(|l| !l.is_empty())
    }
}

/// Read `source` to its end, line by line, keeping a bounded [`Captured`].
/// `keep` sees every line first and may consume it (returns `false`).
///
/// It never stops reading early: a stalled pipe would stall QPDF.
fn capture(mut source: impl Read, mut keep: impl FnMut(&str) -> bool) -> Captured {
    let mut head = Vec::new();
    let mut tail = std::collections::VecDeque::new();
    let mut omitted = 0usize;
    let mut line = Vec::with_capacity(MAX_LINE_BYTES);
    let mut push = |raw: &mut Vec<u8>| {
        let text = super::printable(&String::from_utf8_lossy(raw));
        raw.clear();
        if !keep(&text) {
            return;
        }
        if head.len() < HEAD_LINES {
            head.push(text);
        } else {
            tail.push_back(text);
            if tail.len() > TAIL_LINES {
                tail.pop_front();
                omitted += 1;
            }
        }
    };
    let mut buf = [0u8; 8192];
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &byte in &buf[..n] {
            if byte == b'\n' {
                push(&mut line);
            } else if line.len() < MAX_LINE_BYTES {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        push(&mut line);
    }
    head.extend(tail);
    Captured { lines: head, omitted }
}

/// The percentage from a `qpdf: <file>: write progress: N%` line.
///
/// Matched at the END of the line: the file name before it is echoed by QPDF
/// and could itself contain the words.
pub fn parse_progress(line: &str) -> Option<u8> {
    let (_, tail) = line.rsplit_once("write progress: ")?;
    let percent: u8 = tail.strip_suffix('%')?.parse().ok()?;
    Some(percent.min(100))
}

/// What a run needs besides its arguments.
pub struct Context<'a> {
    /// The app's shared job.
    pub job: &'a JobObject,
    /// Set by the UI to stop the work.
    pub cancel: &'a AtomicBool,
    /// Called from a reader thread with every `write progress` percentage.
    pub progress: &'a (dyn Fn(u8) + Sync),
    /// See [`HANG_AFTER`]; shorter only in tests.
    pub hang_after: Duration,
}

impl<'a> Context<'a> {
    /// A context without progress reporting and with the standard watchdog.
    pub fn new(job: &'a JobObject, cancel: &'a AtomicBool) -> Context<'a> {
        Context { job, cancel, progress: &|_| {}, hang_after: HANG_AFTER }
    }
}

/// How the process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// It exited by itself with this code (0, 2, 3, or a crash code).
    Exited(i32),
    /// Killed on a cancel request.
    Cancelled,
    /// Killed by the watchdog.
    Hung,
}

/// A finished run.
#[derive(Debug)]
pub struct Finished {
    pub ending: Ending,
    pub stdout: Captured,
    pub stderr: Captured,
    /// The process ran into the job's memory limit. Its exit code and
    /// messages then mean nothing — QPDF may even report a "repaired" file.
    pub memory_limit_hit: bool,
}

/// Why QPDF could not be run at all.
#[derive(Debug)]
pub enum RunError {
    BadArgument(ArgError),
    /// The process could not be created (missing or blocked `qpdf.exe`).
    Start(std::io::Error),
    /// The job refused the process (too many running at once).
    Job(std::io::Error),
}

/// Run `qpdf` (a full path) with `args` and wait for it to end.
pub fn run(qpdf: &Path, args: &[Arg], ctx: &Context) -> Result<Finished, RunError> {
    run_with(qpdf, args, ctx, None).map(|(finished, _)| finished)
}

/// What [`run_into`] passed on from QPDF's stdout.
#[derive(Debug, Default)]
pub struct Copied {
    pub bytes: u64,
    /// More than the limit came: QPDF was stopped there.
    pub over_limit: bool,
    /// Writing failed (the disk is full…): QPDF was stopped.
    pub error: Option<std::io::Error>,
}

/// [`run`], with stdout passed to `sink` as it comes — the contents of an
/// attachment, a JSON report — instead of read as lines. Past `limit` bytes,
/// or when writing fails, QPDF is stopped: a small PDF can unpack into
/// gigabytes.
pub fn run_into(
    qpdf: &Path,
    args: &[Arg],
    ctx: &Context,
    sink: &mut (dyn Write + Send),
    limit: u64,
) -> Result<(Finished, Copied), RunError> {
    run_with(qpdf, args, ctx, Some((sink, limit)))
}

/// [`run`] or [`run_into`].
fn run_with(
    qpdf: &Path,
    args: &[Arg],
    ctx: &Context,
    sink: Option<(&mut (dyn Write + Send), u64)>,
) -> Result<(Finished, Copied), RunError> {
    let input = stdin_text(args).map_err(RunError::BadArgument)?;
    // Stopped already: no process at all. A merge probes its unprobed files
    // one by one, and closing the window started each of 10 000 only to kill
    // it — over a minute (measured).
    if ctx.cancel.load(Ordering::Relaxed) {
        let finished = Finished { ending: Ending::Cancelled, stdout: Captured::default(), stderr: Captured::default(), memory_limit_hit: false };
        return Ok((finished, Copied::default()));
    }
    let child = platform::background_command(qpdf)
        .arg("@-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Start)?;
    let pid = child.id();
    let mut guard = ChildGuard(child);
    ctx.job.assign(&guard.0).map_err(RunError::Job)?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (guard.0.stdin.take(), guard.0.stdout.take(), guard.0.stderr.take())
    else {
        return Err(RunError::Start(std::io::Error::other("no pipes")));
    };
    // Set when the copy to `sink` stops: QPDF is stopped with it.
    let full = AtomicBool::new(false);
    std::thread::scope(|scope| {
        // Written from a thread: a long list can fill the pipe, and the main
        // loop must stay free to cancel. A write error means QPDF already
        // ended; its exit code tells why.
        scope.spawn(move || {
            let _ = stdin.write_all(&input.0);
            drop(input);
            drop(stdin);
        });
        let full = &full;
        let out = scope.spawn(move || match sink {
            Some((sink, limit)) => (Captured::default(), copy(stdout, sink, limit, full)),
            None => {
                let lines = capture(stdout, |line| match parse_progress(line) {
                    Some(percent) => {
                        (ctx.progress)(percent);
                        false
                    }
                    None => true,
                });
                (lines, Copied::default())
            }
        });
        let err = scope.spawn(|| capture(stderr, |_| true));
        let ending = watch(&mut guard, ctx, full);
        // Killing the process closed its ends of the pipes, so the readers
        // finish. A reader cannot panic (`panic = "abort"` in release anyway).
        let (stdout, copied) = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        let memory_limit_hit = ctx.job.hit_memory_limit(pid);
        Ok((Finished { ending, stdout, stderr, memory_limit_hit }, copied))
    })
}

/// Pass `source` on to `sink`, at most `limit` bytes; past that, or when
/// writing fails, `full` is set (QPDF gets stopped) and the rest is only
/// drained — never stop reading early, a stalled pipe would stall QPDF.
fn copy(mut source: impl Read, sink: &mut dyn Write, limit: u64, full: &AtomicBool) -> Copied {
    let mut copied = Copied::default();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if full.load(Ordering::Relaxed) {
            continue;
        }
        if n as u64 > limit.saturating_sub(copied.bytes) {
            copied.over_limit = true;
        } else if let Err(e) = sink.write_all(&buf[..n]) {
            copied.error = Some(e);
        } else {
            copied.bytes += n as u64;
            continue;
        }
        full.store(true, Ordering::Relaxed);
    }
    copied
}

/// Wait for the process, killing it on cancel (or `stop`) or when it hangs.
fn watch(guard: &mut ChildGuard, ctx: &Context, stop: &AtomicBool) -> Ending {
    let mut last_cpu = platform::cpu_time(&guard.0);
    let mut last_change = Instant::now();
    let ending = loop {
        if platform::wait_for_exit(&guard.0, POLL_MS) {
            let code = guard.0.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
            break Ending::Exited(code);
        }
        if ctx.cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            break Ending::Cancelled;
        }
        let cpu = platform::cpu_time(&guard.0);
        if cpu != last_cpu {
            last_cpu = cpu;
            last_change = Instant::now();
        } else if last_change.elapsed() >= ctx.hang_after {
            break Ending::Hung;
        }
    };
    if !matches!(ending, Ending::Exited(_)) {
        let _ = guard.0.kill();
        let _ = guard.0.wait();
    }
    ending
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(args: &[Arg]) -> String {
        String::from_utf8(stdin_text(args).expect("valid").0.clone()).expect("utf-8")
    }

    #[test]
    fn arguments_go_one_per_line_with_the_password_inline() {
        let args = [
            Arg::plain("--check"),
            Arg::Secret("--password", Secret::new("p w=ś".into()).expect("ok")),
            Arg::path(r"C:\Docs\-x.pdf"),
        ];
        assert_eq!(text(&args), "--check\n--password=p w=ś\nC:\\Docs\\-x.pdf\n");
    }

    #[test]
    fn a_run_asked_to_stop_starts_no_process() {
        let job = platform::JobObject::new(1 << 30, 4).expect("job object");
        let cancel = AtomicBool::new(true);
        // A program that is not there: starting it would fail.
        let done = run(Path::new(r"C:\nowhere\qpdf.exe"), &[Arg::plain("--version")], &Context::new(&job, &cancel));
        assert_eq!(done.expect("no start attempted").ending, Ending::Cancelled);
    }

    #[test]
    fn an_empty_password_is_still_one_argument() {
        let args = [Arg::Secret("--user-password", Secret::new(String::new()).expect("ok"))];
        assert_eq!(text(&args), "--user-password=\n");
    }

    #[test]
    fn the_buffer_is_allocated_once_at_its_exact_size() {
        let args = [Arg::plain("--a"), Arg::Secret("--password", Secret::new("xyz".into()).expect("ok"))];
        let buf = stdin_text(&args).expect("valid");
        assert_eq!(buf.0.len(), buf.0.capacity());
    }

    #[test]
    fn values_that_would_split_the_list_are_refused() {
        for bad in ["a\nb", "a\rb", "a\0b"] {
            assert_eq!(stdin_text(&[Arg::plain(bad)]).err(), Some(ArgError::ForbiddenCharacter));
        }
        assert_eq!(stdin_text(&[Arg::plain("")]).err(), Some(ArgError::Empty));
        let path = PathBuf::from("C:\\a\nb.pdf");
        assert_eq!(check_path(&path).err(), Some(ArgError::ForbiddenCharacter));
    }

    #[test]
    fn only_plain_absolute_paths_are_accepted() {
        for good in [r"C:\a.pdf", r"d:\x\-x.pdf", r"C:\@x.pdf", r"\\server\share\a.pdf"] {
            assert!(check_path(Path::new(good)).is_ok(), "{good}");
        }
        for bad in ["-x.pdf", "@x.pdf", r"x\a.pdf", r"\a.pdf", r"C:a.pdf", r"\\?\C:\a.pdf", r"\\.\C:\a.pdf"]
        {
            assert_eq!(check_path(Path::new(bad)), Err(ArgError::NotAbsolute(bad.into())), "{bad}");
        }
    }

    #[test]
    fn paths_beyond_qpdfs_limit_are_refused() {
        let fits = format!(r"C:\{}", "a".repeat(MAX_PATH_CHARS - 3));
        assert!(check_path(Path::new(&fits)).is_ok());
        let long = format!("{fits}b");
        assert_eq!(check_path(Path::new(&long)), Err(ArgError::PathTooLong(long.clone().into())));
        // Counted in UTF-16 units, like Windows does — not in bytes.
        let polish = format!(r"C:\{}", "ż".repeat(MAX_PATH_CHARS - 3));
        assert!(check_path(Path::new(&polish)).is_ok());
    }

    #[test]
    fn a_path_with_an_unpaired_surrogate_is_refused() {
        use std::os::windows::ffi::OsStringExt;
        let wide: Vec<u16> = r"C:\a".encode_utf16().chain([0xD800]).collect();
        let path = PathBuf::from(std::ffi::OsString::from_wide(&wide));
        assert!(matches!(check_path(&path), Err(ArgError::NotUnicode(_))));
    }

    #[test]
    fn progress_is_read_from_the_end_of_the_line() {
        assert_eq!(parse_progress(r"qpdf: C:\a.pdf: write progress: 63%"), Some(63));
        assert_eq!(parse_progress("qpdf: x: write progress: 100%"), Some(100));
        assert_eq!(parse_progress("qpdf: x: write progress: 250%"), Some(100));
        // A file name that imitates progress does not count.
        assert_eq!(parse_progress("qpdf: write progress: 5%.pdf: invalid password"), None);
        assert_eq!(parse_progress("qpdf: x: write progress: -1%"), None);
        assert_eq!(parse_progress("checking x.pdf"), None);
    }

    #[test]
    fn capture_keeps_lines_cleaned_of_control_characters() {
        let got = capture(&b"one\r\ntwo\x1b[31m\x07\nthree"[..], |_| true);
        assert_eq!(got.lines, ["one", "two[31m", "three"]);
        assert_eq!(got.omitted, 0);
    }

    #[test]
    fn capture_bounds_a_flood_but_keeps_the_final_verdict() {
        let mut flood = Vec::new();
        for i in 0..200_000 {
            flood.extend_from_slice(format!("WARNING: x.pdf (object {i} 0): bad\n").as_bytes());
        }
        flood.extend_from_slice(b"qpdf: x.pdf: unable to find trailer dictionary\n");
        let got = capture(&flood[..], |_| true);
        assert_eq!(got.lines.len(), HEAD_LINES + TAIL_LINES);
        assert_eq!(got.omitted, 200_001 - HEAD_LINES - TAIL_LINES);
        assert_eq!(got.last_line(), Some("qpdf: x.pdf: unable to find trailer dictionary"));
        assert!(got.lines[0].starts_with("WARNING: x.pdf (object 0 0)"));
    }

    #[test]
    fn capture_cuts_an_endless_line() {
        let mut huge = vec![b'A'; 10_000_000];
        huge.extend_from_slice(b"\nend\n");
        let got = capture(&huge[..], |_| true);
        assert_eq!(got.lines.len(), 2);
        assert_eq!(got.lines[0].len(), MAX_LINE_BYTES);
        assert_eq!(got.lines[1], "end");
    }

    #[test]
    fn capture_survives_invalid_utf8() {
        let got = capture(&b"\xff\xfe ok\n"[..], |_| true);
        assert_eq!(got.lines, ["\u{fffd}\u{fffd} ok"]);
    }

    #[test]
    fn a_copy_stops_at_its_limit_but_drains_the_rest() {
        let full = AtomicBool::new(false);
        let mut sink = Vec::new();
        let copied = copy(&[7u8; 100][..], &mut sink, 1000, &full);
        assert_eq!((copied.bytes, copied.over_limit, sink.len()), (100, false, 100));
        assert!(!full.load(Ordering::Relaxed));
        let mut sink = Vec::new();
        let copied = copy(&vec![7u8; 200_000][..], &mut sink, 100_000, &full);
        assert!(copied.over_limit && full.load(Ordering::Relaxed));
        assert!(sink.len() <= 100_000, "never past the limit");
    }

    #[test]
    fn consumed_lines_are_not_kept() {
        let got = capture(&b"qpdf: a: write progress: 5%\n178\n"[..], |l| parse_progress(l).is_none());
        assert_eq!(got.lines, ["178"]);
    }

    #[test]
    fn random_output_from_qpdf_stays_bounded_and_clean() {
        let mut rng = crate::fuzz::Rng::new(0x5EED_0004);
        for _ in 0..300 {
            // Bytes of any kind, with line breaks and progress lines between.
            let mut output = Vec::new();
            for _ in 0..rng.below(400) {
                match rng.below(4) {
                    0 => output.push(b'\n'),
                    1 => output.extend_from_slice(format!("qpdf: x: write progress: {}%\n", rng.below(300)).as_bytes()),
                    _ => {
                        let max = if rng.chance(20) { 5_000 } else { 60 };
                        output.extend(rng.bytes(max));
                    }
                }
            }
            let got = capture(&output[..], |line| parse_progress(line).is_none_or(|percent| percent <= 100));
            assert!(got.lines.len() <= HEAD_LINES + TAIL_LINES);
            for line in &got.lines {
                assert!(line.chars().count() <= MAX_LINE_BYTES, "{line:?}");
                assert_eq!(crate::core::printable(line), *line, "{line:?}");
            }
        }
    }

    #[test]
    fn random_arguments_are_one_line_each_or_refused() {
        let alphabet = crate::fuzz::chars(r"aZ09 -=\:./@%");
        let mut rng = crate::fuzz::Rng::new(0x5EED_0005);
        for _ in 0..20_000 {
            let texts: Vec<String> = (0..1 + rng.below(4)).map(|_| rng.text(&alphabet, 12)).collect();
            let args: Vec<Arg> = texts
                .iter()
                .map(|text| match rng.below(3) {
                    0 => Arg::plain(text.as_str()),
                    1 => Arg::path(format!(r"C:\{text}")),
                    _ => match Secret::new(text.clone()) {
                        Ok(secret) => Arg::Secret("--password", secret),
                        Err(_) => Arg::plain(text.as_str()),
                    },
                })
                .collect();
            let Ok(out) = stdin_text(&args) else { continue };
            let text = std::str::from_utf8(&out.0).expect("UTF-8");
            assert!(!text.starts_with('\u{feff}'));
            assert_eq!(text.matches('\n').count(), args.len(), "{texts:?}: one argument, one line");
            assert_eq!(out.0.len(), out.0.capacity(), "allocated once");
        }
    }
}
