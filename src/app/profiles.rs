//! Profiles in the window: the menu above the steps, and the panel's
//! values as `key=value` pairs — read from its controls, written back to
//! them. Protect's passwords and the password for encrypted inputs have no
//! key, so no profile ever holds one; the files' own pages
//! live on the list, not in the panel.
//!
//! A profile is applied over the panel as it started ("Default"), so a key
//! it lacks — saved by an older version, or damaged — falls back to that.

use super::*;

use fltk::button::CheckButton;
use fltk::input::Input;
use fltk::menu::Choice;
use fltk::misc::Spinner;
use qpdf_desk::profiles::{self, Profile};

/// A checkbox as a profile keeps it.
const ON: &str = "yes";
const OFF: &str = "no";

/// One control a profile keeps — a drop-down with the tokens of its items,
/// what is saved (never the text shown).
pub(super) enum Field {
    Check(CheckButton),
    Choice(Choice, Vec<&'static str>),
    Text(Input),
    Number(Spinner),
}

impl Field {
    pub(super) fn widget(&self) -> fltk::widget::Widget {
        match self {
            Field::Check(check) => check.as_base_widget(),
            Field::Choice(choice, _) => choice.as_base_widget(),
            Field::Text(input) => input.as_base_widget(),
            Field::Number(spinner) => spinner.as_base_widget(),
        }
    }

    fn read(&self) -> String {
        match self {
            Field::Check(check) => if check.value() { ON } else { OFF }.into(),
            Field::Choice(choice, tokens) => usize::try_from(choice.value()).ok().and_then(|i| tokens.get(i)).map_or_else(String::new, |t| t.to_string()),
            Field::Text(input) => input.value(),
            Field::Number(spinner) => spinner.value().to_string(),
        }
    }

    /// Set the control to `value`; a value it cannot take leaves it as it is.
    fn write(&mut self, value: &str) {
        match self {
            Field::Check(check) if value == ON || value == OFF => check.set_value(value == ON),
            Field::Choice(choice, tokens) => {
                if let Some(index) = tokens.iter().position(|token| *token == value) {
                    choice.set_value(index as i32);
                }
            }
            Field::Text(input) => input.set_value(value),
            Field::Number(spinner) => {
                if let Some(number) = value.parse::<f64>().ok().filter(|n| (spinner.minimum()..=spinner.maximum()).contains(n)) {
                    spinner.set_value(number.round());
                }
            }
            Field::Check(_) => {}
        }
    }
}

/// The key of each file to attach (a list: the key repeats).
const ATTACH_KEY: &str = "attachments.file";

/// Every value of the panel now, with the files to attach.
pub(super) fn panel(steps: &Steps, attach: &[PathBuf]) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = steps.fields().iter().map(|(key, field)| (key.to_string(), field.read())).collect();
    values.extend(attach.iter().map(|file| (ATTACH_KEY.to_string(), file.display().to_string())));
    values
}

/// What a profile keeps of the panel now: the steps' switches and the
/// options of the steps switched on — Output's always, it has no switch.
/// The others change nothing in the
/// result — a change to them is no change to the profile (`*`) — and a file
/// path of a step switched off is no one's business; loading a profile
/// sets them to Default.
pub(super) fn snapshot(steps: &Steps, st: &State) -> Vec<(String, String)> {
    kept(panel(steps, &st.attach))
}

/// What a profile keeps of the panel `values` ([`snapshot`]).
pub(super) fn kept(values: Vec<(String, String)>) -> Vec<(String, String)> {
    let on: Vec<Step> = Step::ALL
        .into_iter()
        .filter(|&step| switch_key(step).is_some_and(|switch| values.iter().any(|(key, value)| key == switch && value == ON)))
        .collect();
    values.into_iter().filter(|(key, _)| step_of(key).is_none_or(|step| on.contains(&step))).collect()
}

/// The step whose option a profile key is (`protect.copy` → Protection);
/// `None` for a step's own switch, and for Output's options, which no switch
/// takes away.
pub(super) fn step_of(key: &str) -> Option<Step> {
    let prefix = key.split('.').next()?;
    Step::ALL.into_iter().find(|&step| switch_key(step).and_then(|switch| switch.strip_prefix("step.")) == Some(prefix))
}

/// "Profile: <name>", with `*` when the panel has been changed since it was
/// loaded or saved.
pub(super) fn show_profile(steps: &mut Steps, st: &State) {
    let current = st.profile.as_deref().unwrap_or(t!(ItemProfileDefault));
    let mark = if snapshot(steps, st) != st.baseline { " *" } else { "" };
    set_label(&mut steps.profile, &t!(ProfileLabel, name = current, mark = mark), None);
}

/// The menu: "Profile: <name>" over Default, the saved profiles and what can
/// be done with them.
pub(super) fn fill_menu(steps: &mut Steps, st: &State) {
    show_profile(steps, st);
    let menu = &mut steps.profile;
    menu.clear();
    // Names are plain text (`profiles::check_name`): safe as menu items.
    let mut items: Vec<String> = std::iter::once(t!(ItemProfileDefault).to_string()).chain(st.profiles.iter().map(|p| p.name.clone())).collect();
    let last_name = items.len() as i32 - 1;
    items.push(t!(ItemProfileSaveAs).into());
    if let Some(name) = &st.profile {
        items.push(t!(ItemProfileSaveOver, name = name));
        items.push(t!(ItemProfileDelete, name = name));
    }
    menu.add_choice(&items.join("|"));
    menu.set_mode(last_name, fltk::menu::MenuFlag::MenuDivider);
}

/// An item of the profile menu was chosen.
pub(super) fn profile_chosen(state: &SharedState, widgets: &mut Widgets) {
    let index = widgets.steps.profile.value();
    let (count, current) = {
        let st = state.borrow();
        if st.batch.is_some() {
            return;
        }
        (st.profiles.len() as i32, st.profile.clone())
    };
    match index {
        0 => load_profile(state, widgets, None),
        i if i <= count => {
            let name = state.borrow().profiles.get((i - 1) as usize).map(|p| p.name.clone());
            load_profile(state, widgets, name);
        }
        i if i == count + 1 => save_as(state, widgets),
        // "Save the panel to “X”": the profile in use takes the panel as it is now.
        i if i == count + 2 => {
            if let Some(name) = current {
                store(state, widgets, name);
            }
        }
        i if i == count + 3 => delete(state, widgets, current),
        _ => {}
    }
}

/// Set the panel to Default, then `values` over it.
fn write_panel(st: &mut State, steps: &Steps, values: &[(String, String)]) {
    let mut fields = steps.fields();
    st.attach.clear();
    for (key, value) in st.defaults.iter().chain(values) {
        if key == ATTACH_KEY {
            let file = PathBuf::from(value);
            if !st.attach.contains(&file) {
                st.attach.push(file);
            }
        } else if let Some((_, field)) = fields.iter_mut().find(|(k, _)| k == key) {
            field.write(value);
        }
    }
}

/// Set the panel to profile `name` — or Default.
pub(super) fn load_profile(state: &SharedState, widgets: &mut Widgets, name: Option<String>) {
    let message = t!(ProfileLabel, name = name.as_deref().unwrap_or(t!(ItemProfileDefault)), mark = "");
    {
        let mut st = state.borrow_mut();
        let st = &mut *st;
        let profile = name.as_ref().and_then(|name| st.profiles.iter().find(|p| p.name == *name)).map(|p| p.values.clone());
        // Only what a profile keeps: a file saved before keeping it so still
        // holds the options of steps it leaves off, and they are Default.
        write_panel(st, &widgets.steps, &kept(profile.unwrap_or_default()));
        // As the controls took it: a value one cannot take is not a change.
        st.baseline = snapshot(&widgets.steps, st);
        // The overlay file is a path in its field; the probe looks at it anew.
        let overlay = widgets.steps.overlay_file.value();
        st.overlay = (!overlay.is_empty()).then(|| (PathBuf::from(&overlay), Probe::Pending));
        st.probe_overlay();
        // Advanced options set away from the start are shown, not hidden.
        let fields = widgets.steps.fields();
        for advanced in &mut widgets.steps.advanced {
            let set = |key: &str| fields.iter().any(|(k, field)| *k == key && advanced.holds(field));
            let open = st.baseline.iter().any(|pair| set(&pair.0) && !st.defaults.contains(pair));
            set_advanced(advanced, open || advanced.is_open());
        }
        st.profile = name;
        fill_menu(&mut widgets.steps, st);
    }
    // The first step it switches on shows its options.
    if let Some(&(step, _)) = widgets.steps.toggles.iter().find(|(_, toggle)| toggle.value()) {
        show_step(widgets, step);
    }
    job_edited(state, widgets);
    show_status(&state.borrow(), widgets, &message);
}

/// "Save as a new profile…": the panel under a name typed now.
fn save_as(state: &SharedState, widgets: &mut Widgets) {
    // The dialog runs a nested event loop — no borrow is held across it.
    let Some(typed) = ask_text(&widgets.win, t!(ProfileSaveAsTitle), t!(ProfileSaveAsPrompt), t!(ButtonSave)) else { return };
    // Not the name the menu gives the panel as it starts, either.
    let default = t!(ItemProfileDefault);
    let checked = profiles::check_name(&typed).and_then(|name| match name.to_lowercase() == default.to_lowercase() {
        true => Err(t!(ProfileNameDefault, name = default)),
        false => Ok(name),
    });
    let name = match checked {
        Ok(name) => name,
        Err(why) => return show_status(&state.borrow(), widgets, &t!(ProfileNotSaved, why = why)),
    };
    let taken = state.borrow().profiles.iter().any(|p| p.name.eq_ignore_ascii_case(&name));
    if taken && !confirm(&widgets.win, &t!(ProfileReplaceQuestion, name = name), t!(ProfileReplaceDetail), t!(ButtonReplace)) {
        return;
    }
    store(state, widgets, name);
}

/// Keep the panel as profile `name` (replacing one of that name), on disk too.
fn store(state: &SharedState, widgets: &mut Widgets, name: String) {
    let mut st = state.borrow_mut();
    let values = snapshot(&widgets.steps, &st);
    st.baseline = values.clone();
    st.profiles.retain(|p| !p.name.eq_ignore_ascii_case(&name));
    st.profiles.push(Profile { name: name.clone(), values });
    st.profiles.sort_by_key(|p| p.name.to_lowercase());
    st.profile = Some(name.clone());
    let message = written(&st, &t!(ProfileSaved, name = name));
    fill_menu(&mut widgets.steps, &st);
    show_status(&st, widgets, &message);
}

/// "Delete “X”…", after asking; the panel stays as it is.
fn delete(state: &SharedState, widgets: &mut Widgets, current: Option<String>) {
    let Some(name) = current else { return };
    if !confirm(&widgets.win, &t!(ProfileDeleteQuestion, name = name), t!(ProfileDeleteDetail), t!(ButtonDelete)) {
        return;
    }
    let mut st = state.borrow_mut();
    st.profiles.retain(|p| p.name != name);
    st.profile = None;
    st.baseline = kept(st.defaults.clone());
    let message = written(&st, &t!(ProfileDeleted, name = name));
    fill_menu(&mut widgets.steps, &st);
    show_status(&st, widgets, &message);
}

/// Write the profiles to `profiles.txt`: `done`, or why it failed —
/// they stay in the menu until the app ends either way.
fn written(st: &State, done: &str) -> String {
    let saved = match &st.data_dir {
        Some(dir) => profiles::save(dir, &st.profiles).map_err(|e| outcome::from_io_error(&e).sentence().to_string()),
        None => Err(t!(ProfileNoFolder).into()),
    };
    match saved {
        Ok(()) => done.to_string(),
        Err(why) => t!(ProfileNotWritten, done = done, why = why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn a_profile_keeps_only_the_options_of_the_steps_switched_on() {
        let panel = pairs(&[
            ("step.pages", "no"),
            ("step.overlay", "no"),
            ("step.attachments", "no"),
            ("step.protect", "yes"),
            ("pages.range", "1-3"),
            ("overlay.file", r"C:\Letters\stamp.pdf"),
            ("protect.mode", "lock"),
            ("protect.copy", "no"),
            ("attachments.file", r"C:\Private\notes.txt"),
        ]);
        assert_eq!(
            kept(panel),
            pairs(&[("step.pages", "no"), ("step.overlay", "no"), ("step.attachments", "no"), ("step.protect", "yes"), ("protect.mode", "lock"), ("protect.copy", "no")]),
            "no path of a step switched off"
        );
        // A switch missing (an older file) is a step switched off.
        assert_eq!(kept(pairs(&[("pages.range", "1-3")])), pairs(&[]));
    }

    #[test]
    fn every_option_of_the_panel_belongs_to_its_step() {
        super::super::on_fltk_thread(|| {
            let mut col = Flex::default().column();
            let mut middle = Flex::default().row();
            let (.., steps) = build_steps(&mut middle, &mut col);
            col.end();
            let attach = [PathBuf::from(r"C:\a.txt")];
            for (key, _) in panel(&steps, &attach) {
                // Output's options have no switch to come and go with.
                let output = key.starts_with("output.");
                let switch = Step::ALL.into_iter().any(|step| switch_key(step) == Some(key.as_str()));
                assert!(switch || step_of(&key).is_some() || output, "{key} belongs to no step");
            }
            assert!(kept(panel(&steps, &attach)).iter().any(|(key, _)| key == "output.name"), "Output's options are always kept");
            // A profile setting one opens them: each Advanced holds some.
            let fields = steps.fields();
            for advanced in &steps.advanced {
                assert!(fields.iter().any(|(_, field)| advanced.holds(field)), "Advanced options without a key");
            }
            // Each step's options come and go with its switch.
            for (step, mut toggle) in steps.toggles.clone() {
                for on in [false, true] {
                    toggle.set_value(on);
                    let options = kept(panel(&steps, &attach)).into_iter().filter(|(key, _)| step_of(key) == Some(step)).count();
                    assert_eq!(options > 0, on, "{step:?}");
                }
                toggle.set_value(false);
            }
        });
    }
}
