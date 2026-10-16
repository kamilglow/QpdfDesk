//! Pulling attachments out of a PDF — the one read-only tool that
//! writes files.
//!
//! They go into a new folder `<name> (attachments)` next to the PDF (` (2)`…
//! when the name is taken), each under a safe name ([`sanitize::file_name`]):
//! whatever the PDF calls them, nothing lands outside that folder. QPDF
//! writes each one (`--show-attachment`, byte for byte — measured) into a
//! temporary file, which gets the Mark-of-the-Web and only then its name,
//! never over an existing file. Nothing is ever opened. A tiny stream can
//! unpack into gigabytes, so the copy stops before it takes the disk's last
//! [`DISK_RESERVE`] bytes.

use crate::t;
use std::path::{Path, PathBuf};

use super::inspect::Attachment;
use super::outcome::{self, Failure, Verdict};
use super::output::{self, TempFile};
use super::runner::{self, Arg, Context};
use super::sanitize;
use super::secret::Secret;
use crate::platform;

/// Free space an extraction never takes.
pub const DISK_RESERVE: u64 = 64 << 20;

/// What came of an extraction.
#[derive(Debug, Default)]
pub struct Extracted {
    /// The new folder (removed again when nothing landed in it).
    pub folder: Option<PathBuf>,
    /// Each attachment asked for, in the order asked.
    pub files: Vec<Saved>,
    /// Files the drive could not mark as coming from the Internet (FAT).
    pub unmarked: usize,
}

/// One attachment asked for.
#[derive(Debug)]
pub struct Saved {
    /// Its key in the PDF — unique, unlike its name.
    pub key: String,
    /// The file it became, or why not.
    pub file: Result<PathBuf, Failure>,
}

impl Extracted {
    pub fn saved(&self) -> usize {
        self.files.iter().filter(|saved| saved.file.is_ok()).count()
    }
}

/// `<stem> (attachments)` next to `pdf`.
pub fn folder_for(pdf: &Path) -> Option<PathBuf> {
    let stem = pdf.file_stem()?.to_string_lossy();
    Some(pdf.parent()?.join(format!("{stem} ({})", t!(TagAttachmentsFolder))))
}

/// Write `attachments` of `pdf` into a new folder next to it. `password` is
/// given to QPDF only when the file needs it.
pub fn extract(qpdf: &Path, pdf: &Path, password: Option<&Secret>, attachments: &[Attachment], ctx: &Context) -> Extracted {
    let all = |failure: Failure| attachments.iter().map(|a| Saved { key: a.key.clone(), file: Err(failure.clone()) }).collect();
    let folder = match folder_for(pdf).ok_or_else(|| Failure::Other(t!(FailureNoFolder).into())).and_then(|desired| {
        output::first_free(&desired, true, |folder| std::fs::create_dir(folder)).map_err(|e| outcome::from_create_error(&e))
    }) {
        Ok(folder) => folder,
        Err(failure) => return Extracted { files: all(failure), ..Extracted::default() },
    };
    let mut done = Extracted::default();
    let mut cancelled = false;
    for attachment in attachments {
        let file = if cancelled {
            Err(Verdict::Cancelled)
        } else {
            save(qpdf, pdf, password, attachment, &folder.join(sanitize::file_name(&attachment.name)), ctx)
        };
        cancelled |= file == Err(Verdict::Cancelled);
        let file = match file {
            Ok((path, marked)) => {
                done.unmarked += usize::from(!marked);
                Ok(path)
            }
            Err(Verdict::Failed(failure)) => Err(failure),
            Err(verdict) => Err(Failure::Other(verdict.status())),
        };
        done.files.push(Saved { key: attachment.key.clone(), file });
    }
    // Only an empty folder can be removed: one with files stays.
    if std::fs::remove_dir(&folder).is_err() {
        done.folder = Some(folder);
    }
    done
}

/// One attachment into `target` (or `target (2)`…): its final path, and
/// whether it carries the Mark-of-the-Web.
fn save(
    qpdf: &Path,
    pdf: &Path,
    password: Option<&Secret>,
    attachment: &Attachment,
    target: &Path,
    ctx: &Context,
) -> Result<(PathBuf, bool), Verdict> {
    // The key travels as one `@-` line; the runner would
    // refuse it too, but speak of a path.
    if attachment.key.contains(['\n', '\r', '\0']) {
        return Err(Verdict::Failed(Failure::Other(t!(FailureAttachmentKey).into())));
    }
    let (Some(dir), Some(stem)) = (target.parent(), target.file_name()) else {
        return Err(Verdict::Failed(Failure::Other(t!(FailureNoFileName).into())));
    };
    let temp = TempFile::create(dir, &stem.to_string_lossy(), false).map_err(|e| Verdict::Failed(outcome::from_create_error(&e)))?;
    let mut file = std::fs::OpenOptions::new().write(true).open(temp.path()).map_err(|e| Verdict::Failed(outcome::from_io_error(&e)))?;
    let room = platform::free_disk_space(dir).map_or(u64::MAX, |free| free.saturating_sub(DISK_RESERVE));
    let args = Arg::on_file(password, &[&format!("--show-attachment={}", attachment.key)], pdf);
    let (finished, copied) = runner::run_into(qpdf, &args, ctx, &mut file, room)?;
    drop(file);
    if copied.over_limit {
        return Err(Verdict::Failed(Failure::NoDiskSpace));
    }
    if let Some(error) = &copied.error {
        return Err(Verdict::Failed(outcome::from_io_error(error)));
    }
    let verdict = outcome::judge(&finished);
    if !verdict.succeeded() {
        return Err(verdict);
    }
    let marked = platform::mark_of_the_web(temp.path()).is_ok();
    let path = output::commit(temp, target).map_err(|e| Verdict::Failed(outcome::from_io_error(&e)))?;
    Ok((path, marked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_named_after_the_pdf() {
        assert_eq!(folder_for(Path::new(r"C:\Docs\umowa.v2.pdf")), Some(PathBuf::from(r"C:\Docs\umowa.v2 (attachments)")));
        assert_eq!(folder_for(Path::new(r"C:\")), None);
    }
}
