//! Every step with the real QPDF, its result checked by QPDF itself —
//! pages, merging, overlays, forms, metadata, images, encryption, output;
//! a merge follows the order of the list.

mod common;

use common::corpus::{Item, OWNER_PASSWORD, PHOTO_AUTHOR, USER_PASSWORD};
use common::{probe, range, run_qpdf as qpdf, secret};
use qpdf_desk::core::filelist::Probe;
use qpdf_desk::core::job::{
    Angle, Annotations, Attachments, Encrypt, Files, FormAction, Forms, Images, Input, Job, ObjectStreams, Optimize, Output, Overlay, Pages, Placement,
    Print, Privacy, Protect, Rotate, Select,
};
use qpdf_desk::core::outcome::{Failure, Verdict};
use qpdf_desk::core::ranges::PageRange;
use qpdf_desk::core::output::Produced;
use qpdf_desk::core::runner::Context;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Run `job` on a copy of `item` in a fresh folder `name`; the probe result
/// is given as the app would have it.
fn run(exe: &Path, job: &Job, item: Item, probe: Probe, name: &str) -> Produced {
    let (input, _) = common::copy_item(item, &common::scratch(name), "in.pdf");
    run_on(exe, job, &input, probe)
}

/// Run `job` on the file `input` itself.
fn run_on(exe: &Path, job: &Job, input: &Path, probe: Probe) -> Produced {
    assert_eq!(job.check(), Ok(()), "{}", job.describe());
    let size = std::fs::metadata(input).expect("size").len();
    let (job_object, cancel) = (common::job_object(), AtomicBool::new(false));
    let ctx = Context::new(&job_object, &cancel);
    job.produce(exe, &[Input { path: input, size, probe: &probe, range: None, attachments: None }], &ctx).remove(0)
}

/// The result, which must exist.
fn output(done: &Produced) -> PathBuf {
    assert!(done.verdict.succeeded(), "{:?}", done);
    done.output.clone().expect("a result")
}

/// `file` rewritten by QPDF in QDF form (streams uncompressed, objects in
/// page order) — text to search.
fn qdf(exe: &Path, file: &Path, extra: &[&str]) -> String {
    let out = file.with_extension("qdf");
    let mut args = extra.to_vec();
    args.extend(["--qdf", "--object-streams=disable", "--decrypt"]);
    assert_eq!(common::qpdf_write(exe, &args, file, &out), Some(0), "qdf of {}", file.display());
    String::from_utf8_lossy(&std::fs::read(&out).expect("read")).into_owned()
}

/// `file` in QDF form, cut into its pages: `(page number, the text from its
/// `%% Contents for page <n>` comment to the next one)`, in page order. QDF
/// writes the pages in object order, so only the comments tell (measured).
fn page_sections(exe: &Path, file: &Path, extra: &[&str]) -> Vec<(u32, String)> {
    let text = qdf(exe, file, extra);
    let mut sections: Vec<(u32, String)> = text
        .split("%% Contents for page ")
        .skip(1)
        .filter_map(|section| Some((section.split_whitespace().next()?.parse().ok()?, section.to_string())))
        .collect();
    sections.sort_unstable();
    sections
}

/// Which "Page N" of [`Item::Base`] each page of `file` shows, in order.
fn page_order(exe: &Path, file: &Path, extra: &[&str]) -> Vec<u32> {
    page_sections(exe, file, extra)
        .iter()
        .filter_map(|(_, section)| section.split("(Page ").nth(1)?.split(')').next()?.parse().ok())
        .collect()
}

/// Whether QPDF calls `file` linearized (12.4.2 has no `--is-linearized`).
fn linearized(exe: &Path, file: &Path, extra: &[&str]) -> bool {
    let mut args = extra.to_vec();
    args.push("--check");
    qpdf(exe, &args, file).1.lines().any(|line| line == "File is linearized")
}

fn pages_job(select: Select, rotate: Option<Rotate>) -> Job {
    Job { pages: Some(Pages { select, files: Files::Each, rotate }), ..Job::default() }
}

/// Every file into one result: `files` Merge or Collate.
fn merge_job(files: Files, select: Select) -> Job {
    Job { pages: Some(Pages { select, files, rotate: None }), ..Job::default() }
}

#[test]
fn pages_are_kept_deleted_reversed_and_rotated() {
    for exe in common::qpdf_builds() {
        let cases = [
            (Select::Keep(range("3,1")), vec![3, 1]),
            (Select::Delete(range("2").complement().expect("deletable")), vec![1, 3]),
            (Select::Reverse, vec![3, 2, 1]),
            (Select::Keep(range("1-z:odd")), vec![1, 3]),
        ];
        for (select, expected) in cases {
            let done = run(&exe, &pages_job(select, None), Item::Base, common::BASE, "pages");
            assert_eq!(page_order(&exe, &output(&done), &[]), expected);
        }
        let rotated = pages_job(Select::All, Some(Rotate { angle: Angle::Right, pages: Some(range("1-2")) }));
        let text = qdf(&exe, &output(&run(&exe, &rotated, Item::Base, common::BASE, "rotate")), &[]);
        assert_eq!(text.matches("/Rotate 90").count(), 2, "pages 1 and 2 only");
    }
}

#[test]
fn page_numbers_beyond_the_file_fail_cleanly() {
    let Some(exe) = common::qpdf() else { return };
    // Known page count: refused before QPDF runs.
    let done = run(&exe, &pages_job(Select::Keep(range("5")), None), Item::Base, common::BASE, "pages-beyond");
    assert!(matches!(&done.verdict, Verdict::Failed(Failure::Other(m)) if m.contains("page 5 does not exist")), "{done:?}");
    // Unknown (the probe has not finished): QPDF refuses it itself.
    let done = run(&exe, &pages_job(Select::Keep(range("5")), None), Item::Base, Probe::Pending, "pages-beyond-2");
    assert!(matches!(done.verdict, Verdict::Failed(_)), "{done:?}");
    assert_eq!(common::others(&common::scratch_path("pages-beyond-2"), &[]), ["in.pdf"]);
}

#[test]
fn forms_are_flattened_or_removed() {
    for exe in common::qpdf_builds() {
        let flatten = Job { forms: Some(Forms { action: FormAction::Flatten(Annotations::All), flatten_rotation: false }), ..Job::default() };
        let text = qdf(&exe, &output(&run(&exe, &flatten, Item::Base, common::BASE, "flatten")), &[]);
        assert!(!text.contains("/AcroForm") && !text.contains("/Widget"), "no form, no field left");
        assert!(text.contains("hello"), "the field's value is now part of the page");
        let remove = Job { forms: Some(Forms { action: FormAction::RemoveForm, flatten_rotation: false }), ..Job::default() };
        let done = run(&exe, &remove, Item::Base, common::BASE, "remove-form");
        assert_eq!(done.verdict, Verdict::Done, "{done:?}");
        let text = qdf(&exe, &output(&done), &[]);
        assert!(!text.contains("/AcroForm") && text.contains("/Widget"), "the field stays, the form goes");
    }
}

#[test]
fn privacy_removes_the_metadata_and_fixes_the_id() {
    let Some(exe) = common::qpdf() else { return };
    let original = qdf(&exe, &common::corpus::item(Item::Photo), &[]);
    assert_eq!(original.matches(PHOTO_AUTHOR).count(), 2, "in the info and in the XMP");
    let job = Job { privacy: Some(Privacy::default()), ..Job::default() };
    let first = output(&run(&exe, &job, Item::Photo, probe(Some(1)), "privacy"));
    assert!(!qdf(&exe, &first, &[]).contains(PHOTO_AUTHOR));
    // The same input gives the same file: the ID depends on the content only.
    let second = output(&run(&exe, &job, Item::Photo, probe(Some(1)), "privacy-2"));
    assert_eq!(std::fs::read(&first).expect("read"), std::fs::read(&second).expect("read"));
}

#[test]
fn smaller_reencodes_images_as_jpeg() {
    let Some(exe) = common::qpdf() else { return };
    let photo = probe(Some(1));
    let lossless = output(&run(&exe, &Job::optimize_lossless(), Item::Photo, photo.clone(), "lossless"));
    let smaller = Job { optimize: Some(Optimize { images: Images::JpegQuality(60), keep_if_not_smaller: true, ..Optimize::default() }), ..Job::default() };
    let smaller = output(&run(&exe, &smaller, Item::Photo, photo, "smaller"));
    let size = |path: &Path| std::fs::metadata(path).expect("size").len();
    assert!(size(&smaller) * 2 < size(&lossless), "{} vs {}", size(&smaller), size(&lossless));
    assert!(qdf(&exe, &smaller, &[]).contains("/DCTDecode"));
}

/// The stored bytes of the image `/Im1` on the first page of `file`.
fn image_bytes(exe: &Path, file: &Path) -> Vec<u8> {
    let (_, pages, _) = qpdf(exe, &["--show-pages", "--with-images"], file);
    let object = pages.split("/Im1: ").nth(1).and_then(|rest| rest.split(' ').next()).expect("an image");
    let out = std::process::Command::new(exe).arg(format!("--show-object={object}")).arg("--raw-stream-data").arg(file).output().expect("qpdf");
    out.stdout
}

#[test]
fn smaller_leaves_jpegs_alone_unless_a_quality_is_set() {
    // With `--jpeg-quality`, QPDF re-encodes the JPEGs a file has too
    // (measured: 121 183 → 38 945 bytes) — a second loss.
    let Some(exe) = common::qpdf() else { return };
    let smaller = |images| Job { optimize: Some(Optimize { images, keep_if_not_smaller: false, ..Optimize::default() }), ..Job::default() };
    let jpeg = output(&run(&exe, &smaller(Images::JpegQuality(95)), Item::Photo, probe(Some(1)), "jpeg-95"));
    let original = image_bytes(&exe, &jpeg);
    let again = |images| image_bytes(&exe, &output(&run_on(&exe, &smaller(images), &jpeg, probe(Some(1)))));
    assert!(again(Images::Jpeg) == original, "the JPEG as it was");
    assert!(again(Images::JpegQuality(75)).len() * 2 < original.len(), "compressed again");
}

#[test]
fn smaller_images_are_reencoded_once_the_minimum_allows_them() {
    // QPDF's own minimum also counts pixels (16 384): a 100×100 image stayed
    // as it was with the width and height lowered alone (measured).
    let Some(exe) = common::qpdf() else { return };
    let smaller = |min_image| Job {
        optimize: Some(Optimize { images: Images::Jpeg, keep_if_not_smaller: false, min_image, ..Optimize::default() }),
        ..Job::default()
    };
    let jpeg = |min_image, name| qdf(&exe, &output(&run(&exe, &smaller(min_image), Item::SmallPhoto, probe(Some(1)), name)), &[]).contains("/DCTDecode");
    assert!(!jpeg(qpdf_desk::core::job::MIN_IMAGE, "small-default"), "under QPDF's minimum");
    assert!(jpeg((64, 64), "small-64"), "64×64 lets a 100×100 image through");
}

fn encrypt(user: &str, owner: &str) -> Protect {
    Protect::Encrypt(Encrypt {
        user: secret(user),
        owner: secret(owner),
        print: Print::Low,
        copy: true,
        edit: false,
        annotate: true,
        fill_forms: false,
        assemble: false,
    })
}

#[test]
fn encryption_is_aes_256_with_the_chosen_permissions() {
    for exe in common::qpdf_builds() {
        let job = Job { protect: Some(encrypt("open me", "own it")), ..Job::default() };
        let done = run(&exe, &job, Item::Base, common::BASE, "encrypt");
        assert_eq!(done.verdict, Verdict::Done, "verified with the new password: {done:?}");
        let result = output(&done);
        assert_eq!(qpdf(&exe, &["--requires-password"], &result).0, 0, "needs the open password");
        let (_, info, _) = qpdf(&exe, &["--password=open me", "--show-encryption"], &result);
        for line in [
            "R = 6",
            "file encryption method: AESv3",
            "print low resolution: allowed",
            "print high resolution: not allowed",
            "extract for any purpose: allowed",
            "modify other: not allowed",
            "modify annotations: allowed",
            "modify forms: not allowed",
            "modify document assembly: not allowed",
        ] {
            assert!(info.contains(line), "{line} in:\n{info}");
        }
        // Restrictions only: opens without a password.
        let job = Job { protect: Some(encrypt("", "own it")), ..Job::default() };
        let result = output(&run(&exe, &job, Item::Base, common::BASE, "restrict"));
        assert_eq!(qpdf(&exe, &["--requires-password"], &result).0, 3);
    }
}

#[test]
fn encryption_is_removed_with_the_password_and_restrictions_without() {
    let Some(exe) = common::qpdf() else { return };
    let decrypt = Job { protect: Some(Protect::Unlock), input_password: Some(secret(USER_PASSWORD)), ..Job::default() };
    let result = output(&run(&exe, &decrypt, Item::EncryptedOpen, Probe::NeedsPassword, "decrypt"));
    assert_eq!(qpdf(&exe, &["--requires-password"], &result).0, 2, "not encrypted any more");
    // The owner password opens it too.
    let decrypt = Job { protect: Some(Protect::Unlock), input_password: Some(secret(OWNER_PASSWORD)), ..Job::default() };
    assert_eq!(run(&exe, &decrypt, Item::EncryptedOpen, Probe::NeedsPassword, "decrypt-owner").verdict, Verdict::Done);
    let without = Job { protect: Some(Protect::Unlock), ..Job::default() };
    assert_eq!(run(&exe, &without, Item::EncryptedOpen, Probe::NeedsPassword, "decrypt-none").verdict, Verdict::PasswordRequired);
    let lift = Job { protect: Some(Protect::Unlock), ..Job::default() };
    let restricted = Probe::Ready { pages: 3, restricted: true };
    let result = output(&run(&exe, &lift, Item::EncryptedOwnerOnly, restricted, "unrestrict"));
    assert_eq!(qpdf(&exe, &["--requires-password"], &result).0, 2);
}

#[test]
fn an_encrypted_input_stays_encrypted_and_is_verified_with_its_password() {
    let Some(exe) = common::qpdf() else { return };
    let job = Job { input_password: Some(secret(USER_PASSWORD)), ..Job::optimize_lossless() };
    let done = run(&exe, &job, Item::EncryptedOpen, Probe::NeedsPassword, "keep-encryption");
    assert!(matches!(done.verdict, Verdict::Done | Verdict::NotSmaller), "{done:?}");
    if let Some(result) = &done.output {
        assert_eq!(qpdf(&exe, &["--requires-password"], result).0, 0);
    }
}

#[test]
fn output_linearizes() {
    let Some(exe) = common::qpdf() else { return };
    let job = Job { output: Some(Output { split: None, linearize: true, ..Output::default() }), ..Job::default() };
    let done = run(&exe, &job, Item::Base, common::BASE, "linearize");
    assert_eq!(done.verdict, Verdict::Done, "{done:?}");
    assert!(linearized(&exe, &output(&done), &[]));
    assert!(!linearized(&exe, &common::corpus::item(Item::Base), &[]));
    // QPDF 12.4.2 checks its own linearization of the manual (a big outline
    // tree in object streams) with hint-table warnings (measured):
    // the result is kept and says so.
    let done = run(&exe, &job, Item::Manual, probe(Some(178)), "linearize-manual");
    assert_eq!(done.verdict, Verdict::DoneWithWarnings, "{done:?}");
    assert!(done.details.iter().any(|line| line.contains("outlines table")), "{:?}", done.details);
    assert!(linearized(&exe, &output(&done), &[]));
}

#[test]
fn no_step_repairs() {
    let Some(exe) = common::qpdf() else { return };
    let done = run(&exe, &Job::default(), Item::Damaged, common::BASE, "repair");
    assert_eq!(done.verdict, Verdict::DoneWithWarnings, "{done:?}");
    let result = output(&done);
    assert!(result.ends_with("in (repaired).pdf"));
    assert_eq!(qpdf(&exe, &["--check"], &result).0, 0, "the result is sound");
}

#[test]
fn every_step_at_once_is_one_qpdf_call() {
    for exe in common::qpdf_builds() {
        let job = Job {
            pages: Some(Pages { select: Select::Keep(range("3,1")), files: Files::Each, rotate: Some(Rotate { angle: Angle::Left, pages: None }) }),
            forms: Some(Forms { action: FormAction::Flatten(Annotations::All), flatten_rotation: false }),
            privacy: Some(Privacy { fixed_id: false, ..Privacy::default() }),
            optimize: Some(Optimize::default()),
            protect: Some(encrypt("u", "o")),
            output: Some(Output { split: None, linearize: true, ..Output::default() }),
            ..Job::default()
        };
        let done = run(&exe, &job, Item::Base, common::BASE, "all-steps");
        let result = output(&done);
        assert!(result.ends_with("in (processed).pdf"), "{}", result.display());
        assert_eq!(page_order(&exe, &result, &["--password=u"]), [3, 1]);
        let text = qdf(&exe, &result, &["--password=u"]);
        assert_eq!(text.matches("/Rotate 270").count(), 2);
        assert!(!text.contains("/AcroForm"));
        assert!(linearized(&exe, &result, &["--password=u"]));
        assert_eq!(qpdf(&exe, &["--requires-password"], &result).0, 0);
    }
}

#[test]
fn range_counts_agree_with_qpdf() {
    let Some(exe) = common::qpdf() else { return };
    let manual = common::corpus::item(Item::Manual);
    let out = common::scratch("ranges").join("out.pdf");
    for text in ["1-10,x3,7-8,x8", "1-5,7-10:odd", "r3-z", "1-z,x2-r2", "5-3", "1,1,1", "1-z:even", "z-r1", "1-10,x2:odd"] {
        let range = range(text);
        let status = std::process::Command::new(&exe)
            .args(["--empty", "--pages"])
            .arg(&manual)
            .args([range.text(), "--"])
            .arg(&out)
            .status()
            .expect("qpdf");
        assert_eq!(status.code(), Some(0), "{text}");
        let (_, pages, _) = qpdf(&exe, &["--show-npages"], &out);
        assert_eq!(pages.trim().parse::<u64>().ok(), range.count(178).ok(), "{text}");
    }
}

/// Run `job` on copies of `items` — each with its probe result and its own
/// pages — as one result, the way the batch runs a merge.
fn run_many(exe: &Path, job: &Job, items: &[(Item, Probe, Option<&str>)], name: &str) -> Vec<Produced> {
    assert_eq!(job.check(), Ok(()), "{}", job.describe());
    let dir = common::scratch(name);
    let copies: Vec<(PathBuf, u64)> =
        items.iter().zip(1..).map(|((item, _, _), n)| common::copy_item(*item, &dir, &format!("{n}.pdf"))).collect();
    let ranges: Vec<Option<PageRange>> = items.iter().map(|(_, _, pages)| pages.map(range)).collect();
    let inputs: Vec<Input> = copies
        .iter()
        .zip(items)
        .zip(&ranges)
        .map(|(((path, size), (_, probe, _)), range)| Input { path, size: *size, probe, range: range.as_ref(), attachments: None })
        .collect();
    let (job_object, cancel) = (common::job_object(), AtomicBool::new(false));
    job.produce(exe, &inputs, &Context::new(&job_object, &cancel))
}

/// The one result of a merge, which every input must point at.
fn merged(done: &[Produced]) -> PathBuf {
    let result = output(&done[0]);
    assert!(done.iter().all(|d| d == &done[0]), "{done:?}");
    result
}

#[test]
fn a_merge_follows_the_list_order_with_each_file_s_own_pages() {
    // The order of the list is the order of the pages.
    for exe in common::qpdf_builds() {
        let job = Job { input_password: Some(secret(USER_PASSWORD)), ..merge_job(Files::Merge, Select::All) };
        let restricted = Probe::Ready { pages: 3, restricted: true };
        let done = run_many(
            &exe,
            &job,
            &[
                (Item::Base, common::BASE, Some("3")),
                (Item::EncryptedOpen, Probe::NeedsPassword, Some("2-1")),
                (Item::Base, common::BASE, None),
                // Encrypted without an open password: given none (a wrong one
                // would make QPDF refuse it).
                (Item::EncryptedOwnerOnly, restricted, Some("z")),
            ],
            "merge",
        );
        let result = merged(&done);
        assert!(result.ends_with("1 (merged).pdf"), "{}", result.display());
        assert_eq!(page_order(&exe, &result, &[]), [3, 2, 1, 1, 2, 3, 3]);
        assert_eq!(qpdf(&exe, &["--requires-password"], &result).0, 2, "a merge starts unencrypted");
    }
}

#[test]
fn a_file_that_cannot_be_merged_stops_the_merge_before_qpdf_runs() {
    let Some(exe) = common::qpdf() else { return };
    let items = [(Item::Base, common::BASE, None), (Item::EncryptedOpen, Probe::NeedsPassword, None)];
    let done = run_many(&exe, &merge_job(Files::Merge, Select::All), &items, "merge-refused");
    assert_eq!(done[1].verdict, Verdict::PasswordRequired);
    assert_eq!(done[0].verdict, Verdict::Failed(Failure::NotMerged), "{done:?}");
    assert_eq!(common::others(&common::scratch_path("merge-refused"), &[]).len(), 2, "only the inputs");
}

#[test]
fn collating_interleaves_the_files() {
    let Some(exe) = common::qpdf() else { return };
    // A scan of the odd pages and one of the even pages.
    let items = [(Item::Base, common::BASE, Some("1-z:odd")), (Item::Base, common::BASE, Some("2"))];
    let result = merged(&run_many(&exe, &merge_job(Files::Collate, Select::All), &items, "collate"));
    assert!(result.ends_with("1 (mixed).pdf"));
    assert_eq!(page_order(&exe, &result, &[]), [1, 2, 3]);
}

#[test]
fn the_pages_typed_for_a_merge_come_from_every_file_without_its_own() {
    let Some(exe) = common::qpdf() else { return };
    let items = [(Item::Base, common::BASE, None), (Item::Base, common::BASE, Some("3")), (Item::Base, common::BASE, None)];
    let result = merged(&run_many(&exe, &merge_job(Files::Merge, Select::Keep(range("z-2"))), &items, "merge-typed"));
    assert_eq!(page_order(&exe, &result, &[]), [3, 2, 3, 3, 2]);
}

#[test]
fn split_writes_a_folder_of_parts() {
    for exe in common::qpdf_builds() {
        let job = Job { output: Some(Output { split: Some(2), linearize: false, ..Output::default() }), ..Job::default() };
        let done = run(&exe, &job, Item::Base, common::BASE, "split");
        let folder = output(&done);
        assert!(folder.ends_with("in (split)") && folder.is_dir(), "{}", folder.display());
        let mut parts = common::others(&folder, &[]);
        parts.sort();
        assert_eq!(parts, ["in-1-2.pdf", "in-3-3.pdf"]);
        assert_eq!(page_order(&exe, &folder.join("in-1-2.pdf"), &[]), [1, 2]);
        assert_eq!(page_order(&exe, &folder.join("in-3-3.pdf"), &[]), [3]);
        let mut beside = common::others(&common::scratch_path("split"), &[]);
        beside.sort();
        assert_eq!(beside, ["in (split)", "in.pdf"], "no temporary folder left");
    }
}

/// The pages of `file` (by number) on which another file is drawn.
fn covered_pages(exe: &Path, file: &Path) -> Vec<u32> {
    page_sections(exe, file, &[])
        .into_iter()
        // QPDF draws the other file's pages as form XObjects `/Fx<n>`
        // (measured); the base pages draw only text.
        .filter(|(_, section)| section.lines().any(|line| line.starts_with("/Fx") && line.ends_with(" Do")))
        .map(|(page, _)| page)
        .collect()
}

#[test]
fn an_overlay_covers_the_chosen_pages() {
    let Some(exe) = common::qpdf() else { return };
    let stamp = common::corpus::item(Item::Photo);
    let overlay = |under, to: Option<&str>, placement| Overlay { under, file: stamp.clone(), to: to.map(range), placement, probe: probe(Some(1)) };
    let job = Job { overlay: Some(overlay(false, Some("2-z"), Placement::PageOne)), ..Job::default() };
    let done = run(&exe, &job, Item::Base, common::BASE, "overlay");
    assert!(output(&done).ends_with("in (overlaid).pdf"));
    assert_eq!(covered_pages(&exe, &output(&done)), [2, 3]);
    // In order, the one page of the file covers the first page only.
    let job = Job { overlay: Some(overlay(true, None, Placement::InOrder)), ..Job::default() };
    let done = run(&exe, &job, Item::Base, common::BASE, "underlay");
    assert!(output(&done).ends_with("in (underlaid).pdf"));
    assert_eq!(covered_pages(&exe, &output(&done)), [1]);
}

#[test]
fn the_usual_placements_cover_the_pages_they_say() {
    let Some(exe) = common::qpdf() else { return };
    let (one_page, three_pages) = (common::corpus::item(Item::Photo), common::corpus::item(Item::Base));
    for (placement, stamp, pages, on_pages, covered) in [
        (Placement::PageOne, &one_page, 1, "", vec![1, 2, 3]),
        (Placement::PageOne, &three_pages, 3, "1", vec![1]),
        (Placement::PageOne, &one_page, 1, "2-z", vec![2, 3]),
        (Placement::InOrder, &one_page, 1, "", vec![1]),
        (Placement::InOrder, &three_pages, 3, "", vec![1, 2, 3]),
        (Placement::InTurn, &one_page, 1, "", vec![1, 2, 3]),
        (Placement::InTurn, &three_pages, 3, "2-z", vec![2, 3]),
    ] {
        let overlay = Overlay::read(false, stamp.clone(), probe(Some(pages)), placement, on_pages).expect("overlay");
        let job = Job { overlay: Some(overlay), ..Job::default() };
        let done = run(&exe, &job, Item::Base, common::BASE, &format!("placement-{placement:?}-{pages}-{}", on_pages.len()));
        assert_eq!(covered_pages(&exe, &output(&done)), covered, "{placement:?} of {pages} page(s) on “{on_pages}”");
    }
}

#[test]
fn the_password_goes_only_to_a_file_that_needs_it() {
    // Once every file got it, and QPDF refused a file encrypted without an
    // open password as "invalid password" (measured).
    let Some(exe) = common::qpdf() else { return };
    let job = Job { input_password: Some(secret("for another file")), ..Job::optimize_lossless() };
    let restricted = Probe::Ready { pages: 3, restricted: true };
    let done = run(&exe, &job, Item::EncryptedOwnerOnly, restricted, "owner-only");
    assert!(matches!(done.verdict, Verdict::Done | Verdict::NotSmaller), "{done:?}");
}

#[test]
fn advanced_options_reach_qpdf() {
    let Some(exe) = common::qpdf() else { return };
    let raw = |path: &Path| String::from_utf8_lossy(&std::fs::read(path).expect("read")).into_owned();
    // Privacy › Advanced: the page labels go.
    let privacy = Job { privacy: Some(Privacy { remove_page_labels: true, remove_structure: true, ..Privacy::default() }), ..Job::default() };
    let result = output(&run(&exe, &privacy, Item::Base, common::BASE, "no-labels"));
    assert!(qdf(&exe, &common::corpus::item(Item::Base), &[]).contains("/PageLabels"));
    assert!(!qdf(&exe, &result, &[]).contains("/PageLabels"));
    // Forms › Advanced: the rotation becomes part of the page.
    let flat = Job {
        forms: Some(Forms { action: FormAction::Flatten(Annotations::All), flatten_rotation: true }),
        ..pages_job(Select::All, Some(Rotate { angle: Angle::Right, pages: None }))
    };
    let text = qdf(&exe, &output(&run(&exe, &flat, Item::Base, common::BASE, "flat-rotation")), &[]);
    assert!(!text.contains("/Rotate 90"), "no rotation left in the page dictionaries");
    // Optimize › Advanced: no object streams; the default makes them.
    let optimize = |object_streams| Job {
        optimize: Some(Optimize { object_streams, keep_if_not_smaller: false, ..Optimize::default() }),
        ..Job::default()
    };
    let manual = probe(Some(178));
    assert!(!raw(&output(&run(&exe, &optimize(ObjectStreams::Disable), Item::Manual, manual.clone(), "no-objstm"))).contains("/ObjStm"));
    assert!(raw(&output(&run(&exe, &optimize(ObjectStreams::Generate), Item::Manual, manual, "objstm"))).contains("/ObjStm"));
}

#[test]
fn keep_takes_a_files_own_pages_first() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("keep-own");
    let (job_object, cancel) = (common::job_object(), AtomicBool::new(false));
    let ctx = Context::new(&job_object, &cancel);
    let own = range("3,1");
    let mut orders = Vec::new();
    for (name, range) in [("own.pdf", Some(&own)), ("plain.pdf", None)] {
        let (input, size) = common::copy_item(Item::Base, &dir, name);
        let job = pages_job(Select::Keep(common::range("2")), None);
        let input = Input { path: &input, size, probe: &common::BASE, range, attachments: None };
        orders.push(page_order(&exe, &output(&job.produce(&exe, &[input], &ctx).remove(0)), &[]));
    }
    assert_eq!(orders, [vec![3, 1], vec![2]], "own pages win; the field for the rest");
}

#[test]
fn attachments_are_embedded_and_a_taken_name_is_reported() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("attach-files");
    let (note, taken) = (dir.join("zażółć note.txt"), dir.join("a00"));
    std::fs::write(&note, b"note").expect("write");
    std::fs::write(&taken, b"same key as the first attachment").expect("write");
    let attach = |files: Vec<PathBuf>| Job { attachments: Some(Attachments { add: files, remove_all: false }), ..Job::default() };
    let done = run(&exe, &attach(vec![note.clone()]), Item::Attachments, common::BASE, "attach-add");
    assert_eq!(done.verdict, Verdict::Done, "{done:?}");
    let listed = qpdf(&exe, &["--list-attachments"], &output(&done)).1;
    assert!(listed.lines().any(|l| l.starts_with("zażółć note.txt -> ")), "{listed}");
    assert_eq!(listed.lines().count(), 11, "the ten it had and the new one: {listed}");
    assert!(output(&done).file_name().is_some_and(|n| n == "in (attachments).pdf"));
    // The file already has an attachment under that key: nothing replaced.
    let clash = run(&exe, &attach(vec![taken]), Item::Attachments, common::BASE, "attach-clash");
    assert_eq!(clash.verdict, Verdict::Failed(Failure::AttachmentExists), "{clash:?}");
}
