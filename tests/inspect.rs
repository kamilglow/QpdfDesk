//! The read-only tools with the real QPDF: Info, Check and
//! pulling attachments out — hostile attachment names never leave
//! their folder.

mod common;

use common::corpus::{self, item, Item, HOSTILE_ATTACHMENT_NAMES, USER_PASSWORD};
use common::{job_object, secret};
use qpdf_desk::core::attachments::{self, Extracted};
use qpdf_desk::core::filelist::Probe;
use qpdf_desk::core::inspect::{self, Info};
use qpdf_desk::core::outcome::{Failure, Verdict};
use qpdf_desk::core::runner::Context;
use qpdf_desk::core::sanitize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn info(exe: &Path, file: &Path, password: Option<&str>) -> Result<Info, Verdict> {
    let (job, cancel) = (job_object(), AtomicBool::new(false));
    inspect::inspect(exe, file, &Probe::Pending, password.map(secret).as_ref(), &Context::new(&job, &cancel))
}

#[test]
fn info_tells_what_a_file_holds() {
    for exe in common::qpdf_builds() {
        let found = info(&exe, &item(Item::Attachments), None).expect("readable");
        assert_eq!(found.pdf_version.as_deref(), Some("1.7"));
        assert_eq!(found.pages, Some(3));
        assert_eq!(found.linearized, Some(false));
        assert_eq!(found.encryption, None);
        assert!(found.form && found.bookmarks);
        let labels: Vec<String> = (0..3).filter_map(|i| inspect::label(&found.labels, i)).collect();
        assert_eq!(labels, ["i", "ii", "1"]);
        assert_eq!(found.attachments.len(), HOSTILE_ATTACHMENT_NAMES.len() + 1);
        assert_eq!(found.attachments[0].name, r"..\..\outside.txt", "the PDF's own name, made safe only for disk");
        assert!(!found.needed_password);

        let manual = info(&exe, &item(Item::Manual), None).expect("readable");
        assert_eq!(manual.pages, Some(178));
        assert!(manual.bookmarks && !manual.form && manual.attachments.is_empty());
        let meta = |key: &str| manual.metadata.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(meta("Title").as_deref(), Some("qpdf"));
        assert_eq!(meta("Author").as_deref(), Some("Jay Berkenbilt"));
        assert!(meta("CreationDate").is_some_and(|date| date.starts_with("20") && date.contains(' ')), "{:?}", meta("CreationDate"));
    }
}

#[test]
fn info_on_encrypted_files_uses_the_password_only_when_asked() {
    let Some(exe) = common::qpdf() else { return };
    let locked = item(Item::EncryptedOpen);
    assert_eq!(info(&exe, &locked, None), Err(Verdict::PasswordRequired));
    assert_eq!(info(&exe, &locked, Some("wrong")), Err(Verdict::PasswordRequired));
    let opened = info(&exe, &locked, Some(USER_PASSWORD)).expect("opens");
    assert!(opened.needed_password);
    assert_eq!(opened.pages, Some(3));
    assert_eq!(opened.encryption.map(|e| e.method), Some("AES-256".to_string()));
    // Restrictions only: a wrong password must not spoil it.
    let restricted = info(&exe, &item(Item::EncryptedOwnerOnly), Some("wrong")).expect("opens");
    assert!(!restricted.needed_password);
    let encryption = restricted.encryption.expect("encrypted");
    assert!(encryption.not_allowed.contains(&"printing"), "{:?}", encryption.not_allowed);
    let weak = info(&exe, &item(Item::EncryptedRc4), Some(USER_PASSWORD)).expect("opens");
    assert!(weak.encryption.is_some_and(|e| e.method.starts_with("RC4")));
}

#[test]
fn info_on_files_qpdf_cannot_read_says_why() {
    let Some(exe) = common::qpdf() else { return };
    assert_eq!(info(&exe, &item(Item::NotPdf), None), Err(Verdict::Failed(Failure::NotPdf)));
    assert_eq!(info(&exe, &item(Item::Truncated), None), Err(Verdict::Failed(Failure::Damaged)));
    // Repaired with warnings: still readable.
    assert_eq!(info(&exe, &item(Item::Damaged), None).map(|i| i.pages), Ok(Some(3)));
    // 10 000 pages: the keys asked for stay small.
    assert_eq!(info(&exe, &item(Item::Pages10k), None).map(|i| i.pages), Ok(Some(10_000)));
}

#[test]
fn check_gives_each_file_its_verdict() {
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job_object(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let check = |file: Item, password: Option<&str>| inspect::check(&exe, &item(file), &Probe::Pending, password.map(secret).as_ref(), &ctx);
    let ok = check(Item::Base, None);
    assert_eq!(ok.health(), "OK");
    assert!(ok.details.iter().any(|line| line == "File is not linearized"), "{:?}", ok.details);
    let cases = [
        (Item::Damaged, "Warnings"),
        (Item::Truncated, "Damaged"),
        (Item::NotPdf, "Not a PDF"),
        (Item::Empty, "Not a PDF"),
        (Item::EncryptedOpen, "Password required"),
        (Item::EncryptedOwnerOnly, "OK"),
    ];
    for (file, health) in cases {
        assert_eq!(check(file, None).health(), health, "{file:?}");
    }
    assert_eq!(check(Item::EncryptedOpen, Some(USER_PASSWORD)).health(), "OK");
    assert_eq!(check(Item::EncryptedOwnerOnly, Some("wrong")).health(), "OK", "the password only when asked");
    assert!(check(Item::Damaged, None).details.iter().any(|line| qpdf_desk::core::outcome::is_warning(line)));
    // The probe knew it needs one: the password goes at once.
    let known = inspect::check(&exe, &item(Item::EncryptedOpen), &Probe::NeedsPassword, Some(&secret(USER_PASSWORD)), &ctx);
    assert_eq!(known.health(), "OK");
}

/// A copy of the hostile-attachments file in a fresh folder.
fn hostile_copy(name: &str) -> PathBuf {
    common::copy_item(Item::Attachments, &common::scratch(name), "hostile.pdf").0
}

fn extract_all(exe: &Path, pdf: &Path) -> Extracted {
    let (job, cancel) = (job_object(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let found = inspect::inspect(exe, pdf, &Probe::Pending, None, &ctx).expect("readable");
    attachments::extract(exe, pdf, None, &found.attachments, &ctx)
}

#[test]
fn hostile_attachment_names_never_leave_their_folder() {
    for exe in common::qpdf_builds() {
        let pdf = hostile_copy("extract");
        let dir = pdf.parent().expect("dir").to_path_buf();
        let done = extract_all(&exe, &pdf);
        let folder = done.folder.clone().expect("a folder");
        assert_eq!(folder, dir.join("hostile (attachments)"));
        assert_eq!(done.saved(), HOSTILE_ATTACHMENT_NAMES.len() + 1, "{:?}", done.files);
        assert_eq!(done.unmarked, 0, "NTFS takes the Mark-of-the-Web");
        // Nothing but the PDF and the folder next to it; nothing anywhere
        // above it (`..\..\outside.txt`) or in Windows.
        let mut beside = common::others(&dir, &[&pdf]);
        beside.sort();
        assert_eq!(beside, ["hostile (attachments)"]);
        assert!(!common::scratch_path("outside.txt").exists() && !common::project_dir().join("target").join("outside.txt").exists());
        let names: Vec<String> = done.files.iter().map(|saved| saved.file.as_ref().expect("saved").file_name().expect("name").to_string_lossy().into_owned()).collect();
        for (i, (saved, safe)) in done.files.iter().zip(&names).enumerate() {
            let path = saved.file.as_ref().expect("saved");
            assert_eq!(path.parent(), Some(folder.as_path()), "{safe}");
            // The contents byte for byte, as the corpus wrote them.
            let original = HOSTILE_ATTACHMENT_NAMES.get(i).map_or_else(corpus::long_attachment_name, |n| n.to_string());
            assert_eq!(std::fs::read_to_string(path).expect("read"), format!("attachment {i}: {original}\r\n"), "{safe}");
            let zone = std::fs::read_to_string(format!("{}:Zone.Identifier", path.display())).expect("marked");
            assert!(zone.contains("ZoneId=3"), "{safe}");
        }
        assert_eq!(
            &names[..9],
            ["outside.txt", "outside2.txt", "evil.txt", "_CON", "_nul.txt", "a_b.txt", "trailing dot", "zażółć.txt", "invoice_txt.exe"]
        );
        assert!(names[9].ends_with(".txt") && names[9].encode_utf16().count() <= 100);
        // Again: a new folder; the first is left as it was.
        let again = extract_all(&exe, &pdf);
        assert_eq!(again.folder, Some(dir.join("hostile (attachments) (2)")));
        assert_eq!(std::fs::read_dir(&folder).expect("first").count(), names.len());
    }
}

#[test]
fn a_file_without_attachments_leaves_no_folder() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("extract-none");
    let (pdf, _) = common::copy_item(Item::Base, &dir, "plain.pdf");
    let (job, cancel) = (job_object(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    // Asked for one it does not have: QPDF says so, and the empty folder goes.
    let ghost = inspect::Attachment { key: "nope".into(), name: "ghost.txt".into(), description: None };
    let done = attachments::extract(&exe, &pdf, None, &[ghost], &ctx);
    assert_eq!(done.folder, None);
    assert!(matches!(&done.files[0].file, Err(Failure::Other(message)) if message == "attachment nope not found"), "{:?}", done.files);
    assert_eq!(common::others(&dir, &[&pdf]), Vec::<String>::new(), "no folder, no temporary file");
}

#[test]
fn every_safe_name_stays_a_single_file_name() {
    // The rule the extraction relies on, over every hostile name.
    for name in HOSTILE_ATTACHMENT_NAMES.iter().map(|n| n.to_string()).chain([corpus::long_attachment_name()]) {
        let safe = sanitize::file_name(&name);
        assert_eq!(Path::new(&safe).file_name().map(|n| n.to_string_lossy().into_owned()), Some(safe.clone()), "{name:?}");
        assert!(!safe.contains(['\\', '/', ':']) && !safe.ends_with(['.', ' ']), "{name:?} → {safe:?}");
    }
}
