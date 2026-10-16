//! The file list in the window: columns, rows, statuses, moving
//! and removing rows, the tallies.
//!
//! Row rendering uses Browser columns with per-column format codes: `@C<n>`
//! sets the colour, `@B<n>` the background, `@r` right-aligns, and the `@.`
//! terminator ends the format sequence and keeps the rest of the cell literal,
//! so an `@` in a file name stays an `@`. Widths are computed
//! through an injected `measure`, so the rendering is tested without a window.

use super::*;

use std::fmt::Write as _;
use std::path::Path;

use qpdf_desk::core::filelist::{display_name, format_size, Check};
use qpdf_desk::core::outcome::{self, Verdict};

/// Breathing room added to a measured column.
const CELL_PAD: i32 = 14;
/// Never squeeze the name column below this.
const MIN_NAME_W: i32 = 90;
/// Blank space kept between the row number and the file name.
const NUMBER_GAP: i32 = 10;

/// The lock column: an open password is needed / encrypted, but opens
/// without one (restrictions only).
const LOCK_PASSWORD: &str = "🔒";
const LOCK_RESTRICTED: &str = "🔓";
/// The full story of one file for the status line.
pub(super) fn entry_details(entry: &Entry) -> String {
    let name = display_name(&entry.path);
    if let Some(report) = entry.check_report() {
        // Its first word of trouble, in QPDF's words; Info shows them all.
        let trouble = report.details.iter().find(|line| outcome::is_warning(line) || line.starts_with("qpdf: "));
        return match trouble {
            Some(line) => t!(DetailsCheckTrouble, name = name, health = report.health(), line = line),
            None => t!(DetailsCheck, name = name, health = report.health()),
        };
    }
    match &entry.status {
        Status::Finished(done) => {
            let mut text = t!(DetailsStatus, name = name, status = done.verdict.status());
            if let Some(output) = &done.output {
                text.push_str(&format!(" → {}", display_name(output)));
            }
            let warnings = done.details.iter().filter(|line| outcome::is_warning(line)).count();
            if warnings > 0 {
                text = format!("{text} · {}", tn!(DetailsWarnings, warnings));
            }
            text
        }
        _ => {
            let (status, _) = status_text(entry);
            if status.is_empty() {
                name
            } else {
                t!(DetailsStatus, name = name, status = status)
            }
        }
    }
}

/// Which way to move the selected rows.
#[derive(Clone, Copy)]
pub(super) enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

pub(super) fn move_selected(state: &SharedState, widgets: &mut Widgets, step: Move) {
    let mut st = state.borrow_mut();
    if st.batch.is_some() {
        return;
    }
    let selected = selected_rows(&widgets.list, st.list.len());
    if selected.is_empty() {
        return;
    }
    let moved = match step {
        Move::Up => st.list.move_up(&selected),
        Move::Down => st.list.move_down(&selected),
        Move::Top => st.list.move_to_edge(&selected, true),
        Move::Bottom => st.list.move_to_edge(&selected, false),
    };
    // Repainted in the next turns: rows 1 and 10 000 moved repaint them all.
    show_moved(&mut widgets.list, &selected, &moved, |_, index| {
        st.stale.insert(index);
    });
    app::awake();
}

/// Rows at `selected` moved to `moved` (both sorted, non-empty `selected`):
/// `repaint` the rows between the first and the last place a selected row
/// left or reached — only those can have changed, and a key held down must
/// not rebuild 10 000 — and move the selection with them. The file list
/// and the page view.
pub(super) fn show_moved(browser: &mut Browser, selected: &[usize], moved: &[usize], mut repaint: impl FnMut(&mut Browser, usize)) {
    let (Some(&first), Some(&last), Some(&from), Some(&to)) = (moved.first(), moved.last(), selected.first(), selected.last()) else {
        return;
    };
    for index in first.min(from)..=last.max(to) {
        repaint(browser, index);
    }
    for &index in selected {
        browser.deselect(index as i32 + 1);
    }
    for &index in moved {
        browser.select(index as i32 + 1);
    }
    browser.make_visible(first as i32 + 1);
    browser.redraw();
}

/// Sort the list by name; not while a batch runs.
pub(super) fn sort_list(state: &SharedState, widgets: &mut Widgets) {
    let mut st = state.borrow_mut();
    if st.batch.is_some() {
        return;
    }
    // The selection was of files that move.
    for row in selected_rows(&widgets.list, st.shown) {
        widgets.list.deselect(row as i32 + 1);
    }
    st.list.sort_by_name();
    // The same rows and names, in another order: each is painted anew where
    // it is, in the next turns — rebuilding them kept the window busy 50 ms
    // for 10 000 (measured) and scrolled it to the top.
    let shown = st.shown;
    st.stale.extend(0..shown);
    app::awake();
}

/// Remove the selected rows; not while a batch runs.
pub(super) fn remove_selected(state: &SharedState, widgets: &mut Widgets) {
    let mut st = state.borrow_mut();
    if st.batch.is_some() {
        return;
    }
    let selected = selected_rows(&widgets.list, st.list.len());
    if selected.is_empty() {
        return;
    }
    let removed = st.list.remove(&selected);
    if let Some(prober) = &st.prober {
        prober.forget(&removed);
    }
    if st.list.is_empty() {
        st.hover = None;
        rebuild_rows(&mut st, widgets);
    } else {
        take_out_rows(&mut st, &mut widgets.list, &selected);
    }
    update_controls(&st, widgets);
    show_status(&st, widgets, &idle_status(&st));
}

/// The rows at `removed` (sorted) left the list: out of the widget with
/// them, and the rows after the first renumbered in the next turns. The list
/// stays where it was scrolled to — rebuilding its rows put it back at the
/// top.
fn take_out_rows(st: &mut State, list: &mut Browser, removed: &[usize]) {
    let Some(&first) = removed.first() else { return };
    take_out_lines(list, removed, st.shown);
    // How many of `removed` were before `index`.
    let before = |index: usize| removed.partition_point(|&row| row < index);
    st.shown -= before(st.shown);
    // The row under the pointer, where it is now, loses its tint.
    let hovered = st.hover.take().filter(|row| removed.binary_search(row).is_err()).map(|row| row - before(row));
    let cols = layout(list, st);
    let from = if recuts(st, st.cols, cols) { 0 } else { first };
    st.cols = cols;
    set_columns(list, cols);
    // Rows before `from` are where they were; the rest are painted anew.
    st.stale.split_off(&from);
    st.stale.extend((from..st.shown).chain(hovered));
    list.redraw();
    app::awake();
}

/// Take the lines of rows `removed` (sorted) out of `list`, which holds the
/// first `shown` rows — last first, so the others keep their numbers. A list
/// scrolled past its new end scrolls back to it: `Fl_Browser` keeps its
/// position, and the rows taken from the end left empty space.
fn take_out_lines(list: &mut Browser, removed: &[usize], shown: usize) {
    for &row in removed.iter().rev().filter(|&&row| row < shown) {
        list.remove(row as i32 + 1);
    }
    let position = list.position();
    // The last line at the bottom: the furthest the list can scroll.
    list.bottom_line(list.size());
    list.set_position(position.min(list.position()));
}

/// Zero-based indices of the selected rows (`Browser::selected_items` misses
/// a selection made without a "current" line, so every line is asked).
pub(super) fn selected_rows(browser: &Browser, rows: usize) -> Vec<usize> {
    (0..rows).filter(|&i| browser.selected(i as i32 + 1)).collect()
}

/// Open Explorer on the result of the row under the pointer, or on the file
/// itself when it has no result yet. Not below the last row: the list's
/// current line is the last one clicked, which a double-click on the empty
/// space opened.
pub(super) fn reveal_row(state: &SharedState, list: &Browser) {
    let path = {
        let st = state.borrow();
        hovered_row(list, st.cols.row_h).and_then(|i| st.list.entries().get(i)).map(|entry| match &entry.status {
            Status::Finished(done) => done.output.clone().unwrap_or_else(|| entry.path.clone()),
            _ => entry.path.clone(),
        })
    };
    if let Some(path) = path {
        reveal_in_explorer(&path);
    }
}

/// Open Windows Explorer on `path`'s folder with `path` selected. Best-effort.
///
/// Explorer is named by its full path: a bare `explorer.exe` is looked up in
/// the app's own folder FIRST, so an `explorer.exe` lying next to a portable
/// copy in Downloads would run instead (measured in MediaFileValid).
pub(super) fn reveal_in_explorer(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new(platform::windows_dir().join("explorer.exe"))
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();
}

/// Space between the list's rounded frame and what it holds: the list paints
/// its whole box, and 1 px cut into the corners' outline.
const LIST_INSET: i32 = 2;

/// The list in its frame, under the header that names its columns — shown
/// once the list has rows ([`rebuild_rows`]), so the frame never moves.
pub(super) fn framed_list() -> (Flex, Frame, Browser) {
    let mut frame = Flex::default().column();
    frame.set_frame(FrameType::DownBox);
    frame.set_color(Color::Background2);
    frame.set_margin(LIST_INSET);
    frame.set_pad(0);
    let mut head = Frame::default();
    let mut list = list_browser(true);
    // The frame is the box's; a `NoBox` list would be drawn as a `DownBox`.
    list.set_frame(FrameType::FlatBox);
    draw::set_font(Font::Helvetica, LIST_TEXT);
    frame.fixed(&head, draw::height() + 8);
    frame.end();
    head.hide();
    let cells = list.clone();
    head.draw(move |head| draw_head(head, &cells));
    (frame, head, list)
}

/// The header in the list's own cell layout: text 3 px inside each column,
/// as `Fl_Browser` draws a cell; numbers right-aligned over numbers.
fn draw_head(head: &Frame, list: &Browser) {
    let pal = theme();
    let titles = [("", false), (t!(ListHeadName), false), (t!(ListHeadPages), true), (t!(ListHeadSize), true), ("", false), (t!(ListHeadStatus), false)];
    let widths = list.column_widths();
    draw::set_font(Font::Helvetica, LIST_TEXT);
    draw::set_draw_color(Color::from_rgbi(pal.muted));
    let (mut x, end) = (list.x() + list.frame().dx(), head.x() + head.w());
    for (column, (title, right)) in titles.into_iter().enumerate() {
        // The last column takes what is left, as in the list.
        let w = widths.get(column).copied().unwrap_or(end - x);
        let align = if right { Align::Right } else { Align::Left };
        draw::draw_text2(title, x + 3, head.y(), w - 6, head.h(), align | Align::Clip);
        x += w;
    }
    draw::set_draw_color(pal.box_border);
    draw::draw_xyline(head.x(), head.y() + head.h() - 1, end - 1);
}

/// Widths of the columns that do not depend on the window, measured once.
#[derive(Clone, Copy)]
pub(super) struct FixedColumns {
    /// The name's and the status's least: they share what the others leave.
    name: i32,
    pages: i32,
    size: i32,
    lock: i32,
    status: i32,
}

impl FixedColumns {
    pub(super) fn measure() -> FixedColumns {
        Self::measure_with(list_width)
    }

    /// Each column is as wide as the widest thing it shows and its header,
    /// plus padding; a status longer than the status column is cut with `…`
    /// (the whole of it shows in the status line when the row is selected).
    fn measure_with(measure: impl Fn(&str) -> i32) -> FixedColumns {
        let widest = |samples: &[String]| samples.iter().map(|s| measure(s)).max().unwrap_or(0) + CELL_PAD;
        // The longest `format_size` texts below 1 TB (its tests keep them at
        // seven characters in English; digits are equally wide).
        let mut sizes = [1023, 1023 << 10, (99 << 20) / 10, 1023 << 20, (99 << 30) / 10].map(format_size).to_vec();
        sizes.push(t!(ListHeadSize).into());
        let mut statuses = status_samples();
        statuses.push(t!(ListHeadStatus).into());
        FixedColumns {
            name: widest(&[t!(ListHeadName).into()]).max(MIN_NAME_W),
            pages: widest(&["99999".into(), t!(ListHeadPages).into()]),
            size: widest(&sizes),
            lock: widest(&[LOCK_PASSWORD.into(), LOCK_RESTRICTED.into()]),
            status: widest(&statuses),
        }
    }
}

/// The statuses of a run as it goes and as it mostly ends, in the language
/// shown, from the code that shows them: the status column is never cut
/// below them. The rest — a failure's sentence, a check's, "not smaller" —
/// gets what the names leave.
fn status_samples() -> Vec<String> {
    let mut samples: Vec<String> =
        [Verdict::Done, Verdict::DoneWithWarnings, Verdict::PasswordRequired, Verdict::Cancelled].iter().map(Verdict::status).collect();
    samples.extend([t!(StatusWorkingPercent, percent = 100), t!(StatusWorking).into(), t!(StatusWaiting).into(), t!(StatusWaitingCheck).into(), t!(StatusCheckRunning).into()]);
    samples
}

/// Column geometry for the current list.
#[derive(Clone, Copy)]
pub(super) struct Columns {
    num: i32,
    name: i32,
    /// Pixels a name may occupy: the column minus a gap before the next one.
    name_text: i32,
    /// Pixels a status may occupy.
    status_text: i32,
    /// The widest name the columns were laid out for.
    names: i32,
    fixed: FixedColumns,
    /// Height of one row in the list font.
    row_h: i32,
}

/// Whether the rows shown read differently laid out by `new` than by `old`:
/// only a name or a status wider than the narrower of the two cuts is cut
/// differently. The status's cut moves with every pixel of a resize, and
/// repainting every row for it kept the window busy while being dragged.
pub(super) fn recuts(st: &State, old: Columns, new: Columns) -> bool {
    let names = old.name_text != new.name_text && old.names.max(new.names) > old.name_text.min(new.name_text);
    names || (old.status_text != new.status_text && widest_status(st) > old.status_text.min(new.status_text))
}

/// The widest status of the rows shown.
fn widest_status(st: &State) -> i32 {
    st.list.entries().iter().take(st.shown).map(|entry| list_width(&status_text(entry).0)).max().unwrap_or(0)
}

/// Lay out the columns for `rows` entries, the widest of their names
/// `names` pixels, inside the current widget width: the `#` column follows
/// the digit count, the fixed columns their measured widths, the name and
/// the status share the rest ([`split`]). The vertical scrollbar is only
/// subtracted once the list actually scrolls.
pub(super) fn columns(list: &Browser, rows: usize, fixed: FixedColumns, names: i32) -> Columns {
    draw::set_font(Font::Helvetica, LIST_TEXT);
    let row_h = draw::height().max(1);
    let frame = list.frame();
    let visible_rows = ((list.h() - frame.dh()) / row_h).max(1) as usize;
    let scrollbar = if rows > visible_rows {
        let size = list.scrollbar_size();
        if size > 0 { size } else { app::scrollbar_size() }
    } else {
        0
    };
    let num = number_column_w_with(number_digits(rows), list_width);
    let room = list.w() - frame.dw() - scrollbar - num - fixed.pages - fixed.size - fixed.lock;
    let (name, status) = split(room, names, fixed);
    Columns { num, name, name_text: (name - CELL_PAD).max(24), status_text: (status - CELL_PAD).max(0), names, fixed, row_h }
}

/// The name's and the status's columns out of `room` pixels: the name as
/// wide as the widest name needs, as long as the status keeps its least; the
/// status the rest — before a run it is empty, and long names were cut while
/// it held room for its longest text.
fn split(room: i32, names: i32, fixed: FixedColumns) -> (i32, i32) {
    let name = (names + CELL_PAD).min(room - fixed.status).max(fixed.name);
    (name, room - name)
}

/// [`columns`] for the rows the list will have and every name of the list
/// and of the files on their way into it, with its measured fixed columns.
pub(super) fn layout(list: &Browser, st: &State) -> Columns {
    let queued = st.incoming.iter().flat_map(|incoming| incoming.candidates.as_slice()).map(|candidate| &candidate.path);
    columns(list, st.rows_ahead(), st.cols.fixed, widest_name(st.list.entries().iter().map(|entry| &entry.path).chain(queued)))
}

/// [`layout`] for a new size of the list: the names are as they were.
pub(super) fn resized(list: &Browser, st: &State) -> Columns {
    columns(list, st.rows_ahead(), st.cols.fixed, st.cols.names)
}

/// The widest of the names of `paths`.
fn widest_name<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> i32 {
    paths.map(|path| list_width(&display_name(path))).max().unwrap_or(0)
}

/// Width of `text` in the list font, in whole pixels: the sum of its
/// characters' widths, each asked of FLTK once. FLTK asks Windows anew for
/// every text longer than one character — cutting 2 000 long names to their
/// column kept the window busy 53 ms, measuring 10 000 for the widest 66 ms
/// (measured); the sum is a pixel from it at most (tested), well inside a
/// cell's padding.
fn list_width(text: &str) -> i32 {
    CHAR_WIDTHS.with_borrow_mut(|widths| text.chars().map(|c| widths.of(c)).sum::<f64>()).ceil() as i32
}

/// Widths of single characters in the list font, measured once each.
struct CharWidths {
    ascii: [f64; 128],
    other: HashMap<char, f64>,
}

impl CharWidths {
    fn of(&mut self, c: char) -> f64 {
        let measure = || {
            draw::set_font(Font::Helvetica, LIST_TEXT);
            draw::width(c.encode_utf8(&mut [0; 4]))
        };
        match self.ascii.get_mut(c as usize) {
            Some(known) if *known >= 0.0 => *known,
            Some(unknown) => {
                *unknown = measure();
                *unknown
            }
            None => *self.other.entry(c).or_insert_with(measure),
        }
    }
}

thread_local! {
    static CHAR_WIDTHS: RefCell<CharWidths> = RefCell::new(CharWidths { ascii: [-1.0; 128], other: HashMap::new() });
}

pub(super) fn set_columns(list: &mut Browser, cols: Columns) {
    list.set_column_widths(&[cols.num, cols.name, cols.fixed.pages, cols.fixed.size, cols.fixed.lock]);
}

/// Digits in the largest row number (list length), minimum 1.
fn number_digits(count: usize) -> usize {
    count.max(1).to_string().len()
}

/// Width of the `#` column: the widest number it will hold plus a gap.
fn number_column_w_with(digits: usize, measure: impl Fn(&str) -> i32) -> i32 {
    // "8" is the widest digit in practice; the row label is "<number>.".
    measure(&format!("{}.", "8".repeat(digits.max(1)))) + NUMBER_GAP
}

/// Take every row down (the list's shape changed): [`paint_rows`] puts them
/// up anew in the next turns of the event loop.
pub(super) fn rebuild_rows(st: &mut State, widgets: &mut Widgets) {
    // The header names the columns of rows; an empty list has none.
    let rows = !st.list.is_empty();
    if widgets.list_head.visible() != rows {
        set_shown(&mut widgets.list_head, rows);
        widgets.list_box.layout();
    }
    st.cols = layout(&widgets.list, st);
    set_columns(&mut widgets.list, st.cols);
    widgets.list.clear();
    st.shown = 0;
    st.stale.clear();
    if let (true, Some(problem)) = (st.list.is_empty(), &st.qpdf_problem) {
        // No usable QPDF: said where the eye goes first.
        widgets.list.add("");
        widgets.list.add(&cells(&[(&format!("@c@b@C{}", theme().failed), problem)]));
    } else if st.list.is_empty() {
        // An inviting drop zone instead of a blank void.
        let muted = format!("@c@C{}", theme().muted);
        widgets.list.add("");
        widgets.list.add(&cells(&[(&muted, t!(ListEmptyDrag))]));
        widgets.list.add(&cells(&[(&muted, t!(ListEmptyFolder))]));
    } else {
        app::awake();
    }
    widgets.list.redraw();
}

/// Files just came onto the list: the columns are sized for them. The rows
/// shown stay unless the new files changed how a name or a status is cut (a
/// longer name, the scrollbar appeared, the `#` column grew) — or the list
/// was empty and shows its drop zone.
pub(super) fn fit_rows(st: &mut State, widgets: &mut Widgets) {
    // Only the rows not up yet are new: every name of 10 000 each turn of an
    // addition took 10 ms of it (measured).
    let new = widest_name(st.list.entries().iter().skip(st.shown).map(|entry| &entry.path));
    let cols = columns(&widgets.list, st.rows_ahead(), st.cols.fixed, st.cols.names.max(new));
    if st.shown == 0 || recuts(st, st.cols, cols) {
        return rebuild_rows(st, widgets);
    }
    st.cols = cols;
    set_columns(&mut widgets.list, cols);
}

/// Put up the rows not shown yet, then repaint the changed ones
/// (`State::stale`) — [`ROWS_PER_TURN`] at most: 10 000 rows at once
/// kept the window busy 46–61 ms. A row shows its entry as it is when
/// painted, so a later turn paints it no worse. Whether rows are left for the
/// next turn.
pub(super) fn paint_rows(st: &mut State, list: &mut Browser) -> bool {
    let end = st.list.len().min(st.shown + ROWS_PER_TURN);
    let mut budget = ROWS_PER_TURN - end.saturating_sub(st.shown);
    if st.shown < end {
        for (index, entry) in st.list.entries().iter().enumerate().take(end).skip(st.shown) {
            list.add(&row_line(index + 1, entry, st.cols, st.hover == Some(index)));
        }
        st.shown = end;
        list.redraw();
    }
    // Rows not up yet are painted as they are when they come.
    let shown = st.shown;
    st.stale.split_off(&shown);
    while budget > 0 {
        let Some(index) = st.stale.pop_first() else { break };
        repaint_row(st, list, index);
        budget -= 1;
    }
    st.shown < st.list.len() || !st.stale.is_empty()
}

/// Repaint one row in place (progress, verdicts, probe results, hover) —
/// once it is up.
pub(super) fn repaint_row(st: &State, list: &mut Browser, index: usize) {
    if let Some(entry) = st.list.entries().get(index).filter(|_| index < st.shown) {
        list.set_text(index as i32 + 1, &row_line(index + 1, entry, st.cols, st.hover == Some(index)));
    }
}

/// Zero-based index of the row under the mouse pointer, if any — derived from
/// the uniform row height, since `Fl_Browser` has no "item at position".
/// None below the last row or over the scrollbar.
fn hovered_row(list: &Browser, row_h: i32) -> Option<usize> {
    let scrollbar = list.scrollbar();
    if scrollbar.visible() && app::event_inside_widget(&scrollbar) {
        return None;
    }
    let content_top = list.y() + list.frame().dy();
    let offset = app::event_y() - content_top + list.position();
    if offset < 0 {
        return None;
    }
    let row = (offset / row_h.max(1)) as usize;
    (row < list.size() as usize).then_some(row)
}

/// Move the hover highlight to the row under the pointer, repainting only the
/// rows that changed.
pub(super) fn update_hover(state: &SharedState, widgets: &mut Widgets, pointer_inside: bool) {
    let Ok(mut st) = state.try_borrow_mut() else { return };
    let row = pointer_inside
        .then(|| hovered_row(&widgets.list, st.cols.row_h))
        .flatten()
        .filter(|&row| row < st.list.len()); // the empty-list hint is not a row
    if st.hover == row {
        return; // this runs on every mouse move
    }
    let previous = st.hover;
    st.hover = row;
    for index in [previous, row].into_iter().flatten() {
        repaint_row(&st, &mut widgets.list, index);
    }
}

/// How a status reads at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Plain,
    Muted,
    Done,
    Warning,
    Failed,
    Accent,
}

/// The status column's text for an entry, and its tone — the single
/// source of those strings.
fn status_text(entry: &Entry) -> (String, Tone) {
    let fixed = |text: &str, tone| (text.to_string(), tone);
    // A Check is the latest thing done to the file until a batch takes it.
    match &entry.check {
        Some(Check::Waiting) => return fixed(t!(StatusWaitingCheck), Tone::Muted),
        Some(Check::Running) => return fixed(t!(StatusCheckRunning), Tone::Plain),
        Some(Check::Done(report)) => return (t!(StatusCheck, health = report.health()), tone(&report.verdict)),
        None => {}
    }
    match &entry.status {
        // Before any batch the probe speaks.
        Status::Idle => match &entry.probe {
            Probe::Pending | Probe::Ready { .. } => match &entry.range {
                Some(range) => (t!(StatusOwnPages, pages = range.text()), Tone::Muted),
                None => fixed("", Tone::Plain),
            },
            Probe::NeedsPassword => (Verdict::PasswordRequired.status(), Tone::Accent),
            Probe::Failed(failure) => fixed(failure.sentence(), Tone::Failed),
        },
        Status::Waiting => fixed(t!(StatusWaiting), Tone::Muted),
        Status::Working(None) => fixed(t!(StatusWorking), Tone::Plain),
        Status::Working(Some(percent)) => (t!(StatusWorkingPercent, percent = percent), Tone::Plain),
        Status::Finished(done) => (done.verdict.status(), tone(&done.verdict)),
    }
}

/// How a verdict — of the job or of a Check — reads at a glance.
fn tone(verdict: &Verdict) -> Tone {
    match verdict {
        Verdict::Done | Verdict::NotSmaller => Tone::Done,
        Verdict::DoneWithWarnings => Tone::Warning,
        Verdict::PasswordRequired => Tone::Accent,
        Verdict::Cancelled => Tone::Muted,
        Verdict::Failed(_) => Tone::Failed,
    }
}

/// "Done 6 · Warnings 1 · Failed 1" — the counters of the bottom row, from
/// the finished files; empty when nothing has finished.
pub(super) fn tallies_text(entries: &[Entry]) -> String {
    let mut counts = [0usize; 6];
    for entry in entries {
        if let Status::Finished(done) = &entry.status {
            let slot = match done.verdict {
                Verdict::Done => 0,
                Verdict::DoneWithWarnings => 1,
                Verdict::NotSmaller => 2,
                Verdict::Failed(_) => 3,
                Verdict::PasswordRequired => 4,
                Verdict::Cancelled => 5,
            };
            counts[slot] += 1;
        }
    }
    let names = [t!(TallyDone), t!(TallyWarnings), t!(TallyNotSmaller), t!(TallyFailed), t!(TallyPassword), t!(TallyCancelled)];
    report::counts_text(names.into_iter().zip(counts).filter(|(_, count)| *count > 0))
}

/// Render one row with the real list font.
fn row_line(number: usize, entry: &Entry, cols: Columns, hover: bool) -> String {
    row_line_with(number, entry, cols.name_text, cols.status_text, &theme(), hover, list_width)
}

/// One row as `#\tname\tpages\tsize\tlock\tstatus` with per-cell format codes;
/// `@.` ends each cell's codes and keeps the rest literal. The measurement is
/// injected, so this is testable without an FLTK graphics context.
fn row_line_with(
    number: usize,
    entry: &Entry,
    name_budget: i32,
    status_budget: i32,
    pal: &Palette,
    hover: bool,
    measure: impl Fn(&str) -> i32,
) -> String {
    let color = |tone: Tone| match tone {
        Tone::Plain => None,
        Tone::Muted => Some(pal.muted),
        Tone::Done => Some(pal.done),
        Tone::Warning => Some(pal.warning),
        Tone::Failed => Some(pal.failed),
        Tone::Accent => Some(pal.accent),
    };
    // A waiting row is muted as a whole; otherwise only the status speaks.
    let row_tone = if entry.status == Status::Waiting { Tone::Muted } else { Tone::Plain };
    let (pages, pages_tone) = match entry.probe {
        Probe::Pending => ("…".to_string(), Tone::Muted),
        Probe::Ready { pages, .. } => (pages.to_string(), row_tone),
        Probe::NeedsPassword | Probe::Failed(_) => ("—".to_string(), Tone::Muted),
    };
    let lock = match entry.probe {
        Probe::NeedsPassword => LOCK_PASSWORD,
        Probe::Ready { restricted: true, .. } => LOCK_RESTRICTED,
        _ => "",
    };
    let (status, status_tone) = status_text(entry);
    let status_tone = if row_tone == Tone::Muted { row_tone } else { status_tone };
    let cells = [
        (row_tone, true, format!("{number}.")),
        (row_tone, false, ellipsize_with(&display_name(&entry.path), name_budget, Cut::Middle, &measure)),
        (pages_tone, true, pages),
        (row_tone, true, format_size(entry.size)),
        (Tone::Plain, false, lock.to_string()),
        (status_tone, false, ellipsize_with(&status, status_budget, Cut::End, &measure)),
    ];
    // Format codes are parsed per column, so each cell repeats its prefix.
    let mut out = String::with_capacity(cells.iter().map(|(_, _, text)| text.len() + 24).sum());
    for (i, (tone, right, text)) in cells.iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        if hover {
            let _ = write!(out, "@B{}", pal.hover_bg);
        }
        if let Some(c) = color(*tone) {
            let _ = write!(out, "@C{c}");
        }
        if *right {
            out.push_str("@r");
        }
        out.push_str("@.");
        out.push_str(text);
    }
    out
}

/// Where [`ellipsize_with`] cuts a text too long for its place.
#[derive(Clone, Copy)]
enum Cut {
    /// A sentence: its start says it.
    End,
    /// A file name: names often differ only at their end (`-1`, a date, a
    /// number), so its start and its end stay.
    Middle,
}

/// Truncate `text` to `max_px` pixels, marking the cut with `…`.
fn ellipsize_with(text: &str, max_px: i32, cut: Cut, measure: impl Fn(&str) -> i32) -> String {
    if measure(text) <= max_px {
        return text.to_string();
    }
    // Byte offsets of each character boundary, so the kept parts are plain slices.
    let bounds: Vec<usize> = text.char_indices().map(|(at, _)| at).chain(std::iter::once(text.len())).collect();
    let chars = bounds.len() - 1;
    // `kept` characters of the text around the ellipsis.
    let cut_to = |kept: usize| {
        let tail = match cut {
            Cut::End => 0,
            Cut::Middle => kept / 2,
        };
        format!("{}…{}", &text[..bounds[kept - tail]], &text[bounds[chars - tail]..])
    };
    // Binary search for the most characters that still fit with the ellipsis.
    let (mut lo, mut hi) = (0usize, chars);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if measure(&cut_to(mid)) <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut_to(lo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qpdf_desk::core::filelist::Candidate;
    use qpdf_desk::core::outcome::Failure;
    use qpdf_desk::core::output::Produced;

    /// Fake text measurement: every character is 10 px wide.
    fn measure(text: &str) -> i32 {
        text.chars().count() as i32 * 10
    }
    const WIDE: i32 = 10_000;

    fn pal() -> Palette {
        palette(false)
    }

    /// A list holding one file in the given probe and status state.
    fn one(name: &str, size: u64, probe: Probe, status: Status) -> FileList {
        let mut list = FileList::default();
        list.add(vec![Candidate { path: PathBuf::from(name), key: PathBuf::from(name), size }]);
        let entry = list.entry_mut(0).expect("entry");
        entry.probe = probe;
        entry.status = status;
        list
    }

    fn finished(verdict: Verdict) -> Status {
        Status::Finished(Produced::ended(verdict))
    }

    fn line(list: &FileList, hover: bool) -> String {
        row_line_with(1, &list.entries()[0], WIDE, WIDE, &pal(), hover, measure)
    }

    #[test]
    fn statuses_read_as_shown() {
        let cases = [
            (Probe::Pending, Status::Idle, "", Tone::Plain),
            (Probe::Ready { pages: 3, restricted: false }, Status::Idle, "", Tone::Plain),
            (Probe::NeedsPassword, Status::Idle, "Password required", Tone::Accent),
            (Probe::Failed(Failure::NotPdf), Status::Idle, "Not a PDF file", Tone::Failed),
            (Probe::Pending, Status::Waiting, "Waiting", Tone::Muted),
            (Probe::Pending, Status::Working(None), "Working…", Tone::Plain),
            (Probe::Pending, Status::Working(Some(63)), "Working… 63%", Tone::Plain),
            (Probe::Pending, finished(Verdict::Done), "Done", Tone::Done),
            (Probe::Pending, finished(Verdict::DoneWithWarnings), "Done with warnings", Tone::Warning),
            (Probe::Pending, finished(Verdict::NotSmaller), "Not smaller (kept original)", Tone::Done),
            (Probe::Pending, finished(Verdict::PasswordRequired), "Password required", Tone::Accent),
            (Probe::Pending, finished(Verdict::Cancelled), "Cancelled", Tone::Muted),
            (
                Probe::Pending,
                finished(Verdict::Failed(Failure::Hung)),
                "Failed: QPDF stopped responding",
                Tone::Failed,
            ),
        ];
        for (probe, status, text, tone) in cases {
            let list = one("a.pdf", 1, probe, status);
            assert_eq!(status_text(&list.entries()[0]), (text.to_string(), tone));
        }
    }

    #[test]
    fn a_check_speaks_over_the_status_until_a_batch_takes_the_file() {
        use qpdf_desk::core::inspect::CheckReport;
        let mut list = one("a.pdf", 1, Probe::Ready { pages: 3, restricted: false }, finished(Verdict::Done));
        let entry = list.entry_mut(0).expect("entry");
        entry.range = Some(PageRange::parse("3,1").expect("range"));
        assert_eq!(status_text(entry), ("Done".to_string(), Tone::Done));
        entry.check = Some(Check::Running);
        assert_eq!(status_text(entry), ("Checking…".to_string(), Tone::Plain));
        let report = CheckReport { verdict: Verdict::DoneWithWarnings, details: vec!["WARNING: a.pdf: file is damaged".into()] };
        entry.check = Some(Check::Done(report));
        assert_eq!(status_text(entry), ("Check: Warnings".to_string(), Tone::Warning));
        assert_eq!(entry_details(entry), "a.pdf: Check: Warnings — WARNING: a.pdf: file is damaged");
        entry.check = None;
        entry.status = Status::Idle;
        assert_eq!(status_text(entry), ("Own pages: 3,1".to_string(), Tone::Muted));
    }

    #[test]
    fn a_done_row_has_every_column() {
        let list = one(
            "umowa@2026.pdf",
            412 * 1024,
            Probe::Ready { pages: 6, restricted: false },
            finished(Verdict::Done),
        );
        let green = pal().done;
        assert_eq!(
            line(&list, false),
            format!("@r@.1.\t@.umowa@2026.pdf\t@r@.6\t@r@.412 KB\t@.\t@C{green}@.Done"),
            "an `@` in the name stays literal after `@.`"
        );
    }

    #[test]
    fn probe_results_fill_the_pages_and_lock_columns() {
        let muted = pal().muted;
        let pending = one("a.pdf", 10, Probe::Pending, Status::Idle);
        let text = line(&pending, false);
        assert!(text.contains(&format!("\t@C{muted}@r@.…\t")), "{text}");
        let locked = one("a.pdf", 10, Probe::NeedsPassword, Status::Idle);
        let text = line(&locked, false);
        assert!(text.contains(&format!("@C{muted}@r@.—\t")), "{text}");
        assert!(text.contains(&format!("\t@.{LOCK_PASSWORD}\t")), "{text}");
        let restricted = one("a.pdf", 10, Probe::Ready { pages: 2, restricted: true }, Status::Idle);
        assert!(line(&restricted, false).contains(&format!("\t@.{LOCK_RESTRICTED}\t")));
    }

    #[test]
    fn a_waiting_row_is_muted_as_a_whole() {
        let list = one("a.pdf", 10, Probe::Ready { pages: 1, restricted: false }, Status::Waiting);
        let muted = pal().muted;
        let text = line(&list, false);
        // Every cell with text is muted (the empty lock cell has no colour).
        for cell in text.split('\t').filter(|cell| !cell.ends_with("@.")) {
            assert!(cell.starts_with(&format!("@C{muted}")), "{cell}");
        }
    }

    #[test]
    fn hover_tints_every_cell() {
        let list = one("a.pdf", 10, Probe::Pending, Status::Idle);
        let bg = pal().hover_bg;
        assert!(line(&list, true).split('\t').all(|cell| cell.starts_with(&format!("@B{bg}"))));
    }

    #[test]
    fn long_failure_sentences_are_cut_to_the_column() {
        let list = one("a.pdf", 10, Probe::Pending, finished(Verdict::Failed(Failure::AccessDenied)));
        let text = row_line_with(1, &list.entries()[0], WIDE, 100, &pal(), false, measure);
        let status = text.rsplit("@.").next().expect("status cell");
        assert_eq!(status, "Failed: A…", "10 characters of 10 px, the ellipsis included");
    }

    #[test]
    fn the_status_column_never_cuts_the_statuses_of_a_run() {
        let cols = FixedColumns::measure_with(measure);
        let samples = status_samples();
        for sample in &samples {
            assert!(measure(sample) + CELL_PAD <= cols.status, "{sample}");
        }
        // And they are the texts `status_text` shows for a run.
        let statuses = [Status::Waiting, Status::Working(None), Status::Working(Some(100)), finished(Verdict::Done), finished(Verdict::DoneWithWarnings), finished(Verdict::PasswordRequired), finished(Verdict::Cancelled)];
        for status in statuses {
            let list = one("a.pdf", 1, Probe::Pending, status);
            let (text, _) = status_text(&list.entries()[0]);
            assert!(samples.contains(&text), "{text}");
        }
    }

    #[test]
    fn every_column_fits_its_header() {
        let cols = FixedColumns::measure_with(measure);
        let fits = |title: &str, width: i32| assert!(measure(title) + CELL_PAD <= width, "{title}");
        fits(t!(ListHeadName), cols.name);
        fits(t!(ListHeadPages), cols.pages);
        fits(t!(ListHeadSize), cols.size);
        fits(t!(ListHeadStatus), cols.status);
    }

    #[test]
    fn the_name_takes_what_it_needs_and_the_status_the_rest() {
        let fixed = FixedColumns { name: 90, pages: 0, size: 0, lock: 0, status: 150 };
        // Short names: the status gets the room they leave.
        assert_eq!(split(600, 200, fixed), (200 + CELL_PAD, 400 - CELL_PAD));
        // Long names: every pixel but the status's least — the empty status
        // column held 260 px while the names were cut at 275 (a user's screenshot).
        assert_eq!(split(600, 700, fixed), (450, 150));
        // A narrow window: the name keeps its least, the status is cut.
        assert_eq!(split(200, 700, fixed), (90, 110));
    }

    #[test]
    fn tallies_count_only_what_happened() {
        let mut list = FileList::default();
        let names = ["a", "b", "c", "d", "e"];
        list.add(names.iter().map(|n| Candidate { path: PathBuf::from(n), key: PathBuf::from(n), size: 1 }).collect());
        assert_eq!(tallies_text(list.entries()), "");
        let verdicts = [Verdict::Done, Verdict::Done, Verdict::Failed(Failure::NotPdf), Verdict::NotSmaller];
        for (i, verdict) in verdicts.into_iter().enumerate() {
            list.entry_mut(i).expect("entry").status = finished(verdict);
        }
        assert_eq!(tallies_text(list.entries()), "Done 2 · Not smaller 1 · Failed 1");
    }

    #[test]
    fn long_texts_get_an_ellipsis_on_character_boundaries() {
        assert_eq!(ellipsize_with("short.pdf", 200, Cut::End, measure), "short.pdf");
        assert_eq!(ellipsize_with("abcdefghij", 50, Cut::End, measure), "abcd…");
        assert_eq!(ellipsize_with("abcdef", 10, Cut::End, measure), "…");
        assert_eq!(ellipsize_with("ŻÓŁĆ.pdf", 30, Cut::End, measure), "ŻÓ…");
    }

    #[test]
    fn rows_taken_out_leave_the_list_where_it_was_scrolled() {
        on_fltk_thread(|| {
            let mut list = Browser::new(0, 0, 300, 200, None);
            let fill = |list: &mut Browser| (1..=300).for_each(|row| list.add(&row.to_string()));
            fill(&mut list);
            list.set_position(2_000);
            // Above the view, in it, below it; the 300th not painted yet.
            take_out_lines(&mut list, &[0, 5, 120, 250, 299], 299);
            assert_eq!(list.position(), 2_000, "scrolled as it was");
            assert_eq!((list.size(), list.text(1), list.text(5), list.text(296)), (296, Some("2".into()), Some("7".into()), Some("300".into())));
            // Rebuilding the rows, as Remove did, put the list back at the top.
            list.clear();
            fill(&mut list);
            assert_eq!(list.position(), 0);
        });
    }

    #[test]
    fn rows_taken_from_the_end_leave_no_empty_space() {
        on_fltk_thread(|| {
            let mut list = Browser::new(0, 0, 300, 200, None);
            (1..=300).for_each(|row| list.add(&row.to_string()));
            list.bottom_line(300);
            take_out_lines(&mut list, &(250..300).collect::<Vec<_>>(), 300);
            let scrolled = list.position();
            list.bottom_line(250);
            assert_eq!(scrolled, list.position(), "the last row at the bottom");
            // All rows fit: back at the top.
            take_out_lines(&mut list, &(3..250).collect::<Vec<_>>(), 250);
            assert_eq!(list.position(), 0);
        });
    }

    #[test]
    fn a_summed_width_is_the_measured_one() {
        // Kerning pairs (AV, To, We), Polish letters, a dash and the widest glyphs.
        on_fltk_thread(|| {
            let texts = ["Bilet - WH69213052-1.pdf", "AVATAR To We.pdf", "Zażółć gęślą jaźń — ŻÓŁW.pdf", "WWWWMMMM@@@@.pdf", "iiiilll111.pdf", "Done with warnings"];
            for text in texts {
                draw::set_font(Font::Helvetica, LIST_TEXT);
                let (summed, whole) = (list_width(text), width(text));
                assert!(whole > 50 && (summed - whole).abs() <= 1, "{text}: summed {summed}, measured {whole}");
            }
        });
    }

    #[test]
    fn a_long_name_keeps_its_start_and_its_end() {
        assert_eq!(ellipsize_with("short.pdf", 200, Cut::Middle, measure), "short.pdf");
        // Two names that differ only at their end stay apart.
        assert_eq!(ellipsize_with("Bilet - WH69213052-1.pdf", 150, Cut::Middle, measure), "Bilet -…2-1.pdf");
        assert_eq!(ellipsize_with("Bilet - WH69213053.pdf", 150, Cut::Middle, measure), "Bilet -…053.pdf");
        assert_eq!(ellipsize_with("ŻÓŁĆ.pdf", 40, Cut::Middle, measure), "ŻÓ…f");
        assert_eq!(ellipsize_with("abcdef", 10, Cut::Middle, measure), "…");
    }

    #[test]
    fn the_number_column_follows_the_list_length() {
        assert_eq!(number_digits(0), 1);
        assert_eq!(number_digits(127), 3);
        assert_eq!(number_column_w_with(3, measure), 40 + NUMBER_GAP);
    }
}
