//! The steps panel: the steps, the options of the shown one, the
//! password for encrypted inputs, and the job they make — with the files to
//! attach and the files' own pages, which live outside the panel.

use super::*;

use std::path::Path;

use fltk::button::CheckButton;
use fltk::enums::CallbackTrigger;
use fltk::input::Input;
use fltk::input::InputType;
use fltk::menu::Choice;
use fltk::misc::Spinner;
use fltk::widget::Widget;
use qpdf_desk::core::job::{
    input_password, output_shapes, Angle, Annotations, Attachments, Encrypt, Files, FormAction, Forms, Images, Job, JobError, ObjectStreams,
    Optimize, Output, Overlay, PageMode, Pages, Placement, Print, Privacy, Protect, Rotate, Select,
};
use qpdf_desk::core::filelist::display_name;
use qpdf_desk::core::secret::Secret;

/// Width of the steps panel.
const STEPS_W: i32 = 200;
/// Height of the title and of a step's row: eight steps and a three-line
/// result fit the smallest window (measured).
const STEP_H: i32 = ROW_H - 7;
/// Space between the rows of an options panel.
const OPTION_PAD: i32 = 6;
/// Height of the options area: four rows, as many as Protect needs.
const OPTIONS_H: i32 = 4 * ROW_H + 3 * OPTION_PAD;
/// Narrowest first column of an options panel.
const OPTION_LABEL_W: i32 = 150;

/// Width of the first column of an options panel: its widest label in the
/// language shown — a step's name in bold, Rotate with its box — so a
/// longer translation widens the column instead of being cut.
fn label_column() -> i32 {
    static WIDTH: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *WIDTH.get_or_init(|| {
        let plain = [
            Msg::OverlayFile,
            Msg::OverlayUse,
            Msg::OverlayOnPages,
            Msg::ProtectOpenPassword,
            Msg::ProtectOwnerPassword,
            Msg::ProtectAllow,
            Msg::OutputFileName,
            Msg::OutputNameEnding,
        ]
        .map(|message| text_width(qpdf_desk::text::get(message)));
        let rotate = text_width(t!(PagesRotate)) + CHECK_BOX;
        draw::set_font(Font::HelveticaBold, app::font_size());
        let bold = Step::ALL.map(|step| width(step.name()));
        plain.into_iter().chain(bold).max().unwrap_or(0).saturating_add(LABEL_PAD).max(rotate).max(OPTION_LABEL_W)
    })
}

/// One item of a drop-down: the token `settings.txt` and the profiles keep
/// (never shown, never translated, so a new wording or language keeps what
/// was saved), its text (`None`: the token itself, e.g. a digit), and what
/// it means — the one place a position gets its meaning.
pub(super) type Item<T> = (&'static str, Option<Msg>, T);

/// The text an item shows.
pub(super) fn item_label<T>((token, text, _): &Item<T>) -> &'static str {
    text.map_or(*token, qpdf_desk::text::get)
}

/// The position of the item meaning `value` in `items` (the first when none does).
fn item_index<T: PartialEq>(items: &[Item<T>], value: T) -> i32 {
    items.iter().position(|(_, _, v)| *v == value).unwrap_or(0) as i32
}

const PAGE_MODES: [Item<PageMode>; 4] = [
    ("all", Some(Msg::ItemPagesAll), PageMode::All),
    ("keep", Some(Msg::ItemPagesKeep), PageMode::Keep),
    ("delete", Some(Msg::ItemPagesDelete), PageMode::Delete),
    ("reverse", Some(Msg::ItemPagesReverse), PageMode::Reverse),
];
const FILES: [Item<Files>; 3] = [
    ("each", Some(Msg::ItemFilesEach), Files::Each),
    ("merge", Some(Msg::ItemFilesMerge), Files::Merge),
    ("mix", Some(Msg::ItemFilesMix), Files::Collate),
];
const OVERLAY_MODES: [Item<bool>; 2] = [("on_top", Some(Msg::ItemOverlayOnTop), false), ("under", Some(Msg::ItemOverlayUnder), true)];
const PLACEMENTS: [Item<Placement>; 3] = [
    ("page_one", Some(Msg::ItemPlacementPageOne), Placement::PageOne),
    ("in_order", Some(Msg::ItemPlacementInOrder), Placement::InOrder),
    ("in_turn", Some(Msg::ItemPlacementInTurn), Placement::InTurn),
];
const ANGLES: [Item<Angle>; 3] = [
    ("right", Some(Msg::ItemAngleRight), Angle::Right),
    ("upside_down", Some(Msg::ItemAngleUpsideDown), Angle::UpsideDown),
    ("left", Some(Msg::ItemAngleLeft), Angle::Left),
];
const FORMS: [Item<FormAction>; 4] = [
    ("flatten_all", Some(Msg::ItemFormsFlattenAll), FormAction::Flatten(Annotations::All)),
    ("flatten_print", Some(Msg::ItemFormsFlattenPrint), FormAction::Flatten(Annotations::Print)),
    ("flatten_screen", Some(Msg::ItemFormsFlattenScreen), FormAction::Flatten(Annotations::Screen)),
    ("remove_form", Some(Msg::ItemFormsRemoveForm), FormAction::RemoveForm),
];
/// Optimization: what becomes of the images — the quality comes from its
/// own field ([`read_job`]).
const OPTIMIZE_MODES: [Item<Images>; 3] = [
    ("lossless", Some(Msg::ItemOptimizeLossless), Images::Lossless),
    ("smaller", Some(Msg::ItemOptimizeSmaller), Images::Jpeg),
    ("quality", Some(Msg::ItemOptimizeQuality), Images::JpegQuality(75)),
];
const OBJECT_STREAMS: [Item<ObjectStreams>; 3] = [
    ("generate", Some(Msg::ItemStreamsGenerate), ObjectStreams::Generate),
    ("preserve", Some(Msg::ItemStreamsPreserve), ObjectStreams::Preserve),
    ("disable", Some(Msg::ItemStreamsDisable), ObjectStreams::Disable),
];
/// Output: in a folder of its own, or next to the original.
const PLACES: [Item<bool>; 2] = [("original", Some(Msg::ItemPlaceOriginal), false), ("folder", Some(Msg::ItemPlaceFolder), true)];
/// Protection: lock, or take the lock off.
const PROTECT_MODES: [Item<bool>; 2] = [("lock", Some(Msg::ItemProtectLock), true), ("unlock", Some(Msg::ItemProtectUnlock), false)];
const PRINTS: [Item<Print>; 3] = [
    ("full", Some(Msg::ItemPrintFull), Print::Full),
    ("low", Some(Msg::ItemPrintLow), Print::Low),
    ("none", Some(Msg::ItemPrintNone), Print::None),
];

/// The steps panel: each step's switch and name, its options, and the
/// password for encrypted inputs.
#[derive(Clone)]
pub(super) struct Steps {
    /// The profile menu above the steps.
    pub(super) profile: MenuButton,
    /// Whether each step with a switch ([`switch_key`]) is part of the job.
    pub(super) toggles: Vec<(Step, CheckButton)>,
    /// The step names; a click shows that step's options.
    names: Vec<Button>,
    /// One options panel per step; only the shown one is visible.
    panels: Vec<Flex>,
    /// The result's name, or why the job cannot run.
    result: Frame,
    pages_select: Choice,
    pages_range: Input,
    pages_files: Choice,
    /// The page view of the selected file: its own pages.
    arrange_pages: Button,
    /// What the files' own pages do — or, merging, what is lost.
    pages_hint: Frame,
    rotate: CheckButton,
    rotate_angle: Choice,
    rotate_range: Input,
    overlay_mode: Choice,
    /// The chosen file's path (read-only: chosen with `overlay_choose`).
    pub(super) overlay_file: Input,
    overlay_choose: Button,
    /// What the probe found about the file.
    overlay_info: Frame,
    overlay_placement: Choice,
    /// The pages it goes on (empty: all).
    overlay_to: Input,
    forms_mode: Choice,
    flatten_rotation: CheckButton,
    attach_add: Button,
    attach_clear: Button,
    /// The files chosen to attach.
    attach_info: Frame,
    remove_attachments: CheckButton,
    remove_info: CheckButton,
    remove_xmp: CheckButton,
    fixed_id: CheckButton,
    remove_structure: CheckButton,
    remove_page_labels: CheckButton,
    optimize_mode: Choice,
    jpeg_quality: Spinner,
    keep_original: CheckButton,
    object_streams: Choice,
    keep_inline_images: CheckButton,
    min_width: Spinner,
    min_height: Spinner,
    /// The Advanced options of Forms, Privacy and Optimize.
    pub(super) advanced: Vec<Advanced>,
    protect_mode: Choice,
    /// Unlock: where the password of a locked file goes.
    protect_hint: Frame,
    /// The passwords and permissions — Encrypt's only.
    encrypt_rows: Flex,
    user_password: Input,
    user_repeat: Input,
    owner_password: Input,
    owner_repeat: Input,
    print: Choice,
    allow_copy: CheckButton,
    allow_edit: CheckButton,
    allow_annotate: CheckButton,
    allow_forms: CheckButton,
    allow_assemble: CheckButton,
    output_place: Choice,
    /// The chosen folder's path (read-only: chosen with `output_choose`).
    output_folder: Input,
    output_choose: Button,
    /// "Name ending", or "File name" for a merge.
    output_name_label: Frame,
    output_name: Input,
    /// Where the typed name goes (an empty one shows what it gives).
    output_name_hint: Frame,
    split: CheckButton,
    split_pages: Spinner,
    linearize: CheckButton,
    input_password: Input,
    show_password: CheckButton,
    /// The command for the selected file.
    pub(super) command: Button,
}

/// A step's Advanced options: shown or hidden by `toggle`, applied
/// either way; while shown they take the place of the step's hint, if any.
#[derive(Clone)]
pub(super) struct Advanced {
    toggle: Button,
    /// The step's options, laid out again when the rows show or hide.
    page: Flex,
    rows: Flex,
    hint: Option<Flex>,
}

impl Advanced {
    pub(super) fn is_open(&self) -> bool {
        self.rows.visible()
    }

    /// Whether `field` is one of these options.
    pub(super) fn holds(&self, field: &Field) -> bool {
        field.widget().inside(&self.rows)
    }
}

impl Steps {
    /// The switch of `step`; Output has none.
    pub(super) fn toggle(&self, step: Step) -> Option<&CheckButton> {
        self.toggles.iter().find(|(s, _)| *s == step).map(|(_, toggle)| toggle)
    }

    /// The Advanced options of `step`, if it has any.
    #[cfg(debug_assertions)]
    pub(super) fn advanced_of(&mut self, step: Step) -> Option<&mut Advanced> {
        let panel = &self.panels[step as usize];
        self.advanced.iter_mut().find(|advanced| advanced.rows.inside(panel))
    }

    /// Whether `step` is part of the job: switched on, or without a switch.
    pub(super) fn on(&self, step: Step) -> bool {
        self.toggle(step).is_none_or(|toggle| toggle.value())
    }

    /// Where a dropped path goes: the fields that hold one, and the options
    /// of Attachments for the files to attach.
    pub(super) fn drop_targets(&self) -> [(Target, Widget); 3] {
        [
            (Target::Overlay, self.overlay_file.as_base_widget()),
            (Target::Attachments, self.panels[Step::Attachments as usize].as_base_widget()),
            (Target::OutputFolder, self.output_folder.as_base_widget()),
        ]
    }

    /// The password for encrypted inputs, when one is typed.
    pub(super) fn password(&self) -> Option<Secret> {
        input_password(self.input_password.value()).ok().flatten()
    }

    /// The steps switched on that change the file, in panel order — as
    /// [`Job::steps`] says them, also for a panel that does not read as a job.
    pub(super) fn chosen(&self) -> Vec<Step> {
        let shapes = |step| step != Step::Output || output_shapes(self.split.value(), self.linearize.value());
        Step::ALL.into_iter().filter(|&step| self.on(step) && shapes(step)).collect()
    }

    /// Every control a profile keeps, by key — never a password field. The
    /// overlay file is the path in its field.
    pub(super) fn fields(&self) -> Vec<(&'static str, Field)> {
        use Field::{Check, Choice as Pick, Number, Text};
        let mut fields: Vec<(&'static str, Field)> =
            self.toggles.iter().filter_map(|(step, toggle)| Some((switch_key(*step)?, Check(toggle.clone())))).collect();
        fields.extend([
            ("pages.mode", Pick(self.pages_select.clone(), tokens(&PAGE_MODES))),
            ("pages.range", Text(self.pages_range.clone())),
            ("pages.files", Pick(self.pages_files.clone(), tokens(&FILES))),
            ("pages.rotate", Check(self.rotate.clone())),
            ("pages.rotate_angle", Pick(self.rotate_angle.clone(), tokens(&ANGLES))),
            ("pages.rotate_range", Text(self.rotate_range.clone())),
            ("overlay.mode", Pick(self.overlay_mode.clone(), tokens(&OVERLAY_MODES))),
            ("overlay.file", Text(self.overlay_file.clone())),
            ("overlay.placement", Pick(self.overlay_placement.clone(), tokens(&PLACEMENTS))),
            ("overlay.to", Text(self.overlay_to.clone())),
            ("forms.mode", Pick(self.forms_mode.clone(), tokens(&FORMS))),
            ("forms.flatten_rotation", Check(self.flatten_rotation.clone())),
            ("attachments.remove_all", Check(self.remove_attachments.clone())),
            ("privacy.info", Check(self.remove_info.clone())),
            ("privacy.xmp", Check(self.remove_xmp.clone())),
            ("privacy.fixed_id", Check(self.fixed_id.clone())),
            ("privacy.structure", Check(self.remove_structure.clone())),
            ("privacy.page_labels", Check(self.remove_page_labels.clone())),
            ("optimize.mode", Pick(self.optimize_mode.clone(), tokens(&OPTIMIZE_MODES))),
            ("optimize.jpeg_quality", Number(self.jpeg_quality.clone())),
            ("optimize.keep_original", Check(self.keep_original.clone())),
            ("optimize.object_streams", Pick(self.object_streams.clone(), tokens(&OBJECT_STREAMS))),
            ("optimize.keep_inline_images", Check(self.keep_inline_images.clone())),
            ("optimize.min_width", Number(self.min_width.clone())),
            ("optimize.min_height", Number(self.min_height.clone())),
            ("protect.mode", Pick(self.protect_mode.clone(), tokens(&PROTECT_MODES))),
            ("protect.print", Pick(self.print.clone(), tokens(&PRINTS))),
            ("protect.copy", Check(self.allow_copy.clone())),
            ("protect.edit", Check(self.allow_edit.clone())),
            ("protect.annotate", Check(self.allow_annotate.clone())),
            ("protect.forms", Check(self.allow_forms.clone())),
            ("protect.assemble", Check(self.allow_assemble.clone())),
            ("output.place", Pick(self.output_place.clone(), tokens(&PLACES))),
            ("output.folder", Text(self.output_folder.clone())),
            ("output.name", Text(self.output_name.clone())),
            ("output.split", Check(self.split.clone())),
            ("output.split_pages", Number(self.split_pages.clone())),
            ("output.linearize", Check(self.linearize.clone())),
        ]);
        fields
    }
}

/// The profile key of `step`'s switch — none for Output: where the
/// results go always counts. A step's options are keyed by the same word
/// (`pages.range`).
pub(super) fn switch_key(step: Step) -> Option<&'static str> {
    Some(match step {
        Step::Pages => "step.pages",
        Step::Overlay => "step.overlay",
        Step::Forms => "step.forms",
        Step::Attachments => "step.attachments",
        Step::Privacy => "step.privacy",
        Step::Optimize => "step.optimize",
        Step::Protect => "step.protect",
        Step::Output => return None,
    })
}

/// Build the steps panel at the end of `middle` (and close it), then the
/// options area and the password row in `col`. Returns the three groups
/// (fixed while a batch runs) and the controls.
pub(super) fn build_steps(middle: &mut Flex, col: &mut Flex) -> (Flex, Flex, Flex, Steps) {
    let mut panel = Flex::default().column();
    panel.set_pad(2);
    // The profile in use heads the steps it sets; its items are filled
    // in by `profiles::fill_menu`.
    let mut profile = MenuButton::default();
    profile.set_align(Align::Left | Align::Inside);
    profile.set_label_font(Font::HelveticaBold);
    profile.set_tooltip(
        t!(ProfileTooltip),
    );
    panel.fixed(&profile, STEP_H);
    let mut toggles = Vec::new();
    let mut names = Vec::new();
    for step in Step::ALL {
        let mut row = Flex::default().row();
        row.set_pad(0);
        let toggle = match switch_key(step) {
            None => Frame::default().as_base_widget(),
            Some(_) => {
                let toggle = CheckButton::default();
                toggles.push((step, toggle.clone()));
                toggle.as_base_widget()
            }
        };
        // A button label reads `&` as a shortcut mark.
        let mut name = Button::default().with_label(&step.name().replace('&', "&&"));
        name.set_align(Align::Left | Align::Inside);
        name.set_frame(FrameType::FlatBox);
        name.set_down_frame(FrameType::FlatBox);
        name.clear_visible_focus();
        row.fixed(&toggle, 24);
        row.end();
        panel.fixed(&row, STEP_H);
        names.push(name);
    }
    let mut result = Frame::default();
    result.set_align(Align::Left | Align::Inside | Align::Top | Align::Wrap);
    panel.end();
    middle.fixed(&panel, STEPS_W);
    middle.end();

    let options = Flex::default().column();
    let mut panels = Vec::new();

    // Pages.
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Pages.name(), true);
    let pages_select = choice(&mut row, &PAGE_MODES, PageMode::Keep, 300);
    let mut pages_range = field("");
    pages_range.set_tooltip(t!(RangeSyntaxTip));
    hint(&mut row, t!(PagesExample), true);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, t!(PagesFiles), false);
    let pages_files = choice(&mut row, &FILES, Files::Each, 300);
    let mut arrange_pages = fitted_button(&mut row, t!(PagesArrange));
    arrange_pages.set_tooltip(t!(PagesArrangeTip));
    let _spacer = Frame::default();
    end_row(&mut page, row);
    let mut row = option_row();
    let rotate = CheckButton::default().with_label(t!(PagesRotate));
    row.fixed(&rotate, label_column());
    let rotate_angle = choice(&mut row, &ANGLES, Angle::Right, 300);
    let mut rotate_range = field(t!(PlaceholderAllPages));
    rotate_range.set_tooltip(t!(RangeSyntaxTip));
    hint(&mut row, t!(PagesRotateHint), true);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let pages_hint = hint(&mut row, t!(PagesOwnHint), false);
    end_row(&mut page, row);
    panels.push(end_panel(page));

    // Stamp / background: an overlay or an underlay.
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Overlay.name(), true);
    let overlay_mode = choice(&mut row, &OVERLAY_MODES, false, 300);
    hint(&mut row, t!(OverlayHint), false);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, t!(OverlayFile), false);
    let mut overlay_file = field(t!(JobOverlayChoose));
    overlay_file.set_readonly(true);
    let overlay_choose = sized_button(&mut row, t!(ButtonChoose));
    let overlay_info = Frame::default();
    row.fixed(&overlay_info, 170);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, t!(OverlayUse), false);
    let overlay_placement = choice(&mut row, &PLACEMENTS, Placement::PageOne, 300);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, t!(OverlayOnPages), false);
    let mut overlay_to = field(t!(PlaceholderAllPages));
    overlay_to.set_tooltip(&format!("{}\n\n{}", t!(OverlayToTip), t!(RangeSyntaxTip)));
    row.fixed(&overlay_to, 220);
    hint(&mut row, t!(OverlayOnPagesHint), false);
    end_row(&mut page, row);
    panels.push(end_panel(page));

    // Forms & annotations.
    let mut advanced = Vec::new();
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Forms.name(), true);
    let forms_mode = choice(&mut row, &FORMS, FormAction::Flatten(Annotations::All), 330);
    let toggle = advanced_toggle(&mut row);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    hint(&mut row, t!(FormsHint), false);
    end_row(&mut page, row);
    let mut rows = advanced_rows();
    let mut row = option_row();
    first_column(&mut row, "", false);
    let flatten_rotation = check(&mut row, t!(FormsFlattenRotation), false);
    end_row(&mut rows, row);
    advanced.push(end_advanced(&mut page, rows, toggle, None));
    panels.push(end_panel(page));

    // Attachments.
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Attachments.name(), true);
    let attach_add = sized_button(&mut row, t!(AttachAdd));
    let attach_clear = fitted_button(&mut row, t!(AttachClear));
    let mut attach_info = Frame::default();
    attach_info.set_align(Align::Left | Align::Inside | Align::Clip);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let remove_attachments = check(&mut row, t!(AttachRemoveAll), false);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    hint(&mut row, t!(AttachHint), false);
    end_row(&mut page, row);
    panels.push(end_panel(page));

    // Privacy.
    let privacy = Privacy::default();
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Privacy.name(), true);
    let remove_info = check(&mut row, t!(PrivacyInfo), privacy.remove_info);
    let remove_xmp = check(&mut row, t!(PrivacyXmp), privacy.remove_xmp);
    let toggle = advanced_toggle(&mut row);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let fixed_id = check(&mut row, t!(PrivacyFixedId), privacy.fixed_id);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    hint(&mut row, t!(PrivacyHint), false);
    end_row(&mut page, row);
    let mut rows = advanced_rows();
    let mut row = option_row();
    first_column(&mut row, "", false);
    let mut remove_structure = check(&mut row, t!(PrivacyStructure), privacy.remove_structure);
    remove_structure.set_tooltip(t!(PrivacyStructureTip));
    let remove_page_labels = check(&mut row, t!(PrivacyPageLabels), privacy.remove_page_labels);
    end_row(&mut rows, row);
    advanced.push(end_advanced(&mut page, rows, toggle, None));
    panels.push(end_panel(page));

    // Optimize.
    let optimize = Optimize::default();
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Optimize.name(), true);
    let mut optimize_mode = choice(&mut row, &OPTIMIZE_MODES, optimize.images, 290);
    optimize_mode.set_tooltip(t!(OptimizeModeTip));
    label(&mut row, t!(OptimizeJpegQuality));
    let jpeg_quality = spinner(&mut row, 0.0..=100.0, 5.0, 75.0, 70);
    let toggle = advanced_toggle(&mut row);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let mut keep_original = check(&mut row, t!(OptimizeKeepOriginal), optimize.keep_if_not_smaller);
    keep_original.set_tooltip(t!(OptimizeKeepOriginalTip));
    end_row(&mut page, row);
    let mut hint_row = option_row();
    first_column(&mut hint_row, "", false);
    hint(&mut hint_row, t!(OptimizeHint), false);
    end_row(&mut page, hint_row.clone());
    let mut rows = advanced_rows();
    let mut row = option_row();
    first_column(&mut row, "", false);
    label(&mut row, t!(OptimizeObjectStreams));
    let mut object_streams = choice(&mut row, &OBJECT_STREAMS, optimize.object_streams, 170);
    object_streams.set_tooltip(t!(OptimizeObjectStreamsTip));
    end_row(&mut rows, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let mut keep_inline_images = check(&mut row, t!(OptimizeKeepInline), optimize.keep_inline_images);
    keep_inline_images.set_tooltip(t!(OptimizeKeepInlineTip));
    label(&mut row, t!(OptimizeSkipUnder));
    let min_width = spinner(&mut row, 0.0..=100_000.0, 16.0, f64::from(optimize.min_image.0), 75);
    label(&mut row, "×");
    let min_height = spinner(&mut row, 0.0..=100_000.0, 16.0, f64::from(optimize.min_image.1), 75);
    label(&mut row, t!(OptimizePixels));
    end_row(&mut rows, row);
    advanced.push(end_advanced(&mut page, rows, toggle, Some(hint_row)));
    panels.push(end_panel(page));

    // Protect.
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Protect.name(), true);
    let protect_mode = choice(&mut row, &PROTECT_MODES, true, 300);
    let protect_hint = hint(&mut row, t!(ProtectUnlockHint), false);
    end_row(&mut page, row);
    let mut encrypt_rows = Flex::default().column();
    encrypt_rows.set_pad(OPTION_PAD);
    let mut row = option_row();
    first_column(&mut row, t!(ProtectOpenPassword), false);
    let user_password = secret_input(t!(PlaceholderOptional));
    label(&mut row, t!(ProtectRepeat));
    let user_repeat = secret_input("");
    end_row(&mut encrypt_rows, row);
    let mut row = option_row();
    first_column(&mut row, t!(ProtectOwnerPassword), false);
    let owner_password = secret_input(t!(PlaceholderRequired));
    label(&mut row, t!(ProtectRepeat));
    let owner_repeat = secret_input("");
    end_row(&mut encrypt_rows, row);
    let mut row = option_row();
    first_column(&mut row, t!(ProtectAllow), false);
    label(&mut row, t!(ProtectPrint));
    let print = choice(&mut row, &PRINTS, Print::Full, 110);
    let allow_copy = check(&mut row, t!(ProtectCopy), true);
    let allow_edit = check(&mut row, t!(ProtectEdit), false);
    let allow_annotate = check(&mut row, t!(ProtectAnnotate), true);
    let allow_forms = check(&mut row, t!(ProtectFillForms), true);
    let allow_assemble = check(&mut row, t!(ProtectAssemble), false);
    let _spacer = Frame::default();
    end_row(&mut encrypt_rows, row);
    encrypt_rows.end();
    page.fixed(&encrypt_rows, 3 * ROW_H + 2 * OPTION_PAD);
    panels.push(end_panel(page));

    // Output: where, under which name, in what form.
    let mut page = options_panel();
    let mut row = option_row();
    first_column(&mut row, Step::Output.name(), true);
    let output_place = choice(&mut row, &PLACES, false, 190);
    let mut output_folder = Input::default();
    output_folder.set_readonly(true);
    let output_choose = sized_button(&mut row, t!(ButtonChoose));
    end_row(&mut page, row);
    let mut row = option_row();
    let output_name_label = first_column(&mut row, "", false);
    let output_name = field("");
    row.fixed(&output_name, 220);
    let output_name_hint = hint(&mut row, "", false);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let split = check(&mut row, t!(OutputSplit), false);
    let split_pages = spinner(&mut row, 1.0..=100_000.0, 1.0, 1.0, 90);
    label(&mut row, t!(OutputSplitPages));
    // QPDF copies each part's pages into an empty document (its manual).
    hint(&mut row, t!(OutputSplitHint), false);
    end_row(&mut page, row);
    let mut row = option_row();
    first_column(&mut row, "", false);
    let linearize = check(&mut row, t!(OutputLinearize), false);
    hint(&mut row, t!(OutputLinearizeHint), false);
    end_row(&mut page, row);
    panels.push(end_panel(page));

    options.end();
    col.fixed(&options, OPTIONS_H);

    // The password for encrypted inputs: one for the batch, never saved;
    // at the end of the row, under the steps, the command they make.
    let mut password_row = option_row();
    label(&mut password_row, t!(PasswordLabel));
    let mut input_password = secret_input("");
    input_password.set_tooltip(t!(PasswordTip));
    password_row.fixed(&input_password, 220);
    let show_password = check(&mut password_row, t!(PasswordShow), false);
    hint(&mut password_row, t!(PasswordNeverSaved), true);
    let _spacer = Frame::default();
    let mut command = sized_button(&mut password_row, t!(ButtonCommand));
    command.set_tooltip(t!(ButtonCommandTip));
    password_row.end();
    col.fixed(&password_row, ROW_H);

    let steps = Steps {
        profile,
        toggles,
        names,
        panels,
        result,
        pages_select,
        pages_range,
        pages_files,
        arrange_pages,
        pages_hint,
        rotate,
        rotate_angle,
        rotate_range,
        overlay_mode,
        overlay_file,
        overlay_choose,
        overlay_info,
        overlay_placement,
        overlay_to,
        forms_mode,
        flatten_rotation,
        attach_add,
        attach_clear,
        attach_info,
        remove_attachments,
        remove_info,
        remove_xmp,
        fixed_id,
        remove_structure,
        remove_page_labels,
        optimize_mode,
        jpeg_quality,
        keep_original,
        object_streams,
        keep_inline_images,
        min_width,
        min_height,
        advanced,
        protect_mode,
        protect_hint,
        encrypt_rows,
        user_password,
        user_repeat,
        owner_password,
        owner_repeat,
        print,
        allow_copy,
        allow_edit,
        allow_annotate,
        allow_forms,
        allow_assemble,
        output_place,
        output_folder,
        output_choose,
        output_name_label,
        output_name,
        output_name_hint,
        split,
        split_pages,
        linearize,
        input_password,
        show_password,
        command,
    };
    (panel, options, password_row, steps)
}

/// "Advanced ▸" at the end of a step's first row.
fn advanced_toggle(row: &mut Flex) -> Button {
    let _spacer = Frame::default();
    let toggle = Button::default().with_label(&advanced_label(false));
    row.fixed(&toggle, text_width(&advanced_label(true)) + BUTTON_PAD);
    toggle
}

/// The Advanced button, closed and open.
fn advanced_label(open: bool) -> String {
    format!("{} {}", t!(ButtonAdvanced), if open { "▾" } else { "▸" })
}

/// The column of a step's Advanced rows, hidden at first.
fn advanced_rows() -> Flex {
    let mut rows = Flex::default().column();
    rows.set_pad(OPTION_PAD);
    rows
}

/// Close `rows` in `page`: the Advanced options of its step.
fn end_advanced(page: &mut Flex, mut rows: Flex, toggle: Button, hint: Option<Flex>) -> Advanced {
    rows.end();
    let count = rows.children();
    page.fixed(&rows, count * ROW_H + (count - 1).max(0) * OPTION_PAD);
    rows.hide();
    Advanced { toggle, page: page.clone(), rows, hint }
}

/// Show or hide the Advanced options of `advanced`.
pub(super) fn set_advanced(advanced: &mut Advanced, open: bool) {
    if open == advanced.is_open() {
        return;
    }
    set_shown(&mut advanced.rows, open);
    if let Some(hint) = advanced.hint.as_mut() {
        set_shown(hint, !open);
    }
    advanced.toggle.set_label(&advanced_label(open));
    advanced.page.layout();
    advanced.page.redraw();
}

/// A number field of `range`, stepping by `step`.
fn spinner(row: &mut Flex, range: std::ops::RangeInclusive<f64>, step: f64, value: f64, width: i32) -> Spinner {
    let mut spinner = Spinner::default();
    spinner.set_range(*range.start(), *range.end());
    spinner.set_step(step);
    spinner.set_value(value);
    row.fixed(&spinner, width);
    spinner
}

/// A new options panel (a column of rows, top-aligned).
fn options_panel() -> Flex {
    let mut page = Flex::default().column();
    page.set_pad(OPTION_PAD);
    page
}

/// Close `page`: a filler below its rows keeps them at the top.
fn end_panel(page: Flex) -> Flex {
    let _filler = Frame::default();
    page.end();
    page
}

fn option_row() -> Flex {
    let mut row = Flex::default().row();
    row.set_pad(6);
    row
}

fn end_row(page: &mut Flex, row: Flex) {
    row.end();
    page.fixed(&row, ROW_H);
}

/// A label as wide as its text.
fn label(row: &mut Flex, text: &str) {
    let mut frame = Frame::default().with_label(text);
    frame.set_align(Align::Left | Align::Inside);
    row.fixed(&frame, text_width(text) + LABEL_PAD);
}

/// The first column of an options row: the step's name in bold, a field's
/// label, or nothing — so the controls after it line up.
fn first_column(row: &mut Flex, text: &str, bold: bool) -> Frame {
    let mut frame = Frame::default().with_label(text);
    frame.set_align(Align::Left | Align::Inside);
    if bold {
        frame.set_label_font(Font::HelveticaBold);
    }
    row.fixed(&frame, label_column());
    frame
}

/// Muted text: as wide as itself when `fit`, else the rest of the row.
fn hint(row: &mut Flex, text: &str, fit: bool) -> Frame {
    let mut frame = Frame::default().with_label(text);
    frame.set_align(Align::Left | Align::Inside | Align::Clip);
    frame.set_label_color(Color::from_rgbi(theme().muted));
    if fit {
        row.fixed(&frame, text_width(text) + LABEL_PAD);
    }
    frame
}

/// A text field showing `placeholder` in grey while it is empty.
fn field(placeholder: &str) -> Input {
    let mut input = Input::default();
    // The label holds it: FLTK draws an input's label nowhere when it is
    // aligned inside, so it shows only as drawn here.
    input.set_label(placeholder);
    input.set_align(Align::Left | Align::Inside);
    input.draw(|input| {
        let text = input.label();
        // An inactive field shows none: its grey would pass for a value.
        if !input.value().is_empty() || text.is_empty() || !input.active_r() {
            return;
        }
        let frame = input.frame();
        draw::set_font(input.text_font(), input.text_size());
        draw::set_draw_color(Color::from_rgbi(theme().muted));
        // Where FLTK starts the text (`Fl_Input_::drawtext`: 1 px in).
        let (x, y) = (input.x() + frame.dx() + 1, input.y() + frame.dy());
        draw::draw_text2(&text, x, y, input.w() - frame.dw() - 2, input.h() - frame.dh(), Align::Left | Align::Clip);
    });
    input
}

/// Show `placeholder` in `input` (made by [`field`]) from now on.
fn set_placeholder(input: &mut Input, placeholder: &str) {
    if input.label() != placeholder {
        input.set_label(placeholder);
        input.redraw();
    }
}

fn check(row: &mut Flex, text: &str, on: bool) -> CheckButton {
    let mut button = CheckButton::default().with_label(text);
    button.set_value(on);
    row.fixed(&button, text_width(text) + CHECK_BOX);
    button
}

/// Room a check button needs beyond its label: the box and the gap after it.
const CHECK_BOX: i32 = 24;

/// Room a drop-down needs beyond its widest item: the arrow box and the
/// insets.
const CHOICE_PAD: i32 = 40;

/// A drop-down of `items` with the item meaning `selected` chosen, at
/// least `width` wide and as wide as its widest item. (An item must not hold
/// `|`, which separates them, nor `/`, which makes a submenu — the build
/// checks the catalogs.)
fn choice<T: PartialEq>(row: &mut Flex, items: &[Item<T>], selected: T, width: i32) -> Choice {
    let mut choice = Choice::default();
    let mut widest = 0;
    for item in items {
        let label = item_label(item);
        widest = widest.max(text_width(label));
        choice.add_choice(label);
    }
    choice.set_value(item_index(items, selected));
    row.fixed(&choice, width.max(widest + CHOICE_PAD));
    choice
}

/// The value of `choice` from its `table` (the first if nothing is chosen).
fn pick<T: Copy>(table: &[Item<T>], choice: &Choice) -> T {
    usize::try_from(choice.value()).ok().and_then(|i| table.get(i)).unwrap_or(&table[0]).2
}

/// The tokens of `table`, in item order, for the profiles.
fn tokens<T>(table: &[Item<T>]) -> Vec<&'static str> {
    table.iter().map(|(token, _, _)| *token).collect()
}

/// A masked text field, `placeholder` in grey while empty.
fn secret_input(placeholder: &str) -> Input {
    let mut input = field(placeholder);
    input.set_type(InputType::Secret);
    input
}

/// Wire every control of the steps panel: a change re-reads the job.
pub(super) fn wire_steps(widgets: &Widgets, state: &SharedState) {
    let steps = &widgets.steps;
    // Every control a profile keeps re-reads the job; the password fields
    // have no key. The controls that do more get their own callback below,
    // which replaces this one.
    for (_, field) in steps.fields() {
        match field {
            Field::Check(check) => on_click(&check, state, widgets, job_edited),
            Field::Choice(choice, _) => on_click(&choice, state, widgets, job_edited),
            Field::Number(spinner) => on_click(&spinner, state, widgets, job_edited),
            Field::Text(input) => on_typing(&input, state, widgets),
        }
    }
    for input in [&steps.user_password, &steps.user_repeat, &steps.owner_password, &steps.owner_repeat, &steps.input_password] {
        on_typing(input, state, widgets);
    }
    for &(step, ref toggle) in &steps.toggles {
        on_click(toggle, state, widgets, move |state, w| {
            if w.steps.on(step) {
                show_step(w, step);
            }
            job_edited(state, w);
        });
    }
    for (step, name) in Step::ALL.into_iter().zip(&steps.names) {
        on_click(name, state, widgets, move |_, w| show_step(w, step));
    }
    // "In this folder:" without one yet: choose it at once.
    on_click(&steps.output_place, state, widgets, |state, w| {
        if pick(&PLACES, &w.steps.output_place) && w.steps.output_folder.value().is_empty() {
            choose_output_folder(state, w);
        }
        job_edited(state, w);
    });
    for (index, advanced) in steps.advanced.iter().enumerate() {
        on_click(&advanced.toggle, state, widgets, move |_, w| {
            let advanced = &mut w.steps.advanced[index];
            let open = !advanced.is_open();
            set_advanced(advanced, open);
        });
    }
    on_click(&steps.arrange_pages, state, widgets, open_pages);
    on_click(&steps.output_choose, state, widgets, choose_output_folder);
    on_click(&steps.command, state, widgets, open_command);
    on_click(&steps.profile, state, widgets, profile_chosen);
    on_click(&steps.attach_add, state, widgets, add_attachments);
    on_click(&steps.attach_clear, state, widgets, |state, w| {
        state.borrow_mut().attach.clear();
        job_edited(state, w);
    });
    on_click(&steps.overlay_choose, state, widgets, choose_overlay);
    on_click(&steps.show_password, state, widgets, |_, w| {
        let shown = w.steps.show_password.value();
        let input = &mut w.steps.input_password;
        input.set_type(if shown { InputType::Normal } else { InputType::Secret });
        input.redraw();
    });
}

/// Re-read the job at every keystroke in `input`, so the result line and
/// Start follow the typing.
fn on_typing(input: &Input, state: &SharedState, widgets: &Widgets) {
    input.clone().set_trigger(CallbackTrigger::Changed);
    on_click(input, state, widgets, job_edited);
}

/// Show the options of `step` and mark its name.
pub(super) fn show_step(widgets: &mut Widgets, step: Step) {
    let pal = theme();
    let steps = &mut widgets.steps;
    for (index, (panel, name)) in steps.panels.iter_mut().zip(&mut steps.names).enumerate() {
        let shown = index == step as usize;
        set_shown(panel, shown);
        name.set_color(if shown { pal.selection } else { Color::Background });
        name.set_label_font(if shown { Font::HelveticaBold } else { Font::Helvetica });
        name.redraw();
    }
    widgets.options.layout();
    widgets.options.redraw();
}

/// The steps panel changed: re-read the job, show what it makes, and
/// let Start follow.
pub(super) fn job_edited(state: &SharedState, widgets: &mut Widgets) {
    let Ok(mut st) = state.try_borrow_mut() else { return };
    let steps = &mut widgets.steps;
    // A step switched off has its options grey: they do nothing.
    for step in Step::ALL {
        let on = steps.on(step);
        set_active(&mut steps.panels[step as usize], on);
    }
    // Controls that mean nothing in the current choice are off.
    let pages_mode = pick(&PAGE_MODES, &steps.pages_select);
    let (range_placeholder, typed) = match pages_mode {
        PageMode::Keep => (t!(PlaceholderPagesKeep), true),
        PageMode::Delete => (t!(PlaceholderPagesDelete), true),
        PageMode::All | PageMode::Reverse => ("", false),
    };
    set_active(&mut steps.pages_range, typed);
    set_placeholder(&mut steps.pages_range, range_placeholder);
    let merges = pick(&FILES, &steps.pages_files).merges();
    // QPDF builds a merge from an empty document: no outline survives.
    set_label(&mut steps.pages_hint, if merges { t!(PagesBookmarksHint) } else { t!(PagesOwnHint) }, None);
    let rotating = steps.rotate.value();
    set_active(&mut steps.rotate_angle, rotating);
    set_active(&mut steps.rotate_range, rotating);
    let images = pick(&OPTIMIZE_MODES, &steps.optimize_mode);
    set_active(&mut steps.jpeg_quality, matches!(images, Images::JpegQuality(_)));
    let smaller = images.smaller();
    set_active(&mut steps.keep_inline_images, smaller);
    set_active(&mut steps.min_width, smaller);
    set_active(&mut steps.min_height, smaller);
    let locks = pick(&PROTECT_MODES, &steps.protect_mode);
    set_active(&mut steps.encrypt_rows, locks);
    set_label(&mut steps.protect_hint, if locks { "" } else { t!(ProtectUnlockHint) }, None);
    set_active(&mut steps.output_folder, pick(&PLACES, &steps.output_place));
    set_active(&mut steps.split_pages, steps.split.value());
    set_label(&mut steps.overlay_info, &overlay_text(st.overlay.as_ref()), None);
    set_label(&mut steps.attach_info, &attach_text(&st.attach), None);
    let merges = steps.on(Step::Pages) && merges;
    set_label(&mut steps.output_name_label, if merges { t!(OutputFileName) } else { t!(OutputNameEnding) }, None);

    let job = read_job(steps, &st);
    // Optimize alone — Output may only place the result.
    let shaping = steps.chosen();
    set_active(&mut steps.keep_original, Job::keeps_original_applies(&shaping, !locks));
    // What an empty name gives, in the field itself.
    let tag = job.as_ref().map_or("", Job::tag);
    let (placeholder, name_hint) = match merges {
        true if !tag.is_empty() => (t!(OutputFirstFile, tag = tag), t!(OutputWholeName)),
        true => (String::new(), t!(OutputWholeName)),
        false => (tag.to_string(), t!(OutputNameEndingHint)),
    };
    set_placeholder(&mut steps.output_name, &placeholder);
    set_label(&mut steps.output_name_hint, name_hint, None);
    let pal = theme();
    let (text, color) = match &job {
        Ok(job) => {
            // The name exactly as `output` makes it.
            let (path, split) = job.result_path(Path::new(&format!("{}.pdf", t!(ResultSampleName)))).unwrap_or_default();
            let name = display_name(&path);
            let mut result = match split {
                Some(_) => t!(ResultParts, name = name),
                None if job.merges() => t!(ResultMerged, name = name),
                None => t!(Result, name = name),
            };
            if let Some(folder) = job.output.as_ref().and_then(|o| o.folder.as_deref()) {
                result = t!(ResultInFolder, result = result, folder = display_name(folder));
            }
            if shaping.is_empty() {
                (format!("{}\n{result}", t!(ResultNoStep)), Color::from_rgbi(pal.muted))
            } else {
                (result, Color::Foreground)
            }
        }
        Err(error) => (error.sentence(), Color::from_rgbi(pal.failed)),
    };
    set_label(&mut steps.result, &text, Some(color));
    st.job = job.ok().map(|job| job.work_key());
    show_profile(&mut widgets.steps, &st);
    update_controls(&st, widgets);
}

/// What the probe found about the overlay file, for its options.
fn overlay_text(overlay: Option<&(PathBuf, Probe)>) -> String {
    let Some((_, probe)) = overlay else { return String::new() };
    match probe {
        Probe::Pending => t!(OverlayChecking).into(),
        Probe::Ready { pages, .. } => tn!(OverlayPages, *pages),
        Probe::NeedsPassword => t!(OverlayNeedsPassword).into(),
        Probe::Failed(failure) => failure.sentence().into(),
    }
}

/// The files chosen to attach, as one line.
fn attach_text(files: &[PathBuf]) -> String {
    match files {
        [] => t!(AttachNone).into(),
        [one] => display_name(one),
        many => {
            let names: Vec<String> = many.iter().map(|file| display_name(file)).collect();
            tn!(AttachFiles, many.len(), names = names.join(", "))
        }
    }
}

/// "Add files…" of the Attachments step: any kind of file, each once.
fn add_attachments(state: &SharedState, widgets: &mut Widgets) {
    if state.borrow().batch.is_some() {
        return;
    }
    let chooser = NativeFileChooser::new(NativeFileChooserType::BrowseMultiFile);
    if let Some(picked) = pick_paths(state, chooser, false) {
        attach(state, widgets, picked);
    }
}

/// `files` join the files to attach, each once — chosen or dropped.
pub(super) fn attach(state: &SharedState, widgets: &mut Widgets, files: Vec<PathBuf>) {
    {
        let mut st = state.borrow_mut();
        for file in files {
            if !st.attach.contains(&file) {
                st.attach.push(file);
            }
        }
    }
    job_edited(state, widgets);
}

/// The rows at `indices` (`what` names them) get their own pages — from
/// the page view: shown on their rows, and the job re-read (the Pages step
/// with nothing chosen is fine only while some file has its own pages).
pub(super) fn apply_own_pages(state: &SharedState, widgets: &mut Widgets, indices: &[usize], range: Option<&PageRange>, what: &str) {
    {
        let mut st = state.borrow_mut();
        st.set_own_pages(indices, range);
        let pages = range.map_or(t!(PlaceholderAllPages), PageRange::text);
        show_status(&st, widgets, &t!(OwnPagesSet, who = what, pages = pages));
    }
    job_edited(state, widgets);
}

/// "Choose…" the folder of the results.
fn choose_output_folder(state: &SharedState, widgets: &mut Widgets) {
    if state.borrow().batch.is_some() {
        return;
    }
    let chooser = NativeFileChooser::new(NativeFileChooserType::BrowseDir);
    if let Some(folder) = pick_one(state, chooser, true) {
        set_output_folder(state, widgets, &folder);
    }
}

/// The results go in `folder` — chosen or dropped.
pub(super) fn set_output_folder(state: &SharedState, widgets: &mut Widgets, folder: &Path) {
    widgets.steps.output_folder.set_value(&folder.display().to_string());
    widgets.steps.output_place.set_value(item_index(&PLACES, true));
    job_edited(state, widgets);
}

/// "Choose…" the overlay file.
fn choose_overlay(state: &SharedState, widgets: &mut Widgets) {
    if state.borrow().batch.is_some() {
        return;
    }
    let chooser = pdf_chooser(NativeFileChooserType::BrowseFile);
    if let Some(path) = pick_one(state, chooser, false) {
        set_overlay(state, widgets, path);
    }
}

/// `path` is the overlay file — chosen or dropped; the probe finds its pages.
pub(super) fn set_overlay(state: &SharedState, widgets: &mut Widgets, path: PathBuf) {
    {
        let mut st = state.borrow_mut();
        st.overlay = Some((path.clone(), Probe::Pending));
        st.probe_overlay();
    }
    widgets.steps.overlay_file.set_value(&path.display().to_string());
    job_edited(state, widgets);
}

/// The job as the steps panel sets it, checked, with what lives
/// outside the panel: the overlay file and what the probe found about it,
/// the files to attach, whether some file has pages of its own.
pub(super) fn read_job(steps: &Steps, st: &State) -> Result<Job, JobError> {
    let mut job = Job::default();
    if steps.on(Step::Pages) {
        let select = Select::read(pick(&PAGE_MODES, &steps.pages_select), &steps.pages_range.value())?;
        let rotate = match steps.rotate.value() {
            true => Some(Rotate::read(pick(&ANGLES, &steps.rotate_angle), &steps.rotate_range.value())?),
            false => None,
        };
        job.pages = Some(Pages::read(select, pick(&FILES, &steps.pages_files), rotate, st.own_pages())?);
    }
    if steps.on(Step::Overlay) {
        let Some((file, probe)) = &st.overlay else {
            return Err(JobError::new(Step::Overlay, t!(JobOverlayChoose)));
        };
        job.overlay = Some(Overlay::read(
            pick(&OVERLAY_MODES, &steps.overlay_mode),
            file.clone(),
            probe.clone(),
            pick(&PLACEMENTS, &steps.overlay_placement),
            &steps.overlay_to.value(),
        )?);
    }
    if steps.on(Step::Forms) {
        job.forms = Some(Forms { action: pick(&FORMS, &steps.forms_mode), flatten_rotation: steps.flatten_rotation.value() });
    }
    if steps.on(Step::Attachments) {
        job.attachments = Some(Attachments { add: st.attach.clone(), remove_all: steps.remove_attachments.value() });
    }
    if steps.on(Step::Privacy) {
        job.privacy = Some(Privacy {
            remove_info: steps.remove_info.value(),
            remove_xmp: steps.remove_xmp.value(),
            fixed_id: steps.fixed_id.value(),
            remove_structure: steps.remove_structure.value(),
            remove_page_labels: steps.remove_page_labels.value(),
        });
    }
    if steps.on(Step::Optimize) {
        // The spinners keep their values within range; `as` saturates anyway.
        job.optimize = Some(Optimize {
            images: match pick(&OPTIMIZE_MODES, &steps.optimize_mode) {
                Images::JpegQuality(_) => Images::JpegQuality(steps.jpeg_quality.value() as u8),
                images => images,
            },
            keep_if_not_smaller: steps.keep_original.value(),
            object_streams: pick(&OBJECT_STREAMS, &steps.object_streams),
            keep_inline_images: steps.keep_inline_images.value(),
            min_image: (steps.min_width.value() as u32, steps.min_height.value() as u32),
        });
    }
    if steps.on(Step::Protect) {
        job.protect = Some(match pick(&PROTECT_MODES, &steps.protect_mode) {
            true => Protect::Encrypt(read_encrypt(steps)?),
            false => Protect::Unlock,
        });
    }
    // Output has no switch; as it starts, it changes nothing.
    let folder = match (pick(&PLACES, &steps.output_place), steps.output_folder.value()) {
        (true, chosen) if chosen.is_empty() => return Err(JobError::new(Step::Output, t!(JobOutputFolder))),
        // Whether it is still there, Start looks.
        (true, chosen) => Some(PathBuf::from(chosen)),
        (false, _) => None,
    };
    let name = steps.output_name.value();
    let output = Output {
        // The spinner keeps it within 1..=100 000.
        split: steps.split.value().then(|| steps.split_pages.value() as u32),
        linearize: steps.linearize.value(),
        folder,
        name: (!name.trim().is_empty()).then_some(name),
    };
    job.output = (output != Output::default()).then_some(output);
    job.input_password = input_password(steps.input_password.value())?;
    job.check()?;
    Ok(job)
}

/// The Encrypt options; each password is typed twice.
fn read_encrypt(steps: &Steps) -> Result<Encrypt, JobError> {
    Ok(Encrypt {
        user: Encrypt::password(steps.user_password.value(), steps.user_repeat.value(), false)?,
        owner: Encrypt::password(steps.owner_password.value(), steps.owner_repeat.value(), true)?,
        print: pick(&PRINTS, &steps.print),
        copy: steps.allow_copy.value(),
        edit: steps.allow_edit.value(),
        annotate: steps.allow_annotate.value(),
        fill_forms: steps.allow_forms.value(),
        assemble: steps.allow_assemble.value(),
    })
}

