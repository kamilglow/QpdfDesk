//! The Info and Pages windows: one file at a time, modal. The
//! window shows at once with "Reading the file…"; what the file holds comes
//! from a background read (`tasks::spawn_inspect`) as an event, and so do the
//! attachments pulled out of it. Closing the window stops that work at once;
//! the app waits for it only when it ends. The Command window is one of them
//! too, with nothing to read.
//!
//! Text from the PDF goes into list cells after `@.`, and
//! attachments are shown with the name they get on disk.

use super::*;

use std::collections::HashMap;

use fltk::output::MultilineOutput;
use qpdf_desk::core::attachments::Extracted;
use qpdf_desk::core::job::Input as JobInput;
use qpdf_desk::core::filelist::{self as files, display_name, format_size};
use qpdf_desk::core::inspect::{self, Info, LabelRange};
use qpdf_desk::core::outcome::{Failure, Verdict};
use qpdf_desk::core::ranges::RangeError;
use qpdf_desk::core::sanitize;
use qpdf_desk::tasks::Read;

/// An open Info or Pages window.
pub(super) struct Dialog {
    win: Window,
    /// The request its answers carry; older answers are ignored.
    request: u64,
    /// The file it is about (its list id).
    id: u64,
    kind: Kind,
    /// Its background work.
    errand: Option<Errand>,
}

enum Kind {
    Info(InfoParts),
    Pages(PagesParts),
    /// The command preview: nothing to read, nothing to wait for.
    Command,
}

struct InfoParts {
    details: Browser,
    attachments: Browser,
    extract_selected: Button,
    extract_all: Button,
    show_folder: Button,
    status: Frame,
    info: Option<Info>,
    /// Where the last extraction went.
    folder: Option<PathBuf>,
}

struct PagesParts {
    list: Browser,
    /// ↑, ↓, Remove, Reverse, Reset, OK — off until the pages are known.
    controls: Vec<Button>,
    summary: Frame,
    total: u64,
    labels: Vec<LabelRange>,
    /// The pages in their new order.
    order: Vec<u64>,
}

impl Drop for Dialog {
    fn drop(&mut self) {
        self.win.hide();
        Window::delete(self.win.clone());
    }
}

/// The Info window is this big, the Pages window narrower.
const INFO_W: i32 = 580;
const INFO_H: i32 = 540;
const PAGES_W: i32 = 420;
const PAGES_H: i32 = 540;
/// The key column of the Info details.
const KEY_W: i32 = 130;
/// Most pages the page view lists — every edit redraws them within the UI's
/// budget (10 000 rows ≈ 46 ms, measured); for more, a range is typed.
const PAGE_VIEW_MAX: u64 = 10_000;

/// The file a window is about: the one selected row, or the only file.
fn chosen_file(st: &State, widgets: &mut Widgets) -> Option<(u64, PathBuf, Probe)> {
    let selected = selected_rows(&widgets.list, st.list.len());
    let index = match selected[..] {
        [index] => index,
        [] if st.list.len() == 1 => 0,
        _ => {
            show_status(st, widgets, t!(StatusSelectOne));
            return None;
        }
    };
    st.list.entries().get(index).map(|entry| (entry.id, entry.path.clone(), entry.probe.clone()))
}

/// Open a window of `kind` about the chosen file and start reading it.
fn open(state: &SharedState, widgets: &mut Widgets, read: Read, build: impl FnOnce(&SharedState, &mut Widgets, &str) -> (Window, Kind)) {
    let mut st = state.borrow_mut();
    let Some((id, path, probe)) = chosen_file(&st, widgets) else { return };
    let Some(wiring) = st.wiring() else { return };
    st.requests += 1;
    let request = st.requests;
    // The password for encrypted inputs — used only where it is needed.
    let errand = tasks::spawn_inspect(wiring, request, read, path.clone(), probe, widgets.steps.password());
    drop(st);
    let (win, kind) = build(state, widgets, &display_name(&path));
    state.borrow_mut().dialog = Some(Dialog { win: win.clone(), request, id, kind, errand: Some(errand) });
    show_dialog(state, widgets, win);
}

/// Show `win` and wire its close — the title bar's ✕ or Escape.
fn show_dialog(state: &SharedState, widgets: &Widgets, mut win: Window) {
    let state = state.clone();
    win.set_callback(move |_| close_dialog(&state));
    show_modal(&mut win, Some(&widgets.win));
}

fn close_dialog(state: &SharedState) {
    let mut st = state.borrow_mut();
    let Some(mut dialog) = st.dialog.take() else { return };
    // Its QPDF is stopped, but not waited for: the window goes at once.
    if let Some(errand) = dialog.errand.take() {
        errand.stop();
        st.retired.push(errand);
    }
    st.retired.retain(|errand| !errand.is_finished());
}

fn button(row: &mut Flex, label: &str) -> Button {
    let mut button = fitted_button(row, label);
    button.deactivate();
    button
}

/// A list in the list font, cells split by tabs; `multi`: rows can be
/// selected — the default type selects none.
pub(super) fn list_browser(multi: bool) -> Browser {
    let mut browser = Browser::default();
    if multi {
        browser.set_type(BrowserType::Multi);
    }
    browser.set_text_size(LIST_TEXT);
    browser.set_column_char('\t');
    browser.set_frame(FrameType::DownBox); // flat inset surface, see apply_theme
    browser.set_selection_color(theme().selection);
    browser
}

/// Info: what the selected file holds, and its attachments.
pub(super) fn open_info(state: &SharedState, widgets: &mut Widgets) {
    open(state, widgets, inspect::inspect, |state, widgets, name| {
        let win = window(INFO_W, INFO_H, &t!(InfoTitle, name = name));
        let mut col = Flex::default_fill().column();
        col.set_margin(MARGIN);
        col.set_pad(8);
        let mut details = list_browser(false);
        details.set_column_widths(&[KEY_W]);
        details.add(&plain(t!(DialogReading)));
        let mut title = Frame::default().with_label(t!(StepAttachments));
        title.set_align(Align::Left | Align::Inside);
        title.set_label_font(Font::HelveticaBold);
        col.fixed(&title, 22);
        let attachments = list_browser(true);
        col.fixed(&attachments, 120);
        let mut status = Frame::default();
        status.set_align(Align::Left | Align::Inside | Align::Clip);
        col.fixed(&status, 22);
        let mut row = Flex::default().row();
        row.set_pad(6);
        let extract_selected = button(&mut row, t!(InfoExtractSelected));
        let extract_all = button(&mut row, t!(InfoExtractAll));
        let show_folder = button(&mut row, t!(InfoShowFolder));
        let _filler = Frame::default();
        let close = sized_button(&mut row, t!(ButtonClose));
        row.end();
        col.fixed(&row, ROW_H);
        col.end();
        win.end();

        on_click(&extract_selected, state, widgets, |state, w| extract(state, w, false));
        on_click(&extract_all, state, widgets, |state, w| extract(state, w, true));
        on_click(&show_folder, state, widgets, |state, _| {
            let folder = match &state.borrow().dialog {
                Some(Dialog { kind: Kind::Info(parts), .. }) => parts.folder.clone(),
                _ => None,
            };
            if let Some(folder) = folder {
                reveal_in_explorer(&folder);
            }
        });
        on_click(&close, state, widgets, |state, _| close_dialog(state));
        let parts = InfoParts { details, attachments, extract_selected, extract_all, show_folder, status, info: None, folder: None };
        (win, Kind::Info(parts))
    });
}

/// Arrange pages…: the selected file's pages by number and label, to pick
/// and order as its own pages.
pub(super) fn open_pages(state: &SharedState, widgets: &mut Widgets) {
    if state.borrow().batch.is_some() {
        return;
    }
    open(state, widgets, inspect::page_labels, |state, widgets, name| {
        let win = window(PAGES_W, PAGES_H, &t!(PagesTitle, name = name));
        let mut col = Flex::default_fill().column();
        col.set_margin(MARGIN);
        col.set_pad(8);
        let mut help = Frame::default().with_label(t!(PagesHelp));
        help.set_align(Align::Left | Align::Inside | Align::Wrap);
        col.fixed(&help, 40);
        let mut middle = Flex::default().row();
        middle.set_pad(8);
        let mut list = list_browser(true);
        list.set_column_widths(&[56]);
        list.add(&plain(t!(DialogReading)));
        let mut buttons = Flex::default().column();
        buttons.set_pad(6);
        let mut controls = Vec::new();
        let labels = PAGE_EDITS.each_ref().map(item_label);
        let column = button_width(&labels);
        for (label, (_, _, edit)) in labels.into_iter().zip(PAGE_EDITS) {
            let mut button = Button::default().with_label(label);
            button.deactivate();
            buttons.fixed(&button, ROW_H);
            on_click(&button, state, widgets, move |state, _| edit_pages(state, edit));
            controls.push(button);
        }
        let _filler = Frame::default();
        buttons.end();
        middle.fixed(&buttons, column);
        middle.end();
        let mut summary = Frame::default();
        summary.set_align(Align::Left | Align::Inside | Align::Clip);
        col.fixed(&summary, 24);
        let mut row = Flex::default().row();
        row.set_pad(6);
        let _filler = Frame::default();
        let mut ok = sized_button(&mut row, t!(ButtonOk));
        ok.deactivate();
        let cancel = sized_button(&mut row, t!(ButtonCancel));
        row.end();
        col.fixed(&row, ROW_H);
        col.end();
        win.end();

        // Delete removes the selected pages, as on the file list.
        {
            let state = state.clone();
            list.clone().handle(move |_, event| match event {
                Event::KeyDown if app::event_key() == Key::Delete => {
                    edit_pages(&state, PageEdit::Remove);
                    true
                }
                _ => false,
            });
        }
        on_click(&ok, state, widgets, keep_pages);
        on_click(&cancel, state, widgets, |state, _| close_dialog(state));
        controls.push(ok);
        (win, Kind::Pages(PagesParts { list, controls, summary, total: 0, labels: Vec::new(), order: Vec::new() }))
    });
}

/// Command…: the command for cmd.exe that makes what Start makes of
/// the selected file (or the first) — of the whole list for a merge — with
/// every password as `********`, to copy.
pub(super) fn open_command(state: &SharedState, widgets: &mut Widgets) {
    let (text, notes, title) = {
        let st = state.borrow();
        let Some(qpdf) = st.qpdf.as_ref().map(|q| q.exe.clone()) else { return };
        let job = match read_job(&widgets.steps, &st) {
            Ok(job) => job,
            Err(error) => return show_status(&st, widgets, &error.sentence()),
        };
        let entries = st.list.entries();
        let first = selected_rows(&widgets.list, entries.len()).first().copied().unwrap_or(0);
        let chosen = if job.merges() { entries } else { entries.get(first..=first).unwrap_or_default() };
        let Some(entry) = chosen.first() else { return show_status(&st, widgets, t!(StatusAddFirst)) };
        // The keys are read when the file runs; one stands for them all.
        let keys = [String::from("<key>")];
        let inputs: Vec<JobInput> = chosen
            .iter()
            .map(|e| JobInput { path: &e.path, size: e.size, probe: &e.probe, range: e.range.as_ref(), attachments: Some(&keys[..]) })
            .collect();
        let title = match chosen {
            [_] => display_name(&entry.path),
            all => tn!(CommandMerged, all.len()),
        };
        let (text, mut notes) = match job.command(&qpdf, &inputs) {
            Ok(line) => (line, vec![t!(CommandNote).to_string()]),
            Err(why) => (String::new(), vec![t!(CommandRefused, why = why)]),
        };
        if text.contains("********") {
            notes.push(t!(CommandPasswords).into());
        }
        if job.lists_attachments() {
            notes.push(t!(CommandKeys).into());
        }
        // Whether it needs the password is not known yet.
        if chosen.iter().any(|e| e.probe == Probe::Pending) {
            notes.push(t!(CommandNotChecked).into());
        }
        (text, notes.join("\n"), title)
    };
    let win = window(COMMAND_W, COMMAND_H, &t!(CommandTitle, name = title));
    let mut col = Flex::default_fill().column();
    col.set_margin(MARGIN);
    col.set_pad(8);
    // A read-only text field, not FLTK's text display: that one alone made
    // the executable 26 KB bigger (measured).
    let mut display = MultilineOutput::default();
    display.set_wrap(true);
    display.set_text_font(Font::Courier);
    display.set_text_size(LIST_TEXT);
    display.set_selection_color(theme().selection);
    // Shown without control or direction marks; copied exact.
    display.set_value(&printable(&text));
    let about = muted(&notes);
    col.fixed(&about, 64);
    let mut row = Flex::default().row();
    row.set_pad(6);
    let mut copy = sized_button(&mut row, t!(ButtonCopy));
    set_active(&mut copy, !text.is_empty());
    let _filler = Frame::default();
    let close = sized_button(&mut row, t!(ButtonClose));
    row.end();
    col.fixed(&row, ROW_H);
    col.end();
    win.end();
    on_click(&copy, state, widgets, move |state, w| {
        app::copy(&text);
        show_status(&state.borrow(), w, t!(StatusCommandCopied));
    });
    on_click(&close, state, widgets, |state, _| close_dialog(state));
    state.borrow_mut().dialog = Some(Dialog { win: win.clone(), request: 0, id: 0, kind: Kind::Command, errand: None });
    show_dialog(state, widgets, win);
}

/// The command window's size.
const COMMAND_W: i32 = 620;
const COMMAND_H: i32 = 280;

/// The background read answered.
pub(super) fn inspected(st: &mut State, request: u64, result: Result<Info, Verdict>) {
    let State { dialog, list, .. } = st;
    let Some(dialog) = dialog.as_mut().filter(|d| d.request == request) else { return };
    dialog.errand = None;
    let Some(entry) = list.find(dialog.id) else { return };
    match (&mut dialog.kind, result) {
        (Kind::Info(parts), Ok(info)) => {
            fill_lines(&mut parts.details, info_rows(entry, &info).iter().map(|(key, value)| cells(&[("@b", key), ("", value)])));
            fill_lines(&mut parts.attachments, info.attachments.iter().map(|a| attachment_line(a, None)));
            if !info.attachments.is_empty() {
                parts.extract_selected.activate();
                parts.extract_all.activate();
            }
            set_label(&mut parts.status, &tn!(InfoAttachments, info.attachments.len()), None);
            parts.info = Some(info);
        }
        (Kind::Pages(parts), Ok(info)) => {
            let Some(total) = info.pages.filter(|&n| n > 0) else {
                return fill_lines(&mut parts.list, [plain(t!(PagesCountUnknown))]);
            };
            if total > PAGE_VIEW_MAX {
                return fill_lines(&mut parts.list, [plain(&t!(PagesTooMany, total = total))]);
            }
            parts.total = total;
            parts.labels = info.labels;
            // Its own pages — when they fit the view (`1-z,1-z,…` may not).
            let own = entry.range.as_ref().filter(|range| range.count(total).is_ok_and(|n| n <= PAGE_VIEW_MAX));
            parts.order = own.and_then(|range| range.pages(total).ok()).unwrap_or_else(|| (1..=total).collect());
            show_pages(parts);
            for control in &mut parts.controls {
                control.activate();
            }
        }
        (Kind::Info(parts), Err(verdict)) => fill_lines(&mut parts.details, [cells(&[("@b", t!(InfoCannotRead)), ("", &unreadable(&verdict))])]),
        (Kind::Pages(parts), Err(verdict)) => fill_lines(&mut parts.list, [plain(&unreadable(&verdict))]),
        (Kind::Command, _) => {}
    }
}

/// Whether the Info window has read its file (dev builds: `QD_TOOL=extract`).
#[cfg(debug_assertions)]
pub(super) fn info_read(st: &State) -> bool {
    matches!(&st.dialog, Some(Dialog { kind: Kind::Info(InfoParts { info: Some(_), .. }), .. }))
}

/// Why a file could not be read, and what helps.
fn unreadable(verdict: &Verdict) -> String {
    match verdict {
        Verdict::PasswordRequired => t!(InfoPasswordRequired, field = t!(JobInputPassword)),
        other => other.status(),
    }
}

/// One list line of `(codes, text)` cells: each text after `@.`, so an `@`
/// in it stays literal, and printable, so no tab splits it.
/// Codes: `@b` bold, `@r` right-aligned.
pub(super) fn cells(cells: &[(&str, &str)]) -> String {
    cells.iter().map(|(codes, text)| format!("{codes}@.{}", printable(text))).collect::<Vec<_>>().join("\t")
}

/// A line of one plain cell.
fn plain(text: &str) -> String {
    cells(&[("", text)])
}

/// Replace every line of `browser`.
fn fill_lines(browser: &mut Browser, lines: impl IntoIterator<Item = String>) {
    browser.clear();
    for line in lines {
        browser.add(&line);
    }
    browser.redraw();
}

/// The details of the Info window as `(key, value)`, values from the PDF
/// already made printable by `inspect`.
fn info_rows(entry: &Entry, info: &Info) -> Vec<(String, String)> {
    let yes_no = |yes: bool| if yes { t!(InfoYes) } else { t!(InfoNo) }.to_string();
    let pages = info.pages.or_else(|| entry.probe.pages());
    let mut rows = vec![
        (t!(InfoFile).to_string(), display_name(&entry.path)),
        (t!(InfoFolder).into(), entry.path.parent().map(|p| printable(&p.display().to_string())).unwrap_or_default()),
        (t!(InfoSize).into(), format_size(entry.size)),
        (t!(StepPages).into(), pages.map_or(t!(InfoNotKnown).into(), |p| p.to_string())),
        (t!(InfoPageLabels).into(), labels_summary(&info.labels, pages)),
        (t!(InfoPdfVersion).into(), info.pdf_version.clone().unwrap_or_else(|| t!(InfoNotKnown).into())),
    ];
    let encryption = match &info.encryption {
        None => t!(InfoNone).to_string(),
        Some(encryption) => {
            let mut text = encryption.method.clone();
            if info.needed_password {
                text = t!(InfoOpenPassword, method = text);
            }
            if !encryption.not_allowed.is_empty() {
                text = t!(InfoNotAllowed, method = text, what = encryption.not_allowed.join(", "));
            }
            text
        }
    };
    rows.push((t!(InfoEncryption).into(), encryption));
    rows.push((t!(InfoLinearized).into(), info.linearized.map_or(t!(InfoNotKnown).into(), yes_no)));
    rows.push((t!(InfoForm).into(), yes_no(info.form)));
    rows.push((t!(InfoBookmarks).into(), yes_no(info.bookmarks)));
    rows.push((t!(StepAttachments).into(), info.attachments.len().to_string()));
    rows.extend(info.metadata.iter().map(|(key, value)| (inspect::metadata_name(key).to_string(), value.clone())));
    if let Some(report) = entry.check_report() {
        rows.push((t!(ToolCheck).into(), report.health()));
        rows.extend(report.details.iter().map(|line| (String::new(), line.clone())));
    }
    rows
}

/// "i–iv, 1–186": each range of labels from its first page to its last.
fn labels_summary(ranges: &[LabelRange], pages: Option<u64>) -> String {
    const SHOWN: usize = 6;
    if ranges.is_empty() {
        return t!(InfoNone).into();
    }
    let mut parts: Vec<String> = Vec::new();
    for (i, range) in ranges.iter().enumerate().take(SHOWN) {
        let last = match ranges.get(i + 1) {
            Some(next) => next.first.checked_sub(1),
            None => pages.and_then(|p| p.checked_sub(1)),
        };
        let first_label = inspect::label(ranges, range.first).unwrap_or_default();
        match last.filter(|&last| last > range.first) {
            Some(last) => parts.push(format!("{first_label}–{}", inspect::label(ranges, last).unwrap_or_default())),
            None => parts.push(first_label),
        }
    }
    if ranges.len() > SHOWN {
        parts.push("…".into());
    }
    parts.join(", ")
}

/// One attachment: its name in the PDF, the name it gets on disk when that
/// differs, and what became of it.
fn attachment_line(attachment: &inspect::Attachment, outcome: Option<&Result<PathBuf, Failure>>) -> String {
    let name = printable(&attachment.name);
    let safe = sanitize::file_name(&attachment.name);
    let mut line = if safe == name { name } else { format!("{name}  →  {safe}") };
    if let Some(description) = &attachment.description {
        line.push_str(&format!(" — {description}"));
    }
    match outcome {
        // The name it got, when another file had it already.
        Some(Ok(path)) if display_name(path) != safe => line.push_str(&format!("  ✓ {}", display_name(path))),
        Some(Ok(_)) => line.push_str("  ✓"),
        Some(Err(failure)) => line.push_str(&format!("  ✗ {}", failure.sentence())),
        None => {}
    }
    plain(&line)
}

/// Extract selected / Extract all.
pub(super) fn extract(state: &SharedState, widgets: &mut Widgets, all: bool) {
    let mut st = state.borrow_mut();
    let Some(wiring) = st.wiring() else { return };
    st.requests += 1;
    let request = st.requests;
    let State { dialog, list, .. } = &mut *st;
    let Some(dialog) = dialog.as_mut() else { return };
    let Some(path) = list.find(dialog.id).map(|entry| entry.path.clone()) else { return };
    let Kind::Info(parts) = &mut dialog.kind else { return };
    let Some(info) = &parts.info else { return };
    let chosen: Vec<inspect::Attachment> = if all {
        info.attachments.clone()
    } else {
        selected_rows(&parts.attachments, info.attachments.len()).into_iter().filter_map(|i| info.attachments.get(i).cloned()).collect()
    };
    if chosen.is_empty() {
        set_label(&mut parts.status, t!(InfoSelectAttachments), None);
        return;
    }
    let password = info.password_for(widgets.steps.password());
    for button in [&mut parts.extract_selected, &mut parts.extract_all] {
        button.deactivate();
    }
    set_label(&mut parts.status, t!(InfoExtracting), None);
    dialog.request = request;
    dialog.errand = Some(tasks::spawn_extract(wiring, request, path, password, chosen));
}

/// Attachments were pulled out: say where, and how each one went.
pub(super) fn extracted_files(st: &mut State, request: u64, extracted: Extracted) {
    let Some(dialog) = st.dialog.as_mut().filter(|d| d.request == request) else { return };
    dialog.errand = None;
    let Kind::Info(parts) = &mut dialog.kind else { return };
    let Some(info) = &parts.info else { return };
    fill_lines(&mut parts.attachments, extraction_lines(&info.attachments, &extracted));
    let (saved, asked) = (extracted.saved(), extracted.files.len());
    let mut text = match &extracted.folder {
        Some(folder) => t!(InfoSaved, saved = saved, asked = asked, folder = display_name(folder)),
        None => t!(InfoNothingSaved, asked = asked),
    };
    if extracted.unmarked > 0 {
        text = format!("{text} · {}", t!(InfoUnmarked));
    }
    set_label(&mut parts.status, &text, None);
    parts.folder = extracted.folder;
    set_active(&mut parts.show_folder, parts.folder.is_some());
    for button in [&mut parts.extract_selected, &mut parts.extract_all] {
        button.activate();
    }
}

/// Each attachment's row with how its extraction went — matched by key:
/// names repeat, keys do not.
fn extraction_lines(attachments: &[inspect::Attachment], extracted: &Extracted) -> Vec<String> {
    let outcomes: HashMap<&str, &Result<PathBuf, Failure>> = extracted.files.iter().map(|saved| (saved.key.as_str(), &saved.file)).collect();
    attachments.iter().map(|attachment| attachment_line(attachment, outcomes.get(attachment.key.as_str()).copied())).collect()
}

/// One row of the page view: the page and its label.
fn page_line(page: u64, labels: &[LabelRange]) -> String {
    let label = page.checked_sub(1).and_then(|index| inspect::label(labels, index)).unwrap_or_default();
    cells(&[("@r", &format!("{page}.")), ("", &label)])
}

/// The pages as the file's own range: `None` for all of them in order.
fn own_range(parts: &PagesParts) -> Result<Option<PageRange>, RangeError> {
    if parts.order.iter().copied().eq(1..=parts.total) {
        Ok(None)
    } else {
        PageRange::from_pages(&parts.order).map(Some)
    }
}

/// Show the pages in their order, and what they make.
fn show_pages(parts: &mut PagesParts) {
    let lines: Vec<String> = parts.order.iter().map(|&page| page_line(page, &parts.labels)).collect();
    fill_lines(&mut parts.list, lines);
    show_pages_summary(parts);
}

fn show_pages_summary(parts: &mut PagesParts) {
    let text = match own_range(parts) {
        Ok(None) => tn!(PagesAll, parts.total),
        Ok(Some(range)) => t!(PagesSome, chosen = parts.order.len(), total = parts.total, pages = range.text()),
        Err(_) => t!(PagesShuffled, chosen = parts.order.len(), total = parts.total),
    };
    set_label(&mut parts.summary, &text, None);
}

/// What a button of the page view does.
#[derive(Clone, Copy)]
enum PageEdit {
    Up,
    Down,
    Remove,
    Reverse,
    All,
}

/// The buttons of the page view: an arrow, or a text.
const PAGE_EDITS: [Item<PageEdit>; 5] = [
    ("↑", None, PageEdit::Up),
    ("↓", None, PageEdit::Down),
    ("", Some(Msg::PagesRemove), PageEdit::Remove),
    ("", Some(Msg::PagesReverse), PageEdit::Reverse),
    ("", Some(Msg::PagesRestore), PageEdit::All),
];

fn edit_pages(state: &SharedState, edit: PageEdit) {
    let mut st = state.borrow_mut();
    let Some(Dialog { kind: Kind::Pages(parts), .. }) = st.dialog.as_mut() else { return };
    if parts.order.is_empty() {
        return;
    }
    let selected = selected_rows(&parts.list, parts.order.len());
    match edit {
        PageEdit::Up | PageEdit::Down => {
            let moved = match edit {
                PageEdit::Up => files::move_up(&mut parts.order, &selected),
                _ => files::move_down(&mut parts.order, &selected),
            };
            let (order, labels) = (&parts.order, &parts.labels);
            show_moved(&mut parts.list, &selected, &moved, |list, index| list.set_text(index as i32 + 1, &page_line(order[index], labels)));
            return show_pages_summary(parts);
        }
        PageEdit::Remove if selected.len() >= parts.order.len() => {
            set_label(&mut parts.summary, t!(PagesOneMustStay), None);
            return;
        }
        PageEdit::Remove => files::remove_at(&mut parts.order, &selected),
        PageEdit::Reverse => parts.order.reverse(),
        PageEdit::All => parts.order = (1..=parts.total).collect(),
    }
    show_pages(parts);
}

/// OK in the page view: the pages become the file's own.
fn keep_pages(state: &SharedState, widgets: &mut Widgets) {
    let (index, range, name) = {
        let mut st = state.borrow_mut();
        let Some(Dialog { id, kind: Kind::Pages(parts), .. }) = st.dialog.as_mut() else { return };
        let id = *id;
        let range = match own_range(parts) {
            Ok(range) => range,
            Err(_) => {
                set_label(&mut parts.summary, t!(PagesTooShuffled), None);
                return;
            }
        };
        let Some(index) = st.list.index_of(id) else { return };
        (index, range, display_name(&st.list.entries()[index].path))
    };
    close_dialog(state);
    apply_own_pages(state, widgets, &[index], range.as_ref(), &name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use qpdf_desk::core::attachments::Saved;
    use qpdf_desk::core::filelist::Candidate;
    use qpdf_desk::core::inspect::{Attachment, CheckReport, Encryption};

    fn entry_list() -> FileList {
        let mut list = FileList::default();
        let path = PathBuf::from(r"C:\Docs\umowa.pdf");
        list.add(vec![Candidate { key: path.clone(), path, size: 2048 }]);
        list
    }

    #[test]
    fn info_reads_like_a_list_of_facts() {
        let mut list = entry_list();
        let info = Info {
            pdf_version: Some("1.7".into()),
            pages: Some(12),
            linearized: Some(false),
            encryption: Some(Encryption { method: "AES-256".into(), not_allowed: vec!["printing", "copying"] }),
            form: true,
            labels: vec![LabelRange { first: 0, style: Some('r'), prefix: String::new(), start: 1 }],
            metadata: vec![("Title".into(), "Umowa".into())],
            needed_password: true,
            ..Info::default()
        };
        let report = CheckReport { verdict: Verdict::DoneWithWarnings, details: vec!["WARNING: x".into()] };
        list.entry_mut(0).expect("entry").check = Some(Check::Done(report));
        let rows = info_rows(&list.entries()[0], &info);
        let find = |key: &str| rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(find("File"), Some("umowa.pdf"));
        assert_eq!(find("Folder"), Some(r"C:\Docs"));
        assert_eq!(find("Size"), Some("2 KB"));
        assert_eq!(find("Pages"), Some("12"));
        assert_eq!(find("Page labels"), Some("i–xii"));
        assert_eq!(find("Encryption"), Some("AES-256, open password · not allowed: printing, copying"));
        assert_eq!(find("Linearized"), Some("no"));
        assert_eq!(find("Form"), Some("yes"));
        assert_eq!(find("Title"), Some("Umowa"));
        assert_eq!(find("Check"), Some("Warnings"));
        assert_eq!(rows.last(), Some(&(String::new(), "WARNING: x".to_string())));
    }

    #[test]
    fn label_ranges_are_summed_up() {
        let ranges = [
            LabelRange { first: 0, style: Some('r'), prefix: String::new(), start: 1 },
            LabelRange { first: 4, style: Some('D'), prefix: String::new(), start: 1 },
            LabelRange { first: 10, style: None, prefix: "Back".into(), start: 1 },
        ];
        assert_eq!(labels_summary(&ranges, Some(11)), "i–iv, 1–6, Back");
        assert_eq!(labels_summary(&ranges[..2], None), "i–iv, 1");
        assert_eq!(labels_summary(&[], Some(3)), "none");
    }

    #[test]
    fn attachments_show_the_name_they_get_on_disk() {
        let hostile = Attachment { key: "a00".into(), name: r"..\..\outside.txt".into(), description: None };
        assert_eq!(attachment_line(&hostile, None), r"@...\..\outside.txt  →  outside.txt");
        let plain = Attachment { key: "k".into(), name: "a@b.txt".into(), description: Some("note".into()) };
        assert_eq!(attachment_line(&plain, None), "@.a@b.txt — note", "an @ stays literal after @.");
        let saved = attachment_line(&plain, Some(&Ok(PathBuf::from(r"C:\x\a@b (2).txt"))));
        assert!(saved.ends_with("  ✓ a@b (2).txt"), "{saved}");
        let failed = attachment_line(&plain, Some(&Err(Failure::NoDiskSpace)));
        assert!(failed.ends_with("  ✗ Not enough disk space"), "{failed}");
    }

    #[test]
    fn each_attachment_shows_its_own_outcome_when_names_repeat() {
        // Outcomes matched by name once made both rows show one.
        let twins = [
            Attachment { key: "k1".into(), name: "a.txt".into(), description: None },
            Attachment { key: "k2".into(), name: "a.txt".into(), description: None },
        ];
        let extracted = Extracted {
            folder: Some(PathBuf::from(r"C:\x")),
            files: vec![
                Saved { key: "k1".into(), file: Ok(PathBuf::from(r"C:\x\a.txt")) },
                Saved { key: "k2".into(), file: Err(Failure::NoDiskSpace) },
            ],
            unmarked: 0,
        };
        let lines = extraction_lines(&twins, &extracted);
        assert!(lines[0].ends_with("✓") && lines[1].ends_with("✗ Not enough disk space"), "{lines:?}");
    }

    #[test]
    fn a_page_row_has_its_number_and_label() {
        let labels = [LabelRange { first: 0, style: Some('D'), prefix: "A-".into(), start: 5 }];
        assert_eq!(page_line(3, &labels), "@r@.3.\t@.A-7");
        assert_eq!(page_line(3, &[]), "@r@.3.\t@.");
    }

    #[test]
    fn a_cell_holds_its_text_whatever_it_is() {
        assert_eq!(cells(&[("@b", "Title"), ("", "a\tb@C1\u{202E}")]), "@b@.Title\t@.ab@C1", "no tab splits it, no code or mark acts");
    }
}
