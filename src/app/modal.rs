//! Small modal windows in the app's own look — a question, a line of text,
//! About — instead of FLTK's stock message boxes, whose bevelled icons look
//! decades old. Themed like the rest, in the middle of the
//! main window's monitor, with a ✕: Enter answers with the action, Escape and
//! ✕ cancel. Each runs its own event loop until it closes, as FLTK's did.

use super::*;

use std::cell::{Cell, RefCell};

use fltk::enums::Shortcut;
use fltk::input::Input;
use qpdf_desk::core::qpdf::Qpdf;

/// Width of a question; About is wider.
const ASK_W: i32 = 440;
const ABOUT_W: i32 = 560;
/// The mark beside a question, and the icon in About.
const MARK: i32 = 40;
const PAD: i32 = 16;
/// The `!` of the warning mark: dark on its yellow in both themes (11:1).
const MARK_GLYPH: u32 = 0x1B1B_1B00;

/// The mark beside a question — every one asks about an action that
/// cannot be undone: a dark `!` on a yellow badge, as Windows warns, in both
/// themes (a white one on the dark theme's yellow was hard to read).
fn warning_mark(row: &mut Flex) {
    let mut frame = Frame::default();
    frame.draw(|f| {
        let pal = theme();
        let d = f.w().min(f.h()).min(MARK);
        let (x, y) = (f.x() + (f.w() - d) / 2, f.y());
        draw::set_draw_color(Color::from_rgbi(pal.mark));
        draw::draw_pie(x, y, d, d, 0.0, 360.0);
        draw::set_draw_color(Color::from_rgbi(MARK_GLYPH));
        draw::set_font(Font::HelveticaBold, d * 3 / 5);
        draw::draw_text2("!", x, y, d, d, Align::Center);
    });
    row.fixed(&frame, MARK);
}

/// The app's icon beside a message that only informs, and in About.
fn app_mark(row: &mut Flex) {
    let mut icon = Frame::default();
    // FLTK scales it with the window.
    icon.set_image(app_icon_at(32));
    row.fixed(&icon, MARK);
}

/// A text label: left, wrapped, `@` literal.
fn text(label: &str, bold: bool) -> Frame {
    let mut frame = Frame::default().with_label(&label_text(label));
    frame.set_align(Align::Left | Align::Inside | Align::Top | Align::Wrap | Align::Clip);
    if bold {
        frame.set_label_font(Font::HelveticaBold);
    }
    frame
}

/// A muted text label.
pub(super) fn muted(label: &str) -> Frame {
    let mut frame = text(label, false);
    frame.set_label_color(Color::from_rgbi(theme().muted));
    frame
}

/// A window of `w`×`h` with `title` (a file name in it shown without control
/// characters) and the app's icon.
pub(super) fn window(w: i32, h: i32, title: &str) -> Window {
    let mut win = Window::default().with_size(w, h).with_label(&printable(title));
    win.set_icon(window_icon());
    win
}

/// The buttons row: `secondary` (Cancel, Copy; when given) and `action` at
/// the right; the action answers Enter.
fn buttons(col: &mut Flex, secondary: Option<&str>, action: &str) -> (Option<Button>, Button) {
    let mut row = Flex::default().row();
    row.set_pad(8);
    let _filler = Frame::default();
    let secondary = secondary.map(|label| sized_button(&mut row, label));
    let mut ok = sized_button(&mut row, action);
    ok.set_shortcut(Shortcut::None | Key::Enter);
    row.end();
    col.fixed(&row, ROW_H);
    (secondary, ok)
}

/// Run `win` until it closes; its ✕ and Escape close it.
fn run(mut win: Window, owner: Option<&Window>) {
    win.set_callback(|win| win.hide());
    show_modal(&mut win, owner);
    while win.shown() {
        app::wait();
    }
    Window::delete(win);
}

/// Ask `question` (bold) with `detail` under it, beside the warning mark —
/// every question asked is about an action that cannot be undone: `true`
/// for `action`.
pub(super) fn confirm(owner: &Window, question: &str, detail: &str, action: &str) -> bool {
    let answer = Rc::new(Cell::new(false));
    let win = window(ASK_W, 150, "QpdfDesk");
    let mut col = Flex::default_fill().column();
    col.set_margin(PAD);
    col.set_pad(12);
    let mut row = Flex::default().row();
    row.set_pad(14);
    warning_mark(&mut row);
    let mut lines = Flex::default().column();
    lines.set_pad(4);
    let question = text(question, true);
    lines.fixed(&question, 22);
    let _detail = text(detail, false);
    lines.end();
    row.end();
    let (cancel, mut ok) = buttons(&mut col, Some(t!(ButtonCancel)), action);
    col.end();
    win.end();
    if let Some(cancel) = &cancel {
        closes(cancel, &win);
    }
    {
        let (win, answer) = (win.clone(), Rc::clone(&answer));
        ok.clone().set_callback(move |_| {
            answer.set(true);
            win.clone().hide();
        });
    }
    let _ = ok.take_focus();
    run(win, Some(owner));
    answer.get()
}

/// Ask for a line of text under `prompt`: `None` when cancelled.
pub(super) fn ask_text(owner: &Window, title: &str, prompt: &str, action: &str) -> Option<String> {
    let answer: Rc<RefCell<Option<String>>> = Rc::default();
    let win = window(ASK_W, 150, title);
    let mut col = Flex::default_fill().column();
    col.set_margin(PAD);
    col.set_pad(10);
    let prompt = text(prompt, false);
    col.fixed(&prompt, 22);
    let mut input = Input::default();
    col.fixed(&input, ROW_H - 4);
    let _filler = Frame::default();
    let (cancel, ok) = buttons(&mut col, Some(t!(ButtonCancel)), action);
    col.end();
    win.end();
    if let Some(cancel) = &cancel {
        closes(cancel, &win);
    }
    {
        let (win, answer, input) = (win.clone(), Rc::clone(&answer), input.clone());
        ok.clone().set_callback(move |_| {
            *answer.borrow_mut() = Some(input.value());
            win.clone().hide();
        });
    }
    let _ = input.take_focus();
    run(win, Some(owner));
    answer.take()
}

/// A message with OK, before the main window exists ("already running").
pub(super) fn message(text_line: &str) {
    let win = window(ASK_W, 110, "QpdfDesk");
    let mut col = Flex::default_fill().column();
    col.set_margin(PAD);
    col.set_pad(12);
    let mut row = Flex::default().row();
    row.set_pad(14);
    app_mark(&mut row);
    let _line = text(text_line, true);
    row.end();
    let (_, ok) = buttons(&mut col, None, t!(ButtonOk));
    col.end();
    win.end();
    closes(&ok, &win);
    run(win, None);
}

/// A click on `button` closes `win`.
fn closes(button: &Button, win: &Window) {
    let win = win.clone();
    button.clone().set_callback(move |_| win.clone().hide());
}

/// The About window: what the app is, its version, release date, author,
/// licence — and the QPDF in use, which "Copy" puts on the clipboard with
/// the version for a report.
pub(super) fn about(owner: &Window, qpdf: Option<&Qpdf>) {
    let version = env!("CARGO_PKG_VERSION");
    let released = env!("QD_RELEASE_DATE");
    let author = env!("QD_AUTHOR");
    // A path has no spaces to wrap at: the version, then the path on its own line.
    let qpdf_line = qpdf.map_or_else(|| t!(AboutQpdfNotFound).to_string(), |q| format!("{}\n{}", q.version, q.exe.display()));
    let win = window(ABOUT_W, 350, t!(AboutTitle));
    let mut col = Flex::default_fill().column();
    col.set_margin(PAD + 4);
    col.set_pad(8);
    let mut head = Flex::default().row();
    head.set_pad(14);
    app_mark(&mut head);
    let mut names = Flex::default().column();
    names.set_pad(0);
    let mut name = text("QpdfDesk", true);
    name.set_label_size(app::font_size() + 6);
    names.fixed(&name, 26);
    let _version = muted(&match released {
        "" => t!(AboutVersionUnreleased, version = version),
        date => t!(AboutVersion, version = version, date = date),
    });
    names.end();
    head.end();
    col.fixed(&head, MARK + 4);
    let description = text(t!(AboutDescription), false);
    col.fixed(&description, 40);
    // (label, value, lines it may take)
    let facts = [
        (t!(AboutAuthor), author.to_string(), 1),
        (t!(AboutLicense), format!("MIT — {}", env!("QD_COPYRIGHT")), 2),
        (t!(AboutQpdf), qpdf_line.clone(), 2),
    ];
    // The names in bold, as wide as the widest in the language shown.
    draw::set_font(Font::HelveticaBold, app::font_size());
    let key_w = facts.iter().map(|(key, _, _)| width(key) + LABEL_PAD).max().unwrap_or(0).max(100);
    for (key, value, lines) in facts {
        let mut row = Flex::default().row();
        row.set_pad(8);
        let key = text(key, true);
        row.fixed(&key, key_w);
        let _value = text(&value, false);
        row.end();
        col.fixed(&row, 20 * lines + 2);
    }
    let note = muted(t!(AboutQpdfNote));
    col.fixed(&note, 40);
    let _filler = Frame::default();
    let (copy, close) = buttons(&mut col, Some(t!(ButtonCopy)), t!(ButtonClose));
    col.end();
    win.end();
    if let Some(mut copy) = copy {
        let details = format!("QpdfDesk {version} ({released})\nQPDF {}", qpdf_line.replace('\n', ", "));
        copy.set_tooltip(t!(AboutCopyTip));
        copy.set_callback(move |_| app::copy(&details));
    }
    closes(&close, &win);
    run(win, Some(owner));
}
