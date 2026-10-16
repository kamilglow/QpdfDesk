//! Damaged PDFs of every kind through the real QPDF: bytes
//! flipped, numbers changed, the file cut or stuffed — the probe, a job,
//! Check and Info each end with a verdict, never a panic; the original stays
//! as it was, nothing is left beside it, and what is made passes QPDF's check.

mod common;

use common::corpus::{Item, USER_PASSWORD};
use common::fuzz::Rng;
use qpdf_desk::core::inspect;
use qpdf_desk::core::job::{Input, Job};
use qpdf_desk::core::runner::Context;
use qpdf_desk::tasks;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// `bytes` damaged one of five ways.
fn damage(rng: &mut Rng, bytes: &mut Vec<u8>) {
    let at = |rng: &mut Rng, len: usize| rng.below(len.max(1) as u64) as usize;
    match rng.below(5) {
        // Bytes anywhere: in a stream, a keyword, the cross-reference table.
        0 => {
            for _ in 0..1 + rng.below(8) {
                let i = at(rng, bytes.len());
                bytes[i] = rng.next() as u8;
            }
        }
        // A number: a length, an offset, a count, an object number.
        1 => {
            let digits: Vec<usize> = bytes.iter().enumerate().filter(|(_, b)| b.is_ascii_digit()).map(|(i, _)| i).collect();
            for _ in 0..1 + rng.below(4) {
                let i = *rng.pick(&digits);
                bytes[i] = b'0' + rng.below(10) as u8;
            }
        }
        2 => bytes.truncate(at(rng, bytes.len())),
        3 => {
            let i = at(rng, bytes.len());
            let junk = rng.bytes(200);
            bytes.splice(i..i, junk);
        }
        _ => {
            let (from, len) = (at(rng, bytes.len()), 1 + rng.below(2_000) as usize);
            let chunk = bytes[from..(from + len).min(bytes.len())].to_vec();
            let i = at(rng, bytes.len());
            bytes.splice(i..i, chunk);
        }
    }
}

#[test]
fn damaged_files_end_in_a_verdict_and_leave_no_trace() {
    let Some(exe) = common::qpdf() else { return };
    let (job_object, cancel) = (common::job_object(), AtomicBool::new(false));
    let ctx = Context::new(&job_object, &cancel);
    let password = common::secret(USER_PASSWORD);
    let mut rng = Rng::new(0x5EED_0101);
    let mut verdicts = std::collections::BTreeMap::new();
    // About 1.4 s a round: seven QPDF runs.
    for round in 0..24 {
        let item = *rng.pick(&[Item::Base, Item::Attachments, Item::EncryptedOpen, Item::Photo]);
        let mut bytes = std::fs::read(common::corpus::item(item)).expect("read");
        damage(&mut rng, &mut bytes);
        let dir = common::scratch(&format!("random_pdfs_{round}"));
        let input = dir.join("in.pdf");
        std::fs::write(&input, &bytes).expect("write");

        // As the app does it: the probe, then the job with what it found.
        let probe = tasks::probe_file(&exe, &input, &ctx);
        let job = Job { input_password: Some(password.duplicate()), ..Job::optimize_lossless() };
        let input_entry = Input { path: &input, size: bytes.len() as u64, probe: &probe, range: None, attachments: None };
        let done = job.produce(&exe, &[input_entry], &ctx).remove(0);
        let report = inspect::check(&exe, &input, &probe, Some(&password), &ctx);
        let _ = inspect::inspect(&exe, &input, &probe, Some(&password), &ctx);

        let what = format!("round {round}: {item:?}, probe {probe:?}, {:?}", done.verdict);
        assert!(!done.verdict.status().is_empty() && !report.health().is_empty(), "{what}");
        assert_eq!(std::fs::read(&input).expect("the original"), bytes, "{what}: the original is untouched");
        let made: Vec<&Path> = done.output.as_deref().into_iter().collect();
        if done.verdict.succeeded() {
            let output = made.first().unwrap_or_else(|| panic!("{what}: a result"));
            // An encrypted input's result keeps its password.
            let (code, _, _) = common::run_qpdf(&exe, &["--check", &format!("--password={USER_PASSWORD}")], output);
            assert!(code == 0 || code == 3, "{what}: the result passes QPDF's check ({code})");
        }
        let mut keep = made.clone();
        keep.push(&input);
        assert_eq!(common::others(&dir, &keep), Vec::<String>::new(), "{what}: no temporary file left");
        *verdicts.entry(done.verdict.status()).or_insert(0) += 1;
    }
    // Not all alike: the damage reaches QPDF's verdicts of every kind.
    assert!(verdicts.len() >= 3, "{verdicts:?}");
}
