//! Background work: finding QPDF, adding files, the probe, the batch —
//! the job or a Check — and the windows' errands (Info, Pages,
//! extracting attachments), all off the UI thread.
//!
//! Workers NEVER touch widgets. Discrete events travel through an `mpsc`
//! channel and each one is followed by `notify` so the UI wakes at once — the
//! app passes `fltk::app::awake`, tests a no-op, which keeps this module free
//! of any UI dependency. The write progress of the running files is shared
//! through atomics that the UI reads on its timer, which runs only while a
//! batch does.
//!
//! Every QPDF process goes into the app's one [`JobObject`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::core::attachments::{self, Extracted};
use crate::core::dedup::canonical_key;
use crate::core::filelist::{Candidate, Entry, Probe, MAX_FILES};
use crate::core::inspect::{self, Attachment, CheckReport, Info};
use crate::core::job::{Input, Job};
use crate::core::outcome::Verdict;
use crate::core::output::Produced;
use crate::core::qpdf::{self, Missing, Qpdf};
use crate::core::ranges::PageRange;
use crate::core::runner::{self, Arg, Context};
use crate::core::scandir::{is_pdf_file, scan_folder, FirstPdfs};
use crate::core::secret::Secret;
use crate::platform::JobObject;

/// Wakes the UI after an event was sent.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// What the background work reports.
#[derive(Debug)]
pub enum Event {
    /// The search for QPDF ended.
    Located(Result<Qpdf, Missing>),
    /// One batch of additions is ready, in natural order.
    Added(Collected),
    /// The probe has looked at a file.
    Probed { id: u64, probe: Probe },
    /// The overlay file has been looked at the same way.
    OverlayProbed { path: PathBuf, probe: Probe },
    /// A file of the batch has started.
    Started { id: u64 },
    /// A file of the batch is done with.
    Finished { id: u64, produced: Produced },
    /// A file of a Check batch has been checked.
    Checked { id: u64, report: CheckReport },
    /// The batch is over; every worker has stopped.
    BatchDone { cancelled: bool },
    /// What a file holds, for the Info or Pages window that asked (`request`).
    Inspected { request: u64, result: Result<Info, Verdict> },
    /// Attachments were pulled out of a file, for the window that asked.
    Extracted { request: u64, extracted: Extracted },
}

fn send(tx: &Sender<Event>, notify: &Notify, event: Event) -> bool {
    send_all(tx, notify, [event])
}

/// [`send`] every event, then wake the UI once: a merge of 10 000 files
/// must not post 10 000 wake-ups (Windows caps a thread's message queue at
/// 10 000 by default).
fn send_all(tx: &Sender<Event>, notify: &Notify, events: impl IntoIterator<Item = Event>) -> bool {
    let sent = events.into_iter().all(|event| tx.send(event).is_ok());
    notify();
    sent
}

/// Lock `mutex`, also after a panic elsewhere: every value guarded here stays
/// consistent between statements, so a poisoned lock is still usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Look for QPDF in the standard places (a ~64 ms process per candidate, so
/// not on the way to the first frame).
pub fn spawn_locate(job: Arc<JobObject>, tx: Sender<Event>, notify: Notify) {
    thread::spawn(move || {
        let found = qpdf::locate(&qpdf::default_candidates(), &job);
        send(&tx, &notify, Event::Located(found));
    });
}

/// Turn dropped or picked paths into list candidates on a background thread:
/// folders walked recursively, only `.pdf` files kept, each with its size and
/// duplicate key, in natural order.
pub fn spawn_add(paths: Vec<PathBuf>, stop: Arc<AtomicBool>, tx: Sender<Event>, notify: Notify) {
    thread::spawn(move || {
        send(&tx, &notify, Event::Added(collect_candidates(paths, MAX_FILES, &stop)));
    });
}

/// What [`collect_candidates`] found.
#[derive(Debug, Default)]
pub struct Collected {
    /// The first `limit` PDF files in natural order.
    pub candidates: Vec<Candidate>,
    /// Other files.
    pub skipped: usize,
    /// PDF files after the first `limit`.
    pub beyond: usize,
    /// The user stopped it: nothing was collected.
    pub cancelled: bool,
}

/// The work of [`spawn_add`]. Every folder is walked first, so the files kept
/// are the first in the order the user reads them; only those get their
/// duplicate key (a file-system call each). `stop` ends it at once.
pub fn collect_candidates(paths: Vec<PathBuf>, limit: usize, stop: &AtomicBool) -> Collected {
    let mut skipped = 0usize;
    let mut found = FirstPdfs::new(limit);
    for path in paths {
        // Only folders and existing PDF files: a drop can carry plain text,
        // such as a link from a browser, which only looks like a path.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => skipped += scan_folder(&path, &mut found, stop),
            Ok(meta) if meta.is_file() && is_pdf_file(&path) => found.push((path, meta.len())),
            _ => skipped += 1,
        }
    }
    let (found, beyond) = found.sorted();
    let mut candidates = Vec::with_capacity(found.len());
    for (path, size) in found {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        candidates.push(Candidate { key: canonical_key(&path), path, size });
    }
    if stop.load(Ordering::Relaxed) {
        return Collected { cancelled: true, ..Collected::default() };
    }
    Collected { candidates, skipped, beyond, cancelled: false }
}

/// Look at one file outside the list — the overlay file — at once, not behind
/// the files waiting for the probe.
pub fn spawn_probe(wiring: Wiring, path: PathBuf) {
    thread::spawn(move || {
        let probe = probe_file(&wiring.qpdf, &path, &Context::new(&wiring.job, &AtomicBool::new(false)));
        send(&wiring.tx, &wiring.notify, Event::OverlayProbed { path, probe });
    });
}

/// One piece of work for a window — reading a file for Info or Pages,
/// pulling out attachments — on its own thread. [`Errand::stop`] kills its
/// QPDF without waiting (the window closes at once); dropping it waits for
/// the thread, which the app does only when it ends.
pub struct Errand {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Errand {
    /// Run `work` with QPDF and its context; its answer is sent unless the
    /// errand was stopped meanwhile (its window is closed).
    fn start(wiring: Wiring, work: impl FnOnce(&Path, &Context) -> Event + Send + 'static) -> Errand {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let thread = thread::spawn(move || {
            let answer = work(&wiring.qpdf, &Context::new(&wiring.job, &flag));
            if !flag.load(Ordering::Relaxed) {
                send(&wiring.tx, &wiring.notify, answer);
            }
        });
        Errand { cancel, thread: Some(thread) }
    }

    /// Ask it to stop: its QPDF is killed within the runner's poll.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Errand {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What background work needs: the QPDF in use, the shared job, and the way
/// back to the window.
#[derive(Clone)]
pub struct Wiring {
    pub qpdf: PathBuf,
    pub job: Arc<JobObject>,
    pub tx: Sender<Event>,
    pub notify: Notify,
}

/// How a window reads a file: [`inspect::inspect`] for Info,
/// [`inspect::page_labels`] for the page view.
pub type Read = fn(&Path, &Path, &Probe, Option<&Secret>, &Context) -> Result<Info, Verdict>;

/// Read `path` for a window; `probe` is what the probe found. The
/// answer comes as [`Event::Inspected`] for `request`.
pub fn spawn_inspect(wiring: Wiring, request: u64, read: Read, path: PathBuf, probe: Probe, password: Option<Secret>) -> Errand {
    Errand::start(wiring, move |qpdf, ctx| Event::Inspected { request, result: read(qpdf, &path, &probe, password.as_ref(), ctx) })
}

/// Pull `chosen` attachments out of `path`; the answer comes as
/// [`Event::Extracted`] for `request`.
pub fn spawn_extract(wiring: Wiring, request: u64, path: PathBuf, password: Option<Secret>, chosen: Vec<Attachment>) -> Errand {
    Errand::start(wiring, move |qpdf, ctx| Event::Extracted { request, extracted: attachments::extract(qpdf, &path, password.as_ref(), &chosen, ctx) })
}

/// The probe: one `qpdf --show-encryption --show-npages` per added file,
/// `workers` files at a time, in the background — one at a time, 10 000
/// files took some 11 minutes (~64 ms each). Files removed from the list are
/// dropped from its queue, and those being probed are stopped.
pub struct Prober {
    shared: Arc<ProbeShared>,
    threads: Vec<JoinHandle<()>>,
}

struct ProbeShared {
    state: Mutex<ProbeState>,
    wake: Condvar,
    /// Stops the file each worker is probing (removed from the list, or
    /// shutdown).
    cancel: Vec<AtomicBool>,
}

#[derive(Default)]
struct ProbeState {
    /// Unknown until QPDF is found; the queue waits until then.
    qpdf: Option<PathBuf>,
    queue: VecDeque<(u64, PathBuf)>,
    /// The file each worker is on.
    current: Vec<Option<u64>>,
    shutdown: bool,
}

impl Prober {
    pub fn new(job: Arc<JobObject>, tx: Sender<Event>, notify: Notify, workers: usize) -> Prober {
        let workers = workers.max(1);
        let shared = Arc::new(ProbeShared {
            state: Mutex::new(ProbeState { current: vec![None; workers], ..ProbeState::default() }),
            wake: Condvar::new(),
            cancel: (0..workers).map(|_| AtomicBool::new(false)).collect(),
        });
        let threads = (0..workers)
            .map(|slot| {
                let (shared, job, tx, notify) = (Arc::clone(&shared), Arc::clone(&job), tx.clone(), Arc::clone(&notify));
                thread::spawn(move || probe_loop(slot, &shared, &job, &tx, &notify))
            })
            .collect();
        Prober { shared, threads }
    }

    fn state(&self) -> MutexGuard<'_, ProbeState> {
        lock(&self.shared.state)
    }

    /// QPDF is known — start on the queue.
    pub fn set_qpdf(&self, exe: PathBuf) {
        self.state().qpdf = Some(exe);
        self.shared.wake.notify_all();
    }

    /// Queue `files` — each once: one waiting already, or being probed now,
    /// is not queued again (a Check ending handed back every unprobed file,
    /// which doubled the queue).
    pub fn enqueue(&self, files: Vec<(u64, PathBuf)>) {
        {
            let mut state = self.state();
            let mut known: std::collections::HashSet<u64> =
                state.queue.iter().map(|(id, _)| *id).chain(state.current.iter().flatten().copied()).collect();
            state.queue.extend(files.into_iter().filter(|(id, _)| known.insert(*id)));
        }
        self.shared.wake.notify_all();
    }

    /// Drop files from the queue; those being probed now are stopped. (A set:
    /// clearing a list of 10 000 must not compare every pair on the UI thread.)
    pub fn forget(&self, ids: &[u64]) {
        let ids: std::collections::HashSet<u64> = ids.iter().copied().collect();
        let mut state = self.state();
        state.queue.retain(|(id, _)| !ids.contains(id));
        for (slot, current) in state.current.iter().enumerate() {
            if current.is_some_and(|id| ids.contains(&id)) {
                self.shared.cancel[slot].store(true, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Prober {
    fn drop(&mut self) {
        self.state().shutdown = true;
        for cancel in &self.shared.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.shared.wake.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Worker `slot` of the probe.
fn probe_loop(slot: usize, shared: &ProbeShared, job: &JobObject, tx: &Sender<Event>, notify: &Notify) {
    let cancel = &shared.cancel[slot];
    loop {
        let (qpdf, id, path) = {
            let mut state = lock(&shared.state);
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(qpdf) = state.qpdf.clone() {
                    if let Some((id, path)) = state.queue.pop_front() {
                        state.current[slot] = Some(id);
                        cancel.store(false, Ordering::Relaxed);
                        break (qpdf, id, path);
                    }
                }
                // Idle: asleep on the condition variable, no polling.
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        let probe = probe_file(&qpdf, &path, &Context::new(job, cancel));
        let stopped = cancel.load(Ordering::Relaxed);
        lock(&shared.state).current[slot] = None;
        if !stopped && !send(tx, notify, Event::Probed { id, probe }) {
            return; // the UI is gone
        }
    }
}

/// One `qpdf --show-encryption --show-npages`.
pub fn probe_file(qpdf: &Path, path: &Path, ctx: &Context) -> Probe {
    Probe::read(&runner::run(qpdf, &Arg::on_file(None, &["--show-encryption", "--show-npages"], path), ctx))
}

/// What the probe knows of each file of `unit` — and for one not probed yet,
/// or with a failure that may have passed (a file closed since), a probe
/// now, so the checks before QPDF runs (page numbers, encryption) are
/// complete and true. `parallel` at a time: a merge started right after
/// adding 10 000 files waited for them one by one.
fn probe_unit(qpdf: &Path, unit: &[BatchItem], parallel: usize, ctx: &Context, tx: &Sender<Event>, notify: &Notify) -> Vec<Probe> {
    let mut probes: Vec<Probe> = unit.iter().map(|item| item.probe.clone()).collect();
    let todo: Vec<usize> = (0..unit.len()).filter(|&i| matches!(unit[i].probe, Probe::Pending | Probe::Failed(_))).collect();
    let share = todo.len().div_ceil(parallel.max(1)).max(1);
    let probed: Vec<(usize, Probe)> = thread::scope(|scope| {
        let parts: Vec<_> = todo
            .chunks(share)
            .map(|part| {
                scope.spawn(move || {
                    let probe_one = |&i: &usize| {
                        let probe = probe_file(qpdf, &unit[i].path, &Context::new(ctx.job, ctx.cancel));
                        send(tx, notify, Event::Probed { id: unit[i].id, probe: probe.clone() });
                        (i, probe)
                    };
                    part.iter().map(probe_one).collect::<Vec<_>>()
                })
            })
            .collect();
        // A part that panicked leaves its files as they were.
        parts.into_iter().flat_map(|part| part.join().unwrap_or_default()).collect()
    });
    for (i, probe) in probed {
        probes[i] = probe;
    }
    probes
}

/// One file of a batch.
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub id: u64,
    pub path: PathBuf,
    pub size: u64,
    /// What the probe found: the page count the result is checked against,
    /// whether the file is encrypted.
    pub probe: Probe,
    /// Its own pages; `None` = no own pages.
    pub range: Option<PageRange>,
}

impl From<&Entry> for BatchItem {
    fn from(entry: &Entry) -> BatchItem {
        BatchItem {
            id: entry.id,
            path: entry.path.clone(),
            size: entry.size,
            probe: entry.probe.clone(),
            range: entry.range.clone(),
        }
    }
}

/// The files of one result: one file, or every file of a merge.
pub type Unit = Vec<BatchItem>;

/// What a batch does with its files.
pub enum Task {
    /// Run the job: one result per unit.
    Process(Box<Job>),
    /// `qpdf --check` each file, with the password for encrypted
    /// inputs for a file that asks for one.
    Check(Option<Secret>),
}

/// Sentinel for "no write progress yet".
const PERCENT_UNKNOWN: u8 = u8::MAX;

/// A running batch: a pool of `threads` workers taking its units in order.
pub struct Batch {
    shared: Arc<BatchShared>,
    coordinator: Option<JoinHandle<()>>,
}

struct BatchShared {
    wiring: Wiring,
    task: Task,
    /// QPDF processes one unit may run at once (a merge probing its files).
    parallel: usize,
    cancel: AtomicBool,
    next: AtomicUsize,
    units: Vec<Unit>,
    files: usize,
    /// Which unit each worker is on (by index), read by the UI timer.
    slots: Mutex<Vec<Option<usize>>>,
    percents: Vec<AtomicU8>,
    /// Files finished with, and their bytes.
    done_files: AtomicUsize,
    done_bytes: AtomicU64,
    total_bytes: u64,
}

fn unit_bytes(unit: &[BatchItem]) -> u64 {
    unit.iter().fold(0u64, |sum, item| sum.saturating_add(item.size))
}

impl Batch {
    /// Start `task` on `units` with `threads` workers.
    pub fn start(wiring: Wiring, task: Task, units: Vec<Unit>, threads: usize) -> Batch {
        let parallel = threads.max(1);
        let threads = parallel.min(units.len().max(1));
        let shared = Arc::new(BatchShared {
            wiring,
            task,
            parallel,
            cancel: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            total_bytes: units.iter().fold(0u64, |sum, unit| sum.saturating_add(unit_bytes(unit))),
            files: units.iter().map(Vec::len).sum(),
            units,
            slots: Mutex::new(vec![None; threads]),
            percents: (0..threads).map(|_| AtomicU8::new(PERCENT_UNKNOWN)).collect(),
            done_files: AtomicUsize::new(0),
            done_bytes: AtomicU64::new(0),
        });
        let workers: Vec<JoinHandle<()>> = (0..threads)
            .map(|slot| {
                let shared = Arc::clone(&shared);
                thread::spawn(move || worker(slot, &shared))
            })
            .collect();
        let coordinator_shared = Arc::clone(&shared);
        let coordinator = thread::spawn(move || {
            for worker in workers {
                let _ = worker.join();
            }
            let shared = coordinator_shared;
            let cancelled = shared.cancel.load(Ordering::Relaxed);
            send(&shared.wiring.tx, &shared.wiring.notify, Event::BatchDone { cancelled });
        });
        Batch { shared, coordinator: Some(coordinator) }
    }

    /// Stop: the running QPDF processes are killed and their temporary files
    /// deleted; files not started stay untouched.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    /// Whether it is a Check, not the job.
    pub fn is_check(&self) -> bool {
        matches!(self.shared.task, Task::Check(_))
    }

    /// `(files finished with, files in the batch)`.
    pub fn counts(&self) -> (usize, usize) {
        (self.shared.done_files.load(Ordering::Relaxed), self.shared.files)
    }

    /// Where the batch stands now: what [`Progress`] says.
    pub fn progress(&self) -> Progress {
        let shared = &self.shared;
        let active: Vec<(&Unit, Option<u8>)> = lock(&shared.slots)
            .iter()
            .enumerate()
            .filter_map(|(slot, index)| {
                let unit = shared.units.get((*index)?)?;
                let percent = match shared.percents[slot].load(Ordering::Relaxed) {
                    PERCENT_UNKNOWN => None,
                    percent => Some(percent.min(100)),
                };
                Some((unit, percent))
            })
            .collect();
        let fraction = (shared.total_bytes > 0).then(|| {
            let running: f64 =
                active.iter().map(|(unit, percent)| unit_bytes(unit) as f64 * f64::from(percent.unwrap_or(0)) / 100.0).sum();
            let done = shared.done_bytes.load(Ordering::Relaxed) as f64 + running;
            (done / shared.total_bytes as f64).clamp(0.0, 1.0)
        });
        // A merge shows its progress on its first file: repainting every one
        // of thousands of rows on each tick would stall the window.
        let active = active.into_iter().filter_map(|(unit, percent)| Some((unit.first()?.id, percent))).collect();
        Progress { fraction, active }
    }
}

/// A look at a running batch, for the UI timer.
#[derive(Debug)]
pub struct Progress {
    /// Overall `0.0..=1.0`, weighted by file size and including the share of
    /// the files being written; `None` for a batch of empty files.
    pub fraction: Option<f64>,
    /// `(file id, write progress)` of every result being written (a merge:
    /// its first file).
    pub active: Vec<(u64, Option<u8>)>,
}

impl Drop for Batch {
    /// Leaving a batch behind always stops it — no orphan QPDF.
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.take() {
            self.cancel();
            let _ = coordinator.join();
        }
    }
}

fn worker(slot: usize, shared: &BatchShared) {
    let Wiring { qpdf, job: job_object, tx, notify } = &shared.wiring;
    let set_slot = |value: Option<usize>| lock(&shared.slots)[slot] = value;
    loop {
        if shared.cancel.load(Ordering::Relaxed) {
            break;
        }
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        let Some(unit) = shared.units.get(index) else { break };
        shared.percents[slot].store(PERCENT_UNKNOWN, Ordering::Relaxed);
        set_slot(Some(index));
        if !send_all(tx, notify, unit.iter().map(|item| Event::Started { id: item.id })) {
            break;
        }
        let progress = |percent: u8| shared.percents[slot].store(percent, Ordering::Relaxed);
        let ctx = Context { progress: &progress, ..Context::new(job_object, &shared.cancel) };
        let events: Vec<Event> = match &shared.task {
            Task::Process(job) => process(qpdf, job, unit, shared.parallel, &ctx, tx, notify)
                .into_iter()
                .zip(unit)
                .map(|(produced, item)| Event::Finished { id: item.id, produced })
                .collect(),
            Task::Check(password) => unit
                .iter()
                .map(|item| Event::Checked { id: item.id, report: inspect::check(qpdf, &item.path, &item.probe, password.as_ref(), &ctx) })
                .collect(),
        };
        shared.done_files.fetch_add(unit.len(), Ordering::Relaxed);
        shared.done_bytes.fetch_add(unit_bytes(unit), Ordering::Relaxed);
        set_slot(None);
        if !send_all(tx, notify, events) {
            break;
        }
    }
}

/// Run `job` on one unit: what came of each of its files.
fn process(
    qpdf: &Path,
    job: &Job,
    unit: &[BatchItem],
    parallel: usize,
    ctx: &Context,
    tx: &Sender<Event>,
    notify: &Notify,
) -> Vec<Produced> {
    let probes = probe_unit(qpdf, unit, parallel, ctx, tx, notify);
    // "Remove all attachments": which ones the file has.
    let mut keys: Vec<Option<Vec<String>>> = vec![None; unit.len()];
    if job.lists_attachments() {
        for (slot, (item, probe)) in keys.iter_mut().zip(unit.iter().zip(&probes)) {
            // It gives the password only where it is needed.
            match inspect::attachment_keys(qpdf, &item.path, probe, job.input_password.as_ref(), ctx) {
                Ok(found) => *slot = Some(found),
                Err(verdict) => return vec![Produced::ended(verdict); unit.len()],
            }
        }
    }
    let inputs: Vec<Input> = unit
        .iter()
        .zip(&probes)
        .zip(&keys)
        .map(|((item, probe), keys)| Input {
            path: &item.path,
            size: item.size,
            probe,
            range: item.range.as_ref(),
            attachments: keys.as_deref(),
        })
        .collect();
    job.produce(qpdf, &inputs, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_pdfs_in_natural_order() {
        let dir = std::env::temp_dir().join(format!("qd_tasks_add_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("dir");
        for name in ["skan10.pdf", "Skan1.PDF", "skan2.pdf", "notes.txt", r"sub\a.pdf"] {
            std::fs::write(dir.join(name), b"x").expect("write");
        }
        let loose = dir.join("skan2.pdf");
        let paths = vec![dir.clone(), dir.join("missing.pdf"), dir.join("notes.txt"), loose];
        let go = AtomicBool::new(false);
        let Collected { candidates, skipped, beyond, .. } = collect_candidates(paths.clone(), MAX_FILES, &go);
        let names = |candidates: &[Candidate]| -> Vec<String> {
            candidates.iter().map(|c| c.path.strip_prefix(&dir).expect("inside").display().to_string()).collect()
        };
        // The loose duplicate is still a candidate: the list drops it by key.
        assert_eq!(names(&candidates), ["Skan1.PDF", "skan2.pdf", "skan2.pdf", "skan10.pdf", r"sub\a.pdf"]);
        assert_eq!((skipped, beyond), (3, 0), "notes.txt in the folder, the missing file and the loose .txt");
        assert_eq!(candidates[1].key, candidates[2].key);
        assert!(candidates.iter().all(|c| c.size == 1));
        // Past the limit: the first ones in reading order, not in the file system's.
        let few = collect_candidates(paths.clone(), 2, &go);
        assert_eq!((names(&few.candidates), few.beyond), (vec!["Skan1.PDF".to_string(), "skan2.pdf".into()], 3));
        // Stopped: nothing at all, said so.
        let stopped = collect_candidates(paths, MAX_FILES, &AtomicBool::new(true));
        assert!(stopped.cancelled && stopped.candidates.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_waits_for_the_probe_once() {
        let job = Arc::new(JobObject::new(1 << 30, 4).expect("job object"));
        let (tx, _rx) = std::sync::mpsc::channel();
        // No QPDF yet: the queue only waits.
        let prober = Prober::new(job, tx, Arc::new(|| {}), 2);
        let files = |ids: std::ops::Range<u64>| ids.map(|id| (id, PathBuf::from(format!("{id}.pdf")))).collect();
        prober.enqueue(files(0..3));
        prober.enqueue(files(1..5)); // as a Check ending hands every unprobed file back
        let queued: Vec<u64> = prober.state().queue.iter().map(|(id, _)| *id).collect();
        assert_eq!(queued, [0, 1, 2, 3, 4]);
    }
}
