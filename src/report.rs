//! The Check report: a plain text file, one section per checked file,
//! with QPDF's own lines. It holds paths and QPDF's words only — never a
//! password. Lines end with CRLF, so Notepad of any Windows
//! version shows them.

use crate::text::{self, Msg};
use crate::{t, tn};
use std::path::Path;

use crate::core::inspect::CheckReport;
use crate::core::printable;

/// One checked file of the report.
pub struct Checked<'a> {
    pub path: &'a Path,
    pub report: &'a CheckReport,
}

/// The report on `files`, in list order; `made` is when, `qpdf` which QPDF.
pub fn check_report(files: &[Checked], made: &str, qpdf: &str) -> String {
    let mut lines = vec![
        format!("QpdfDesk {} — check report", env!("CARGO_PKG_VERSION")),
        format!("Made {made} with QPDF {}", printable(qpdf)),
        tn!(ReportFiles, files.len(), tally = tally_text(files.iter().map(|file| file.report))),
        String::new(),
    ];
    for (number, file) in files.iter().enumerate() {
        lines.push(format!("{}. {}", number + 1, printable(&file.path.display().to_string())));
        lines.push(format!("   {}", file.report.health()));
        lines.extend(file.report.details.iter().map(|line| format!("     {line}")));
        lines.push(String::new());
    }
    let mut text = lines.join("\r\n");
    text.push_str("\r\n");
    text
}

/// [`tally`] as text: `OK 6 · Warnings 1`.
pub fn tally_text<'a>(reports: impl Iterator<Item = &'a CheckReport>) -> String {
    counts_text(tally(reports).into_iter().map(|(kind, n)| (text::get(kind), n)))
}

/// Counts as one line: `Done 6 · Warnings 1`.
pub fn counts_text<'a>(counts: impl IntoIterator<Item = (&'a str, usize)>) -> String {
    counts.into_iter().map(|(name, count)| t!(TallyCount, name = name, count = count)).collect::<Vec<_>>().join(" · ")
}

/// Each [`CheckReport::kind`] (`OK`, `Warnings`, `Damaged`…) and how many
/// files have it, in order of first appearance.
fn tally<'a>(reports: impl Iterator<Item = &'a CheckReport>) -> Vec<(Msg, usize)> {
    let mut counts: Vec<(Msg, usize)> = Vec::new();
    for report in reports {
        let kind = report.kind();
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, count)) => *count += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outcome::{Failure, Verdict};

    fn report(verdict: Verdict, details: &[&str]) -> CheckReport {
        CheckReport { verdict, details: details.iter().map(|l| l.to_string()).collect() }
    }

    #[test]
    fn the_report_lists_every_file_with_qpdfs_lines() {
        let ok = report(Verdict::Done, &["checking a.pdf", "PDF Version: 1.7", "File is not encrypted"]);
        let damaged = report(Verdict::DoneWithWarnings, &["WARNING: b.pdf: file is damaged"]);
        let locked = report(Verdict::Failed(Failure::InUse), &[]);
        let files = [
            Checked { path: Path::new(r"C:\Docs\a.pdf"), report: &ok },
            Checked { path: Path::new(r"C:\Docs\b.pdf"), report: &damaged },
            Checked { path: Path::new(r"C:\Docs\c.pdf"), report: &locked },
        ];
        let text = check_report(&files, "2026-10-10 17:45", r"12.4.2 (C:\q\qpdf.exe)");
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert!(lines[0].starts_with("QpdfDesk ") && lines[0].ends_with(" — check report"));
        assert_eq!(lines[1], r"Made 2026-10-10 17:45 with QPDF 12.4.2 (C:\q\qpdf.exe)");
        assert_eq!(lines[2], "3 files: OK 1 · Warnings 1 · Unreadable 1");
        assert_eq!(&lines[4..9], [r"1. C:\Docs\a.pdf", "   OK", "     checking a.pdf", "     PDF Version: 1.7", "     File is not encrypted"]);
        assert!(text.contains("\r\n   Unreadable: File is in use by another program\r\n"));
        assert!(text.ends_with("\r\n") && !text.contains("\n\n"), "CRLF only");
    }
}
