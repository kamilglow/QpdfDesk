//! The generated corpus is what it claims to be — checked with the real QPDF,
//! both builds.

mod common;

use common::corpus::{self, Item, OWNER_PASSWORD, USER_PASSWORD};
use common::run_qpdf as qpdf;

#[test]
fn every_item_behaves_as_documented() {
    for exe in common::qpdf_builds() {
        let get = corpus::item;
        let npages = |item, extra: &[&str]| {
            let mut args = extra.to_vec();
            args.push("--show-npages");
            qpdf(&exe, &args, &get(item))
        };
        for item in [Item::Base, Item::Attachments, Item::DashName, Item::AtName, Item::UnicodeName] {
            let (code, out, err) = npages(item, &[]);
            assert_eq!((code, out.trim()), (0, "3"), "{item:?} with {exe:?}: {err}");
            let (code, _, err) = qpdf(&exe, &["--check"], &get(item));
            assert_eq!(code, 0, "{item:?} must be a clean PDF: {err}");
        }
        assert_eq!(npages(Item::Pages10k, &[]).1.trim(), "10000");
        assert_eq!(npages(Item::Heavy, &[]).1.trim(), "400");
        assert_eq!(npages(Item::Manual, &[]).1.trim(), "178");
        assert_eq!(npages(Item::Photo, &[]).1.trim(), "1");
        assert_eq!(qpdf(&exe, &["--check"], &get(Item::Photo)).0, 0);

        let (code, _, err) = qpdf(&exe, &["--check"], &get(Item::Damaged));
        assert_eq!(code, 3, "damaged is repairable: {err}");
        for item in [Item::Truncated, Item::NotPdf, Item::Empty] {
            let (code, _, err) = qpdf(&exe, &["--check"], &get(item));
            assert_eq!(code, 2, "{item:?} is beyond repair: {err}");
        }

        // Encrypted: open password required / not required.
        let user = format!("--password={USER_PASSWORD}");
        let owner = format!("--password={OWNER_PASSWORD}");
        assert_eq!(qpdf(&exe, &["--requires-password"], &get(Item::EncryptedOpen)).0, 0);
        assert_eq!(qpdf(&exe, &["--requires-password"], &get(Item::EncryptedOwnerOnly)).0, 3);
        assert_eq!(qpdf(&exe, &["--requires-password"], &get(Item::Base)).0, 2);
        assert_eq!(npages(Item::EncryptedOpen, &[]).0, 2, "no password, no pages");
        assert_eq!(npages(Item::EncryptedOpen, &[&user]).1.trim(), "3");
        assert_eq!(npages(Item::EncryptedOpen, &[&owner]).1.trim(), "3");
        assert_eq!(npages(Item::EncryptedOwnerOnly, &[]).1.trim(), "3");
        assert_eq!(npages(Item::EncryptedRc4, &[&user]).1.trim(), "3");
        let (_, out, _) = qpdf(&exe, &["--show-encryption"], &get(Item::EncryptedOwnerOnly));
        assert!(out.contains("print low resolution: not allowed"), "{out}");
    }
}

#[test]
fn hostile_attachment_names_are_stored_verbatim() {
    let Some(exe) = common::qpdf() else { return };
    let (code, out, err) = qpdf(&exe, &["--list-attachments", "--verbose"], &corpus::item(Item::Attachments));
    assert_eq!(code, 0, "{err}");
    for name in corpus::HOSTILE_ATTACHMENT_NAMES {
        assert!(out.contains(name), "{name:?} missing from:\n{out}");
    }
    assert!(out.contains(&corpus::long_attachment_name()));
}

#[test]
fn the_long_path_is_beyond_qpdf() {
    let Some(exe) = common::qpdf() else { return };
    let path = corpus::item(Item::LongPath);
    assert!(path.as_os_str().len() > 259, "{}", path.as_os_str().len());
    assert!(std::fs::metadata(&path).is_ok(), "Windows itself can reach it");
    // QPDF 12.4.2 cannot open it — the reason for the
    // up-front length check in `runner::check_path`.
    assert_eq!(qpdf(&exe, &["--check"], &path).0, 2);
}
