//! Files dragged onto the window. Only the list and the fields that hold a
//! path take them, each only what fits it: while a drag is over one, the
//! cursor says "copy" and the target is outlined; anywhere else it says no.
//!
//! FLTK hands a drag to the widget under the pointer, and a text field takes
//! any: it grabs the focus and types the paths in. So every drag event is
//! answered before FLTK dispatches it ([`answer`]) and none reaches a widget;
//! the drop then comes to the main window as a paste, which [`on_drop`] hands
//! on with the target it landed on.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use fltk::{app, draw, enums::{Color, Event}, prelude::*, widget::Widget, window::Window};
use qpdf_desk::text::Msg;

/// Where a drop can land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Target {
    /// The list: files and folders to add.
    List,
    /// Stamp / background: its one PDF file.
    Overlay,
    /// Attachments: files to embed.
    Attachments,
    /// Output: the one folder of the results.
    OutputFolder,
}

/// The targets of the main window, and whether one takes a drop now.
struct Zones {
    win: Window,
    targets: Vec<(Target, Widget)>,
    /// Asked once per target a drag enters; `false` refuses it (it may say
    /// why in the status line).
    accepts: Box<dyn FnMut(Target) -> bool>,
}

thread_local! {
    static ZONES: RefCell<Option<Zones>> = const { RefCell::new(None) };
    /// The target under the drag and whether it takes it (outlined if so).
    static HOVER: Cell<Option<(Target, bool)>> = const { Cell::new(None) };
    /// Where the last drop landed: the paste that follows goes there.
    static DROPPED: Cell<Option<Target>> = const { Cell::new(None) };
}

/// Width of the outline around the target under a drag.
const OUTLINE: i32 = 2;

/// `targets` of `win` take drops, as long as `accepts` says so.
pub(super) fn install(win: &mut Window, targets: Vec<(Target, Widget)>, accepts: impl FnMut(Target) -> bool + 'static) {
    win.draw(|_| {
        let Some((target, true)) = HOVER.get() else { return };
        ZONES.with(|zones| {
            let Ok(zones) = zones.try_borrow() else { return };
            if let Some((_, widget)) = zones.as_ref().and_then(|z| z.targets.iter().find(|(t, _)| *t == target)) {
                draw::set_draw_color(Color::from_rgbi(super::theme().accent));
                for inset in 0..OUTLINE {
                    draw::draw_rect(widget.x() + inset, widget.y() + inset, widget.w() - 2 * inset, widget.h() - 2 * inset);
                }
            }
        });
    });
    ZONES.with_borrow_mut(|zones| *zones = Some(Zones { win: win.clone(), targets, accepts: Box::new(accepts) }));
}

/// The answer to a drag event of any window — `None` for any other event.
pub(super) fn answer(event: Event) -> Option<bool> {
    Some(match event {
        Event::DndEnter | Event::DndDrag => hover(),
        Event::DndLeave => {
            set_hover(None);
            true
        }
        Event::DndRelease => {
            let target = HOVER.get().filter(|(_, takes)| *takes).map(|(target, _)| target);
            set_hover(None);
            DROPPED.set(target);
            // The paste goes to the widget under the pointer: the window.
            if target.is_some() {
                ZONES.with_borrow(|zones| zones.as_ref().map(|z| app::set_belowmouse(&z.win)));
            }
            target.is_some()
        }
        _ => return None,
    })
}

/// A drag moved: outline the target under it; `true` when it takes the drop.
fn hover() -> bool {
    let now = ZONES.with_borrow_mut(|zones| {
        let zones = zones.as_mut()?;
        // A dialog open: the main window takes nothing.
        if app::modal().is_some() {
            return None;
        }
        let (x, y) = (app::event_x(), app::event_y());
        let (target, _) = zones.targets.iter().find(|(_, w)| w.visible_r() && (w.x()..w.x() + w.w()).contains(&x) && (w.y()..w.y() + w.h()).contains(&y))?;
        match HOVER.get() {
            Some((same, takes)) if same == *target => Some((same, takes)),
            _ => Some((*target, (zones.accepts)(*target))),
        }
    });
    set_hover(now);
    now.is_some_and(|(_, takes)| takes)
}

/// Mark `now` as under the drag, repainting the outline when it moved.
fn set_hover(now: Option<(Target, bool)>) {
    if HOVER.replace(now) != now {
        ZONES.with_borrow(|zones| zones.as_ref().map(|z| z.win.clone().redraw()));
    }
}

/// A drop on `win`: `dropped` gets its target and paths. The window answers
/// the paste before FLTK's own handling does: that offers an event it does
/// not know to every child from the focused one on, and a text field took
/// the paths as typed text (measured) — so a drop never reaches it.
pub(super) fn on_drop(win: &mut Window, mut dropped: impl FnMut(Target, Vec<PathBuf>) + 'static) {
    win.super_handle_first(false);
    win.handle(move |_, event| {
        // Only a drop: a paste anywhere else is the focused field's.
        let Some(target) = (event == Event::Paste).then(|| DROPPED.take()).flatten() else { return false };
        // One path per line, as Explorer delivers them.
        dropped(target, app::event_text().lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect());
        true
    });
}

/// What of `paths` a drop on `target` takes, or why it takes none: the
/// overlay one file, the output one folder, the attachments only files.
/// The list takes everything, unlooked at: thousands of paths dropped at
/// once are sorted out by the worker that adds them, not on the UI thread.
pub(super) fn fit(target: Target, paths: Vec<PathBuf>, is_dir: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>, Msg> {
    if matches!(target, Target::List) {
        return Ok(paths);
    }
    let folders = paths.iter().filter(|path| is_dir(path)).count();
    match target {
        Target::Overlay if paths.len() != 1 || folders > 0 => Err(Msg::DropOnePdf),
        Target::OutputFolder if paths.len() != 1 || folders != 1 => Err(Msg::DropOneFolder),
        Target::Attachments if folders > 0 => Err(Msg::DropFilesOnly),
        Target::List | Target::Overlay | Target::OutputFolder | Target::Attachments => Ok(paths),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn a_drop_never_reaches_the_focused_field() {
        super::super::on_fltk_thread(|| {
            // Off screen: the window must be shown for its field to take events.
            let mut win = Window::new(-10_000, -10_000, 200, 100, None);
            let mut field = fltk::input::Input::new(10, 10, 100, 30, None);
            win.end();
            let pastes = Rc::new(Cell::new(0));
            let counted = Rc::clone(&pastes);
            field.handle(move |_, event| {
                if event == Event::Paste {
                    counted.set(counted.get() + 1);
                }
                false
            });
            let drops = Rc::new(Cell::new(0));
            let counted = Rc::clone(&drops);
            on_drop(&mut win, move |_, _| counted.set(counted.get() + 1));
            win.show();
            let _ = field.take_focus();
            DROPPED.set(Some(Target::List));
            assert!(win.handle_event(Event::Paste));
            assert_eq!((drops.get(), pastes.get()), (1, 0), "the drop is the window's, never the focused field's");
            win.handle_event(Event::Paste);
            assert_eq!((drops.get(), pastes.get()), (1, 1), "any other paste is the focused field's");
            win.hide();
        });
    }

    #[test]
    fn each_target_takes_only_what_fits_it() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let is_dir = |path: &Path| !path.to_string_lossy().ends_with(".pdf");
        let fit = |target, names: &[&str]| fit(target, paths(names), is_dir);
        assert_eq!(fit(Target::List, &["a.pdf", "Scans"]), Ok(paths(&["a.pdf", "Scans"])));
        assert_eq!(fit(Target::Overlay, &["stamp.pdf"]), Ok(paths(&["stamp.pdf"])));
        assert_eq!(fit(Target::Overlay, &["a.pdf", "b.pdf"]), Err(Msg::DropOnePdf), "not the first of several");
        assert_eq!(fit(Target::Overlay, &["Scans"]), Err(Msg::DropOnePdf));
        assert_eq!(fit(Target::OutputFolder, &["Results"]), Ok(paths(&["Results"])));
        assert_eq!(fit(Target::OutputFolder, &["a.pdf"]), Err(Msg::DropOneFolder));
        assert_eq!(fit(Target::OutputFolder, &["One", "Two"]), Err(Msg::DropOneFolder));
        assert_eq!(fit(Target::Attachments, &["a.pdf", "b.pdf"]), Ok(paths(&["a.pdf", "b.pdf"])));
        assert_eq!(fit(Target::Attachments, &["a.pdf", "Scans"]), Err(Msg::DropFilesOnly));
    }
}
