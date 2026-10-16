//! FLTK front-end — the whole window, in parts: this module (the
//! window, its theme, the toolbar, Start ⇄ Cancel and Check, the progress, the
//! events of the background work), [`list`] (the file list), [`steps`] (the
//! steps panel), [`profiles`] (the profile menu), [`dialogs`] (the Info,
//! Pages and Command windows), [`modal`] (questions, a line of text, About)
//! and [`dnd`] (dragging files onto the window). The parts share one `State`
//! and one `Widgets`.
//!
//! All work happens in `tasks` (finding QPDF, adding files, the probe, the
//! batch); the window only reacts to its events, drained in the main loop
//! after every `awake`, and — only while a batch runs — to a timer that reads
//! the write progress (no timer when idle, so no processor use). The list is
//! the model in `core::filelist`; rows are re-rendered from it. Labels that may carry a
//! path double every `@`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use fltk::{
    app::{self, App, Scheme},
    browser::{Browser, BrowserType},
    button::Button,
    dialog::{NativeFileChooser, NativeFileChooserType},
    draw,
    enums::{Align, Color, ColorDepth, Event, Font, FrameType, Key},
    frame::Frame,
    group::Flex,
    image::RgbImage,
    menu::MenuButton,
    misc::Progress,
    prelude::*,
    window::Window,
};

use qpdf_desk::core::filelist::{display_name, Added, Candidate, Check, Entry, FileList, Probe, Status, MAX_FILES};
use qpdf_desk::core::outcome;
use qpdf_desk::core::job::{Step, WorkKey};
use qpdf_desk::text::Msg;
use qpdf_desk::{t, tn};
use qpdf_desk::core::printable;
use qpdf_desk::core::ranges::PageRange;
use qpdf_desk::core::qpdf::{Missing, Qpdf, MIN_VERSION};
use qpdf_desk::icon;
use qpdf_desk::instance::{InstanceLock, LockError};
use qpdf_desk::platform::{self, JobObject, StayAwake};
use qpdf_desk::report;
use qpdf_desk::settings::{self, Settings};
use qpdf_desk::tasks::{self, Batch, BatchItem, Collected, Errand, Event as Work, Notify, Prober, Task, Unit, Wiring};

mod dialogs;
mod dnd;
#[cfg(debug_assertions)]
mod fit;
#[cfg(debug_assertions)]
mod lag;
mod list;
mod modal;
mod profiles;
mod steps;

use dialogs::*;
use dnd::Target;
use list::*;
use modal::*;
use profiles::*;
use steps::*;

/// Set by the title bar's "About QpdfDesk…" or F1; the main loop opens About
/// (the menu item arrives inside Windows' message handling, where no window
/// may be opened yet).
static ABOUT_ASKED: AtomicBool = AtomicBool::new(false);

/// The size the window opens at, and its smallest: what the layout is
/// designed to survive — the steps and a three-line result under them (520
/// high clipped it), and room for longer translations of the options. A
/// size the user set is remembered in `settings.txt`.
const WINDOW_W: i32 = 800;
const WINDOW_H: i32 = 600;
const MARGIN: i32 = 12;
const ROW_H: i32 = 30;
const BTN_W: i32 = 100;
const SMALL_BTN_W: i32 = 36;
/// Room around a button's label: a button is its label's width plus this.
const BUTTON_PAD: i32 = 28;
/// Room a label needs beyond its text: FLTK draws an inside label 3 px in
/// from each side (measured: “optional” lost its last letter with 4).
const LABEL_PAD: i32 = 10;
/// A menu button's ▾, on top of [`BUTTON_PAD`].
const MENU_ARROW: i32 = 16;
const PROGRESS_W: i32 = 220;
/// List text size; also drives row height and all width measurements.
const LIST_TEXT: i32 = 14;
/// How often the running files' progress is read — only during a batch.
const TICK: f64 = 0.5;

/// Every colour that depends on the light/dark theme, in one table and
/// resolved once at startup. Row colours are kept as raw bits because they are
/// embedded in Browser format codes (`@C{n}`).
#[derive(Clone, Copy)]
struct Palette {
    /// The dark theme (for the native title bars of new windows).
    dark: bool,
    /// Window surface, list/input surface and text, as `(r, g, b)`.
    surface: (u8, u8, u8),
    surface2: (u8, u8, u8),
    text: (u8, u8, u8),
    /// Selected list row.
    selection: Color,
    /// Empty part of the progress bar.
    trough: Color,
    /// How far a control's fill is lifted towards white above its surface,
    /// and the controls' 1 px outline — Windows 11 control values.
    box_lift: f32,
    box_border: Color,
    /// Muted gray for hints and waiting rows.
    muted: u32,
    /// Status colours: done, done with warnings, failed, password.
    done: u32,
    warning: u32,
    failed: u32,
    accent: u32,
    /// The badge of the warning mark beside a question.
    mark: u32,
    /// Subtle background of the row under the mouse pointer.
    hover_bg: u32,
}

fn palette(dark: bool) -> Palette {
    let bits = |r, g, b| Color::from_rgb(r, g, b).bits();
    if dark {
        Palette {
            dark,
            surface: (0x20, 0x20, 0x20),
            surface2: (0x2B, 0x2B, 0x2B),
            text: (0xF0, 0xF0, 0xF0),
            selection: Color::from_rgb(0x2C, 0x4B, 0x6E),
            trough: Color::from_rgb(0x3A, 0x3A, 0x3A),
            box_lift: 0.06, // #202020 → #2D2D2D buttons
            box_border: Color::from_rgb(0x40, 0x40, 0x40),
            muted: bits(0x9A, 0x9A, 0x9A),
            done: bits(0x6C, 0xCB, 0x5F),
            warning: bits(0xFC, 0xE1, 0x00),
            failed: bits(0xFF, 0x99, 0xA4),
            accent: bits(0x60, 0xCD, 0xFF),
            mark: bits(0xFC, 0xE1, 0x00),
            hover_bg: bits(0x2E, 0x2E, 0x2E),
        }
    } else {
        Palette {
            dark,
            surface: (0xF3, 0xF3, 0xF3), // Windows 11 "mica" gray
            surface2: (0xFF, 0xFF, 0xFF),
            text: (0x1B, 0x1B, 0x1B),
            selection: Color::from_rgb(0xCC, 0xE0, 0xF5),
            trough: Color::from_rgb(0xD8, 0xD8, 0xD8),
            box_lift: 0.67, // #F3F3F3 → #FBFBFB buttons
            box_border: Color::from_rgb(0xCF, 0xCF, 0xCF),
            muted: bits(0x70, 0x70, 0x70),
            done: bits(0x0F, 0x7B, 0x0F),
            warning: bits(0x9D, 0x5D, 0x00),
            failed: bits(0xC4, 0x2B, 0x1C),
            accent: bits(0x00, 0x5F, 0xB8),
            mark: bits(0xFF, 0xC8, 0x3D),
            hover_bg: bits(0xEF, 0xF3, 0xF9),
        }
    }
}

/// A batch in flight.
struct RunningBatch {
    batch: Batch,
    /// Write progress of the first running file, as of the last tick.
    current: Option<u8>,
    /// The row of every file in the batch: the list is fixed while it runs,
    /// and a search per event of a 10 000-file merge would stall the window.
    rows: HashMap<u64, usize>,
    /// Keeps the machine awake for as long as the batch exists (released on
    /// completion, cancel or exit — never held while idle).
    _stay_awake: StayAwake,
}

/// Application state shared by callbacks (single-threaded, behind `RefCell`).
struct State {
    list: FileList,
    /// The QPDF in use, once found.
    qpdf: Option<Qpdf>,
    /// Why there is none, once the search has ended without one.
    qpdf_problem: Option<String>,
    /// The one Job Object of every QPDF process.
    job_object: Option<Arc<JobObject>>,
    prober: Option<Prober>,
    batch: Option<RunningBatch>,
    /// Additions still being collected in the background, or still queued
    /// in `incoming`.
    adding: usize,
    /// Stops the additions being collected now ([`cancel_adding`]).
    add_stop: Arc<AtomicBool>,
    /// Additions collected and waiting for the list, in the order they came.
    incoming: std::collections::VecDeque<Incoming>,
    /// Rows of the list up in the list widget; the rest come in the next
    /// turns of the event loop ([`paint_rows`]).
    shown: usize,
    /// Rows to repaint in the next turns of the event loop.
    stale: std::collections::BTreeSet<usize>,
    tx: Sender<Work>,
    rx: Receiver<Work>,
    notify: Notify,
    /// Last folder used in a file dialog (persisted).
    last_folder: Option<PathBuf>,
    threads: usize,
    /// Index of the row under the mouse pointer, if any.
    hover: Option<usize>,
    /// The layout the rows were last rendered with — kept in step by
    /// `rebuild_rows`, `fit_rows` and the resize callback.
    cols: Columns,
    /// The job as the steps panel sets it, when it can run — refreshed
    /// on every edit, never polled.
    job: Option<WorkKey>,
    /// The overlay file, once chosen, and what the probe found about it.
    overlay: Option<(PathBuf, Probe)>,
    /// Files the Attachments step embeds.
    attach: Vec<PathBuf>,
    /// The Info or Pages window, while open.
    dialog: Option<Dialog>,
    /// Numbers the dialogs' requests, so a late answer to a closed one is
    /// recognised.
    requests: u64,
    /// Dev builds: what to do as soon as everything is ready — `start` the
    /// batch (`QD_START`), or open a tool (`QD_TOOL=check|info|pages|extract|command|about|clear|sort`).
    #[cfg(debug_assertions)]
    autostart: Option<String>,
    /// Dev builds (`QD_TOOL=extract`): Extract all once Info has read the file.
    #[cfg(debug_assertions)]
    extract_when_read: bool,
    /// Errands of closed windows, stopped but not yet joined: a window closes
    /// at once, and the app waits for them only when it ends.
    retired: Vec<Errand>,
    /// The app's folder (`settings.txt`, `profiles.txt`), when known.
    data_dir: Option<PathBuf>,
    /// The saved profiles, by name, and the one last loaded or saved.
    profiles: Vec<qpdf_desk::profiles::Profile>,
    profile: Option<String>,
    /// The steps panel as it started: "Default", under every profile.
    defaults: Vec<(String, String)>,
    /// The panel as its profile (or Default) was last loaded or saved: what
    /// differs from it is marked `*`.
    baseline: Vec<(String, String)>,
}

/// One addition (a drop, a pick) on its way into the list.
struct Incoming {
    candidates: std::vec::IntoIter<Candidate>,
    skipped: usize,
    /// So far; `beyond` starts with the files the worker left out.
    tally: Added,
}

impl From<Collected> for Incoming {
    fn from(collected: Collected) -> Incoming {
        let tally = Added { beyond: collected.beyond, ..Added::default() };
        Incoming { candidates: collected.candidates.into_iter(), skipped: collected.skipped, tally }
    }
}

/// Most rows added or repainted in one turn of the event loop: 10 000 at once
/// kept the window busy 58 ms when added and 61 ms when a batch took them (the
/// limit is 50, measured); 2 000 take about 12 ms.
const ROWS_PER_TURN: usize = 2_000;

impl State {
    /// The rows the list will have once every queued addition is in — the
    /// columns are sized for them at once, so the rows already shown need
    /// not be cut anew on the way (a `#` column one digit wider).
    fn rows_ahead(&self) -> usize {
        (self.list.len() + self.incoming.iter().map(|incoming| incoming.candidates.len()).sum::<usize>()).min(MAX_FILES)
    }

    /// Take the next files of the queued additions into the list (`None`
    /// when none are queued), with the message once an addition is all in.
    fn take_incoming(&mut self) -> Option<Option<String>> {
        let incoming = self.incoming.front_mut()?;
        let chunk: Vec<Candidate> = incoming.candidates.by_ref().take(ROWS_PER_TURN).collect();
        let done = incoming.candidates.len() == 0;
        let added = self.list.add(chunk);
        let new = added.added;
        incoming.tally += added;
        let message = done.then(|| added_message(&incoming.tally, incoming.skipped));
        if done {
            self.incoming.pop_front();
            self.adding = self.adding.saturating_sub(1);
        }
        // New files are the last ones (looking each up by id would be
        // quadratic — 265 ms on the UI thread for 10 000, measured).
        let first_new = self.list.len() - new;
        let fresh: Vec<(u64, PathBuf)> = self.list.entries()[first_new..].iter().map(|e| (e.id, e.path.clone())).collect();
        if let Some(prober) = &self.prober {
            prober.enqueue(fresh);
        }
        Some(message)
    }
    /// The row of file `id` and its entry.
    fn entry(&mut self, id: u64) -> Option<(usize, &mut Entry)> {
        match self.batch.as_ref().and_then(|running| running.rows.get(&id).copied()) {
            Some(index) => self.list.entry_mut(index).map(|entry| (index, entry)),
            None => self.list.find_mut(id),
        }
    }

    /// `change` the entry of file `id` (when still on the list), and repaint
    /// its row in the next turns.
    fn touch(&mut self, id: u64, change: impl FnOnce(&mut Entry)) {
        if let Some((index, entry)) = self.entry(id) {
            change(entry);
            self.stale.insert(index);
        }
    }

    /// Whether some file has pages of its own.
    fn own_pages(&self) -> bool {
        self.list.entries().iter().any(|entry| entry.range.is_some())
    }

    /// Give the rows at `indices` their own pages: a result made from
    /// other pages of theirs is out of date.
    fn set_own_pages(&mut self, indices: &[usize], range: Option<&PageRange>) {
        let changed = self.list.set_range(indices, range);
        // Repainted in the next turns: Ctrl+A over 10 000 rows.
        self.stale.extend(changed.iter().copied());
        app::awake();
    }

    /// What background work needs, once QPDF is known.
    fn wiring(&self) -> Option<Wiring> {
        let qpdf = self.qpdf.as_ref()?.exe.clone();
        let job = Arc::clone(self.job_object.as_ref()?);
        Some(Wiring { qpdf, job, tx: self.tx.clone(), notify: Arc::clone(&self.notify) })
    }

    /// Probe the overlay file, once QPDF is known.
    fn probe_overlay(&self) {
        if let (Some(wiring), Some((path, Probe::Pending))) = (self.wiring(), &self.overlay) {
            tasks::spawn_probe(wiring, path.clone());
        }
    }
}

type SharedState = Rc<RefCell<State>>;

/// Cheap-to-clone handles to the widgets the callbacks and the timer touch.
#[derive(Clone)]
struct Widgets {
    add_files: Button,
    add_folder: Button,
    remove: Button,
    clear: Button,
    up: Button,
    down: Button,
    sort: Button,
    /// Check ▾: check the selected files, every file, save the report.
    check: MenuButton,
    /// Info about the selected file; Enter on the list does the same.
    info: Button,
    /// The list's frame, holding its header and the list.
    list_box: Flex,
    list_head: Frame,
    list: Browser,
    /// The steps panel, the options area and the password row — fixed
    /// while a batch runs.
    steps_panel: Flex,
    options: Flex,
    password_row: Flex,
    steps: Steps,
    start: Button,
    progress: Progress,
    /// Bottom row, re-laid out whenever the tallies change width.
    status_row: Flex,
    status: Frame,
    tallies: Frame,
    win: Window,
}

/// Build the main window and run the FLTK event loop until the window closes.
pub fn run() {
    // Oxy for its control shapes (the dropdown's slim arrow and separator);
    // its glossy gradient boxes are replaced by flat ones in `apply_theme`.
    let app = App::default().with_scheme(Scheme::Oxy);

    // Match the Windows light/dark preference before ANY widget appears — the
    // "already running" message below included (unthemed, it came out as a
    // gray 1990s box). Dev builds accept QD_THEME=dark|light, so either theme
    // can be checked without flipping the whole desktop's setting.
    let dark = match std::env::var("QD_THEME").as_deref() {
        Ok("dark") if cfg!(debug_assertions) => true,
        Ok("light") if cfg!(debug_assertions) => false,
        _ => system_prefers_dark(),
    };
    let pal = palette(dark);
    apply_theme(pal);

    // Data files (settings, lock) live next to the executable.
    // Persisted preferences; any problem silently means defaults.
    let app_dir = settings::data_dir();
    let saved = app_dir.as_deref().map(Settings::load).unwrap_or_default();
    // The language, before any text is shown: settings.txt, else Windows'.
    // Dev builds accept QD_LANG=<code>.
    let chosen = std::env::var("QD_LANG").ok().filter(|_| cfg!(debug_assertions)).or_else(|| saved.language.clone());
    qpdf_desk::text::init(chosen.as_deref(), platform::ui_language());

    // Single instance: a second copy would race the first for outputs.
    let _lock = match app_dir.as_deref().map(InstanceLock::acquire) {
        Some(Err(LockError::AlreadyRunning)) => {
            message(t!(AlreadyRunning));
            return;
        }
        // No lock dir / lock unavailable → run unguarded rather than block.
        Some(Ok(lock)) => Some(lock),
        _ => None,
    };

    // Size is remembered, position is not: always open centered on the screen
    // the mouse is on. Predictable on a multi-monitor desk, and immune to the
    // restore-drift a saved position suffers from.
    //
    // The centered position is worked out BEFORE the window is created, so the
    // very first frame the user sees is already in place (`center_on_monitor`
    // afterwards only trims any rounding leftovers).
    let (w, h) = saved.window_size.unwrap_or((WINDOW_W, WINDOW_H));
    let (w, h) = (w.max(WINDOW_W), h.max(WINDOW_H));
    let (x, y) = centered_client_pos(w, h).unwrap_or_else(|| centered_on_active_screen(w, h));

    let mut win = Window::new(x, y, w, h, "");
    win.set_label(&app_title(None));
    win.set_icon(window_icon());
    win.make_resizable(true);

    // Vertical layout: [ toolbar ] [ list (grows) | steps ] [ options of the
    // shown step ] [ password for encrypted inputs ] [ Start · progress ·
    // status · tallies ].
    let mut col = Flex::default_fill().column();
    col.set_margin(MARGIN);
    col.set_pad(MARGIN - 4);

    let mut toolbar = Flex::default().row();
    toolbar.set_pad(6);
    let add_files = Button::default().with_label(t!(ToolAddFiles));
    let add_folder = Button::default().with_label(t!(ToolAddFolder));
    let remove = Button::default().with_label(t!(ToolRemove));
    let clear = Button::default().with_label(t!(ToolClear));
    let gap = Frame::default();
    let mut up = Button::default().with_label("↑");
    up.set_tooltip(t!(ToolUpTip));
    let mut down = Button::default().with_label("↓");
    down.set_tooltip(t!(ToolDownTip));
    let sort = fitted_button(&mut toolbar, t!(ToolSort));
    let _spacer = Frame::default();
    // The read-only tools, on the right.
    let mut check = MenuButton::default().with_label(t!(ToolCheck));
    for item in [t!(ItemCheckSelected), t!(ItemCheckEvery), t!(ItemCheckReport)] {
        check.add_choice(item);
    }
    check.set_tooltip(t!(ToolCheckTip));
    let mut info = fitted_button(&mut toolbar, t!(ToolInfo));
    info.set_tooltip(t!(ToolInfoTip));
    toolbar.fixed(&check, text_width(t!(ToolCheck)) + BUTTON_PAD + MENU_ARROW);
    // Each pair as wide as the wider of the two, whatever the language.
    for pair in [[&add_files, &add_folder], [&remove, &clear]] {
        let width = button_width(&pair.map(|button| button.label()).each_ref().map(String::as_str));
        for button in pair {
            toolbar.fixed(button, width);
        }
    }
    toolbar.fixed(&gap, 4);
    toolbar.fixed(&up, SMALL_BTN_W);
    toolbar.fixed(&down, SMALL_BTN_W);
    toolbar.end();
    col.fixed(&toolbar, ROW_H);

    let mut middle = Flex::default().row();
    middle.set_pad(MARGIN - 4);
    let (list_box, list_head, list) = framed_list();
    let (steps_panel, options, password_row, steps) = build_steps(&mut middle, &mut col);

    // Bottom row: Start ⇄ Cancel, the overall bar, what is happening, the
    // counters derived from the list.
    let mut status_row = Flex::default().row();
    status_row.set_pad(MARGIN);
    let start = Button::default().with_label(t!(ButtonStart));
    let mut progress = Progress::default();
    // Rounded and borderless: the fill is drawn with the same box, and an
    // outline would run across the blue.
    progress.set_frame(FrameType::RFlatBox);
    progress.set_minimum(0.0);
    progress.set_maximum(1.0);
    progress.set_color(pal.trough);
    let [r, g, b] = icon::BLUE; // same accent as the app icon
    progress.set_selection_color(Color::from_rgb(r, g, b));
    progress.set_label("");
    let mut status = Frame::default();
    // Clip: a long message must not run under the tallies.
    status.set_align(Align::Left | Align::Inside | Align::Clip);
    let mut tallies = Frame::default();
    tallies.set_align(Align::Right | Align::Inside);
    status_row.fixed(&start, button_width(&[t!(ButtonStart), t!(ButtonRepair), t!(ButtonCancel)]));
    status_row.fixed(&progress, PROGRESS_W);
    // Sized to its text in `refresh_tallies`. Never 0: Flex reads a fixed size
    // of 0 as "flexible", and the empty frame took half the row (measured).
    status_row.fixed(&tallies, 1);
    status_row.end();
    col.fixed(&status_row, ROW_H);

    col.end();
    win.resizable(&col);
    win.end();
    win.size_range(WINDOW_W, WINDOW_H, 0, 0);
    // Without this the position passed to `Window::new` is only a hint and the
    // OS picks its own spot, so the window would first appear elsewhere and
    // then jump into place.
    win.force_position(true);
    show_centered(&mut win, None, false);
    // About lives in the title bar's menu (right click, or Alt+Space) — no
    // button takes room for it — and on F1.
    win32::add_about_item(win.raw_handle());
    // SAFETY: `dispatch` hands every event it does not answer on with the
    // window pointer FLTK gave it.
    unsafe { app::event_dispatch(dispatch) };

    // Background work: one channel for every event, `awake` to wake the loop.
    let (tx, rx) = mpsc::channel();
    let notify: Notify = Arc::new(app::awake);
    // One Job Object for every QPDF process; the search for
    // QPDF runs in the background, so the window shows at once.
    let job_object = JobObject::new(saved.qpdf_memory_limit(platform::total_memory()), saved.qpdf_process_limit())
        .map(Arc::new);
    let (prober, qpdf_problem) = match &job_object {
        Ok(job) => {
            tasks::spawn_locate(Arc::clone(job), tx.clone(), Arc::clone(&notify));
            (Some(Prober::new(Arc::clone(job), tx.clone(), Arc::clone(&notify), saved.threads())), None)
        }
        Err(e) => (None, Some(t!(QpdfCannotRun, why = e))),
    };

    let mut widgets = Widgets {
        add_files,
        add_folder,
        remove,
        clear,
        up,
        down,
        sort,
        check,
        info,
        list_box,
        list_head,
        list,
        steps_panel,
        options,
        password_row,
        steps,
        start,
        progress,
        status_row,
        status,
        tallies,
        win: win.clone(),
    };
    let state: SharedState = Rc::new(RefCell::new(State {
        cols: columns(&widgets.list, 0, FixedColumns::measure(), 0),
        list: FileList::default(),
        qpdf: None,
        qpdf_problem,
        job_object: job_object.ok(),
        prober,
        batch: None,
        adding: 0,
        add_stop: Arc::new(AtomicBool::new(false)),
        incoming: std::collections::VecDeque::new(),
        shown: 0,
        stale: std::collections::BTreeSet::new(),
        tx,
        rx,
        notify,
        last_folder: saved.last_folder.clone(),
        threads: saved.threads(),
        hover: None,
        job: None,
        overlay: None,
        attach: Vec::new(),
        dialog: None,
        requests: 0,
        #[cfg(debug_assertions)]
        autostart: None,
        #[cfg(debug_assertions)]
        extract_when_read: false,
        retired: Vec::new(),
        profiles: app_dir.as_deref().map(qpdf_desk::profiles::load).unwrap_or_default(),
        data_dir: app_dir.clone(),
        profile: None,
        defaults: Vec::new(),
        baseline: Vec::new(),
    }));

    wire_callbacks(&mut win, &widgets, &state);
    wire_steps(&widgets, &state);
    {
        let mut st = state.borrow_mut();
        rebuild_rows(&mut st, &mut widgets);
        if st.qpdf_problem.is_some() {
            widgets.win.set_label(&app_title(Some(t!(QpdfTitleUnusable))));
        }
        // No step chosen: what to do is the user's call. The first step's
        // options are shown.
        show_step(&mut widgets, Step::ALL[0]);
        st.defaults = panel(&widgets.steps, &st.attach);
        st.baseline = kept(st.defaults.clone());
        fill_menu(&mut widgets.steps, &st);
    }
    // Dev builds: QD_SHOW=<step> turns that step on and shows its options
    // (the start of its English name, e.g. `protection`, in any language) —
    // with `+`, its Advanced ones too — for screenshots without clicking.
    #[cfg(debug_assertions)]
    if let Ok(shown) = std::env::var("QD_SHOW") {
        let name = shown.trim_end_matches('+').to_lowercase();
        if let Some(step) = Step::ALL.into_iter().find(|s| qpdf_desk::text::english(s.message()).to_lowercase().starts_with(&name)) {
            // Output has no switch.
            if let Some(mut toggle) = widgets.steps.toggle(step).cloned() {
                toggle.set_value(true);
            }
            if let Some(advanced) = widgets.steps.advanced_of(step).filter(|_| shown.ends_with('+')) {
                set_advanced(advanced, true);
            }
            show_step(&mut widgets, step);
        }
    }
    job_edited(&state, &mut widgets);
    // Dev builds: QD_PROFILE=<name> loads that profile of `profiles.txt`.
    #[cfg(debug_assertions)]
    if let Ok(name) = std::env::var("QD_PROFILE") {
        load_profile(&state, &mut widgets, Some(name));
    }
    {
        let st = state.borrow();
        show_status(&st, &mut widgets, &idle_status(&st));
    }

    // Dev builds: QD_ADD="path;path" pre-fills the list and QD_START starts
    // the batch once QPDF is found, so rows, statuses and progress can be
    // checked with real files without driving the native file dialog.
    #[cfg(debug_assertions)]
    if let Ok(paths) = std::env::var("QD_ADD") {
        let paths = paths.split(';').filter(|p| !p.is_empty()).map(PathBuf::from).collect();
        let tool = std::env::var("QD_TOOL").ok();
        state.borrow_mut().autostart = if std::env::var_os("QD_START").is_some() { Some("start".into()) } else { tool };
        begin_add(&state, &mut widgets, paths);
    }

    // Dev builds: QD_FIT=<file> lists every label that does not fit its
    // control, in every step's options (with QD_LANG, of a translation).
    #[cfg(debug_assertions)]
    if let Some(file) = std::env::var_os("QD_FIT") {
        let _ = std::fs::write(file, fit::check(&mut widgets));
    }

    // Dev builds: QD_LAG=<file> measures responsiveness — how long the UI
    // thread keeps a waiting callback waiting — and writes it to <file> on exit.
    #[cfg(debug_assertions)]
    let lag = std::env::var_os("QD_LAG").map(|file| (PathBuf::from(file), lag::start()));

    // Main loop: drain background events on every wake (immediate on `awake`).
    while app.wait() {
        if ABOUT_ASKED.swap(false, Ordering::Relaxed) {
            let qpdf = state.borrow().qpdf.clone();
            about(&widgets.win, qpdf.as_ref());
        }
        pump_events(&state, &mut widgets);
    }

    #[cfg(debug_assertions)]
    if let Some((file, started)) = lag {
        let _ = std::fs::write(file, lag::report(started));
    }

    // Closing: stop the batch — its workers kill QPDF and delete their
    // temporary files — and wait for every worker and the probe to end.
    let (dialog, retired, batch, prober) = {
        let mut st = state.borrow_mut();
        (st.dialog.take(), std::mem::take(&mut st.retired), st.batch.take(), st.prober.take())
    };
    drop(dialog);
    drop(retired);
    drop(batch);
    drop(prober);

    if let Some(dir) = app_dir {
        let st = state.borrow();
        // Not the panel: every start begins with no step chosen.
        Settings { window_size: Some((win.w(), win.h())), last_folder: st.last_folder.clone(), ..saved }.save(&dir);
    }
}

/// Every event of every window, before FLTK sends it on: the drags
/// ([`dnd::answer`]) and F1, which asks for About. Not `app::add_handler`:
/// fltk-rs 1.5 drops a handler's answer, and FLTK took what was left in its
/// place as "handled" for the window messages it does not know itself — the
/// title stopped changing (measured).
fn dispatch(event: Event, win: app::WindowPtr) -> bool {
    if let Some(answer) = dnd::answer(event) {
        return answer;
    }
    if matches!(event, Event::KeyDown | Event::Shortcut) && app::event_key() == Key::F1 {
        ABOUT_ASKED.store(true, Ordering::Relaxed);
        app::awake();
        return true;
    }
    // SAFETY: the window pointer is FLTK's own, passed straight back.
    unsafe { app::handle_raw(event, win) }
}

/// Window title: the app and its version, then what is known of QPDF —
/// e.g. `QpdfDesk v1.0.0 — QPDF 12.4.2`, or `— QPDF not found`.
fn app_title(qpdf: Option<&str>) -> String {
    let mut title = format!("QpdfDesk v{}", env!("CARGO_PKG_VERSION"));
    if let Some(qpdf) = qpdf {
        title.push_str(&format!(" — {qpdf}"));
    }
    title
}

/// What the title says of a QPDF that cannot be used.
fn missing_title(missing: &Missing) -> &'static str {
    match missing {
        Missing::NotFound => t!(QpdfTitleNotFound),
        Missing::TooOld(_) | Missing::Unusable(_) => t!(QpdfTitleUnusable),
    }
}

/// Width of `text` in the UI font.
fn text_width(text: &str) -> i32 {
    draw::set_font(Font::Helvetica, app::font_size());
    width(text)
}

/// Width of `text` in the font last set, in whole pixels (rounded up, so a
/// text measured to fit always does).
fn width(text: &str) -> i32 {
    draw::width(text).ceil() as i32
}

/// Run `action` when `widget` is clicked.
fn on_click<W: WidgetExt + Clone>(
    widget: &W,
    state: &SharedState,
    widgets: &Widgets,
    action: impl Fn(&SharedState, &mut Widgets) + 'static,
) {
    let state = state.clone();
    let mut w = widgets.clone();
    widget.clone().set_callback(move |_| action(&state, &mut w));
}

/// Attach all callbacks: toolbar, list, Start, window-level drag&drop.
fn wire_callbacks(win: &mut Window, widgets: &Widgets, state: &SharedState) {
    // Add files / Add folder: same flow, different dialog; the background
    // worker filters and sorts both.
    for (button, kind, picks_folders) in [
        (&widgets.add_files, NativeFileChooserType::BrowseMultiFile, false),
        (&widgets.add_folder, NativeFileChooserType::BrowseDir, true),
    ] {
        on_click(button, state, widgets, move |state, w| {
            if state.borrow().batch.is_some() {
                return;
            }
            let chooser = if picks_folders { NativeFileChooser::new(kind) } else { pdf_chooser(kind) };
            if let Some(picked) = pick_paths(state, chooser, picks_folders) {
                begin_add(state, w, picked);
            }
        });
    }

    on_click(&widgets.remove, state, widgets, remove_selected);
    on_click(&widgets.clear, state, widgets, clear_list);
    on_click(&widgets.up, state, widgets, |state, w| move_selected(state, w, Move::Up));
    on_click(&widgets.down, state, widgets, |state, w| move_selected(state, w, Move::Down));
    on_click(&widgets.sort, state, widgets, sort_list);
    // Check ▾ and Info.
    on_click(&widgets.check, state, widgets, |state, w| match w.check.value() {
        0 => start_check(state, w, false),
        1 => start_check(state, w, true),
        _ => save_report(state, w),
    });
    on_click(&widgets.info, state, widgets, open_info);
    // The morphing Start ⇄ Cancel button.
    on_click(&widgets.start, state, widgets, |state, w| {
        {
            let mut st = state.borrow_mut();
            if let Some(running) = &st.batch {
                running.batch.cancel();
                show_batch_state(&st, w, None);
                update_controls(&st, w); // guards against double-clicks
                return;
            }
            if st.adding > 0 {
                return cancel_adding(&mut st, w);
            }
        }
        start_batch(state, w);
    });

    // Column widths and name ellipses follow the window size.
    {
        let state = state.clone();
        widgets.list.clone().resize_callback(move |browser, _x, _y, _w, _h| {
            // try_borrow_mut: resizes can fire while another callback holds the
            // state; skipping one cosmetic re-render is harmless then.
            let Ok(mut st) = state.try_borrow_mut() else { return };
            let cols = resized(browser, &st);
            set_columns(browser, cols);
            // A drag fires this dozens of times a second — repaint rows only
            // when the truncation could actually differ, a turn at a time.
            if recuts(&st, st.cols, cols) {
                let shown = st.shown;
                st.stale.extend(0..shown);
                app::awake();
            }
            st.cols = cols;
        });
    }

    // Selecting a single row shows its full status in the status line — the
    // list cell may be cut short.
    {
        let state = state.clone();
        let mut w = widgets.clone();
        widgets.list.clone().set_callback(move |browser| {
            let Ok(st) = state.try_borrow() else { return };
            let selected = selected_rows(browser, st.list.len());
            if let [index] = selected[..] {
                if let Some(entry) = st.list.entries().get(index) {
                    show_status(&st, &mut w, &entry_details(entry));
                }
            }
        });
    }

    // Keys and the mouse on the list: Delete removes, Ctrl+↑/↓ move the
    // selection and Ctrl+Home/End to the top or bottom, a double-click
    // shows the result in Explorer; hover highlights the row under the pointer.
    {
        let state = state.clone();
        let mut w = widgets.clone();
        widgets.list.clone().handle(move |browser, event| match event {
            Event::KeyDown if app::event_key() == Key::Delete => {
                remove_selected(&state, &mut w);
                true
            }
            Event::KeyDown if app::is_event_ctrl() => {
                let step = match app::event_key() {
                    Key::Up => Move::Up,
                    Key::Down => Move::Down,
                    Key::Home => Move::Top,
                    Key::End => Move::Bottom,
                    _ => return false,
                };
                move_selected(&state, &mut w, step);
                true
            }
            Event::KeyDown if matches!(app::event_key(), Key::Enter | Key::KPEnter) => {
                open_info(&state, &mut w);
                true
            }
            // On release the row is already selected; the event still goes on
            // to the list's own handling.
            Event::Released if app::event_clicks() => {
                reveal_row(&state, browser);
                false
            }
            Event::Enter | Event::Move => {
                update_hover(&state, &mut w, true);
                // Returning true for Enter keeps Move events coming.
                true
            }
            Event::Leave => {
                update_hover(&state, &mut w, false);
                true
            }
            _ => false,
        });
    }

    // Drag&drop onto the list and the path fields (`dnd`); refused while a
    // batch runs (the list and the panel are fixed then). FLTK closes a
    // window on Escape by default — only a real close request may end the app.
    {
        let (state, mut w) = (state.clone(), widgets.clone());
        let mut targets = vec![(Target::List, widgets.list_box.as_base_widget())];
        targets.extend(widgets.steps.drop_targets());
        dnd::install(win, targets, move |_| {
            let st = state.borrow();
            if st.batch.is_some() {
                show_status(&st, &mut w, t!(StatusAddAfterBatch));
            }
            st.batch.is_none()
        });
    }
    {
        let state = state.clone();
        let mut w = widgets.clone();
        dnd::on_drop(win, move |target, dropped| {
            match dnd::fit(target, dropped, Path::is_dir) {
                Err(why) => show_status(&state.borrow(), &mut w, qpdf_desk::text::get(why)),
                Ok(paths) if paths.is_empty() => {}
                Ok(paths) => match target {
                    Target::List => begin_add(&state, &mut w, paths),
                    Target::Attachments => attach(&state, &mut w, paths),
                    Target::Overlay => set_overlay(&state, &mut w, paths[0].clone()),
                    Target::OutputFolder => set_output_folder(&state, &mut w, &paths[0]),
                },
            }
        });
    }
    // FLTK closes a window on Escape: only the title bar's ✕ (or Alt+F4)
    // does here. Escape stops an addition still being collected instead.
    let state = state.clone();
    let mut w = widgets.clone();
    win.set_callback(move |win| {
        if app::event() == Event::Close {
            win.hide();
        } else if app::event_key() == Key::Escape {
            let mut st = state.borrow_mut();
            if st.adding > 0 && st.batch.is_none() {
                cancel_adding(&mut st, &mut w);
            }
        }
    });
}

/// Stop every addition still being collected: the workers end at their next
/// file and report nothing. Files already on their way into the list
/// still come.
fn cancel_adding(st: &mut State, widgets: &mut Widgets) {
    st.add_stop.store(true, Ordering::Relaxed);
    // A fresh flag for the additions to come.
    st.add_stop = Arc::new(AtomicBool::new(false));
    show_status(st, widgets, t!(StatusCancellingAdding));
    update_controls(st, widgets);
}

/// Hand `paths` to the background add worker.
fn begin_add(state: &SharedState, widgets: &mut Widgets, paths: Vec<PathBuf>) {
    let mut st = state.borrow_mut();
    st.adding += 1;
    tasks::spawn_add(paths, Arc::clone(&st.add_stop), st.tx.clone(), Arc::clone(&st.notify));
    show_status(&st, widgets, t!(StatusAdding));
    update_controls(&st, widgets);
}

/// Clear the list — always after asking.
fn clear_list(state: &SharedState, widgets: &mut Widgets) {
    {
        let st = state.borrow();
        if st.batch.is_some() || st.list.is_empty() {
            return;
        }
    }
    // The dialog runs a nested event loop — never hold a borrow across it.
    if !confirm(&widgets.win, t!(ClearQuestion), t!(ClearDetail), t!(ClearAction)) {
        return;
    }
    let mut st = state.borrow_mut();
    let ids = st.list.clear();
    if let Some(prober) = &st.prober {
        prober.forget(&ids);
    }
    st.hover = None;
    rebuild_rows(&mut st, widgets);
    update_controls(&st, widgets);
    set_progress(&mut widgets.progress, None);
    show_status(&st, widgets, &idle_status(&st));
}

/// Start the batch over every file that is not done with yet: a new
/// Start after a cancel or a failure goes on with the rest, files already
/// done are left alone — unless the job has changed since, then every file
/// runs again.
fn start_batch(state: &SharedState, widgets: &mut Widgets) {
    let mut st = state.borrow_mut();
    if st.qpdf.is_none() || st.batch.is_some() {
        return;
    }
    let Ok(job) = read_job(&widgets.steps, &st) else { return };
    // Remembered from last time or from a profile, the folder may be gone.
    // Looked at here, not on every edit: a network drive that is gone can
    // keep the window waiting for seconds.
    if let Some(folder) = job.output.as_ref().and_then(|output| output.folder.as_deref()).filter(|folder| !folder.is_dir()) {
        let message = t!(OutputFolderGone, step = t!(StepOutput), folder = display_name(folder));
        return show_status(&st, widgets, &message);
    }
    st.list.prepare(job.work_key());
    // A merge is one result of every file — also of those done before.
    if !st.list.has_pending() {
        return;
    }
    let units: Vec<Unit> = st
        .list
        .begin_batch(job.merges())
        .into_iter()
        .map(|unit| unit.into_iter().map(BatchItem::from).collect())
        .collect();
    // The batch probes the files the probe has not reached; one look is enough.
    if let Some(prober) = &st.prober {
        let unprobed: Vec<u64> = units.iter().flatten().filter(|item| item.probe == Probe::Pending).map(|item| item.id).collect();
        prober.forget(&unprobed);
    }
    let rows = 0..st.list.len();
    run_batch(state, &mut st, widgets, Task::Process(Box::new(job)), units, rows);
}

/// Start `task` on `units` as the running batch, refresh the window and
/// start the progress timer — which runs only while the batch does.
/// The batch's `rows` are repainted in the next turns. `false` without QPDF.
fn run_batch(
    state: &SharedState,
    st: &mut std::cell::RefMut<State>,
    widgets: &mut Widgets,
    task: Task,
    units: Vec<Unit>,
    rows: impl IntoIterator<Item = usize>,
) -> bool {
    let Some(wiring) = st.wiring() else { return false };
    st.stale.extend(rows);
    app::awake();
    let batch = Batch::start(wiring, task, units, st.threads);
    let rows = st.list.entries().iter().enumerate().map(|(index, entry)| (entry.id, index)).collect();
    // A long batch can run for hours; do not let the system idle-sleep through it.
    st.batch = Some(RunningBatch { batch, current: None, rows, _stay_awake: StayAwake::begin() });
    set_progress(&mut widgets.progress, Some(0.0));
    show_batch_state(st, widgets, None);
    update_controls(st, widgets);
    let state = state.clone();
    let mut w = widgets.clone();
    app::add_timeout3(TICK, move |handle| {
        if tick_progress(&state, &mut w) {
            app::repeat_timeout3(TICK, handle);
        }
    });
    true
}

/// Check ▾: `qpdf --check` on the selected files — or on every file —
/// as a batch of its own; each row shows its verdict until a batch takes it.
fn start_check(state: &SharedState, widgets: &mut Widgets, every: bool) {
    let mut st = state.borrow_mut();
    if st.batch.is_some() {
        return;
    }
    let rows: Vec<usize> = if every { (0..st.list.len()).collect() } else { selected_rows(&widgets.list, st.list.len()) };
    if rows.is_empty() {
        return show_status(&st, widgets, t!(StatusSelectToCheck));
    }
    // The password for encrypted inputs opens a file that asks for one.
    let units: Vec<Unit> = st.list.begin_check(&rows).into_iter().map(|entry| vec![BatchItem::from(entry)]).collect();
    // Only the checked rows change: a check of one file repaints one row.
    if !run_batch(state, &mut st, widgets, Task::Check(widgets.steps.password()), units, rows) {
        st.list.end_check();
    }
}

/// Check ▾ › Save the report…: every file with a Check verdict, in
/// list order, into a text file the user names. A failed write says so; the
/// verdicts stay.
fn save_report(state: &SharedState, widgets: &mut Widgets) {
    let text = {
        let st = state.borrow();
        let checked: Vec<report::Checked> = st
            .list
            .entries()
            .iter()
            .filter_map(|entry| Some(report::Checked { path: &entry.path, report: entry.check_report()? }))
            .collect();
        if checked.is_empty() {
            return show_status(&st, widgets, t!(StatusNothingChecked));
        }
        let qpdf = st.qpdf.as_ref().map(|q| format!("{} ({})", q.version, q.exe.display())).unwrap_or_default();
        report::check_report(&checked, &platform::local_time(), &qpdf)
    };
    let mut chooser = NativeFileChooser::new(NativeFileChooserType::BrowseSaveFile);
    chooser.set_option(fltk::dialog::NativeFileChooserOptions::SaveAsConfirm);
    chooser.set_filter(&format!("{}\t*.txt", t!(FilterText)));
    chooser.set_preset_file(&format!("{}.txt", t!(ReportFileName)));
    // The dialog runs a nested event loop — no borrow is held across it.
    let Some(mut path) = pick_one(state, chooser, false) else { return };
    if path.extension().is_none() {
        path.set_extension("txt");
    }
    // Through a temporary file: a full disk never leaves half a report under
    // the chosen name.
    let message = match qpdf_desk::settings::write_text(&path, &text) {
        Ok(()) => t!(StatusReportSaved, name = display_name(&path)),
        Err(e) => t!(StatusReportFailed, why = outcome::from_io_error(&e).sentence()),
    };
    show_status(&state.borrow(), widgets, &message);
}

/// Timer: refresh the bar and the running files' rows. Returns whether the
/// batch still runs (otherwise the timer stops).
fn tick_progress(state: &SharedState, widgets: &mut Widgets) -> bool {
    let Ok(mut st) = state.try_borrow_mut() else { return true };
    let Some(progress) = st.batch.as_mut().map(|running| {
        let progress = running.batch.progress();
        running.current = progress.active.first().and_then(|&(_, percent)| percent);
        progress
    }) else {
        return false;
    };
    set_progress(&mut widgets.progress, progress.fraction);
    for (id, percent) in progress.active {
        // A file whose `Started` event is still on its way is left to it.
        let Some((index, entry)) = st.entry(id) else { continue };
        if !matches!(entry.status, Status::Working(_)) || entry.status == Status::Working(percent) {
            continue;
        }
        entry.status = Status::Working(percent);
        repaint_row(&st, &mut widgets.list, index);
    }
    show_batch_state(&st, widgets, None);
    true
}

/// Drain the background events and reflect them in the window.
fn pump_events(state: &SharedState, widgets: &mut Widgets) {
    let Ok(mut st) = state.try_borrow_mut() else { return };
    let events: Vec<Work> = st.rx.try_iter().collect();
    if events.is_empty() && st.incoming.is_empty() && st.stale.is_empty() && st.shown >= st.list.len() {
        return;
    }
    let mut status: Option<String> = None;
    let mut batch_over: Option<bool> = None;
    let mut overlay_probed = false;
    for event in events {
        match event {
            Work::Located(found) => {
                match found {
                    Ok(qpdf) => {
                        if let Some(prober) = &st.prober {
                            prober.set_qpdf(qpdf.exe.clone());
                        }
                        widgets.win.set_label(&app_title(Some(&format!("QPDF {}", qpdf.version))));
                        st.qpdf = Some(qpdf);
                        st.probe_overlay();
                    }
                    Err(missing) => {
                        widgets.win.set_label(&app_title(Some(missing_title(&missing))));
                        st.qpdf_problem = Some(missing_message(&missing));
                        // The empty list says it, where the eye goes first.
                        if st.list.is_empty() {
                            rebuild_rows(&mut st, widgets);
                        }
                    }
                }
                // Replaces only "Looking for QPDF…" or reports a problem — a
                // fresher message (such as "Added…") stays.
                if st.qpdf_problem.is_some() || (st.list.is_empty() && st.adding == 0) {
                    status = Some(idle_status(&st));
                }
            }
            Work::Added(collected) if collected.cancelled => {
                st.adding = st.adding.saturating_sub(1);
                status = Some(t!(StatusAddingCancelled).into());
            }
            Work::Added(collected) => st.incoming.push_back(collected.into()),
            // Only for the file still chosen: an answer about one chosen before
            // may still arrive.
            Work::OverlayProbed { path, probe } => {
                if let Some((_, known)) = st.overlay.as_mut().filter(|(chosen, _)| *chosen == path) {
                    *known = probe;
                    overlay_probed = true;
                }
            }
            Work::Probed { id, probe } => st.touch(id, |entry| entry.probe = probe),
            // A file waiting to be checked is in a Check, any other in the job.
            Work::Started { id } => st.touch(id, |entry| {
                if entry.check == Some(Check::Waiting) {
                    entry.check = Some(Check::Running);
                } else {
                    entry.status = Status::Working(None);
                }
            }),
            Work::Finished { id, produced } => st.touch(id, |entry| entry.status = Status::Finished(produced)),
            Work::Checked { id, report } => st.touch(id, |entry| entry.check = Some(Check::Done(report))),
            Work::BatchDone { cancelled } => batch_over = Some(cancelled),
            Work::Inspected { request, result } => inspected(&mut st, request, result),
            Work::Extracted { request, extracted } => extracted_files(&mut st, request, extracted),
        }
    }

    // An addition goes into the list a chunk per turn of the event loop;
    // the next turn comes at once.
    if let Some(message) = st.take_incoming() {
        status = message.or(status.take());
        fit_rows(&mut st, widgets);
    }

    if let Some(cancelled) = batch_over {
        // Every worker has stopped; dropping the batch only joins its thread.
        let checking = st.batch.take().is_some_and(|running| running.batch.is_check());
        // Files the cancel stopped before they started are back to idle, and
        // those the batch took from the probe, still unprobed, go back to it.
        let ended: Vec<usize> = st.list.end_batch().into_iter().chain(st.list.end_check()).collect();
        st.stale.extend(ended);
        let unprobed: Vec<(u64, PathBuf)> =
            st.list.entries().iter().filter(|e| e.probe == Probe::Pending).map(|e| (e.id, e.path.clone())).collect();
        if let Some(prober) = &st.prober {
            prober.enqueue(unprobed);
        }
        if !cancelled {
            set_progress(&mut widgets.progress, Some(1.0));
        }
        if checking {
            // Short: the rows say it all, the report keeps it.
            let counts = report::tally_text(st.list.entries().iter().filter_map(Entry::check_report));
            let ended = if cancelled { t!(StatusCheckCancelled) } else { t!(StatusChecked) };
            show_status(&st, widgets, &format!("{ended} · {counts}"));
        } else {
            show_batch_state(&st, widgets, Some(cancelled));
        }
    } else if let Some(message) = status {
        show_status(&st, widgets, &message);
    } else if st.batch.is_some() {
        show_batch_state(&st, widgets, None);
    }

    // What is left waits for the next turn, which comes at once.
    if paint_rows(&mut st, &mut widgets.list) || !st.incoming.is_empty() {
        app::awake();
    }
    update_controls(&st, widgets);
    drop(st);
    // The overlay file's pages and password decide whether the job can run.
    if overlay_probed {
        job_edited(state, widgets);
    }
    #[cfg(debug_assertions)]
    dev_autostart(state, widgets);
}

/// Dev builds: QD_START, QD_TOOL — once QPDF is known and the files are in.
#[cfg(debug_assertions)]
fn dev_autostart(state: &SharedState, widgets: &mut Widgets) {
    let (autostart, extract_now) = {
        let Ok(mut st) = state.try_borrow_mut() else { return };
        let ready = st.qpdf.is_some() && st.adding == 0 && !st.list.is_empty();
        let autostart = if ready { st.autostart.take() } else { None };
        st.extract_when_read |= autostart.as_deref() == Some("extract");
        let extract_now = st.extract_when_read && info_read(&st);
        st.extract_when_read &= !extract_now;
        (autostart, extract_now)
    };
    match autostart.as_deref() {
        Some("start") => start_batch(state, widgets),
        Some("check") => start_check(state, widgets, true),
        Some("info" | "extract") => {
            widgets.list.select(1);
            open_info(state, widgets);
        }
        Some("pages") => {
            widgets.list.select(1);
            open_pages(state, widgets);
        }
        Some("command") => {
            widgets.list.select(1);
            open_command(state, widgets);
        }
        Some("about") => ABOUT_ASKED.store(true, Ordering::Relaxed),
        Some("clear") => clear_list(state, widgets),
        // A second later, as a click would come: not in the turn that put up
        // the last rows (`QD_LAG` measures it alone).
        Some("sort") => {
            let (state, mut widgets) = (state.clone(), widgets.clone());
            app::add_timeout3(1.0, move |_| sort_list(&state, &mut widgets));
        }
        _ => {}
    }
    if extract_now {
        extract(state, widgets, true);
    }
}

/// "Added N file(s) · skipped M non-PDF · K duplicate(s) ignored", and
/// how many did not fit the list.
fn added_message(tally: &Added, skipped: usize) -> String {
    let Added { added, duplicates, beyond } = *tally;
    let mut parts = vec![tn!(AddedFiles, added)];
    if skipped > 0 {
        parts.push(tn!(AddedSkipped, skipped));
    }
    if duplicates > 0 {
        parts.push(tn!(AddedDuplicates, duplicates));
    }
    if beyond > 0 {
        parts.push(tn!(AddedLeftOut, beyond, limit = MAX_FILES));
    }
    parts.join(" · ")
}

/// What to tell the user when there is no usable QPDF.
fn missing_message(missing: &Missing) -> String {
    match missing {
        Missing::NotFound => {
            // Short enough for the smallest window (measured).
            t!(QpdfNotFound).to_string()
        }
        Missing::TooOld(found) => t!(QpdfTooOld, found = found.version, needed = MIN_VERSION),
        Missing::Unusable(exe) => t!(QpdfUnusable, path = printable(&exe.display().to_string())),
    }
}

/// The status line when nothing is running.
fn idle_status(st: &State) -> String {
    // No usable QPDF: an empty list says it in the middle, so the line
    // stays empty; once files are on it, the line says it.
    if let Some(problem) = &st.qpdf_problem {
        return if st.list.is_empty() { String::new() } else { problem.clone() };
    }
    if st.qpdf.is_none() {
        return t!(StatusLookingForQpdf).to_string();
    }
    if st.list.is_empty() {
        t!(StatusReady).to_string()
    } else {
        tn!(StatusFilesOnList, st.list.len())
    }
}

/// Enable/disable the controls for the current state.
fn update_controls(st: &State, widgets: &mut Widgets) {
    let running = st.batch.is_some();
    let has_files = !st.list.is_empty();
    // The list is fixed while a batch runs: its order is the work order.
    for button in [&mut widgets.add_files, &mut widgets.add_folder] {
        set_active(button, !running);
    }
    for button in [&mut widgets.remove, &mut widgets.clear, &mut widgets.up, &mut widgets.down, &mut widgets.sort] {
        set_active(button, !running && has_files);
    }
    // Info only reads, so it works while a batch runs.
    set_active(&mut widgets.check, st.qpdf.is_some() && !running && has_files);
    set_active(&mut widgets.info, st.qpdf.is_some() && has_files);
    set_active(&mut widgets.steps.command, st.qpdf.is_some() && st.job.is_some() && has_files);
    // The job is fixed too: the running files were started with it.
    for group in [&mut widgets.steps_panel, &mut widgets.options, &mut widgets.password_row] {
        set_active(group, !running);
    }
    // Runs on every event and keystroke; a label set again repaints.
    // Adding files: Cancel stops a walk through a whole drive.
    let adding = st.adding > 0 && !running;
    let label = match (running || adding, widgets.steps.chosen().is_empty()) {
        (true, _) => t!(ButtonCancel),
        (false, true) => t!(ButtonRepair),
        (false, false) => t!(ButtonStart),
    };
    set_label(&mut widgets.start, label, None);
    if running {
        // Stays inactive once Cancel was pressed, until the batch has ended.
        let cancelling = st.batch.as_ref().is_some_and(|r| r.batch.is_cancelled());
        set_active(&mut widgets.start, !cancelling);
    } else if adding {
        set_active(&mut widgets.start, true);
    } else {
        // Two passes over the list — not on every event of a batch. A
        // changed job runs on every file again.
        let new_job = st.job.as_ref().is_some_and(|job| st.list.is_new_work(job));
        let something_to_do = st.list.has_pending() || (new_job && has_files);
        let can_start = st.qpdf.is_some() && st.job.is_some() && something_to_do && st.adding == 0;
        set_active(&mut widgets.start, can_start);
    }
}

/// Enable or disable a widget (FLTK has no single "set enabled" call).
fn set_active(widget: &mut impl WidgetExt, active: bool) {
    if active {
        widget.activate();
    } else {
        widget.deactivate();
    }
}

/// A button in `row`, as wide as its label.
fn fitted_button(row: &mut Flex, label: &str) -> Button {
    let button = Button::default().with_label(label);
    row.fixed(&button, text_width(label) + BUTTON_PAD);
    button
}

/// A button in `row` of [`button_width`].
fn sized_button(row: &mut Flex, label: &str) -> Button {
    let button = Button::default().with_label(label);
    row.fixed(&button, button_width(&[label]));
    button
}

/// Width of a button that shows any of `labels`: the widest, at least
/// [`BTN_W`] — so a longer translation widens it instead of being cut.
fn button_width(labels: &[&str]) -> i32 {
    labels.iter().map(|label| text_width(label) + BUTTON_PAD).max().unwrap_or(0).max(BTN_W)
}

fn set_shown(widget: &mut impl WidgetExt, shown: bool) {
    if shown {
        widget.show();
    } else {
        widget.hide();
    }
}

/// Show a native file dialog opened where the user last was, and remember
/// where they ended up: a picked folder itself, or a picked file's folder.
/// `None` when the dialog failed or was dismissed without a pick.
fn pick_paths(state: &SharedState, mut chooser: NativeFileChooser, picks_folders: bool) -> Option<Vec<PathBuf>> {
    let start = state.borrow().last_folder.clone();
    if let Some(folder) = start {
        let _ = chooser.set_directory(&folder);
    }
    chooser.try_show().ok()?;
    let picked = chooser.filenames();
    let first = picked.first()?;
    let folder = if picks_folders { Some(first.as_path()) } else { first.parent() };
    if let Some(folder) = folder {
        state.borrow_mut().last_folder = Some(folder.to_path_buf());
    }
    Some(picked)
}

/// [`pick_paths`] of a dialog that picks one file or folder.
fn pick_one(state: &SharedState, chooser: NativeFileChooser, picks_folders: bool) -> Option<PathBuf> {
    pick_paths(state, chooser, picks_folders)?.into_iter().next()
}

/// `text` with every `@` doubled: FLTK reads an `@` in a label or tooltip as
/// the start of a symbol and drops the rest of the text (measured).
fn label_text(text: &str) -> String {
    text.replace('@', "@@")
}

/// Set a label's text (and color), every `@` doubled ([`label_text`]). Only
/// a change repaints — most calls, such as one per keystroke in the steps
/// panel, leave the label as it was. Whether it changed.
fn set_label(widget: &mut impl WidgetExt, text: &str, color: Option<Color>) -> bool {
    let text = label_text(text);
    let changed = widget.label() != text || color.is_some_and(|color| widget.label_color() != color);
    if changed {
        widget.set_label(&text);
        if let Some(color) = color {
            widget.set_label_color(color);
        }
        widget.redraw();
    }
    changed
}

/// A file dialog of `kind` showing PDF files.
fn pdf_chooser(kind: NativeFileChooserType) -> NativeFileChooser {
    let mut chooser = NativeFileChooser::new(kind);
    chooser.set_filter(&format!("{}\t*.pdf", t!(FilterPdf)));
    chooser
}

/// Left side of the bottom row: what the app is doing or just did.
fn show_status(st: &State, widgets: &mut Widgets, status: &str) {
    // The row is repainted under the label (it has no box of its own) —
    // only when it changed: every event of a batch comes through here.
    if set_label(&mut widgets.status, status, None) {
        widgets.status_row.redraw();
        // A message the row cuts short — now, or once the window is narrower —
        // is whole in its tooltip (drawn like a label: `@` doubled).
        widgets.status.set_tooltip(&label_text(status));
    }
    refresh_tallies(st, widgets);
}

/// Where a batch stands — running (`finished = None`) or just ended.
fn show_batch_state(st: &State, widgets: &mut Widgets, finished: Option<bool>) {
    let status = match (finished, &st.batch) {
        (None, Some(running)) if running.batch.is_cancelled() => t!(StatusCancelling).to_string(),
        (None, Some(running)) if running.batch.is_check() => {
            let (done, total) = running.batch.counts();
            t!(StatusChecking, done = done, total = total)
        }
        (None, Some(running)) => {
            let (done, total) = running.batch.counts();
            match running.current {
                Some(percent) => t!(StatusProcessingCurrent, done = done, total = total, percent = percent),
                None => t!(StatusProcessing, done = done, total = total),
            }
        }
        (Some(true), _) => t!(StatusBatchCancelled).to_string(),
        _ => t!(StatusBatchComplete).to_string(),
    };
    show_status(st, widgets, &status);
}

/// Right side of the bottom row, derived from the list itself — never
/// passed in, so it cannot disagree with the rows.
fn refresh_tallies(st: &State, widgets: &mut Widgets) {
    let tallies = tallies_text(st.list.entries());
    // Runs on every event; only a changed text needs measuring.
    if tallies == widgets.tallies.label() {
        return;
    }
    let width = if tallies.is_empty() { 1 } else { text_width(&tallies) + MARGIN };
    widgets.tallies.set_label(&tallies);
    widgets.status_row.fixed(&widgets.tallies, width);
    widgets.status_row.layout();
    widgets.status_row.redraw();
}

/// Update the overall progress bar: fill and "NN%". `None` shows an empty bar
/// without dividing by zero.
fn set_progress(progress: &mut Progress, fraction: Option<f64>) {
    match fraction {
        Some(fraction) => {
            let fraction = fraction.clamp(0.0, 1.0);
            progress.set_value(fraction);
            progress.set_label(&format!("{}%", (fraction * 100.0).round() as i32));
        }
        None => {
            progress.set_value(0.0);
            progress.set_label("");
        }
    }
    progress.redraw();
}
/// Fallback centering in FLTK coordinates, used only if the Win32 path fails.
fn centered_on_active_screen(w: i32, h: i32) -> (i32, i32) {
    let (mx, my) = app::get_mouse();
    let screen = app::screen_num(mx, my);
    let (sx, sy, sw, sh) = app::screen_work_area(screen);
    win32::centered_in(win32::Rect { left: sx, top: sy, right: sx + sw, bottom: sy + sh }, w, h)
}

/// Minimal Win32 declarations for window placement.
///
/// Placement is done in physical pixels through Win32 rather than in FLTK
/// coordinates on purpose: FLTK positions a window by its CLIENT area and
/// multiplies coordinates by the monitor's scale factor (`Fl_win32.cxx`), so on
/// a multi-monitor / HiDPI desk the result drifts — that is how the window kept
/// landing under the taskbar in MediaFileValid. `rcWork` excludes the taskbar.
mod win32 {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }
    #[repr(C)]
    pub struct MonitorInfo {
        pub cb_size: u32,
        pub rc_monitor: Rect,
        pub rc_work: Rect,
        pub flags: u32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    pub const MONITOR_DEFAULTTONEAREST: u32 = 2;
    pub const SWP_NOSIZE: u32 = 0x0001;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_NOACTIVATE: u32 = 0x0010;
    /// `WS_OVERLAPPEDWINDOW` — the style FLTK uses for a bordered, resizable
    /// window; the frame metrics follow from it.
    pub const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;

    pub const GWL_EXSTYLE: i32 = -20;
    pub const WS_EX_LAYERED: isize = 0x0008_0000;
    pub const LWA_ALPHA: u32 = 0x0000_0002;
    pub const GWL_STYLE: i32 = -16;
    pub const WS_SYSMENU: isize = 0x0008_0000;
    pub const SWP_NOMOVE: u32 = 0x0002;
    pub const SWP_FRAMECHANGED: u32 = 0x0020;

    #[link(name = "user32")]
    extern "system" {
        pub fn GetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32) -> isize;
        pub fn SetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32, value: isize) -> isize;
        pub fn SetLayeredWindowAttributes(
            hwnd: *mut core::ffi::c_void,
            key: u32,
            alpha: u8,
            flags: u32,
        ) -> i32;
        pub fn GetCursorPos(point: *mut Point) -> i32;
        pub fn MonitorFromPoint(point: Point, flags: u32) -> *mut core::ffi::c_void;
        pub fn MonitorFromWindow(hwnd: *mut core::ffi::c_void, flags: u32) -> *mut core::ffi::c_void;
        pub fn GetMonitorInfoW(monitor: *mut core::ffi::c_void, info: *mut MonitorInfo) -> i32;
        pub fn GetWindowRect(hwnd: *mut core::ffi::c_void, rect: *mut Rect) -> i32;
        pub fn AdjustWindowRectEx(rect: *mut Rect, style: u32, menu: i32, ex_style: u32) -> i32;
        pub fn SetWindowPos(
            hwnd: *mut core::ffi::c_void,
            after: *mut core::ffi::c_void,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
    }

    /// Size of a window's large icon at the system's scale (`SM_CXICON`).
    pub fn icon_size() -> Option<u32> {
        #[link(name = "user32")]
        extern "system" {
            fn GetSystemMetrics(index: i32) -> i32;
        }
        const SM_CXICON: i32 = 11;
        // SAFETY: no pointers; 0 means failure.
        u32::try_from(unsafe { GetSystemMetrics(SM_CXICON) }).ok().filter(|&size| (16..=256).contains(&size))
    }

    /// Work area (taskbar excluded) of the monitor under the mouse pointer.
    pub fn work_area_under_cursor() -> Option<Rect> {
        let mut cursor = Point::default();
        // SAFETY: a plain out-parameter.
        if unsafe { GetCursorPos(&mut cursor) } == 0 {
            return None;
        }
        work_area(unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) })
    }

    /// Work area of the monitor `hwnd` is on (most of it).
    pub fn work_area_of(hwnd: *mut core::ffi::c_void) -> Option<Rect> {
        work_area(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })
    }

    fn work_area(monitor: *mut core::ffi::c_void) -> Option<Rect> {
        if monitor.is_null() {
            return None;
        }
        let mut info =
            MonitorInfo { cb_size: core::mem::size_of::<MonitorInfo>() as u32, rc_monitor: Rect::default(), rc_work: Rect::default(), flags: 0 };
        // SAFETY: `info` is sized as the call requires.
        (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info.rc_work)
    }

    const GWLP_WNDPROC: i32 = -4;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const MF_STRING: u32 = 0x0000;
    const MF_SEPARATOR: u32 = 0x0800;
    /// Our item of the system menu: below 0xF000 and a multiple of 16 (Windows
    /// uses the low four bits of the command).
    const IDM_ABOUT: usize = 0x0010;

    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMenu(hwnd: *mut core::ffi::c_void, revert: i32) -> *mut core::ffi::c_void;
        fn AppendMenuW(menu: *mut core::ffi::c_void, flags: u32, id: usize, text: *const u16) -> i32;
        fn CallWindowProcW(previous: isize, hwnd: *mut core::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    /// FLTK's window procedure of the main window, called for everything but
    /// our menu item.
    static FLTK_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

    /// Add "About QpdfDesk…" to the main window's system menu (the title bar's
    /// right-click menu). Best effort: without it, F1 still opens About.
    pub fn add_about_item(hwnd: *mut core::ffi::c_void) {
        if hwnd.is_null() {
            return;
        }
        let label = qpdf_desk::platform::wide(qpdf_desk::t!(MenuAbout));
        unsafe {
            let menu = GetSystemMenu(hwnd, 0);
            if menu.is_null() || AppendMenuW(menu, MF_SEPARATOR, 0, core::ptr::null()) == 0 {
                return;
            }
            if AppendMenuW(menu, MF_STRING, IDM_ABOUT, label.as_ptr()) != 0 {
                let previous = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, about_proc as *const () as isize);
                FLTK_PROC.store(previous, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// The main window's procedure: our menu item asks the main loop for
    /// About; everything else goes to FLTK as before.
    extern "system" fn about_proc(hwnd: *mut core::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> isize {
        if msg == WM_SYSCOMMAND && wparam & 0xFFF0 == IDM_ABOUT {
            super::ABOUT_ASKED.store(true, std::sync::atomic::Ordering::Relaxed);
            fltk::app::awake();
            return 0;
        }
        // SAFETY: the procedure FLTK gave the window, kept when ours replaced it.
        unsafe { CallWindowProcW(FLTK_PROC.load(std::sync::atomic::Ordering::Relaxed), hwnd, msg, wparam, lparam) }
    }

    /// Give a shown window the ✕ of its title bar: FLTK draws a modal window
    /// without one (no system menu).
    pub fn add_close_box(hwnd: *mut core::ffi::c_void) {
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            if style & WS_SYSMENU == 0 {
                SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_SYSMENU);
                SetWindowPos(hwnd, core::ptr::null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
        }
    }

    /// Put a shown window in the middle of the work area of `owner`'s monitor
    /// — without one, of the monitor under the mouse pointer — shrunk to fit
    /// it. `false` when the placement could not be found.
    pub fn fit_and_center(hwnd: *mut core::ffi::c_void, owner: Option<*mut core::ffi::c_void>) -> bool {
        let Some(work) = owner.map_or_else(work_area_under_cursor, work_area_of) else { return false };
        let mut frame = Rect::default();
        // SAFETY: a plain out-parameter of a live window.
        if unsafe { GetWindowRect(hwnd, &mut frame) } == 0 {
            return false;
        }
        let w = (frame.right - frame.left).min(work.right - work.left);
        let h = (frame.bottom - frame.top).min(work.bottom - work.top);
        let (x, y) = centered_in(work, w, h);
        unsafe { SetWindowPos(hwnd, core::ptr::null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) != 0 }
    }

    /// Top-left of a `win_w`×`win_h` window centered in `work`, clamped so the
    /// whole window stays inside it.
    pub fn centered_in(work: Rect, win_w: i32, win_h: i32) -> (i32, i32) {
        let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
        let x = (work.left + (work_w - win_w) / 2).clamp(work.left, (work.right - win_w).max(work.left));
        let y = (work.top + (work_h - win_h) / 2).clamp(work.top, (work.bottom - win_h).max(work.top));
        (x, y)
    }
}

/// Position (in FLTK units, i.e. what `Window::new` expects) of the CLIENT area
/// that puts the whole decorated window in the middle of the monitor under the
/// mouse — computed **before the window exists**, so its first visible frame is
/// already in the right place.
///
/// The frame thickness comes from `AdjustWindowRectEx`, the scale factor from
/// FLTK; both are needed because FLTK takes logical client coordinates while
/// Windows works in physical pixels around the decorated rectangle.
fn centered_client_pos(w: i32, h: i32) -> Option<(i32, i32)> {
    use win32::*;

    let work = work_area_under_cursor()?;
    let (mx, my) = app::get_mouse();
    let scale = f64::from(app::screen_scale(app::screen_num(mx, my))).max(0.1);

    // Client rectangle in physical pixels, grown to the decorated rectangle.
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: (f64::from(w) * scale).round() as i32,
        bottom: (f64::from(h) * scale).round() as i32,
    };
    unsafe {
        if AdjustWindowRectEx(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0) == 0 {
            return None;
        }
    }
    let (outer_w, outer_h) = (rect.right - rect.left, rect.bottom - rect.top);
    let (outer_x, outer_y) = centered_in(work, outer_w, outer_h);

    // `-rect.left` is the left border, `-rect.top` the border plus title bar.
    let client_x = outer_x - rect.left;
    let client_y = outer_y - rect.top;
    Some((
        (f64::from(client_x) / scale).round() as i32,
        (f64::from(client_y) / scale).round() as i32,
    ))
}

/// Make the window invisible to the compositor (without hiding it, which would
/// disturb focus and the taskbar) while it is being moved into position, then
/// restore it. This is what turns the unavoidable post-show correction into
/// something the eye never catches.
fn set_window_transparent(win: &Window, transparent: bool) {
    use win32::*;

    let hwnd = win.raw_handle();
    if hwnd.is_null() {
        return;
    }
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if transparent {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED);
            SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        } else {
            // Dropping the layered style restores normal, fully opaque drawing.
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !WS_EX_LAYERED);
            win.clone().redraw();
        }
    }
}

/// Show `win`, a modal window, whole in the middle of the monitor `owner` is
/// on (none: the one under the mouse pointer), with a ✕ in its title bar,
/// themed. Placed through Win32 like the main window: FLTK's own placement
/// over the main one left the Info window's buttons below the screen on a
/// scaled monitor, and a modal FLTK window has no ✕. Transparent while it
/// moves, so the move is never seen.
fn show_modal(win: &mut Window, owner: Option<&Window>) {
    win.make_modal(true);
    // Where it stays should Win32 fail: over the owner.
    if let Some(owner) = owner {
        win.set_pos((owner.x() + (owner.w() - win.w()) / 2).max(0), (owner.y() + (owner.h() - win.h()) / 2).max(0));
    }
    show_centered(win, owner, true);
}

/// Show `win` centred on the monitor of `owner` — or of the mouse pointer —
/// and shrunk to fit it, with a ✕ for a dialog (`close_box`); the native
/// title bar follows the theme (best effort). FLTK can only *suggest* a
/// position before `show()`, and on a multi-monitor desk the OS lands the
/// window somewhere else, so the exact placement happens after it — with
/// the window fully transparent meanwhile, so the correction is never seen
/// as a jump.
fn show_centered(win: &mut Window, owner: Option<&Window>, close_box: bool) {
    win.show();
    set_window_transparent(win, true);
    let hwnd = win.raw_handle();
    if close_box && !hwnd.is_null() {
        win32::add_close_box(hwnd);
    }
    let placed = !hwnd.is_null() && win32::fit_and_center(hwnd, owner.map(|owner| owner.raw_handle()));
    if !placed && owner.is_none() {
        let (x, y) = centered_on_active_screen(win.w(), win.h());
        win.set_pos(x, y);
    }
    set_window_transparent(win, false);
    apply_dark_title_bar(win, theme().dark);
}

/// Read the Windows "apps use light theme" preference; light on failure.
///
/// Queried straight from the registry rather than by shelling out to `reg.exe`:
/// no subprocess on the way to the first frame, and no dependency on the
/// tool's output format. Reading only — the app never writes the registry.
fn system_prefers_dark() -> bool {
    const HKEY_CURRENT_USER: *mut core::ffi::c_void = 0x8000_0001u32 as usize as _;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    const ERROR_SUCCESS: i32 = 0;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: *mut core::ffi::c_void,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }

    use platform::wide;
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut light: u32 = 1; // default to the light theme
    let mut size = core::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            core::ptr::null_mut(),
            &mut light as *mut u32 as *mut core::ffi::c_void,
            &mut size,
        )
    };
    // 0 means "apps use dark theme"; anything unreadable stays light.
    status == ERROR_SUCCESS && light == 0
}

/// Apply the global look for the chosen theme: Windows-native font, the
/// Windows 11 surface grays, and flat boxes in place of FLTK's bevels.
fn apply_theme(pal: Palette) {
    // Segoe UI is the Windows system font; if absent, FLTK keeps its default.
    Font::set_font(Font::Helvetica, "Segoe UI");
    Font::set_font(Font::HelveticaBold, "Segoe UI Semibold");
    // The command preview: a command reads best in fixed width.
    Font::set_font(Font::Courier, "Consolas");
    app::set_font_size(14);

    let (r, g, b) = pal.surface;
    app::background(r, g, b);
    let (r, g, b) = pal.surface2;
    app::background2(r, g, b);
    let (r, g, b) = pal.text;
    app::foreground(r, g, b);
    // The box drawers below are plain `fn` pointers and cannot capture, so
    // they read the theme from here. It is set once, before any drawing.
    let _ = THEME.set(pal);

    // Every raised control (buttons, dropdowns, scrollbar thumbs) and every
    // sunken one (the list) draws flat with a hairline outline. The insets
    // tell FLTK the 1 px border is not part of the content area.
    app::set_frame_type_cb(FrameType::UpBox, flat_raised_box, 1, 1, 2, 2);
    app::set_frame_type_cb(FrameType::DownBox, flat_sunken_box, 1, 1, 2, 2);
    app::set_frame_border_radius_max(CORNER);
    // Keyboard focus stays visible on purpose: the list must be focusable for
    // the Delete key to reach it.
}

/// Corner radius of controls — Windows 11 uses 4 px.
const CORNER: i32 = 4;

/// The theme as the box drawers see it (set by `apply_theme`).
static THEME: OnceLock<Palette> = OnceLock::new();

fn theme() -> Palette {
    *THEME.get_or_init(|| palette(false))
}

/// A button at rest: its colour lifted slightly off the surface, outlined.
/// Lifting the colour FLTK passes in (rather than using a fixed fill) keeps
/// inactive buttons dimmed the way FLTK dims them.
fn flat_raised_box(x: i32, y: i32, w: i32, h: i32, c: Color) {
    let pal = theme();
    draw::draw_rbox(x, y, w, h, CORNER, true, Color::color_average(Color::White, c, pal.box_lift));
    draw::draw_rbox(x, y, w, h, CORNER, false, pal.box_border);
}

/// A pressed button or an inset surface (the list): flush with its colour.
fn flat_sunken_box(x: i32, y: i32, w: i32, h: i32, c: Color) {
    draw::draw_rbox(x, y, w, h, CORNER, true, c);
    draw::draw_rbox(x, y, w, h, CORNER, false, theme().box_border);
}

/// Ask DWM for a dark title bar (Windows 11 / Windows 10 20H1+, attribute 20;
/// earlier Windows 10 builds use 19). Best effort: where neither exists the
/// call fails silently and the bar stays light.
fn apply_dark_title_bar(win: &Window, dark: bool) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut core::ffi::c_void,
            attr: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: u32 = 19;
    let value: i32 = i32::from(dark);
    let hwnd = win.raw_handle();
    unsafe {
        let ptr = &value as *const i32 as *const core::ffi::c_void;
        if DwmSetWindowAttribute(hwnd as _, DWMWA_USE_IMMERSIVE_DARK_MODE, ptr, 4) != 0 {
            DwmSetWindowAttribute(hwnd as _, DWMWA_USE_IMMERSIVE_DARK_MODE_OLD, ptr, 4);
        }
    }
}

/// The app's icon at `size` px: the same drawing `build.rs` bakes into the
/// executable, so the title bar, the taskbar and Explorer all show one
/// picture (see [`icon`]).
fn app_icon_at(size: u32) -> Option<RgbImage> {
    RgbImage::new(&icon::render_rgba(size), size as i32, size as i32, ColorDepth::Rgba8).ok()
}

/// A window's icon, drawn at the size Windows asks for (32 px at 100 %, 40 at
/// 125 %, 48 at 150 %). The taskbar draws this one, not the executable's
/// (measured on Windows 11): a fixed 32 px came out enlarged, soft, from 150 %.
fn window_icon() -> Option<RgbImage> {
    app_icon_at(win32::icon_size().unwrap_or(32))
}

/// Run `test` on the one thread the tests use FLTK on, after the others —
/// FLTK is not made for several threads, and the test harness runs tests on
/// several: a window made while another test measured text crashed the
/// tests, and text measured on another thread than a window's came out as
/// garbage, kept in FLTK's cache of widths (measured). A failure is the
/// test's.
#[cfg(test)]
fn on_fltk_thread(test: impl FnOnce() + Send + 'static) {
    use std::sync::{mpsc, Mutex, OnceLock};
    type Test = (Box<dyn FnOnce() + Send>, mpsc::Sender<std::thread::Result<()>>);
    static FLTK: OnceLock<Mutex<mpsc::Sender<Test>>> = OnceLock::new();
    let tests = FLTK.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Test>();
        std::thread::spawn(move || {
            for (test, done) in rx {
                let _ = done.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(test)));
            }
        });
        Mutex::new(tx)
    });
    let (done, outcome) = mpsc::channel();
    let sent = tests.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).send((Box::new(test), done));
    assert!(sent.is_ok(), "the FLTK thread is gone");
    match outcome.recv() {
        Ok(Ok(())) => {}
        Ok(Err(failure)) => std::panic::resume_unwind(failure),
        Err(_) => panic!("the FLTK thread is gone"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_for_adding_and_a_missing_qpdf() {
        let tally = |added, duplicates, beyond| Added { added, duplicates, beyond };
        assert_eq!(added_message(&tally(3, 0, 0), 0), "Added 3 files");
        assert_eq!(added_message(&tally(1, 4, 0), 2), "Added 1 file · skipped 2 non-PDF · 4 duplicates ignored");
        assert_eq!(added_message(&tally(0, 0, 7), 0), "Added 0 files · 7 left out (limit 10000)");
        let old = Qpdf { exe: PathBuf::from(r"C:\q\qpdf.exe"), version: qpdf_desk::core::qpdf::Version(11, 9, 1) };
        assert_eq!(missing_message(&Missing::TooOld(old)), "QPDF 11.9.1 is too old — QpdfDesk needs 12.4.0 or newer");
        assert!(missing_message(&Missing::NotFound).contains("READ_ME_FIRST.txt"));
    }

    #[test]
    fn the_title_says_what_is_known_of_qpdf() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(app_title(None), format!("QpdfDesk v{version}"));
        assert_eq!(app_title(Some("QPDF 12.4.2")), format!("QpdfDesk v{version} — QPDF 12.4.2"));
        assert_eq!(app_title(Some(missing_title(&Missing::NotFound))), format!("QpdfDesk v{version} — QPDF not found"));
        assert_eq!(missing_title(&Missing::Unusable(PathBuf::from(r"C:\q\qpdf.exe"))), "QPDF cannot be used");
    }
}
