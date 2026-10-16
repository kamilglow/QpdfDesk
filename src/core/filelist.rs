//! The file list: what is on it, in which order, and how far
//! each file has got.
//!
//! **The order of the list is the merge order**, so the list never sorts
//! itself as a whole: each batch (one "Add files", one "Add folder", one drop)
//! arrives already in natural order and is appended at the end; after that only
//! the user moves rows (up/down, to the top/bottom, "Sort by name").
//!
//! Every entry has an `id` that never changes, because background work (the
//! probe, a running batch) reports back by file, while rows can move.

use crate::{t, tn};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::inspect::CheckReport;
use super::job::WorkKey;
use super::outcome::{self, Failure, Verdict};
use super::output::Produced;
use super::ranges::PageRange;
use super::runner::{Finished, RunError};
use super::scandir::sort_naturally;
use super::secret::Secret;

/// A file about to join the list, as found by the add worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// [`super::dedup::canonical_key`] of `path`.
    pub key: PathBuf,
    pub size: u64,
}

/// What the background probe found out about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Not probed yet.
    Pending,
    /// Readable: its page count, and whether it is encrypted without an open
    /// password (restrictions only).
    Ready { pages: u64, restricted: bool },
    /// Needs an open password.
    NeedsPassword,
    /// Not a PDF, damaged beyond repair, unreadable…
    Failed(Failure),
}

impl Probe {
    /// The page count, when known.
    pub fn pages(&self) -> Option<u64> {
        match self {
            Probe::Ready { pages, .. } => Some(*pages),
            _ => None,
        }
    }

    /// Whether the file is known to be encrypted.
    pub fn encrypted(&self) -> bool {
        matches!(self, Probe::NeedsPassword | Probe::Ready { restricted: true, .. })
    }

    /// The password for encrypted inputs, for this file: only a file that
    /// needs one to open gets it — QPDF refuses a file encrypted without an
    /// open password when given a wrong one (measured). Every run of QPDF on
    /// a listed file asks this.
    pub fn password<'a>(&self, password: Option<&'a Secret>) -> Option<&'a Secret> {
        password.filter(|_| *self == Probe::NeedsPassword)
    }

    /// What `qpdf --show-encryption --show-npages <file>` says about a file.
    ///
    /// Measured on 12.4.2: a readable file prints its page count as the first
    /// line, then "File is not encrypted" or the encryption details. A file
    /// that needs an open password exits with 0 too, but without a page count
    /// and with the line "Incorrect password supplied". Both are whole lines
    /// of QPDF's own, never echoed names, so matching them is safe.
    pub fn read(result: &Result<Finished, RunError>) -> Probe {
        let run = match result {
            Ok(run) => run,
            Err(error) => return Probe::Failed(outcome::from_run_error(error)),
        };
        match outcome::judge(run) {
            Verdict::Done | Verdict::DoneWithWarnings => {}
            Verdict::PasswordRequired => return Probe::NeedsPassword,
            Verdict::Failed(failure) => return Probe::Failed(failure),
            Verdict::Cancelled | Verdict::NotSmaller => return Probe::Pending,
        }
        let lines = &run.stdout.lines;
        if lines.iter().any(|line| line == "Incorrect password supplied") {
            return Probe::NeedsPassword;
        }
        match lines.first().and_then(|line| line.trim().parse::<u64>().ok()) {
            Some(pages) => {
                Probe::Ready { pages, restricted: !lines.iter().any(|line| line == "File is not encrypted") }
            }
            None => Probe::Failed(Failure::Other(t!(FailureNoPageCount).into())),
        }
    }
}

/// Where a file stands in the current or last batch.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// Not part of a batch yet.
    Idle,
    /// In the running batch, not started.
    Waiting,
    /// Being processed; the write progress when QPDF reports one.
    Working(Option<u8>),
    /// Done with, one way or another.
    Finished(Produced),
}

/// Where a file stands in a Check — shown instead of its status, as
/// the latest thing done to it, until a batch takes the file again.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    Waiting,
    Running,
    Done(CheckReport),
}

/// One file on the list.
#[derive(Debug)]
pub struct Entry {
    pub id: u64,
    pub path: PathBuf,
    key: PathBuf,
    pub size: u64,
    pub probe: Probe,
    /// Its own pages, in place of the ones chosen for all; `None` = no own pages.
    pub range: Option<PageRange>,
    pub status: Status,
    /// The last Check, until a batch takes the file again.
    pub check: Option<Check>,
}

impl Entry {
    /// Whether the file is finished with — a new Start leaves it alone.
    pub fn settled(&self) -> bool {
        matches!(&self.status, Status::Finished(done) if done.verdict.settled())
    }

    /// What its last finished Check said.
    pub fn check_report(&self) -> Option<&CheckReport> {
        match &self.check {
            Some(Check::Done(report)) => Some(report),
            _ => None,
        }
    }
}

/// The list and its duplicate guard.
#[derive(Default)]
pub struct FileList {
    entries: Vec<Entry>,
    /// The [`super::dedup::canonical_key`] of every entry.
    keys: HashSet<PathBuf>,
    next_id: u64,
    /// What the results on the list were made with: Start with other work
    /// runs on every file again.
    made: Option<Made>,
}

/// The work the results on the list come from: the job, and for a merge the
/// files it took, in order, with their pages.
struct Made {
    job: WorkKey,
    merged_from: Vec<(u64, Option<PageRange>)>,
}

/// The outcome of one batch of additions: the new files are the last `added`
/// entries of the list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Added {
    pub added: usize,
    pub duplicates: usize,
    /// Files left out: the list was full ([`MAX_FILES`]).
    pub beyond: usize,
}

impl std::ops::AddAssign for Added {
    fn add_assign(&mut self, more: Added) {
        self.added += more.added;
        self.duplicates += more.duplicates;
        self.beyond += more.beyond;
    }
}

/// Most files on the list: what was measured to keep the window responsive
/// and light (20 MB). A drive added by mistake (`C:\`) stops here.
pub const MAX_FILES: usize = 10_000;

impl FileList {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }

    /// The entry with `id`.
    pub fn find(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// The entry with `id` and its row index.
    pub fn find_mut(&mut self, id: u64) -> Option<(usize, &mut Entry)> {
        self.entries.iter_mut().enumerate().find(|(_, entry)| entry.id == id)
    }

    pub fn entry_mut(&mut self, index: usize) -> Option<&mut Entry> {
        self.entries.get_mut(index)
    }

    /// Whether a Start would have anything to do.
    pub fn has_pending(&self) -> bool {
        self.entries.iter().any(|entry| !entry.settled())
    }

    /// Mark every file not yet finished with — or every file, for a merge —
    /// as waiting, and return the work of a new batch in list order:
    /// one unit per result, so one per file, or one of them all for a merge.
    pub fn begin_batch(&mut self, merge: bool) -> Vec<Vec<&Entry>> {
        for entry in self.entries.iter_mut().filter(|entry| merge || !entry.settled()) {
            entry.status = Status::Waiting;
            entry.check = None;
        }
        let waiting = self.entries.iter().filter(|entry| entry.status == Status::Waiting);
        if merge {
            let all: Vec<&Entry> = waiting.collect();
            if all.is_empty() { Vec::new() } else { vec![all] }
        } else {
            waiting.map(|entry| vec![entry]).collect()
        }
    }

    /// Whether Start with `job` would make other results than those on the
    /// list: another job, or for a merge other files, in another order, or
    /// with other pages.
    pub fn is_new_work(&self, job: &WorkKey) -> bool {
        self.made.as_ref().is_none_or(|made| made.job != *job || (job.merges() && !self.merges_from(&made.merged_from)))
    }

    /// Before a batch of `job`: results of other work are forgotten — the
    /// new work runs on every file — and what it is made of is kept.
    pub fn prepare(&mut self, job: WorkKey) {
        if !self.is_new_work(&job) {
            return;
        }
        self.reset_results();
        let merged_from = if job.merges() { self.merge_inputs() } else { Vec::new() };
        self.made = Some(Made { job, merged_from });
    }

    /// Forget every result — they belong to another job, and the new one
    /// must run on every file.
    fn reset_results(&mut self) {
        for entry in self.entries.iter_mut().filter(|entry| matches!(entry.status, Status::Finished(_))) {
            entry.status = Status::Idle;
        }
    }

    /// Give the rows at `indices` their own pages; returns the rows
    /// whose pages changed. A result made from other pages of theirs is out
    /// of date: back to idle.
    pub fn set_range(&mut self, indices: &[usize], range: Option<&PageRange>) -> Vec<usize> {
        let shaped = self.made.as_ref().is_some_and(|made| made.job.own_pages());
        let mut changed = Vec::new();
        for &index in indices {
            if let Some(entry) = self.entries.get_mut(index).filter(|entry| entry.range.as_ref() != range) {
                entry.range = range.cloned();
                if shaped && matches!(entry.status, Status::Finished(_)) {
                    entry.status = Status::Idle;
                }
                changed.push(index);
            }
        }
        changed
    }

    /// Mark the rows at `indices` as waiting for a Check and return them, in
    /// list order.
    pub fn begin_check(&mut self, indices: &[usize]) -> Vec<&Entry> {
        let flags = self.flags(indices);
        for (entry, flag) in self.entries.iter_mut().zip(flags) {
            if flag {
                entry.check = Some(Check::Waiting);
            }
        }
        self.entries.iter().filter(|entry| entry.check == Some(Check::Waiting)).collect()
    }

    /// After a Check: files it never got to (a cancel) lose their mark.
    /// Returns their row indices.
    pub fn end_check(&mut self) -> Vec<usize> {
        self.reset_where(|entry| matches!(entry.check, Some(Check::Waiting | Check::Running)), |entry| entry.check = None)
    }

    /// `reset` every entry that is `stale`; their row indices.
    fn reset_where(&mut self, stale: impl Fn(&Entry) -> bool, reset: impl Fn(&mut Entry)) -> Vec<usize> {
        let mut indices = Vec::new();
        for (index, entry) in self.entries.iter_mut().enumerate().filter(|(_, entry)| stale(entry)) {
            reset(entry);
            indices.push(index);
        }
        indices
    }

    /// What a merge of the list is made of: its files in order, each with
    /// its pages.
    fn merge_inputs(&self) -> Vec<(u64, Option<PageRange>)> {
        self.entries.iter().map(|entry| (entry.id, entry.range.clone())).collect()
    }

    /// Whether a merge of the list now would be made of `inputs` — or the
    /// files, their order or their pages have changed since.
    fn merges_from(&self, inputs: &[(u64, Option<PageRange>)]) -> bool {
        self.entries.len() == inputs.len()
            && self.entries.iter().zip(inputs).all(|(entry, (id, range))| entry.id == *id && entry.range == *range)
    }

    /// After a batch: files it never finished (stopped by a cancel) are back
    /// to idle. Returns their row indices.
    pub fn end_batch(&mut self) -> Vec<usize> {
        self.reset_where(|entry| matches!(entry.status, Status::Waiting | Status::Working(_)), |entry| entry.status = Status::Idle)
    }

    /// Append `batch` in the order given (it comes naturally sorted), skipping
    /// files already on the list, and those past [`MAX_FILES`].
    pub fn add(&mut self, batch: Vec<Candidate>) -> Added {
        let mut added = Added::default();
        for Candidate { path, key, size } in batch {
            if self.keys.contains(&key) {
                added.duplicates += 1;
                continue;
            }
            if self.entries.len() >= MAX_FILES {
                added.beyond += 1;
                continue;
            }
            self.keys.insert(key.clone());
            self.next_id += 1;
            added.added += 1;
            self.entries.push(Entry {
                id: self.next_id,
                path,
                key,
                size,
                probe: Probe::Pending,
                range: None,
                status: Status::Idle,
                check: None,
            });
        }
        added
    }

    /// Remove the rows at `indices` (any order, out-of-range ignored); returns
    /// the ids removed, so background work on them can be dropped.
    pub fn remove(&mut self, indices: &[usize]) -> Vec<u64> {
        // One pass: removing rows one by one would shift the rest each time.
        let mut doomed = self.flags(indices).into_iter();
        let mut removed = Vec::new();
        let keys = &mut self.keys;
        self.entries.retain(|entry| {
            if !doomed.next().unwrap_or(false) {
                return true;
            }
            keys.remove(&entry.key);
            removed.push(entry.id);
            false
        });
        removed
    }

    /// Empty the list; returns every id that was on it.
    pub fn clear(&mut self) -> Vec<u64> {
        self.keys.clear();
        self.entries.drain(..).map(|entry| entry.id).collect()
    }

    /// [`move_up`] the selected rows.
    pub fn move_up(&mut self, selected: &[usize]) -> Vec<usize> {
        move_up(&mut self.entries, selected)
    }

    /// [`move_down`] the selected rows.
    pub fn move_down(&mut self, selected: &[usize]) -> Vec<usize> {
        move_down(&mut self.entries, selected)
    }

    /// Move the selected rows to the top (or the bottom), keeping their order
    /// among themselves and the order of the rest. Returns the new indices.
    pub fn move_to_edge(&mut self, selected: &[usize], top: bool) -> Vec<usize> {
        let flags = self.flags(selected);
        let count = flags.iter().filter(|&&f| f).count();
        let (mut picked, mut rest): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
        for (entry, flag) in self.entries.drain(..).zip(flags) {
            if flag {
                picked.push(entry);
            } else {
                rest.push(entry);
            }
        }
        let start = if top { 0 } else { rest.len() };
        if top {
            picked.extend(rest);
            self.entries = picked;
        } else {
            rest.extend(picked);
            self.entries = rest;
        }
        (start..start + count).collect()
    }

    /// Put the whole list in natural path order — on request only.
    pub fn sort_by_name(&mut self) {
        sort_naturally(&mut self.entries, |entry| &entry.path);
    }

    fn flags(&self, selected: &[usize]) -> Vec<bool> {
        flags(self.entries.len(), selected)
    }
}

/// `len` flags, set at the `selected` indices (out-of-range ones ignored).
fn flags(len: usize, selected: &[usize]) -> Vec<bool> {
    let mut flags = vec![false; len];
    for &index in selected {
        if let Some(flag) = flags.get_mut(index) {
            *flag = true;
        }
    }
    flags
}

fn indices(flags: &[bool]) -> Vec<usize> {
    flags.iter().enumerate().filter(|(_, &f)| f).map(|(i, _)| i).collect()
}

/// Move each selected item one place up, as a block: an item stops below the
/// top or below another selected item that cannot move — the list's rows
/// and the pages of the page view. Returns the new indices of the
/// selection.
pub fn move_up<T>(items: &mut [T], selected: &[usize]) -> Vec<usize> {
    let mut flags = flags(items.len(), selected);
    for i in 1..items.len() {
        if flags[i] && !flags[i - 1] {
            items.swap(i - 1, i);
            flags.swap(i - 1, i);
        }
    }
    indices(&flags)
}

/// Remove the items at `selected` (any order, out-of-range ignored) in one
/// pass — the pages of the page view.
pub fn remove_at<T>(items: &mut Vec<T>, selected: &[usize]) {
    let mut doomed = flags(items.len(), selected).into_iter();
    items.retain(|_| !doomed.next().unwrap_or(false));
}

/// [`move_up`], downwards.
pub fn move_down<T>(items: &mut [T], selected: &[usize]) -> Vec<usize> {
    let mut flags = flags(items.len(), selected);
    for i in (0..items.len().saturating_sub(1)).rev() {
        if flags[i] && !flags[i + 1] {
            items.swap(i, i + 1);
            flags.swap(i, i + 1);
        }
    }
    indices(&flags)
}

/// The name shown for `path`: its file name, [`super::printable`].
pub fn display_name(path: &Path) -> String {
    super::printable(&path.file_name().unwrap_or(path.as_os_str()).to_string_lossy())
}

/// A file size for the list: `0 B`, `980 KB`, `1.2 MB`, `3.4 GB` (1 KB = 1024 B).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return tn!(SizeBytes, bytes);
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    // The unit follows the ROUNDED value: 1023.7 KB reads `1.0 MB`, not
    // `1024 KB`.
    while value.round() >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // One decimal only where it says something: `1.2 MB`, never in KB, and
    // never `10.0 MB` (9.97 MB rounds to `10 MB`).
    if unit > 0 && (value * 10.0).round() < 100.0 {
        format!("{value:.1} {}", UNITS[unit]).replace('.', t!(NumberDecimalPoint))
    } else {
        format!("{:.0} {}", value.round(), UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str) -> Candidate {
        Candidate { path: PathBuf::from(name), key: PathBuf::from(name.to_lowercase()), size: 10 }
    }

    fn list(names: &[&str]) -> FileList {
        let mut list = FileList::default();
        list.add(names.iter().map(|n| candidate(n)).collect());
        list
    }

    fn names(list: &FileList) -> Vec<String> {
        list.entries().iter().map(|e| e.path.display().to_string()).collect()
    }

    #[test]
    fn only_a_file_that_needs_the_password_gets_it() {
        let password = Secret::new("open".into()).expect("a password");
        assert!(Probe::NeedsPassword.password(Some(&password)).is_some());
        // Encrypted without an open password: a wrong one makes QPDF refuse it.
        let not_needing = [Probe::Ready { pages: 1, restricted: true }, Probe::Ready { pages: 1, restricted: false }, Probe::Pending, Probe::Failed(Failure::NotPdf)];
        for probe in not_needing {
            assert!(probe.password(Some(&password)).is_none(), "{probe:?}");
        }
        assert!(Probe::NeedsPassword.password(None).is_none());
    }

    #[test]
    fn batches_are_appended_in_the_order_given() {
        let mut list = list(&["b.pdf", "a.pdf"]);
        let added = list.add(vec![candidate("c.pdf"), candidate("B.pdf")]);
        assert_eq!(names(&list), ["b.pdf", "a.pdf", "c.pdf"], "never sorted as a whole");
        assert_eq!(added, Added { added: 1, duplicates: 1, beyond: 0 }, "same key = same file");
    }

    #[test]
    fn the_list_takes_at_most_its_limit() {
        let mut list = list(&[]);
        let many: Vec<Candidate> = (0..MAX_FILES - 1).map(|n| candidate(&format!("{n}.pdf"))).collect();
        assert_eq!(list.add(many).added, MAX_FILES - 1);
        let added = list.add(vec![candidate("x.pdf"), candidate("0.pdf"), candidate("y.pdf"), candidate("z.pdf")]);
        assert_eq!(added, Added { added: 1, duplicates: 1, beyond: 2 }, "a duplicate is no file left out");
        assert_eq!(list.len(), MAX_FILES);
        assert_eq!(list.add(vec![candidate("y.pdf")]).beyond, 1, "the files left out are not known to the list");
    }

    #[test]
    fn ids_are_unique_and_survive_moves() {
        let mut list = list(&["a", "b", "c"]);
        let id_of_c = list.entries()[2].id;
        list.move_to_edge(&[2], true);
        assert_eq!(list.index_of(id_of_c), Some(0));
        assert!(list.find(id_of_c).is_some_and(|entry| entry.path == Path::new("c")) && list.find(u64::MAX).is_none());
        let ids: Vec<u64> = list.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| ids.iter().filter(|x| *x == id).count() == 1));
    }

    #[test]
    fn removed_files_can_be_added_again() {
        let mut list = list(&["a", "b", "c"]);
        let ids: Vec<u64> = list.entries().iter().map(|e| e.id).collect();
        let removed = list.remove(&[2, 0, 7, 0]);
        assert_eq!(removed, [ids[0], ids[2]]);
        assert_eq!(names(&list), ["b"]);
        assert_eq!(list.add(vec![candidate("a")]).added, 1);
        assert_eq!(list.add(vec![candidate("b")]).duplicates, 1, "the kept file is still known");
        let all = list.clear();
        assert_eq!(all.len(), 2);
        assert!(list.is_empty());
        assert_eq!(list.add(vec![candidate("b")]).added, 1, "clear forgets keys too");
    }

    fn finished(verdict: Verdict) -> Status {
        Status::Finished(Produced::ended(verdict))
    }

    #[test]
    fn a_batch_takes_what_is_not_finished_with() {
        let mut list = list(&["a", "b", "c", "d"]);
        list.entry_mut(0).expect("a").status = finished(Verdict::Done);
        list.entry_mut(1).expect("b").status = finished(Verdict::NotSmaller);
        list.entry_mut(2).expect("c").status = finished(Verdict::Cancelled);
        assert!(list.has_pending());
        let units: Vec<Vec<String>> =
            list.begin_batch(false).iter().map(|unit| unit.iter().map(|e| e.path.display().to_string()).collect()).collect();
        assert_eq!(units, [["c"], ["d"]], "one unit per file");
        // The batch started c, then was cancelled.
        list.entry_mut(2).expect("c").status = Status::Working(Some(40));
        assert_eq!(list.end_batch(), [2, 3]);
        assert!(list.entries()[2..].iter().all(|e| e.status == Status::Idle));
        list.entry_mut(2).expect("c").status = finished(Verdict::Done);
        list.entry_mut(3).expect("d").status = finished(Verdict::DoneWithWarnings);
        assert!(!list.has_pending());
        assert!(list.begin_batch(false).is_empty());
        // A merge is one unit of the whole list.
        assert_eq!(list.begin_batch(true).iter().map(Vec::len).collect::<Vec<_>>(), [4]);
        list.end_batch();
        // Another job: every file again.
        list.reset_results();
        assert!(list.entries().iter().all(|e| e.status == Status::Idle));
        assert_eq!(list.begin_batch(false).len(), 4);
        assert!(FileList::default().begin_batch(true).is_empty(), "no unit of nothing");
    }

    #[test]
    fn other_work_runs_on_every_file_again() {
        use crate::core::job::{Files, Job, Pages, Select};
        let pages = |files| Job { pages: Some(Pages { select: Select::All, files, rotate: None }), ..Job::default() }.work_key();
        let mut list = list(&["a", "b", "c"]);
        let optimize = || Job::optimize_lossless().work_key();
        assert!(list.is_new_work(&optimize()), "nothing made yet");
        list.prepare(optimize());
        list.begin_batch(false);
        list.entries.iter_mut().for_each(|e| e.status = finished(Verdict::Done));
        assert!(!list.is_new_work(&optimize()), "the same work");
        assert!(!list.has_pending());
        // Another job: every result is out of date once Start prepares it.
        assert!(list.is_new_work(&Job::default().work_key()));
        list.prepare(Job::default().work_key());
        assert!(list.entries().iter().all(|e| e.status == Status::Idle));
        // Own pages count only for work that takes them.
        list.entries.iter_mut().for_each(|e| e.status = finished(Verdict::Done));
        let range = PageRange::parse("1").expect("range");
        list.set_range(&[0], Some(&range));
        assert!(!list.has_pending(), "repair takes no own pages");
        list.prepare(pages(Files::Each));
        list.entries.iter_mut().for_each(|e| e.status = finished(Verdict::Done));
        assert_eq!(list.set_range(&[1], Some(&range)), [1]);
        assert_eq!(list.entries()[1].status, Status::Idle, "made from other pages");
        assert!(list.entries()[2].settled(), "the others stay done");
        // A merge: the same job over other files, order or pages is new work.
        let merge = || pages(Files::Merge);
        list.prepare(merge());
        assert!(!list.is_new_work(&merge()));
        list.move_down(&[0]);
        assert!(list.is_new_work(&merge()), "another order");
        list.prepare(merge());
        list.remove(&[2]);
        assert!(list.is_new_work(&merge()), "a file fewer");
    }

    #[test]
    fn a_merge_knows_what_it_was_made_of() {
        let mut list = list(&["a", "b", "c"]);
        let made = list.merge_inputs();
        assert!(list.merges_from(&made));
        let range = PageRange::parse("1-2").expect("range");
        assert_eq!(list.set_range(&[0, 2, 7], Some(&range)), [0, 2]);
        assert_eq!(list.set_range(&[0, 1], Some(&range)), [1], "only real changes");
        list.set_range(&[1], None);
        assert_eq!(list.entries()[0].range.as_ref(), Some(&range));
        assert_eq!(list.entries()[1].range, None, "untouched");
        assert!(!list.merges_from(&made), "other pages");
        list.set_range(&[0, 2], None);
        assert!(list.merges_from(&made), "the same pages again");
        list.move_down(&[0]);
        assert!(!list.merges_from(&made), "another order");
        list.move_up(&[1]);
        list.remove(&[2]);
        assert!(!list.merges_from(&made), "a file fewer");
    }

    #[test]
    fn a_check_marks_its_files_until_a_batch_takes_them() {
        let mut list = list(&["a", "b", "c"]);
        let checked: Vec<u64> = list.begin_check(&[2, 0, 9]).iter().map(|e| e.id).collect();
        assert_eq!(checked, [list.entries()[0].id, list.entries()[2].id], "list order");
        list.entry_mut(0).expect("a").check = Some(Check::Done(CheckReport { verdict: Verdict::Done, details: Vec::new() }));
        list.entry_mut(2).expect("c").check = Some(Check::Running);
        assert_eq!(list.end_check(), [2], "the cancelled one");
        assert!(matches!(list.entries()[0].check, Some(Check::Done(_))));
        list.begin_batch(false);
        assert!(list.entries().iter().all(|e| e.check.is_none()), "the batch is newer");
    }

    #[test]
    fn entries_are_found_by_id() {
        let mut list = list(&["a", "b"]);
        let id = list.entries()[1].id;
        assert_eq!(list.find_mut(id).map(|(index, _)| index), Some(1));
        assert!(list.find_mut(id + 100).is_none());
    }

    #[test]
    fn moving_up_moves_the_selection_as_a_block() {
        let mut list = list(&["a", "b", "c", "d", "e"]);
        assert_eq!(list.move_up(&[2, 3]), [1, 2]);
        assert_eq!(names(&list), ["a", "c", "d", "b", "e"]);
        // At the top it stays; a row under it still closes up.
        assert_eq!(list.move_up(&[0, 3]), [0, 2]);
        assert_eq!(names(&list), ["a", "c", "b", "d", "e"]);
    }

    #[test]
    fn moving_down_mirrors_moving_up() {
        let mut list = list(&["a", "b", "c", "d"]);
        assert_eq!(list.move_down(&[0, 1]), [1, 2]);
        assert_eq!(names(&list), ["c", "a", "b", "d"]);
        assert_eq!(list.move_down(&[3]), [3], "the last row stays");
        assert_eq!(list.move_down(&[]), Vec::<usize>::new());
    }

    #[test]
    fn moving_to_the_edges_keeps_both_orders() {
        let mut list = list(&["a", "b", "c", "d", "e"]);
        assert_eq!(list.move_to_edge(&[3, 1], true), [0, 1]);
        assert_eq!(names(&list), ["b", "d", "a", "c", "e"]);
        assert_eq!(list.move_to_edge(&[0], false), [4]);
        assert_eq!(names(&list), ["d", "a", "c", "e", "b"]);
    }

    #[test]
    fn sort_by_name_is_natural_and_only_on_request() {
        let mut list = list(&["skan10.pdf", "Skan1.pdf", "skan2.pdf"]);
        assert_eq!(names(&list), ["skan10.pdf", "Skan1.pdf", "skan2.pdf"]);
        list.sort_by_name();
        assert_eq!(names(&list), ["Skan1.pdf", "skan2.pdf", "skan10.pdf"]);
    }

    fn run(code: i32, stdout: &[&str], stderr: &[&str]) -> Result<Finished, RunError> {
        use super::super::runner::{Captured, Ending};
        let captured = |lines: &[&str]| Captured { lines: lines.iter().map(|l| l.to_string()).collect(), omitted: 0 };
        Ok(Finished {
            ending: Ending::Exited(code),
            stdout: captured(stdout),
            stderr: captured(stderr),
            memory_limit_hit: false,
        })
    }

    #[test]
    fn the_probe_reads_pages_and_encryption() {
        // Real QPDF 12.4.2 output.
        assert_eq!(
            Probe::read(&run(0, &["3", "File is not encrypted"], &[])),
            Probe::Ready { pages: 3, restricted: false }
        );
        assert_eq!(
            Probe::read(&run(0, &["3", "R = 6", "P = -2056", "User password = ", "Supplied password is user password"], &[])),
            Probe::Ready { pages: 3, restricted: true }
        );
        assert_eq!(
            Probe::read(&run(0, &["Incorrect password supplied", "R = 6", "P = -4"], &[])),
            Probe::NeedsPassword
        );
        // Repaired with warnings: still readable.
        assert_eq!(
            Probe::read(&run(3, &["3", "File is not encrypted"], &["WARNING: x.pdf: file is damaged"])),
            Probe::Ready { pages: 3, restricted: false }
        );
    }

    #[test]
    fn the_probe_reports_failures() {
        let not_pdf = run(
            2,
            &[],
            &["WARNING: x.pdf: can't find PDF header", "qpdf: x.pdf: unable to find trailer dictionary while recovering damaged file"],
        );
        assert_eq!(Probe::read(&not_pdf), Probe::Failed(Failure::NotPdf));
        let locked = run(2, &[], &[r"qpdf: open C:\x.pdf: Permission denied"]);
        assert_eq!(Probe::read(&locked), Probe::Failed(Failure::AccessDenied));
        assert!(matches!(Probe::read(&run(0, &["junk"], &[])), Probe::Failed(Failure::Other(_))));
        let too_long = Err(RunError::BadArgument(super::super::runner::ArgError::PathTooLong(PathBuf::from("x"))));
        assert_eq!(Probe::read(&too_long), Probe::Failed(Failure::PathTooLong));
    }

    #[test]
    fn display_names_lose_control_characters() {
        assert_eq!(display_name(Path::new(r"C:\a\umowa.pdf")), "umowa.pdf");
        assert_eq!(display_name(Path::new("tab\there\u{1b}[31m.pdf")), "tabhere[31m.pdf");
    }

    #[test]
    fn sizes_read_like_explorer() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1 KB", "kilobytes are whole, like Explorer");
        assert_eq!(format_size(1536), "2 KB");
        assert_eq!(format_size(412 * 1024), "412 KB");
        assert_eq!(format_size(1_258_291), "1.2 MB");
        assert_eq!(format_size(58 << 20), "58 MB");
        assert_eq!(format_size(u64::MAX), "16777216 TB");
        // Rounding never shows a number that belongs to the next step.
        assert_eq!(format_size((1 << 20) - 1), "1.0 MB", "not 1024 KB");
        assert_eq!(format_size((10 << 20) - 1), "10 MB", "not 10.0 MB");
        assert_eq!(format_size((10 << 30) - 1), "10 GB");
        assert_eq!(format_size(1023 * 1024 + 511), "1023 KB");
    }

    #[test]
    fn sizes_never_need_more_than_seven_characters_below_a_terabyte() {
        // The list's size column is measured on such samples (app/list.rs).
        let mut bytes = 1u64;
        while bytes < 1 << 40 {
            for b in [bytes - 1, bytes, bytes + bytes / 3, bytes * 1023 / 1024] {
                assert!(format_size(b).chars().count() <= 7, "{b}: {}", format_size(b));
            }
            bytes = bytes * 2 + 1;
        }
    }
}
