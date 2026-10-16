//! From a finished QPDF run to a status and a sentence for a person.
//!
//! The exit code decides first: 0 done, 3 done with warnings, 2 failed,
//! anything else a crash. Only then is QPDF's text read — and
//! only to choose the human sentence or tell "password required" from other
//! failures. Matching is done at the END of QPDF's own last line: the part
//! before it echoes paths and names from the PDF, which must never decide
//! anything (a file may well be called `x: invalid password.pdf`).
//!
//! A run that hit the job's memory limit is a failure whatever QPDF says:
//! it may have taken the failed allocation for damage and "repaired" the file
//! into something incomplete (measured, see `platform::JobObject`).

use std::io;

use super::runner::{ArgError, Captured, Ending, Finished, RunError};
use crate::t;

/// What became of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Done,
    /// QPDF exit code 3 — the result is kept.
    DoneWithWarnings,
    /// The result was not smaller than the original, so it was discarded
    /// ("Keep original if not smaller").
    NotSmaller,
    /// The file needs an open password, and none or a wrong one was given.
    PasswordRequired,
    Cancelled,
    Failed(Failure),
}

/// QPDF could not be run at all: the file failed.
impl From<RunError> for Verdict {
    fn from(error: RunError) -> Self {
        Verdict::Failed(from_run_error(&error))
    }
}

/// Why a file failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    NotPdf,
    Damaged,
    /// Opened by another program (sharing violation, our own check).
    InUse,
    /// QPDF's "Permission denied" — no access, or open in another program;
    /// QPDF does not say which.
    AccessDenied,
    /// The file is gone (or never was).
    NotFound,
    /// Beyond what QPDF can open.
    PathTooLong,
    /// The job's memory limit.
    MemoryLimit,
    /// The watchdog killed it.
    Hung,
    /// Exit code other than 0, 2 or 3.
    Crashed,
    /// Too little free space for the result.
    NoDiskSpace,
    /// No file may be created in the result's folder (read-only, no
    /// permission) — measured with an ACL denying it.
    FolderNotWritable,
    /// The result's folder is gone (deleted, or a drive no longer there).
    FolderMissing,
    /// The written result did not pass `qpdf --check` or has the wrong page
    /// count.
    VerificationFailed,
    /// Privacy's fixed document ID on a file that stays encrypted: QPDF
    /// refuses it (measured) — `job.rs` sees it coming when the
    /// probe has run, QPDF's own message says so otherwise.
    FixedIdEncrypted,
    /// Attachments: the file already has one under the name being added.
    AttachmentExists,
    /// A file of a merge left alone because another one cannot be used.
    NotMerged,
    /// QPDF's own last message (without the `qpdf: ` prefix), for failures
    /// with no sentence of ours; or an I/O error text of our own.
    Other(String),
}

impl Verdict {
    /// The status text of the file list.
    pub fn status(&self) -> String {
        match self {
            Verdict::Done => t!(VerdictDone).into(),
            Verdict::DoneWithWarnings => t!(VerdictWarnings).into(),
            Verdict::NotSmaller => t!(VerdictNotSmaller).into(),
            Verdict::PasswordRequired => t!(VerdictPassword).into(),
            Verdict::Cancelled => t!(VerdictCancelled).into(),
            Verdict::Failed(failure) => t!(VerdictFailed, why = failure.sentence()),
        }
    }

    /// Whether a usable result exists (it may still have to be verified).
    pub fn succeeded(&self) -> bool {
        matches!(self, Verdict::Done | Verdict::DoneWithWarnings)
    }

    /// Whether the file is finished with — a new Start leaves it alone.
    pub fn settled(&self) -> bool {
        self.succeeded() || *self == Verdict::NotSmaller
    }
}

impl Failure {
    pub fn sentence(&self) -> &str {
        match self {
            Failure::NotPdf => t!(FailureNotPdf),
            Failure::Damaged => t!(FailureDamaged),
            Failure::InUse => t!(FailureInUse),
            Failure::AccessDenied => t!(FailureAccessDenied),
            Failure::NotFound => t!(FailureNotFound),
            Failure::PathTooLong => t!(FailurePathTooLong),
            Failure::MemoryLimit => t!(FailureMemoryLimit),
            Failure::Hung => t!(FailureHung),
            Failure::Crashed => t!(FailureCrashed),
            Failure::NoDiskSpace => t!(FailureNoDiskSpace),
            Failure::FolderNotWritable => t!(FailureFolderNotWritable),
            Failure::FolderMissing => t!(FailureFolderMissing),
            Failure::VerificationFailed => t!(FailureVerification),
            Failure::FixedIdEncrypted => t!(FailureFixedIdEncrypted),
            Failure::AttachmentExists => t!(FailureAttachmentExists),
            Failure::NotMerged => t!(FailureNotMerged),
            Failure::Other(message) => message,
        }
    }
}

/// The verdict on a finished run.
pub fn judge(run: &Finished) -> Verdict {
    if run.memory_limit_hit {
        return Verdict::Failed(Failure::MemoryLimit);
    }
    match run.ending {
        Ending::Cancelled => Verdict::Cancelled,
        Ending::Hung => Verdict::Failed(Failure::Hung),
        Ending::Exited(0) => Verdict::Done,
        Ending::Exited(3) => Verdict::DoneWithWarnings,
        Ending::Exited(2) => from_message(&run.stderr),
        Ending::Exited(_) => Verdict::Failed(Failure::Crashed),
    }
}

/// Exit code 2: which failure, from QPDF's last line (see the module notes).
fn from_message(stderr: &Captured) -> Verdict {
    let last = stderr.last_line().unwrap_or("");
    if last.ends_with(": invalid password") {
        return Verdict::PasswordRequired;
    }
    let failure = if last.ends_with("the file to be written is encrypted (even though the file may not require a password)") {
        Failure::FixedIdEncrypted
    } else if last.ends_with("; use --replace to replace or --key to specify a different key") {
        Failure::AttachmentExists
    } else if last.ends_with(": Permission denied") {
        Failure::AccessDenied
    } else if last.ends_with(": No such file or directory") {
        Failure::NotFound
    } else if warning_ends_with(stderr, ": can't find PDF header") {
        Failure::NotPdf
    } else if warning_ends_with(stderr, ": file is damaged") {
        Failure::Damaged
    } else {
        match last.strip_prefix("qpdf: ") {
            Some(message) if !message.is_empty() => Failure::Other(message.to_string()),
            _ => Failure::Other(t!(FailureQpdfSilent).into()),
        }
    };
    Verdict::Failed(failure)
}

/// Whether a file-level `WARNING: <file>: …` line ends with `ending`. Lines
/// about single objects (`WARNING: <file> (object …): …`) quote text from
/// inside the PDF, so they are not looked at.
fn warning_ends_with(stderr: &Captured, ending: &str) -> bool {
    warnings(stderr).any(|line| line.ends_with(ending) && !line.contains(" (object "))
}

/// QPDF's warnings, for the details of "Done with warnings" and the report.
pub fn warnings(stderr: &Captured) -> impl Iterator<Item = &str> {
    stderr.lines.iter().map(String::as_str).filter(|line| is_warning(line))
}

/// Whether `line` is one of QPDF's warnings.
pub fn is_warning(line: &str) -> bool {
    line.starts_with("WARNING: ")
}

/// A path or value QPDF cannot be given (refused before it starts).
pub fn from_arg_error(error: &ArgError) -> Failure {
    match error {
        ArgError::PathTooLong(_) => Failure::PathTooLong,
        ArgError::NotUnicode(_) | ArgError::ForbiddenCharacter => {
            Failure::Other(t!(FailurePathCharacters).into())
        }
        ArgError::NotAbsolute(_) => Failure::Other(t!(FailurePathNotFull).into()),
        ArgError::Empty => Failure::Other(t!(FailureEmptyValue).into()),
    }
}

/// QPDF could not be run at all.
pub fn from_run_error(error: &RunError) -> Failure {
    match error {
        RunError::BadArgument(e) => from_arg_error(e),
        RunError::Start(e) => Failure::Other(t!(FailureQpdfNotStarted, why = e)),
        RunError::Job(_) => Failure::Other(t!(FailureTooManyProcesses).into()),
    }
}

/// An error creating a new file or folder IN a folder: access denied or not
/// found there is the folder's fault — "open in another program" or "file
/// not found" would mislead.
pub fn from_create_error(error: &io::Error) -> Failure {
    match from_io_error(error) {
        Failure::AccessDenied => Failure::FolderNotWritable,
        Failure::NotFound => Failure::FolderMissing,
        failure => failure,
    }
}

/// A failure from our own file access (before or after QPDF runs).
pub fn from_io_error(error: &io::Error) -> Failure {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    const ERROR_HANDLE_DISK_FULL: i32 = 39;
    const ERROR_DISK_FULL: i32 = 112;
    const ERROR_FILENAME_EXCED_RANGE: i32 = 206;
    match error.raw_os_error() {
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => Failure::NotFound,
        Some(ERROR_ACCESS_DENIED) => Failure::AccessDenied,
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => Failure::InUse,
        Some(ERROR_HANDLE_DISK_FULL | ERROR_DISK_FULL) => Failure::NoDiskSpace,
        Some(ERROR_FILENAME_EXCED_RANGE) => Failure::PathTooLong,
        _ => Failure::Other(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run with `code` and these stderr lines (real QPDF 12.4.2 text).
    fn exited(code: i32, stderr: &[&str]) -> Finished {
        Finished {
            ending: Ending::Exited(code),
            stdout: Captured::default(),
            stderr: Captured { lines: stderr.iter().map(|l| l.to_string()).collect(), omitted: 0 },
            memory_limit_hit: false,
        }
    }

    #[test]
    fn the_exit_code_decides_first() {
        assert_eq!(judge(&exited(0, &[])), Verdict::Done);
        assert_eq!(judge(&exited(3, &["WARNING: a.pdf: file is damaged"])), Verdict::DoneWithWarnings);
        assert_eq!(judge(&exited(-1073741819, &[])), Verdict::Failed(Failure::Crashed));
        assert_eq!(judge(&exited(1, &[])), Verdict::Failed(Failure::Crashed));
        // Exit 0 with alarming text is still done: the text never decides.
        assert_eq!(judge(&exited(0, &["qpdf: x: invalid password"])), Verdict::Done);
    }

    #[test]
    fn real_qpdf_failures_get_their_sentences() {
        let cases: [(&[&str], Verdict); 8] = [
            (
                &[r"qpdf: C:.pdf already has attachments with the following keys: x.txt; use --replace to replace or --key to specify a different key"],
                Verdict::Failed(Failure::AttachmentExists),
            ),
            (
                &["qpdf: QPDFWriter: unable to generated a deterministic ID because the file to be written is encrypted (even though the file may not require a password)"],
                Verdict::Failed(Failure::FixedIdEncrypted),
            ),
            (&["qpdf: enc.pdf: invalid password"], Verdict::PasswordRequired),
            (
                &[r"qpdf: open C:\x\locked.pdf: Permission denied"],
                Verdict::Failed(Failure::AccessDenied),
            ),
            (&[r"qpdf: open C:\x\missing.pdf: No such file or directory"], Verdict::Failed(Failure::NotFound)),
            (
                &[
                    "WARNING: notpdf.pdf: can't find PDF header",
                    "WARNING: notpdf.pdf: file is damaged",
                    "WARNING: notpdf.pdf: can't find startxref",
                    "WARNING: notpdf.pdf: Attempting to reconstruct cross-reference table",
                    "qpdf: notpdf.pdf: unable to find trailer dictionary while recovering damaged file",
                ],
                Verdict::Failed(Failure::NotPdf),
            ),
            (
                &[
                    "WARNING: trunc.pdf: file is damaged",
                    "WARNING: trunc.pdf: can't find startxref",
                    "WARNING: trunc.pdf (object 1711 0, offset 300000): EOF after endobj",
                    "qpdf: trunc.pdf: unable to find trailer dictionary while recovering damaged file",
                ],
                Verdict::Failed(Failure::Damaged),
            ),
            (
                &[
                    "qpdf: refusing to write a file with RC4, a weak cryptographic algorithm",
                    "qpdf: refusing to write a file with weak crypto",
                ],
                Verdict::Failed(Failure::Other("refusing to write a file with weak crypto".into())),
            ),
        ];
        for (stderr, expected) in cases {
            assert_eq!(judge(&exited(2, stderr)), expected, "{stderr:?}");
        }
    }

    #[test]
    fn names_from_the_pdf_or_the_path_do_not_decide() {
        // The echoed path imitates the "invalid password" message, but QPDF's
        // own verdict at the end of the line is something else.
        let run = exited(2, &[r"qpdf: open C:\x: invalid password.pdf: No such file or directory"]);
        assert_eq!(judge(&run), Verdict::Failed(Failure::NotFound));
        // An object-level warning quotes PDF content: not taken for a header
        // problem.
        let run = exited(
            2,
            &[
                "WARNING: a.pdf (object 5 0, offset 9): can't find PDF header",
                "qpdf: a.pdf: something else",
            ],
        );
        assert_eq!(judge(&run), Verdict::Failed(Failure::Other("a.pdf: something else".into())));
    }

    #[test]
    fn a_silent_failure_still_says_something() {
        assert_eq!(
            judge(&exited(2, &[])),
            Verdict::Failed(Failure::Other("QPDF failed without a message".into()))
        );
    }

    #[test]
    fn the_memory_limit_overrides_everything() {
        for code in [0, 2, 3, -1073741571] {
            let mut run = exited(code, &["qpdf: x.pdf: unable to find trailer dictionary while recovering damaged file"]);
            run.memory_limit_hit = true;
            assert_eq!(judge(&run), Verdict::Failed(Failure::MemoryLimit), "exit {code}");
        }
    }

    #[test]
    fn watchdog_and_cancel_have_their_own_verdicts() {
        let mut run = exited(0, &[]);
        run.ending = Ending::Hung;
        assert_eq!(judge(&run), Verdict::Failed(Failure::Hung));
        run.ending = Ending::Cancelled;
        assert_eq!(judge(&run), Verdict::Cancelled);
    }

    #[test]
    fn statuses_read_as_shown() {
        assert_eq!(Verdict::DoneWithWarnings.status(), "Done with warnings");
        assert_eq!(Verdict::Failed(Failure::PathTooLong).status(), "Failed: Path too long for QPDF");
        assert_eq!(Verdict::Failed(Failure::Other("x".into())).status(), "Failed: x");
        assert!(Verdict::DoneWithWarnings.succeeded() && !Verdict::Cancelled.succeeded());
        assert_eq!(Verdict::NotSmaller.status(), "Not smaller (kept original)");
        assert!(!Verdict::NotSmaller.succeeded() && Verdict::NotSmaller.settled());
        assert!(!Verdict::PasswordRequired.settled() && !Verdict::Cancelled.settled());
    }

    #[test]
    fn warnings_are_listed_for_the_details() {
        let run = exited(3, &["WARNING: a.pdf: file is damaged", "qpdf: operation succeeded with warnings"]);
        assert_eq!(warnings(&run.stderr).collect::<Vec<_>>(), ["WARNING: a.pdf: file is damaged"]);
    }

    #[test]
    fn io_errors_map_to_failures() {
        let os = io::Error::from_raw_os_error;
        assert_eq!(from_io_error(&os(32)), Failure::InUse);
        assert_eq!(from_io_error(&os(5)), Failure::AccessDenied);
        assert_eq!(from_io_error(&os(2)), Failure::NotFound);
        assert_eq!(from_io_error(&os(112)), Failure::NoDiskSpace);
        assert_eq!(from_io_error(&os(206)), Failure::PathTooLong);
        assert!(matches!(from_io_error(&os(1234)), Failure::Other(_)));
        assert_eq!(from_create_error(&os(5)), Failure::FolderNotWritable, "creating in a folder: the folder's fault");
        assert_eq!(from_create_error(&os(112)), Failure::NoDiskSpace);
        assert_eq!(from_create_error(&os(3)), Failure::FolderMissing, "a folder deleted during a batch, not a missing file");
    }

    #[test]
    fn random_messages_never_overrule_the_exit_code() {
        const LINES: [&str; 8] = [
            "qpdf: a.pdf: invalid password",
            "WARNING: a.pdf: file is damaged",
            "WARNING: a.pdf: can't find PDF header",
            "qpdf: a.pdf: Permission denied",
            "qpdf: operation succeeded with warnings",
            "WARNING: a.pdf (object 1 0): file is damaged",
            "qpdf: ",
            "",
        ];
        let alphabet = crate::fuzz::chars("qpdf: WARNING a.pdf(object)");
        let mut rng = crate::fuzz::Rng::new(0x5EED_000B);
        for _ in 0..20_000 {
            let lines: Vec<String> =
                (0..rng.below(5)).map(|_| if rng.chance(2) { rng.pick(&LINES).to_string() } else { rng.text(&alphabet, 40) }).collect();
            let code = *rng.pick(&[0, 2, 3, 1, -1073741571, 255]);
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let mut run = exited(code, &refs);
            run.memory_limit_hit = rng.chance(10);
            let verdict = judge(&run);
            let expected = match (run.memory_limit_hit, code) {
                (true, _) => Some(Verdict::Failed(Failure::MemoryLimit)),
                (_, 0) => Some(Verdict::Done),
                (_, 3) => Some(Verdict::DoneWithWarnings),
                (_, 2) => None,
                _ => Some(Verdict::Failed(Failure::Crashed)),
            };
            match expected {
                Some(expected) => assert_eq!(verdict, expected, "{code} {lines:?}"),
                None => assert!(!verdict.succeeded() && !verdict.status().is_empty(), "{lines:?}"),
            }
        }
    }
}
