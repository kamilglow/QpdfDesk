//! Dev builds only (`QD_FIT=<file>`, with `QD_LANG=<code>` for a
//! translation): every label that does not fit its control, panel by panel
//! — what a new language is checked for before it ships, besides the
//! screenshots. Choices and the controls sized by their text fit by
//! construction; this finds the rest: hints, fixed columns, placeholders.

use super::*;

use std::collections::BTreeSet;

/// Room FLTK takes around an inside label: 3 px on each side (measured).
const INSET: i32 = 6;

/// Show every step's options, Advanced open, and list what does not fit.
pub(super) fn check(widgets: &mut Widgets) -> String {
    let mut found = BTreeSet::new();
    // The window lays its rows out in the event loop; before that, the steps
    // panel is 0 px tall (measured).
    let _ = app::wait_for(0.2);
    for step in Step::ALL {
        show_step(widgets, step);
        for advanced in &mut widgets.steps.advanced {
            set_advanced(advanced, true);
        }
        widgets.win.redraw();
        app::flush();
        walk(&widgets.win.as_group(), step.name(), &mut found);
    }
    let mut report: Vec<String> = found.into_iter().collect();
    if report.is_empty() {
        report.push("Every label fits.".into());
    }
    report.join("\r\n") + "\r\n"
}

/// The visible children of `group` whose inside label is wider than they are.
fn walk(group: &Option<fltk::group::Group>, step: &str, found: &mut BTreeSet<String>) {
    let Some(group) = group else { return };
    for index in 0..group.children() {
        let Some(widget) = group.child(index) else { continue };
        if !widget.visible_r() {
            continue;
        }
        let label = widget.label();
        if !label.is_empty() && widget.align().contains(Align::Inside) {
            if widget.align().contains(Align::Wrap) {
                // A wrapped label needs the height of its lines.
                draw::set_font(widget.label_font(), widget.label_size());
                let (_, needed) = draw::wrap_measure(&label, widget.w() - INSET, true);
                if needed + INSET > widget.h() {
                    found.insert(format!("{step}: “{label}” needs {} px of height, has {}", needed + INSET, widget.h()));
                }
            } else {
                let (needed, _) = widget.measure_label();
                if needed + INSET > widget.w() {
                    found.insert(format!("{step}: “{label}” needs {} px, has {}", needed + INSET, widget.w()));
                }
            }
        }
        walk(&widget.as_group(), step, found);
    }
}
