//! Background work with the real QPDF: the probe and a whole batch —
//! A batch of 100 files from start to end.

mod common;

use common::corpus::{self, Item};
use qpdf_desk::core::filelist::Probe;
use qpdf_desk::core::job::{Attachments, Files, Job, Pages, Select};
use qpdf_desk::core::outcome::{Failure, Verdict};
use qpdf_desk::platform::JobObject;
use qpdf_desk::core::output::Produced;
use qpdf_desk::tasks::{Batch, BatchItem, Event, Notify, Prober, Task, Unit, Wiring};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn job_object() -> Arc<JobObject> {
    Arc::new(common::job_object())
}

/// A copy of `item` as `name` in `dir`, ready for a batch.
fn batch_item(id: u64, item: Item, dir: &Path, name: &str, pages: Option<u64>) -> BatchItem {
    let (path, size) = common::copy_item(item, dir, name);
    BatchItem { id, path, size, probe: common::probe(pages), range: None }
}

fn quiet() -> Notify {
    Arc::new(|| {})
}

fn wiring(qpdf: PathBuf, tx: Sender<Event>) -> Wiring {
    Wiring { qpdf, job: job_object(), tx, notify: quiet() }
}

/// The next event, or a panic after a generous wait.
fn next(rx: &Receiver<Event>) -> Event {
    rx.recv_timeout(Duration::from_secs(120)).expect("an event in time")
}

#[test]
fn the_probe_tells_pages_passwords_and_failures() {
    let Some(exe) = common::qpdf() else { return };
    let (tx, rx) = mpsc::channel();
    let prober = Prober::new(job_object(), tx, quiet(), 2);
    let files = [
        (1, Item::Base, Probe::Ready { pages: 3, restricted: false }),
        (2, Item::EncryptedOwnerOnly, Probe::Ready { pages: 3, restricted: true }),
        (3, Item::EncryptedOpen, Probe::NeedsPassword),
        (4, Item::NotPdf, Probe::Failed(Failure::NotPdf)),
        (5, Item::Damaged, Probe::Ready { pages: 3, restricted: false }),
        (6, Item::Pages10k, Probe::Ready { pages: 10_000, restricted: false }),
    ];
    // Queued before QPDF is known: nothing runs until it is.
    prober.enqueue(files.iter().map(|(id, item, _)| (*id, corpus::item(*item))).collect());
    prober.forget(&[6]);
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "waits for QPDF");
    prober.set_qpdf(exe);
    let mut seen = HashMap::new();
    for _ in 0..5 {
        match next(&rx) {
            Event::Probed { id, probe } => seen.insert(id, probe),
            other => panic!("unexpected {other:?}"),
        };
    }
    for (id, _, expected) in &files[..5] {
        assert_eq!(seen.get(id), Some(expected), "file {id}");
    }
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "the forgotten file is never probed");
    drop(prober); // joins its thread
}

/// Optimize `items` as one batch, each file its own result.
fn run_batch(
    exe: PathBuf,
    items: Vec<BatchItem>,
    threads: usize,
    cancel_after: Option<Duration>,
) -> (HashMap<u64, Produced>, bool) {
    let units = items.into_iter().map(|item| vec![item]).collect();
    run_units(exe, Job::optimize_lossless(), units, threads, cancel_after)
}

/// Run `job` on `units` as one batch; returns the finished verdicts by id
/// and whether it was cancelled.
fn run_units(
    exe: PathBuf,
    job: Job,
    units: Vec<Unit>,
    threads: usize,
    cancel_after: Option<Duration>,
) -> (HashMap<u64, Produced>, bool) {
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let batch = Batch::start(wiring(exe, tx), Task::Process(Box::new(job)), units, threads);
    let mut finished = HashMap::new();
    loop {
        if cancel_after.is_some_and(|after| started.elapsed() >= after) {
            batch.cancel();
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Event::Finished { id, produced }) => {
                finished.insert(id, produced);
            }
            Ok(Event::BatchDone { cancelled }) => {
                assert_eq!(batch.counts().0, finished.len(), "the batch counts what it finished");
                return (finished, cancelled);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                assert!(started.elapsed() < Duration::from_secs(300), "batch never ended");
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_batch_of_100_files_runs_from_start_to_end() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("batch-100");
    let mut items = Vec::new();
    for i in 1..=100u64 {
        // Mostly the small file (−35 %), every tenth the manual, one not a PDF.
        let (item, pages) = match i {
            50 => (Item::NotPdf, None),
            _ if i % 10 == 0 => (Item::Manual, Some(178)),
            _ => (Item::Base, Some(3)),
        };
        items.push(batch_item(i, item, &dir, &format!("file {i}.pdf"), pages));
    }
    let (finished, cancelled) = run_batch(exe, items, 4, None);
    assert!(!cancelled);
    assert_eq!(finished.len(), 100, "every file reported once");
    for (id, produced) in &finished {
        match id {
            50 => assert_eq!(produced.verdict, Verdict::Failed(Failure::NotPdf)),
            _ => {
                assert!(
                    matches!(produced.verdict, Verdict::Done | Verdict::NotSmaller),
                    "file {id}: {:?}",
                    produced
                );
                if produced.verdict == Verdict::Done {
                    let output = produced.output.as_ref().expect("output");
                    assert_eq!(output, &dir.join(format!("file {id} (optimized).pdf")));
                }
            }
        }
    }
    let leftovers: Vec<String> = common::others(&dir, &[]).into_iter().filter(|name| name.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_result_that_is_not_smaller_keeps_the_original() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("not-smaller");
    // Already optimized once, a second pass cannot shrink it.
    let plain = batch_item(1, Item::Base, &dir, "plain.pdf", Some(3));
    let (finished, _) = run_batch(exe.clone(), vec![plain], 1, None);
    let optimized = finished[&1].output.clone().expect("smaller the first time");
    let first = dir.join("once.pdf");
    std::fs::rename(&optimized, &first).expect("rename");
    let size = std::fs::metadata(&first).expect("size").len();
    let (finished, _) = run_batch(exe, vec![BatchItem { id: 2, path: first, size, probe: common::probe(Some(3)), range: None }], 1, None);
    assert_eq!(finished[&2].verdict, Verdict::NotSmaller);
    assert_eq!(finished[&2].output, None);
    let names = common::others(&dir, &[]);
    assert_eq!(names.len(), 2, "only the two inputs: {names:?}");
}

#[test]
fn cancelling_a_batch_stops_qpdf_and_leaves_the_rest_untouched() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("batch-cancel");
    let items: Vec<BatchItem> = [Item::Heavy, Item::Base, Item::Base]
        .into_iter()
        .zip(0u64..)
        .map(|(item, i)| batch_item(i, item, &dir, &format!("{i}.pdf"), None))
        .collect();
    let started = Instant::now();
    let (finished, cancelled) = run_batch(exe, items, 1, Some(Duration::from_millis(800)));
    assert!(cancelled);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(finished.len(), 1, "only the heavy file had started");
    assert_eq!(finished[&0].verdict, Verdict::Cancelled);
    let names = common::others(&dir, &[]);
    assert_eq!(names.len(), 3, "nothing but the inputs: {names:?}");
}

#[test]
fn a_file_the_probe_has_not_reached_is_probed_before_it_runs() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("batch-unprobed");
    let (tx, rx) = mpsc::channel();
    let item = batch_item(7, Item::Base, &dir, "a.pdf", None);
    let batch = Batch::start(wiring(exe, tx), Task::Process(Box::new(Job::optimize_lossless())), vec![vec![item]], 1);
    // The window learns the page count, and the check uses it, before QPDF runs.
    let mut events = Vec::new();
    loop {
        match next(&rx) {
            Event::BatchDone { .. } => break,
            event => events.push(event),
        }
    }
    drop(batch);
    let probed = events.iter().position(|e| matches!(e, Event::Probed { id: 7, probe: Probe::Ready { pages: 3, .. } }));
    let finished = events.iter().position(|e| matches!(e, Event::Finished { id: 7, produced } if produced.verdict.settled()));
    assert!(probed.is_some() && probed < finished, "{events:?}");
}

#[test]
fn a_merge_is_one_result_for_every_file_of_the_list() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("batch-merge");
    let unit: Vec<BatchItem> = (1..=3u64).map(|id| batch_item(id, Item::Base, &dir, &format!("{id}.pdf"), None)).collect();
    let job = Job { pages: Some(Pages { select: Select::All, files: Files::Merge, rotate: None }), ..Job::default() };
    let (finished, cancelled) = run_units(exe.clone(), job, vec![unit], 4, None);
    assert!(!cancelled);
    assert_eq!(finished.len(), 3);
    let merged = dir.join("1 (merged).pdf");
    for id in 1..=3 {
        assert_eq!(finished[&id].verdict, Verdict::Done, "{:?}", finished[&id]);
        assert_eq!(finished[&id].output.as_ref(), Some(&merged), "every file points at the one result");
    }
    assert_eq!(common::run_qpdf(&exe, &["--show-npages"], &merged).1.trim(), "9");
}

#[test]
fn remove_all_attachments_lists_each_files_own_and_replaces_them() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("batch-attachments");
    let hostile = batch_item(1, Item::Attachments, &dir, "hostile.pdf", Some(3));
    let plain = batch_item(2, Item::Base, &dir, "plain.pdf", Some(3));
    let locked = BatchItem { probe: Probe::NeedsPassword, ..batch_item(3, Item::EncryptedOpen, &dir, "locked.pdf", None) };
    let note = dir.join("note.txt");
    std::fs::write(&note, b"a new note").expect("write");
    let job = Job {
        attachments: Some(Attachments { add: vec![note], remove_all: true }),
        ..Job::default()
    };
    let units = vec![vec![hostile], vec![plain], vec![locked]];
    let (finished, _) = run_units(exe.clone(), job, units, 2, None);
    for id in [1, 2] {
        let produced = &finished[&id];
        assert_eq!(produced.verdict, Verdict::Done, "{produced:?}");
        let listed = common::run_qpdf(&exe, &["--list-attachments"], produced.output.as_ref().expect("result")).1;
        assert_eq!(listed.lines().map(|l| l.split(" -> ").next().unwrap_or_default()).collect::<Vec<_>>(), ["note.txt"], "{listed}");
    }
    // Its attachments cannot be listed without the password: nothing runs.
    assert_eq!(finished[&3].verdict, Verdict::PasswordRequired);
}

#[test]
fn a_check_batch_reports_each_file() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("batch-check");
    let files = [(1, Item::Base, "OK"), (2, Item::Damaged, "Warnings"), (3, Item::NotPdf, "Not a PDF"), (4, Item::EncryptedOpen, "OK")];
    let units = files.iter().map(|(id, item, _)| vec![batch_item(*id, *item, &dir, &format!("{id}.pdf"), None)]).collect();
    let (tx, rx) = mpsc::channel();
    let password = Some(common::secret(corpus::USER_PASSWORD));
    let batch = Batch::start(wiring(exe, tx), Task::Check(password), units, 2);
    let mut verdicts = HashMap::new();
    loop {
        match next(&rx) {
            Event::Checked { id, report } => {
                verdicts.insert(id, report.health());
            }
            Event::Finished { .. } => panic!("a check makes no result"),
            Event::BatchDone { cancelled } => {
                assert!(!cancelled);
                break;
            }
            _ => {}
        }
    }
    drop(batch);
    for (id, _, health) in files {
        assert_eq!(verdicts.get(&id).map(String::as_str), Some(health), "file {id}");
    }
    assert_eq!(common::others(&dir, &[]).len(), 4, "nothing written");
}
