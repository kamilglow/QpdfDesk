//! The job: the chosen steps as ONE QPDF call per result,
//! its validation and what each result must look like.
//!
//! A result comes from one file — or, when Pages merges or collates, from
//! every file of the list in list order. Without any step the job is
//! Repair: QPDF rewrites the file, recovering what it can. The same
//! arguments also make the command shown for cmd.exe.

use std::path::{Path, PathBuf};

use super::cmdline::quote;
use super::filelist::{display_name, Probe};
use super::outcome::{Failure, Verdict};
use super::output::{self, output_name, Produced, Target};
use super::ranges::{PageRange, RangeError};
use super::runner::{Arg, Context};
use super::secret::Secret;
use crate::t;
use crate::text::Msg;

/// The steps, in the order of the steps panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Pages,
    Overlay,
    Forms,
    Attachments,
    Privacy,
    Optimize,
    Protect,
    Output,
}

impl Step {
    pub const ALL: [Step; 8] = [
        Step::Pages,
        Step::Overlay,
        Step::Forms,
        Step::Attachments,
        Step::Privacy,
        Step::Optimize,
        Step::Protect,
        Step::Output,
    ];

    pub fn name(self) -> &'static str {
        crate::text::get(self.message())
    }

    /// The catalog message of [`Step::name`].
    pub fn message(self) -> Msg {
        match self {
            Step::Pages => Msg::StepPages,
            Step::Overlay => Msg::StepOverlay,
            Step::Forms => Msg::StepForms,
            Step::Attachments => Msg::StepAttachments,
            Step::Privacy => Msg::StepPrivacy,
            Step::Optimize => Msg::StepOptimize,
            Step::Protect => Msg::StepProtect,
            Step::Output => Msg::StepOutput,
        }
    }
}

/// Pages: which pages of each file, and what becomes of the files — each
/// on its own, or all of them in one result. A file's own pages
/// ([`Input::range`], set with Arrange pages) take the place of the pages
/// chosen here, whatever they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pages {
    pub select: Select,
    pub files: Files,
    /// Pages of the RESULT (measured: QPDF numbers them after selecting).
    pub rotate: Option<Rotate>,
}

/// The pages taken from each file without pages of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Select {
    All,
    Keep(PageRange),
    /// The rest of each file: [`PageRange::complement`] of the pages typed.
    Delete(PageRange),
    Reverse,
}

/// What the Pages step offers, before the range is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageMode {
    All,
    Keep,
    Delete,
    Reverse,
}

/// What becomes of the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Files {
    /// A result of each file.
    Each,
    /// Every file of the list into one result, in list order.
    Merge,
    /// As Merge, but a page of each file in turn (QPDF's `--collate`): a scan
    /// of the odd pages and one of the even pages become one document.
    Collate,
}

impl Files {
    /// Every file of the list into one result.
    pub fn merges(self) -> bool {
        self != Files::Each
    }
}

impl Select {
    /// The pages typed for Keep, the pages Delete leaves; `None` = every
    /// page (Reverse: backwards, [`Job::pages_arg`]).
    fn range(&self) -> Option<&PageRange> {
        match self {
            Select::Keep(range) | Select::Delete(range) => Some(range),
            Select::All | Select::Reverse => None,
        }
    }

    /// The mode it was chosen with.
    pub fn mode(&self) -> PageMode {
        match self {
            Select::All => PageMode::All,
            Select::Keep(_) => PageMode::Keep,
            Select::Delete(_) => PageMode::Delete,
            Select::Reverse => PageMode::Reverse,
        }
    }

    /// The selection as chosen: `text` is the range typed for Keep and Delete.
    pub fn read(mode: PageMode, text: &str) -> Result<Select, JobError> {
        let pages_error = |e: RangeError| JobError::new(Step::Pages, e.sentence());
        match mode {
            PageMode::All => Ok(Select::All),
            PageMode::Keep => Ok(Select::Keep(PageRange::parse(text).map_err(pages_error)?)),
            PageMode::Delete => Ok(Select::Delete(PageRange::parse(text).and_then(|r| r.complement()).map_err(pages_error)?)),
            PageMode::Reverse => Ok(Select::Reverse),
        }
    }
}

impl Pages {
    /// The step as chosen — when it does anything: every page of each file
    /// on its own, unrotated, is a step to do nothing, unless some file has
    /// pages of its own (`own_pages`).
    pub fn read(select: Select, files: Files, rotate: Option<Rotate>, own_pages: bool) -> Result<Pages, JobError> {
        if select == Select::All && files == Files::Each && rotate.is_none() && !own_pages {
            return Err(JobError::new(Step::Pages, t!(JobChoosePages)));
        }
        Ok(Pages { select, files, rotate })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotate {
    pub angle: Angle,
    /// `None` = every page.
    pub pages: Option<PageRange>,
}

impl Rotate {
    /// The rotation as chosen: `text` is the range typed, empty for every page.
    pub fn read(angle: Angle, text: &str) -> Result<Rotate, JobError> {
        Ok(Rotate { angle, pages: optional_range(text, Step::Pages, t!(FieldPagesRotate))? })
    }
}

/// A range typed in a field where empty means "not limited" (`what` names
/// the field in the error).
fn optional_range(text: &str, step: Step, what: &str) -> Result<Option<PageRange>, JobError> {
    match PageRange::parse(text) {
        Err(RangeError::Empty) => Ok(None),
        parsed => Ok(Some(parsed.map_err(range_error(step, what))?)),
    }
}

/// The error of a range in the field `what` of `step`.
fn range_error(step: Step, what: &str) -> impl Fn(RangeError) -> JobError + '_ {
    move |e| JobError::new(step, t!(JobFieldError, field = what, why = e.sentence()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Angle {
    Right,
    UpsideDown,
    Left,
}

/// Overlay / underlay: the pages of another file drawn over (or
/// under) the pages of each result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub under: bool,
    pub file: PathBuf,
    /// Pages of the result to cover (`--to`); `None` = all.
    pub to: Option<PageRange>,
    /// Which of its pages go on them (`--from`, `--repeat`).
    pub placement: Placement,
    /// What the probe found about `file`: its pages for the checks, and
    /// whether it opens only with the password for encrypted inputs.
    pub probe: Probe,
}

/// Which of the file's pages go on the pages covered, in turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Its page 1 on each: a stamp, a watermark, a letterhead.
    PageOne,
    /// Its pages in order, 1 on the first, 2 on the second…; the pages
    /// after them stay bare.
    InOrder,
    /// Its pages in order, again and again: 1, 2, 1, 2… — e.g. one
    /// background for odd pages, another for even ones.
    InTurn,
}

impl Overlay {
    /// The overlay as chosen: `placement` on the pages typed in `on_pages`
    /// (empty: every page).
    pub fn read(under: bool, file: PathBuf, probe: Probe, placement: Placement, on_pages: &str) -> Result<Overlay, JobError> {
        let to = optional_range(on_pages, Step::Overlay, t!(FieldPagesCover))?;
        Ok(Overlay { under, file, to, placement, probe })
    }

    /// QPDF's `--from` and `--repeat` for its placement: `None` = left out —
    /// `--from` left out is every page of the file (QPDF's manual).
    fn qpdf_ranges(&self) -> (Option<&'static str>, Option<&'static str>) {
        match self.placement {
            Placement::PageOne => (Some("1"), Some("1")),
            Placement::InOrder => (None, None),
            Placement::InTurn => (None, Some("1-z")),
        }
    }
}

/// Forms & annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forms {
    pub action: FormAction,
    /// Advanced: each page's rotation made part of its content
    /// (`--flatten-rotation`), for viewers that ignore it.
    pub flatten_rotation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    /// Bake form fields and annotations into the page (`all`, `print` or
    /// `screen` ones); the form is gone afterwards (measured).
    Flatten(Annotations),
    /// Drop the interactive form; the fields keep their look but can no
    /// longer be filled.
    RemoveForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Annotations {
    All,
    Print,
    Screen,
}

/// Attachments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attachments {
    /// Files embedded in every result, under their own names.
    pub add: Vec<PathBuf>,
    /// Remove every attachment the file has (its keys are listed first,
    /// [`Input::attachments`]) — before the new ones are added, so a file of
    /// the same name replaces the old one (measured).
    pub remove_all: bool,
}

/// Privacy: document metadata, not content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Privacy {
    /// Title, author, producer… (`--remove-info`; the modification date stays).
    pub remove_info: bool,
    /// The XMP packet (`--remove-metadata`).
    pub remove_xmp: bool,
    /// A document ID made from the content only, not from the time or the
    /// file name (`--deterministic-id`; impossible for encrypted output).
    pub fixed_id: bool,
    /// Advanced: the structure tree, the tags screen readers follow
    /// (`--remove-structure`).
    pub remove_structure: bool,
    /// Advanced: page labels such as "i, ii, 1…" (`--remove-page-labels`).
    pub remove_page_labels: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Privacy { remove_info: true, remove_xmp: true, fixed_id: true, remove_structure: false, remove_page_labels: false }
    }
}

/// Optimize. (No compression level: 9 instead of zlib's own made the QPDF
/// manual 0.3 % smaller and took 12 % longer — measured.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Optimize {
    pub images: Images,
    /// Discard a result that is not smaller than its original — only when
    /// Optimize is the one step: a smaller file is its only purpose, while
    /// other steps change what the file is.
    pub keep_if_not_smaller: bool,
    /// Advanced: how the objects are packed (`--object-streams`).
    pub object_streams: ObjectStreams,
    /// Advanced, Smaller only: images drawn inside the page's content are
    /// left alone (`--keep-inline-images`).
    pub keep_inline_images: bool,
    /// Advanced, Smaller only: images narrower or lower than this, in
    /// pixels, are left alone (`--oi-min-width`, `--oi-min-height`).
    pub min_image: (u32, u32),
}

/// QPDF's own smallest image it re-encodes, width and height (its manual);
/// only other values are passed.
pub const MIN_IMAGE: (u32, u32) = (128, 128);

/// What Optimize does to the images. QPDF writes a JPEG with the colour at
/// half the resolution of the brightness (4:2:0, measured), and has no
/// choice for it: Lossless is the one way to keep the colours as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Images {
    /// Left as they are.
    Lossless,
    /// Re-encoded as JPEG of quality 75 where that makes them smaller
    /// (`--optimize-images`); JPEGs stay as they are.
    Jpeg,
    /// As Jpeg, of this quality — which makes QPDF re-encode the JPEGs too,
    /// losing quality again (`--jpeg-quality`; measured: 121 183 → 38 945
    /// bytes, while without it the JPEG was left alone).
    JpegQuality(u8),
}

impl Images {
    /// Whether images are re-encoded as JPEG.
    pub fn smaller(self) -> bool {
        self != Images::Lossless
    }
}

impl Default for Optimize {
    fn default() -> Self {
        Optimize {
            images: Images::Lossless,
            keep_if_not_smaller: true,
            object_streams: ObjectStreams::Generate,
            keep_inline_images: false,
            min_image: MIN_IMAGE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectStreams {
    /// Pack the small objects into compressed streams: smaller (measured).
    Generate,
    /// As the file has them.
    Preserve,
    /// None at all: for readers older than PDF 1.5.
    Disable,
}

/// Protect.
pub enum Protect {
    /// AES-256 only; weak encryption is not offered.
    Encrypt(Encrypt),
    /// Remove the encryption (`--decrypt`): the open password (given as the
    /// password for encrypted inputs) and the restrictions of a file that
    /// opens without one — to QPDF the same, so one choice, not two.
    Unlock,
}

pub struct Encrypt {
    /// The open password; empty = the file opens without one.
    pub user: Secret,
    /// Required: without it anyone could lift the restrictions.
    pub owner: Secret,
    pub print: Print,
    pub copy: bool,
    pub edit: bool,
    pub annotate: bool,
    pub fill_forms: bool,
    pub assemble: bool,
}

impl Encrypt {
    /// A password typed twice — the `owner` password or the open one; both
    /// must match.
    pub fn password(first: String, second: String, owner: bool) -> Result<Secret, JobError> {
        let error = |message: String| JobError::new(Step::Protect, message);
        let line_break = || error(t!(PasswordLineBreak).into());
        let (first, second) = (Secret::new(first).map_err(|_| line_break())?, Secret::new(second).map_err(|_| line_break())?);
        match (first.expose() == second.expose(), second.is_empty()) {
            (true, _) => Ok(first),
            (false, true) => Err(error(if owner { t!(PasswordRepeatOwner) } else { t!(PasswordRepeatOpen) }.into())),
            (false, false) => Err(error(if owner { t!(PasswordDifferOwner) } else { t!(PasswordDifferOpen) }.into())),
        }
    }
}

/// The password for encrypted inputs as typed: empty = none.
pub fn input_password(text: String) -> Result<Option<Secret>, JobError> {
    let password = Secret::new(text).map_err(|_| JobError::input_password(t!(PasswordInputLineBreak)))?;
    Ok((!password.is_empty()).then_some(password))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Print {
    Full,
    Low,
    None,
}

/// Output: where the result goes, its name, and its form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// Split the result into files of this many pages, in a folder.
    pub split: Option<u32>,
    /// "Fast web view": a browser shows the first page before the rest arrives.
    pub linearize: bool,
    /// The folder of every result; `None` = next to its original.
    pub folder: Option<PathBuf>,
    /// The name as typed: what goes in the parentheses —
    /// for a merge, the whole name; `None` = the step's tag.
    pub name: Option<String>,
}

/// Whether the Output step changes the file itself: only by splitting it or
/// linearizing it — a folder or a name alone change nothing in it. The
/// panel asks it too, for a panel that does not read as a job.
pub fn output_shapes(split: bool, linearize: bool) -> bool {
    split || linearize
}

/// What to do with the files.
#[derive(Default)]
pub struct Job {
    pub pages: Option<Pages>,
    pub overlay: Option<Overlay>,
    pub forms: Option<Forms>,
    pub attachments: Option<Attachments>,
    pub privacy: Option<Privacy>,
    pub optimize: Option<Optimize>,
    pub protect: Option<Protect>,
    pub output: Option<Output>,
    /// The open password of encrypted inputs, one for the batch.
    pub input_password: Option<Secret>,
}

/// One file going into a result.
pub struct Input<'a> {
    pub path: &'a Path,
    pub size: u64,
    /// What the probe found: the page count the result is checked
    /// against, whether the file is encrypted.
    pub probe: &'a Probe,
    /// Its own pages, in place of the ones chosen for all; `None` = no own pages.
    pub range: Option<&'a PageRange>,
    /// The keys of its attachments, when "Remove all attachments" needs
    /// them ([`Job::lists_attachments`]).
    pub attachments: Option<&'a [String]>,
}

/// A job that cannot run, and the step to blame — `None`: the password
/// for encrypted inputs, which belongs to no step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    pub step: Option<Step>,
    pub message: String,
}

impl JobError {
    pub fn new(step: Step, message: impl Into<String>) -> JobError {
        JobError { step: Some(step), message: message.into() }
    }

    fn input_password(message: &str) -> JobError {
        JobError { step: None, message: message.into() }
    }

    pub fn sentence(&self) -> String {
        let subject = self.step.map_or(t!(JobInputPassword), Step::name);
        t!(JobError, step = subject, why = self.message)
    }
}

/// `range` counted against a RESULT of `pages` pages (`what` names the field).
fn count_in_result(range: &PageRange, pages: u64, step: Step, what: &str) -> Result<u64, JobError> {
    range.count(pages).map_err(|e| {
        let why = match e {
            RangeError::NoSuchPage { page, pages } => t!(RangeNoSuchPageInResult, page = page, pages = pages),
            RangeError::NoPages => t!(RangeNoneInResult).into(),
            e => e.sentence(),
        };
        JobError::new(step, t!(JobFieldError, field = what, why = why))
    })
}

impl Job {
    /// The simplest job: Optimize, lossless.
    pub fn optimize_lossless() -> Job {
        Job { optimize: Some(Optimize::default()), ..Job::default() }
    }

    /// The chosen steps that change the file, in panel order: Output only
    /// with Split or Linearize — a folder or a name alone change nothing in
    /// it (no step = Repair).
    pub fn steps(&self) -> Vec<Step> {
        let chosen = [
            self.pages.is_some(),
            self.overlay.is_some(),
            self.forms.is_some(),
            self.attachments.is_some(),
            self.privacy.is_some(),
            self.optimize.is_some(),
            self.protect.is_some(),
            self.output.as_ref().is_some_and(|o| output_shapes(o.split.is_some(), o.linearize)),
        ];
        Step::ALL.into_iter().zip(chosen).filter(|(_, on)| *on).map(|(step, _)| step).collect()
    }

    /// What becomes of the files: each on its own without the Pages step.
    fn files(&self) -> Files {
        self.pages.as_ref().map_or(Files::Each, |p| p.files)
    }

    /// Whether every file of the list goes into one result.
    pub fn merges(&self) -> bool {
        self.files().merges()
    }

    /// The pages taken from `input` in order, when not all of them: its own,
    /// else the ones chosen (Reverse: [`Job::pages_arg`]).
    fn range_for<'i>(&'i self, input: &Input<'i>) -> Option<&'i PageRange> {
        let pages = self.pages.as_ref()?;
        input.range.or(pages.select.range())
    }

    /// The pages of `input` as QPDF's `--pages` takes them; `None` = every
    /// page in order.
    fn pages_arg<'i>(&'i self, input: &Input<'i>) -> Option<&'i str> {
        let reverse = self.pages.as_ref().is_some_and(|p| p.select == Select::Reverse);
        self.range_for(input).map(PageRange::text).or(reverse.then_some("z-1"))
    }

    /// Whether each file's attachment keys must be listed before QPDF runs:
    /// "Remove all attachments" on a file of its own (a merge starts from an
    /// empty document, which has none — measured).
    pub fn lists_attachments(&self) -> bool {
        self.attachments.as_ref().is_some_and(|a| a.remove_all) && !self.merges()
    }

    /// Whether the job can run as chosen; per-file checks, such as page
    /// numbers, happen in [`Job::produce`].
    pub fn check(&self) -> Result<(), JobError> {
        if let Some(overlay) = &self.overlay {
            let error = |message: String| Err(JobError::new(Step::Overlay, message));
            match &overlay.probe {
                Probe::Failed(failure) => return error(t!(JobOverlayUnusable, why = failure.sentence())),
                Probe::NeedsPassword if self.input_password.is_none() => {
                    return error(t!(JobOverlayPassword).into());
                }
                // Its pages are taken from page 1 on: one is all it needs.
                Probe::Ready { pages: 0, .. } => return error(t!(JobOverlayNoPages).into()),
                Probe::Ready { .. } => {}
                // Its page count and password decide the checks above.
                Probe::Pending => return error(t!(JobOverlayChecking).into()),
                Probe::NeedsPassword => {}
            }
        }
        if let Some(attachments) = &self.attachments {
            if attachments.add.is_empty() && !attachments.remove_all {
                return Err(JobError::new(Step::Attachments, t!(JobAttachChoose)));
            }
            // Each is embedded under its file name, which must be unique.
            let mut names = std::collections::HashSet::new();
            for file in &attachments.add {
                let name = file.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                if !names.insert(name) {
                    let shown = display_name(file);
                    return Err(JobError::new(Step::Attachments, t!(JobAttachSameName, name = shown)));
                }
            }
        }
        if let Some(privacy) = &self.privacy {
            let removes = privacy.remove_info || privacy.remove_xmp || privacy.remove_structure || privacy.remove_page_labels;
            if !(removes || privacy.fixed_id) {
                return Err(JobError::new(Step::Privacy, t!(JobPrivacyChoose)));
            }
            if privacy.fixed_id && self.encrypts(false) {
                return Err(JobError::new(Step::Privacy, t!(JobPrivacyFixedId)));
            }
        }
        if let Some(Protect::Encrypt(encrypt)) = &self.protect {
            if encrypt.owner.is_empty() {
                return Err(JobError::new(Step::Protect, t!(JobOwnerRequired)));
            }
            if encrypt.owner.expose() == encrypt.user.expose() {
                return Err(JobError::new(Step::Protect, t!(JobOwnerDiffers)));
            }
        }
        if self.optimize.is_some_and(|o| matches!(o.images, Images::JpegQuality(q) if q > 100)) {
            return Err(JobError::new(Step::Optimize, t!(JobJpegQuality)));
        }
        let Some(output) = &self.output else { return Ok(()) };
        let error = |message: &str| Err(JobError::new(Step::Output, message));
        if output.split == Some(0) {
            return error(t!(JobSplitPages));
        }
        // Chosen in a dialog it always is; from a hand-edited profile, maybe not.
        if output.folder.as_deref().is_some_and(|folder| super::runner::check_path(folder).is_err()) {
            return error(t!(JobOutputFolderFull));
        }
        if let Some(name) = &output.name {
            super::sanitize::check_typed(name).map_err(|why| JobError::new(Step::Output, t!(JobFieldError, field = t!(FieldName), why = why)))?;
        }
        Ok(())
    }

    /// The suffix of the result's name: the step's, `processed` for
    /// several, `repaired` for none — and for a merge always `merged` or
    /// `mixed`, which says more than any other step.
    pub fn tag(&self) -> &'static str {
        match self.files() {
            Files::Merge => return t!(TagMerged),
            Files::Collate => return t!(TagMixed),
            Files::Each => {}
        }
        let steps = self.steps();
        match steps[..] {
            [] => t!(TagRepaired),
            [Step::Pages] => t!(TagPages),
            [Step::Overlay] => match &self.overlay {
                Some(overlay) if overlay.under => t!(TagUnderlaid),
                _ => t!(TagOverlaid),
            },
            [Step::Forms] => match self.forms.map(|f| f.action) {
                Some(FormAction::RemoveForm) => t!(TagFormRemoved),
                _ => t!(TagFlattened),
            },
            [Step::Attachments] => t!(TagAttachments),
            [Step::Privacy] => t!(TagCleaned),
            [Step::Optimize] => t!(TagOptimized),
            [Step::Protect] => match self.protect {
                Some(Protect::Encrypt(_)) => t!(TagProtected),
                _ => t!(TagUnlocked),
            },
            [Step::Output] => match self.output.as_ref().and_then(|o| o.split) {
                Some(_) => t!(TagSplit),
                None => t!(TagLinearized),
            },
            _ => t!(TagProcessed),
        }
    }

    /// Whether the result is encrypted, for an input that is or is not:
    /// QPDF keeps an input's encryption unless Protect changes it.
    fn encrypts(&self, input_encrypted: bool) -> bool {
        match self.protect {
            Some(Protect::Encrypt(_)) => true,
            Some(Protect::Unlock) => false,
            None => input_encrypted,
        }
    }

    /// The password for `probe`'s file ([`Probe::password`]).
    pub fn password_for(&self, probe: &Probe) -> Option<&Secret> {
        probe.password(self.input_password.as_ref())
    }

    /// [`Job::password_for`] as QPDF's argument.
    fn password_arg(&self, probe: &Probe) -> Option<Arg> {
        Arg::password(self.password_for(probe))
    }

    /// The input the result is rewritten from, keeping its encryption — none
    /// for a merge, which starts from an empty document.
    fn carried<'i, 'a>(&self, inputs: &'i [Input<'a>]) -> Option<&'i Input<'a>> {
        inputs.first().filter(|_| !self.merges())
    }

    /// QPDF's arguments for `inputs`, without the output file (the output
    /// module adds it last). Never `--no-default-limits`.
    pub fn args(&self, inputs: &[Input]) -> Vec<Arg> {
        let mut args: Vec<Arg> = self.carried(inputs).and_then(|input| self.password_arg(input.probe)).into_iter().collect();
        args.extend(self.options());
        // Before the additions: a new file of an old one's name replaces it.
        if self.lists_attachments() {
            let keys = inputs.first().and_then(|input| input.attachments).unwrap_or_default();
            args.extend(keys.iter().map(|key| Arg::plain(format!("--remove-attachment={key}"))));
        }
        args.extend(self.input_args(inputs));
        args
    }

    /// The arguments that do not depend on the inputs, up to `--progress`.
    fn options(&self) -> Vec<Arg> {
        let mut args = Vec::new();
        match &self.protect {
            Some(Protect::Encrypt(encrypt)) => {
                args.push(Arg::plain("--encrypt"));
                if !encrypt.user.is_empty() {
                    args.push(Arg::Secret("--user-password", encrypt.user.duplicate()));
                }
                args.push(Arg::Secret("--owner-password", encrypt.owner.duplicate()));
                args.push(Arg::plain("--bits=256"));
                let print = match encrypt.print {
                    Print::Full => "full",
                    Print::Low => "low",
                    Print::None => "none",
                };
                args.push(Arg::plain(format!("--print={print}")));
                for (option, allowed) in [
                    ("--extract", encrypt.copy),
                    ("--modify-other", encrypt.edit),
                    ("--annotate", encrypt.annotate),
                    ("--form", encrypt.fill_forms),
                    ("--assemble", encrypt.assemble),
                ] {
                    args.push(Arg::plain(format!("{option}={}", if allowed { "y" } else { "n" })));
                }
                args.push(Arg::plain("--"));
            }
            Some(Protect::Unlock) => args.push(Arg::plain("--decrypt")),
            None => {}
        }
        if let Some(forms) = self.forms {
            match forms.action {
                // Fields often have no appearance of their own (measured: without
                // `--generate-appearances` QPDF leaves them out, with a warning).
                FormAction::Flatten(which) => {
                    let which = match which {
                        Annotations::All => "all",
                        Annotations::Print => "print",
                        Annotations::Screen => "screen",
                    };
                    args.push(Arg::plain(format!("--flatten-annotations={which}")));
                    args.push(Arg::plain("--generate-appearances"));
                }
                FormAction::RemoveForm => {
                    args.push(Arg::plain("--generate-appearances"));
                    args.push(Arg::plain("--remove-acroform"));
                }
            }
            if forms.flatten_rotation {
                args.push(Arg::plain("--flatten-rotation"));
            }
        }
        for file in self.attachments.iter().flat_map(|a| &a.add) {
            args.extend([Arg::plain("--add-attachment"), Arg::path(file), Arg::plain("--")]);
        }
        if let Some(privacy) = self.privacy {
            for (option, on) in [
                ("--remove-info", privacy.remove_info),
                ("--remove-metadata", privacy.remove_xmp),
                ("--deterministic-id", privacy.fixed_id),
                ("--remove-structure", privacy.remove_structure),
                ("--remove-page-labels", privacy.remove_page_labels),
            ] {
                if on {
                    args.push(Arg::plain(option));
                }
            }
        }
        if let Some(optimize) = self.optimize {
            // Lossless: object streams, compression of every stream, flate
            // streams recompressed, resources no page uses dropped — only when
            // pages are chosen or split: QPDF looks for them only then (an
            // image no page draws stayed in a plain rewrite, measured). Measured
            // on 12.4.2: the QPDF manual −1 %, a 3-page file −35 %, 10 000
            // pages −94 %.
            let object_streams = match optimize.object_streams {
                ObjectStreams::Generate => "generate",
                ObjectStreams::Preserve => "preserve",
                ObjectStreams::Disable => "disable",
            };
            args.push(Arg::plain("--compress-streams=y"));
            args.push(Arg::plain("--recompress-flate"));
            args.push(Arg::plain(format!("--object-streams={object_streams}")));
            args.push(Arg::plain("--remove-unreferenced-resources=yes"));
            if optimize.images.smaller() {
                args.push(Arg::plain("--optimize-images"));
                if let Images::JpegQuality(quality) = optimize.images {
                    args.push(Arg::plain(format!("--jpeg-quality={quality}")));
                }
                if optimize.keep_inline_images {
                    args.push(Arg::plain("--keep-inline-images"));
                }
                let (width, height) = optimize.min_image;
                for (option, value, default) in [("--oi-min-width", width, MIN_IMAGE.0), ("--oi-min-height", height, MIN_IMAGE.1)] {
                    if value != default {
                        args.push(Arg::plain(format!("{option}={value}")));
                    }
                }
                // QPDF also skips images of fewer than 16 384 pixels (128×128):
                // a 100×100 image stayed as it was with the width and height
                // lowered alone (measured). An image as wide and as high as
                // asked has their product anyway.
                if optimize.min_image != MIN_IMAGE {
                    args.push(Arg::plain("--oi-min-area=0"));
                }
            }
        }
        if let Some(output) = &self.output {
            if output.linearize {
                args.push(Arg::plain("--linearize"));
            }
            if let Some(pages) = output.split {
                args.push(Arg::plain(format!("--split-pages={pages}")));
            }
        }
        if let Some(rotate) = self.pages.as_ref().and_then(|p| p.rotate.as_ref()) {
            let angle = match rotate.angle {
                Angle::Right => "+90",
                Angle::UpsideDown => "+180",
                Angle::Left => "-90",
            };
            args.push(Arg::plain(match &rotate.pages {
                Some(range) => format!("--rotate={angle}:{}", range.text()),
                None => format!("--rotate={angle}"),
            }));
        }
        if let Some(overlay) = &self.overlay {
            args.push(Arg::plain(if overlay.under { "--underlay" } else { "--overlay" }));
            args.push(Arg::path(&overlay.file));
            args.extend(self.password_arg(&overlay.probe));
            let (from, repeat) = overlay.qpdf_ranges();
            for (option, range) in [("--to", overlay.to.as_ref().map(PageRange::text)), ("--from", from), ("--repeat", repeat)] {
                if let Some(range) = range {
                    args.push(Arg::plain(format!("{option}={range}")));
                }
            }
            args.push(Arg::plain("--"));
        }
        args.push(Arg::plain("--progress"));
        args
    }

    /// The inputs, and the pages taken from them.
    fn input_args(&self, inputs: &[Input]) -> Vec<Arg> {
        let mut args = Vec::new();
        let files = self.files();
        if files.merges() {
            // From an empty document: no outline or encryption of the
            // first file is carried over; page labels and form fields of
            // every file are (measured).
            args.push(Arg::plain("--empty"));
            if files == Files::Collate {
                args.push(Arg::plain("--collate"));
            }
            args.push(Arg::plain("--pages"));
            for input in inputs {
                args.push(Arg::path(input.path));
                args.extend(self.password_arg(input.probe));
                // An empty argument cannot pass through `@-`.
                args.push(Arg::plain(self.pages_arg(input).unwrap_or("1-z")));
            }
            args.push(Arg::plain("--"));
            return args;
        }
        let Some(input) = inputs.first() else { return args };
        args.push(Arg::path(input.path));
        // `.` is the input itself (QPDF's shorthand).
        if let Some(range) = self.pages_arg(input) {
            args.extend([Arg::plain("--pages"), Arg::plain("."), Arg::plain(range), Arg::plain("--")]);
        }
        args
    }

    /// The arguments for one file needing the password, as text with every
    /// password masked — what the job does, without any file.
    pub fn describe(&self) -> String {
        let (probe, keys) = (Probe::NeedsPassword, [String::from("<key>")]);
        let args = self.args(&[Input { path: Path::new("<input>"), size: 0, probe: &probe, range: None, attachments: Some(&keys) }]);
        args.iter().map(masked).collect::<Vec<_>>().join(" ")
    }

    /// What tells this job's work apart from another's: what it does,
    /// whatever the inputs.
    pub fn work_key(&self) -> WorkKey {
        let mut words = Vec::new();
        let mut passwords = Vec::new();
        for arg in self.options() {
            match arg {
                // It only opens a file (the overlay's): typing it must not
                // make finished files run again.
                Arg::Secret("--password", _) => {}
                Arg::Secret(_, ref secret) => {
                    words.push(masked(&arg));
                    passwords.push(secret.duplicate());
                }
                _ => words.push(masked(&arg)),
            }
        }
        // The pages taken are no option, yet other pages are other work (a
        // file's own pages are its entry's: `FileList::merges_from`).
        if let Some(pages) = &self.pages {
            // Numbers, not `{:?}`: Debug of these enums would only be in the EXE for this.
            words.push(format!("{}:{}:{}", pages.files as u8, pages.select.mode() as u8, pages.select.range().map_or("", PageRange::text)));
        }
        // The keys removed differ per file; that they are removed does not.
        if self.lists_attachments() {
            words.push("--remove-attachment=*".into());
        }
        // The name and folder, as a file `x.pdf` would get them.
        let place = self.result_path(Path::new("x.pdf")).map(|(path, _)| path.display().to_string()).unwrap_or_default();
        WorkKey {
            words: format!("{place} {}", words.join(" ")),
            passwords,
            merges: self.merges(),
            // A file's own pages count whenever the Pages step is on.
            own_pages: self.pages.is_some(),
        }
    }

    /// Whether "Keep original if not smaller" applies to these steps: to
    /// Optimize alone — the other steps change what the file is — or with
    /// Protect `unlocks`, which changes nothing in a file that is not
    /// encrypted; an encrypted one is then kept whatever its size.
    pub fn keeps_original_applies(steps: &[Step], unlocks: bool) -> bool {
        steps == [Step::Optimize] || (unlocks && steps == [Step::Optimize, Step::Protect])
    }

    /// Whether Protect takes the lock off (Unlock).
    fn unlocks(&self) -> bool {
        matches!(self.protect, Some(Protect::Unlock))
    }

    /// Whether `input` can go into a result, before QPDF runs — and the pages
    /// it gives, when known; or its verdict.
    fn check_input(&self, input: &Input) -> Result<Option<u64>, Verdict> {
        match input.probe {
            Probe::Failed(failure) => return Err(Verdict::Failed(failure.clone())),
            Probe::NeedsPassword if self.input_password.is_none() => return Err(Verdict::PasswordRequired),
            _ => {}
        }
        // QPDF refuses a fixed ID for an encrypted result, also one kept from
        // the input (measured) — say why, and what helps, instead of its
        // message. A merge starts from an empty, unencrypted document.
        if self.privacy.is_some_and(|p| p.fixed_id) && self.encrypts(!self.merges() && input.probe.encrypted()) {
            return Err(Verdict::Failed(Failure::FixedIdEncrypted));
        }
        if self.lists_attachments() && input.attachments.is_none() {
            return Err(Verdict::Failed(Failure::Other(t!(FailureAttachmentsUnlisted).into())));
        }
        let Some(pages) = input.probe.pages() else { return Ok(None) };
        let Some(range) = self.range_for(input) else { return Ok(Some(pages)) };
        range.count(pages).map(Some).map_err(|e| Verdict::Failed(JobError::new(Step::Pages, e.sentence()).into()))
    }

    /// The checks on the whole result of `pages` pages: the ranges that
    /// number its pages.
    fn check_result(&self, pages: u64) -> Result<(), JobError> {
        if let Some(range) = self.pages.as_ref().and_then(|p| p.rotate.as_ref()).and_then(|r| r.pages.as_ref()) {
            count_in_result(range, pages, Step::Pages, t!(FieldPagesRotate))?;
        }
        if let Some(range) = self.overlay.as_ref().and_then(|o| o.to.as_ref()) {
            count_in_result(range, pages, Step::Overlay, t!(FieldPagesCover))?;
        }
        Ok(())
    }

    /// Where the result made from `first` (the first input) goes, before a
    /// ` (2)`… is added: `<name> (<tag>).pdf` next to it or in the
    /// chosen folder, the typed name in place of the tag — for a merge, the
    /// typed name is the whole name. Split: a folder of that name without
    /// `.pdf`, and the name QPDF numbers its parts after (`<name>-1-5.pdf`).
    pub fn result_path(&self, first: &Path) -> Option<(PathBuf, Option<String>)> {
        let output = self.output.as_ref();
        let folder = output.and_then(|o| o.folder.as_deref());
        let typed = output.and_then(|o| o.name.as_deref());
        let (named, parts) = match typed {
            Some(name) if self.merges() => {
                let stem = strip_pdf(name);
                (folder.or_else(|| first.parent())?.join(format!("{stem}.pdf")), stem.to_string())
            }
            _ => (output_name(first, typed.unwrap_or(self.tag()), folder)?, first.file_stem()?.to_string_lossy().into_owned()),
        };
        Some(match output.and_then(|o| o.split) {
            Some(_) => (named.with_extension(""), Some(parts)),
            None => (named, None),
        })
    }

    /// The result to produce from `inputs`, which passed their checks and
    /// give `expected_pages` together (when known).
    fn target<'a>(&self, qpdf: &'a Path, inputs: &[Input], expected_pages: Option<u64>) -> Result<Target<'a>, Failure> {
        let first = inputs.first().ok_or_else(|| Failure::Other(t!(FailureNoFile).into()))?;
        let (desired, split_parts) = self.result_path(first.path).ok_or_else(|| Failure::Other(t!(FailureNoFileName).into()))?;
        if let Some(total) = expected_pages {
            self.check_result(total)?;
        }
        let result_password = match &self.protect {
            Some(Protect::Encrypt(encrypt)) => (!encrypt.user.is_empty()).then(|| encrypt.user.duplicate()),
            Some(Protect::Unlock) => None,
            // An input needing a password stays encrypted with it.
            None => self.carried(inputs).and_then(|input| self.password_for(input.probe)).map(Secret::duplicate),
        };
        let applies = Job::keeps_original_applies(&self.steps(), self.unlocks()) && !(self.unlocks() && first.probe.encrypted());
        Ok(Target {
            qpdf,
            args: self.args(inputs),
            desired,
            split_parts,
            input_bytes: inputs.iter().fold(0u64, |sum, input| sum.saturating_add(input.size)),
            result_password,
            expected_pages,
            keep_if_not_smaller: self.optimize.filter(|o| o.keep_if_not_smaller && applies).map(|_| first.size),
        })
    }

    /// The result to produce from `inputs` — or, when it cannot be made,
    /// what came of each input, before QPDF runs. In a merge, an input that
    /// cannot be used leaves the others alone.
    fn plan<'a>(&self, qpdf: &'a Path, inputs: &[Input]) -> Result<Target<'a>, Vec<Produced>> {
        let checked: Vec<Result<Option<u64>, Verdict>> = inputs.iter().map(|input| self.check_input(input)).collect();
        if checked.iter().any(Result::is_err) {
            return Err(checked.into_iter().map(|c| c.map_or_else(Produced::ended, |_| Produced::failed(Failure::NotMerged))).collect());
        }
        let expected_pages = checked.into_iter().flatten().try_fold(0u64, |sum, pages| pages.map(|p| sum.saturating_add(p)));
        self.target(qpdf, inputs, expected_pages).map_err(|failure| vec![Produced::failed(failure); inputs.len()])
    }

    /// Produce the result of `inputs` — one file, or every file of a merge —
    /// and say what came of each input.
    pub fn produce(&self, qpdf: &Path, inputs: &[Input], ctx: &Context) -> Vec<Produced> {
        match self.plan(qpdf, inputs) {
            Ok(target) => vec![output::produce(target, ctx); inputs.len()],
            Err(refused) => refused,
        }
    }

    /// The command for cmd.exe that makes what Start makes of `inputs`:
    /// `qpdf.exe` and the same arguments, every password as
    /// `********`, writing straight to the result's name — the first one free
    /// now (QpdfDesk itself writes a temporary file, verifies it and renames
    /// it). A split result first needs its folder. `Err`: why a file would
    /// not run, before QPDF does.
    pub fn command(&self, qpdf: &Path, inputs: &[Input]) -> Result<String, String> {
        let target = self.plan(qpdf, inputs).map_err(|refused| {
            // In a merge, the file that stops it — not the others.
            let mut each = refused.iter().zip(inputs);
            let blamed = each.clone().find(|(p, _)| p.verdict != Verdict::Failed(Failure::NotMerged)).or_else(|| each.next());
            blamed.map_or_else(|| t!(FailureNoFile).into(), |(p, input)| format!("{}: {}", display_name(input.path), p.verdict.status()))
        })?;
        let free = |path: &Path, folder| {
            let taken = |candidate: &Path| if candidate.exists() { Err(std::io::ErrorKind::AlreadyExists.into()) } else { Ok(()) };
            output::first_free(path, folder, taken).unwrap_or_else(|_| path.to_path_buf())
        };
        let mut line = String::new();
        let result = match &target.split_parts {
            Some(parts) => {
                let folder = free(&target.desired, true);
                line = format!("mkdir {} && ", quote(&folder.display().to_string()));
                folder.join(format!("{parts}.pdf"))
            }
            None => free(&target.desired, false),
        };
        let words = std::iter::once(qpdf.display().to_string()).chain(target.args.iter().map(masked)).chain([result.display().to_string()]);
        line.push_str(&words.map(|word| quote(&word)).collect::<Vec<_>>().join(" "));
        Ok(line)
    }
}

/// A merge's typed name without the `.pdf` the user may have typed.
fn strip_pdf(name: &str) -> &str {
    let cut = name.len().saturating_sub(4);
    match name.get(cut..) {
        Some(extension) if cut > 0 && extension.eq_ignore_ascii_case(".pdf") => &name[..cut],
        _ => name,
    }
}

impl From<JobError> for Failure {
    fn from(error: JobError) -> Failure {
        Failure::Other(error.sentence())
    }
}

/// An argument as text, a password masked.
fn masked(arg: &Arg) -> String {
    match arg {
        Arg::Plain(text) => text.clone(),
        Arg::Path(path) => path.display().to_string(),
        Arg::Secret(option, _) => format!("{option}=********"),
    }
}

/// What tells two jobs' work apart: the result name and the arguments
/// with the passwords masked, plus the new passwords themselves — a
/// different password is different work. Kept as long as the results it
/// made are on the list; wiped with it.
pub struct WorkKey {
    words: String,
    passwords: Vec<Secret>,
    merges: bool,
    own_pages: bool,
}

impl WorkKey {
    /// Whether the job merges the whole list: then its results also depend
    /// on which files are on the list, in which order, with which pages.
    pub(crate) fn merges(&self) -> bool {
        self.merges
    }

    /// Whether a file's own pages shape its result.
    pub(crate) fn own_pages(&self) -> bool {
        self.own_pages
    }
}

impl PartialEq for WorkKey {
    fn eq(&self, other: &Self) -> bool {
        self.words == other.words
            && self.passwords.len() == other.passwords.len()
            && self.passwords.iter().zip(&other.passwords).all(|(a, b)| a.expose() == b.expose())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(text: &str) -> Secret {
        Secret::new(text.to_string()).expect("valid")
    }

    fn range(text: &str) -> PageRange {
        PageRange::parse(text).expect("valid range")
    }

    fn encrypt(user: &str, owner: &str) -> Protect {
        Protect::Encrypt(Encrypt {
            user: secret(user),
            owner: secret(owner),
            print: Print::Low,
            copy: true,
            edit: false,
            annotate: true,
            fill_forms: true,
            assemble: false,
        })
    }

    fn overlay(probe: Probe, placement: Placement) -> Option<Overlay> {
        Some(Overlay { under: false, file: PathBuf::from(r"C:\stamp.pdf"), to: None, placement, probe })
    }

    fn pages_step(select: Select, rotate: Option<&str>) -> Option<Pages> {
        Some(Pages { select, files: Files::Each, rotate: rotate.map(|r| Rotate { angle: Angle::Right, pages: Some(range(r)) }) })
    }

    /// Every file into one result: `files` Merge or Collate.
    fn merge_step(files: Files, select: Select) -> Option<Pages> {
        Some(Pages { select, files, rotate: None })
    }

    const READY: Probe = Probe::Ready { pages: 10, restricted: false };

    /// An input of 1000 bytes.
    fn input<'a>(path: &'a str, probe: &'a Probe) -> Input<'a> {
        Input { path: Path::new(path), size: 1000, probe, range: None, attachments: None }
    }

    fn plan(job: &Job, inputs: &[Input]) -> Result<Target<'static>, Vec<Produced>> {
        job.plan(Path::new(r"C:\q\qpdf.exe"), inputs)
    }

    /// The target for one file, which must be possible.
    fn target(job: &Job, probe: &Probe) -> Target<'static> {
        plan(job, &[input(r"C:\Docs\a.pdf", probe)]).expect("a target")
    }

    /// Why one file cannot be done.
    fn refused(job: &Job, probe: &Probe) -> Verdict {
        match plan(job, &[input(r"C:\Docs\a.pdf", probe)]) {
            Ok(_) => panic!("not refused"),
            Err(produced) => produced[0].verdict.clone(),
        }
    }

    fn words(args: &[Arg]) -> String {
        args.iter().map(masked).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn lossless_optimize_arguments() {
        let job = Job::optimize_lossless();
        let args = job.args(&[input(r"C:\Docs\-x.pdf", &READY)]);
        assert_eq!(
            job.describe(),
            "--compress-streams=y --recompress-flate --object-streams=generate \
             --remove-unreferenced-resources=yes --progress <input>"
        );
        // The input goes as a path, so the runner's absolute-path rule holds.
        assert!(matches!(args.last(), Some(Arg::Path(p)) if p == Path::new(r"C:\Docs\-x.pdf")));
        assert_eq!(job.tag(), "optimized");
    }

    #[test]
    fn every_step_has_its_arguments() {
        let job = Job {
            pages: Some(Pages {
                select: Select::Delete(range("2,z").complement().expect("deletable")),
                files: Files::Each,
                rotate: Some(Rotate { angle: Angle::Left, pages: Some(range("1-3")) }),
            }),
            overlay: Some(Overlay {
                to: Some(range("1-2")),
                placement: Placement::PageOne,
                ..overlay(Probe::NeedsPassword, Placement::InOrder).expect("overlay")
            }),
            forms: Some(Forms { action: FormAction::Flatten(Annotations::Print), flatten_rotation: false }),
            attachments: Some(Attachments { add: vec![PathBuf::from(r"C:\notes.txt")], remove_all: true }),
            privacy: Some(Privacy { remove_info: true, remove_xmp: false, fixed_id: true, ..Privacy::default() }),
            optimize: Some(Optimize { images: Images::JpegQuality(60), keep_if_not_smaller: true, ..Optimize::default() }),
            protect: Some(Protect::Unlock),
            output: Some(Output { split: Some(2), linearize: true, ..Output::default() }),
            input_password: Some(secret("open sesame")),
        };
        assert_eq!(
            job.describe(),
            "--password=******** --decrypt --flatten-annotations=print --generate-appearances \
             --add-attachment C:\\notes.txt -- \
             --remove-info --deterministic-id --compress-streams=y --recompress-flate \
             --object-streams=generate --remove-unreferenced-resources=yes --optimize-images \
             --jpeg-quality=60 --linearize --split-pages=2 --rotate=-90:1-3 \
             --overlay C:\\stamp.pdf --password=******** --to=1-2 --from=1 --repeat=1 -- \
             --progress --remove-attachment=<key> <input> --pages . 1-z,x2,xz --"
        );
        assert_eq!(job.tag(), "processed");
        assert_eq!(job.check(), Ok(()));
    }

    #[test]
    fn encryption_is_aes_256_with_the_chosen_permissions() {
        let job = Job { protect: Some(encrypt("", "owner")), ..Job::default() };
        assert_eq!(
            job.describe(),
            "--encrypt --owner-password=******** --bits=256 --print=low --extract=y --modify-other=n \
             --annotate=y --form=y --assemble=n -- --progress <input>"
        );
        assert_eq!(job.tag(), "protected");
        let unlock = Job { protect: Some(Protect::Unlock), ..Job::default() };
        assert_eq!((unlock.describe().as_str(), unlock.tag()), ("--decrypt --progress <input>", "unlocked"));
        let with_open = Job { protect: Some(encrypt("user", "owner")), ..Job::default() };
        assert!(with_open.describe().starts_with("--encrypt --user-password=******** --owner-password=********"));
        // The passwords themselves travel only as secrets.
        let args = with_open.args(&[input(r"C:\a.pdf", &READY)]);
        let secrets: Vec<&str> =
            args.iter().filter_map(|a| if let Arg::Secret(_, s) = a { Some(s.expose()) } else { None }).collect();
        assert_eq!(secrets, ["user", "owner"]);
        assert!(args.iter().all(|a| !matches!(a, Arg::Plain(t) if t.contains("owner"))));
    }

    #[test]
    fn mistakes_are_named_with_their_step() {
        let cases: [(Job, Step); 9] = [
            (Job { overlay: overlay(Probe::Failed(Failure::NotPdf), Placement::InOrder), ..Job::default() }, Step::Overlay),
            (Job { overlay: overlay(Probe::NeedsPassword, Placement::InOrder), ..Job::default() }, Step::Overlay),
            (Job { overlay: overlay(Probe::Ready { pages: 0, restricted: false }, Placement::PageOne), ..Job::default() }, Step::Overlay),
            (
                Job { optimize: Some(Optimize { images: Images::JpegQuality(101), keep_if_not_smaller: false, ..Optimize::default() }), ..Job::default() },
                Step::Optimize,
            ),
            (
                Job {
                    privacy: Some(Privacy { remove_info: false, remove_xmp: false, fixed_id: false, ..Privacy::default() }),
                    ..Job::default()
                },
                Step::Privacy,
            ),
            (Job { privacy: Some(Privacy::default()), protect: Some(encrypt("", "o")), ..Job::default() }, Step::Privacy),
            (Job { protect: Some(encrypt("u", "")), ..Job::default() }, Step::Protect),
            (Job { protect: Some(encrypt("same", "same")), ..Job::default() }, Step::Protect),
            (Job { output: Some(Output { split: Some(0), linearize: false, ..Output::default() }), ..Job::default() }, Step::Output),
        ];
        for (job, step) in cases {
            assert_eq!(job.check().map_err(|e| e.step), Err(Some(step)), "{}", job.describe());
        }
        assert_eq!(
            JobError::new(Step::Protect, t!(JobOwnerRequired)).sentence(),
            "Protection: the owner password is required"
        );
        // An overlay file still being probed: its pages and password are not
        // known yet, so Start waits — QPDF would run without them and fail.
        let probing = Job { overlay: overlay(Probe::Pending, Placement::InOrder), ..Job::default() };
        assert_eq!(probing.check().map_err(|e| e.sentence()), Err("Stamp / background: checking the file…".into()));
    }

    #[test]
    fn the_panel_values_are_read_with_their_rules() {
        let select = |mode, text| Select::read(mode, text).map_err(|e| e.sentence());
        assert_eq!(select(PageMode::Keep, "1, 3"), Ok(Select::Keep(range("1,3"))));
        assert_eq!(select(PageMode::Delete, "2,z"), Ok(Select::Delete(range("1-z,x2,xz"))), "the rest is kept");
        assert_eq!(select(PageMode::Delete, "1-3:odd"), Err("Pages: pages to delete cannot use x, :odd or :even".into()));
        assert_eq!(select(PageMode::Keep, ""), Err("Pages: enter the pages, e.g. 1-5,8,z".into()));
        assert_eq!(select(PageMode::Reverse, "ignored"), Ok(Select::Reverse));
        assert_eq!(select(PageMode::All, "ignored"), Ok(Select::All));
        // Every page of each file on its own, unrotated: nothing to do — unless
        // some file has pages of its own.
        let pages = |files, own_pages| Pages::read(Select::All, files, None, own_pages).map(|_| ()).map_err(|e| e.step);
        assert_eq!(pages(Files::Each, false), Err(Some(Step::Pages)));
        assert_eq!(pages(Files::Each, true), Ok(()));
        assert_eq!(pages(Files::Merge, false), Ok(()));
        assert_eq!(Rotate::read(Angle::Left, " ").map(|r| r.pages), Ok(None), "empty = every page");
        assert!(Rotate::read(Angle::Left, "1-").is_err_and(|e| e.sentence().starts_with("Pages: pages to rotate: ")));
        let read = |placement, on_pages| Overlay::read(true, PathBuf::from(r"C:\s.pdf"), Probe::Pending, placement, on_pages);
        let ranges = |o: Overlay| {
            let (from, repeat) = o.qpdf_ranges();
            [o.to.as_ref().map(|r| r.text().to_string()), from.map(String::from), repeat.map(String::from)]
        };
        let some = |text: &str| Some(text.to_string());
        assert_eq!(read(Placement::PageOne, " ").map(ranges), Ok([None, some("1"), some("1")]));
        assert_eq!(read(Placement::PageOne, "2-z").map(ranges), Ok([some("2-z"), some("1"), some("1")]));
        assert_eq!(read(Placement::InOrder, "").map(ranges), Ok([None, None, None]));
        assert_eq!(read(Placement::InTurn, "1").map(ranges), Ok([some("1"), None, some("1-z")]));
        assert!(read(Placement::InOrder, "2-").is_err_and(|e| e.sentence().starts_with("Stamp / background: pages to cover: ")));

        let password = |first: &str, second: &str| {
            Encrypt::password(first.into(), second.into(), true).map(|s| s.expose().to_string()).map_err(|e| e.sentence())
        };
        assert_eq!(password("a b", "a b"), Ok("a b".into()));
        assert_eq!(password("", ""), Ok(String::new()));
        assert_eq!(password("a", ""), Err("Protection: repeat the owner password".into()));
        assert_eq!(password("a", "b"), Err("Protection: the two owner passwords differ".into()));
        assert!(password("a\nb", "a\nb").is_err());

        assert!(input_password(String::new()).is_ok_and(|p| p.is_none()));
        assert!(input_password("in".into()).is_ok_and(|p| p.is_some_and(|s| s.expose() == "in")));
        // Not Protect's fault: that step may be off.
        assert_eq!(
            input_password("a\rb".into()).map(|_| ()).map_err(|e| e.sentence()),
            Err("Password for encrypted inputs: it cannot contain a line break".into())
        );
    }

    #[test]
    fn the_same_work_is_told_apart_by_options_and_new_passwords() {
        let key = |protect, input: Option<&str>| {
            Job { protect: Some(protect), input_password: input.map(secret), ..Job::default() }.work_key()
        };
        assert!(key(encrypt("u", "o"), None) == key(encrypt("u", "o"), None));
        assert!(key(encrypt("u", "o"), None) != key(encrypt("u", "other"), None), "a new password is new work");
        assert!(key(encrypt("u", "o"), None) == key(encrypt("u", "o"), Some("in")), "the input password only opens");
        assert!(Job::optimize_lossless().work_key() != Job::default().work_key());
        assert!(Job { pages: merge_step(Files::Merge, Select::All), ..Job::default() }.work_key().merges());
        // Other pages are other work (they were missing from the key: Keep
        // 1-5 after Keep 1-3 ran nothing again).
        let pages = |pages| Job { pages, ..Job::default() }.work_key();
        assert!(pages(pages_step(Select::Keep(range("1-3")), None)) != pages(pages_step(Select::Keep(range("1-5")), None)));
        assert!(pages(pages_step(Select::Keep(range("1-3")), None)) != pages(pages_step(Select::Delete(range("1-3")), None)));
        assert!(pages(pages_step(Select::All, None)) != pages(pages_step(Select::Reverse, None)));
        assert!(pages(merge_step(Files::Merge, Select::All)) != pages(merge_step(Files::Merge, Select::Keep(range("1")))));
        assert!(pages(merge_step(Files::Merge, Select::All)) != pages(merge_step(Files::Collate, Select::All)));
        assert!(pages(pages_step(Select::Keep(range("1-3")), None)) == pages(pages_step(Select::Keep(range("1-3")), None)));
        assert!(!Job::optimize_lossless().work_key().merges());
        assert!(Job::keeps_original_applies(&[Step::Optimize], false));
        assert!(!Job::keeps_original_applies(&[Step::Privacy, Step::Optimize], false));
        assert!(Job::keeps_original_applies(&[Step::Optimize, Step::Protect], true), "unlocking changes nothing in a file that is not encrypted");
        assert!(!Job::keeps_original_applies(&[Step::Optimize, Step::Protect], false), "encrypting changes the file");
    }

    #[test]
    fn no_step_is_repair() {
        let job = Job::default();
        assert_eq!(job.tag(), "repaired");
        assert_eq!(job.check(), Ok(()));
        assert_eq!(job.describe(), "--progress <input>");
    }

    #[test]
    fn the_target_carries_the_job() {
        let optimized = target(&Job::optimize_lossless(), &READY);
        assert_eq!(optimized.desired, Path::new(r"C:\Docs\a (optimized).pdf"));
        assert_eq!((optimized.input_bytes, optimized.expected_pages, optimized.keep_if_not_smaller), (1000, Some(10), Some(1000)));
        assert!(optimized.result_password.is_none() && optimized.split_parts.is_none());
        // Keeping the original applies to Optimize alone.
        let with_privacy = Job { privacy: Some(Privacy::default()), ..Job::optimize_lossless() };
        assert_eq!(target(&with_privacy, &READY).keep_if_not_smaller, None);
        // Optimize and Unlock: by size for a file that is not
        // encrypted, kept whatever its size for one that is.
        let unlocking = Job { protect: Some(Protect::Unlock), ..Job::optimize_lossless() };
        assert_eq!(target(&unlocking, &READY).keep_if_not_smaller, Some(1000));
        let restricted = Probe::Ready { pages: 10, restricted: true };
        assert_eq!(target(&unlocking, &restricted).keep_if_not_smaller, None);
        assert!(plan(&Job::default(), &[input(r"C:\", &READY)]).is_err());
    }

    #[test]
    fn the_result_page_count_follows_the_pages_step() {
        let expected = |pages| target(&Job { pages, ..Job::default() }, &READY).expected_pages;
        assert_eq!(expected(pages_step(Select::Keep(range("2-4,z")), None)), Some(4));
        assert_eq!(expected(pages_step(Select::Delete(range("2-4,z").complement().expect("ok")), None)), Some(6));
        assert_eq!(expected(pages_step(Select::Reverse, None)), Some(10));
        let refused_with = |pages| match refused(&Job { pages, ..Job::default() }, &READY) {
            Verdict::Failed(Failure::Other(message)) => message,
            other => panic!("{other:?}"),
        };
        assert!(refused_with(pages_step(Select::Keep(range("12")), None)).contains("page 12 does not exist"));
        assert!(refused_with(pages_step(Select::Delete(range("1-z").complement().expect("ok")), None)).contains("no pages"));
        // Rotation numbers the RESULT's pages (measured): two pages kept, so
        // there is no page 3 to rotate — once this passed, and QPDF failed.
        assert_eq!(
            refused_with(pages_step(Select::Keep(range("3,1")), Some("3"))),
            "Pages: pages to rotate: page 3 does not exist (the result has 2)"
        );
        let one_page = Probe::Ready { pages: 1, restricted: false };
        assert_eq!(
            refused(&Job { pages: pages_step(Select::All, Some("1-z:even")), ..Job::default() }, &one_page),
            Verdict::Failed(Failure::Other("Pages: pages to rotate: none of them is in the result".into()))
        );
        let to = Job { overlay: Some(Overlay { to: Some(range("12")), ..overlay(Probe::Pending, Placement::InOrder).expect("o") }), ..Job::default() };
        assert!(matches!(refused(&to, &READY), Verdict::Failed(Failure::Other(m)) if m == "Stamp / background: pages to cover: page 12 does not exist (the result has 10)"));
        // Unknown page count: QPDF checks the numbers itself.
        let job = Job { pages: pages_step(Select::Keep(range("12")), None), ..Job::default() };
        assert_eq!(target(&job, &Probe::Pending).expected_pages, None);
    }

    #[test]
    fn only_a_file_that_needs_the_password_gets_it() {
        let job = Job { input_password: Some(secret("in")), ..Job::optimize_lossless() };
        let kept = target(&job, &Probe::NeedsPassword);
        assert!(words(&kept.args).starts_with("--password=******** "));
        assert_eq!(kept.result_password.map(|s| s.expose().to_string()).as_deref(), Some("in"), "the encryption is kept");
        // Encrypted without an open password: a wrong one would make QPDF
        // refuse it (measured) — once every file got it.
        let restricted = target(&job, &Probe::Ready { pages: 3, restricted: true });
        assert!(!words(&restricted.args).contains("--password") && restricted.result_password.is_none());
        let password = |job: &Job| target(job, &Probe::NeedsPassword).result_password.map(|s| s.expose().to_string());
        let decrypted = Job { input_password: Some(secret("in")), protect: Some(Protect::Unlock), ..Job::default() };
        assert_eq!(password(&decrypted), None);
        let encrypted = Job { input_password: Some(secret("in")), protect: Some(encrypt("new", "o")), ..Job::default() };
        assert_eq!(password(&encrypted).as_deref(), Some("new"));
        let open = Job { protect: Some(encrypt("", "o")), ..Job::default() };
        assert_eq!(target(&open, &READY).result_password.map(|_| ()), None);
        // Without the password, the file is not even tried.
        assert_eq!(refused(&Job::optimize_lossless(), &Probe::NeedsPassword), Verdict::PasswordRequired);
        assert_eq!(refused(&Job::default(), &Probe::Failed(Failure::NotPdf)), Verdict::Failed(Failure::NotPdf));
    }

    #[test]
    fn a_fixed_id_is_refused_for_encrypted_inputs_kept_encrypted() {
        let restricted = Probe::Ready { pages: 3, restricted: true };
        let privacy = Job { privacy: Some(Privacy::default()), ..Job::default() };
        assert_eq!(refused(&privacy, &restricted), Verdict::Failed(Failure::FixedIdEncrypted));
        target(&privacy, &READY);
        let decrypting = Job { protect: Some(Protect::Unlock), ..privacy };
        target(&decrypting, &restricted);
        // A merge starts from an empty, unencrypted document.
        let merging = Job { pages: merge_step(Files::Merge, Select::All), privacy: Some(Privacy::default()), ..Job::default() };
        target(&merging, &restricted);
    }

    #[test]
    fn merging_takes_every_file_in_list_order_with_its_own_pages() {
        fn inputs<'a>(ten: &'a Probe, second: &'a Probe, from_two: &'a PageRange) -> [Input<'a>; 3] {
            [
                input(r"C:\x\a.pdf", ten),
                Input { path: Path::new(r"C:\y\b.pdf"), size: 500, probe: second, range: None, attachments: None },
                Input { range: Some(from_two), ..input(r"C:\y\c.pdf", ten) },
            ]
        }
        let (ten, locked, three, from_two) = (READY, Probe::NeedsPassword, Probe::Ready { pages: 3, restricted: false }, range("2-z"));
        let job = Job { pages: merge_step(Files::Merge, Select::All), input_password: Some(secret("pw")), ..Job::default() };
        assert_eq!(job.tag(), "merged");
        let target = plan(&job, &inputs(&ten, &locked, &from_two)).expect("target");
        assert_eq!(
            words(&target.args),
            r"--progress --empty --pages C:\x\a.pdf 1-z C:\y\b.pdf --password=******** 1-z C:\y\c.pdf 2-z --"
        );
        assert_eq!(target.desired, Path::new(r"C:\x\a (merged).pdf"), "named after the first file");
        assert_eq!((target.input_bytes, target.expected_pages), (2500, None), "b's pages are not known");
        assert!(target.result_password.is_none(), "a merge starts unencrypted");
        let target = plan(&job, &inputs(&ten, &three, &from_two)).expect("target");
        assert_eq!(target.expected_pages, Some(10 + 3 + 9));
        assert!(!words(&target.args).contains("--password"));
        // The pages typed: of every file without its own (they were ignored).
        let first_two = Job { pages: merge_step(Files::Merge, Select::Keep(range("1-2"))), ..Job::default() };
        let target = plan(&first_two, &inputs(&ten, &three, &from_two)).expect("target");
        assert_eq!(words(&target.args), r"--progress --empty --pages C:\x\a.pdf 1-2 C:\y\b.pdf 1-2 C:\y\c.pdf 2-z --");
        assert_eq!(target.expected_pages, Some(2 + 2 + 9));
        // Deleting and reversing go with merging too.
        let backwards = Job { pages: merge_step(Files::Merge, Select::Reverse), ..Job::default() };
        let target = plan(&backwards, &inputs(&ten, &three, &from_two)).expect("target");
        assert_eq!(words(&target.args), r"--progress --empty --pages C:\x\a.pdf z-1 C:\y\b.pdf z-1 C:\y\c.pdf 2-z --");
        let without_first = Job { pages: merge_step(Files::Merge, Select::Delete(range("1").complement().expect("ok"))), ..Job::default() };
        let target = plan(&without_first, &inputs(&ten, &three, &from_two)).expect("target");
        assert_eq!(words(&target.args), r"--progress --empty --pages C:\x\a.pdf 1-z,x1 C:\y\b.pdf 1-z,x1 C:\y\c.pdf 2-z --");
        assert_eq!(target.expected_pages, Some(9 + 2 + 9));

        let collate = Job { pages: merge_step(Files::Collate, Select::All), ..Job::default() };
        assert_eq!(collate.tag(), "mixed");
        let optimized = Job { pages: merge_step(Files::Collate, Select::All), optimize: Some(Optimize::default()), ..Job::default() };
        assert_eq!(optimized.tag(), "mixed", "with other steps too");
        assert!(collate.describe().ends_with("--progress --empty --collate --pages <input> 1-z --"));
        // One file cannot be used: the others are left alone, untouched.
        let verdicts: Vec<Verdict> = match plan(&collate, &inputs(&ten, &locked, &from_two)) {
            Ok(_) => panic!("planned"),
            Err(produced) => produced.into_iter().map(|p| p.verdict).collect(),
        };
        let not_merged = Verdict::Failed(Failure::NotMerged);
        assert_eq!(verdicts, [not_merged.clone(), Verdict::PasswordRequired, not_merged]);
    }

    #[test]
    fn a_files_own_pages_win_whatever_the_pages_chosen() {
        let own = range("3,1");
        let with_own = |probe| Input { range: Some(&own), ..input(r"C:\Docs\a.pdf", probe) };
        let tail = |select| {
            let job = Job { pages: pages_step(select, None), ..Job::default() };
            let (own, plain) = (plan(&job, &[with_own(&READY)]).expect("target"), target(&job, &READY));
            (words(&own.args).rsplit(".pdf").next().map(str::to_string), words(&plain.args).rsplit(".pdf").next().map(str::to_string))
        };
        let pages = |text: &str| Some(text.to_string());
        let own_pages = pages(" --pages . 3,1 --");
        // Its own pages in every mode; the others get what is chosen.
        assert_eq!(tail(Select::Keep(range("1-5"))), (own_pages.clone(), pages(" --pages . 1-5 --")));
        assert_eq!(tail(Select::Delete(range("2").complement().expect("ok"))), (own_pages.clone(), pages(" --pages . 1-z,x2 --")));
        assert_eq!(tail(Select::Reverse), (own_pages.clone(), pages(" --pages . z-1 --")));
        assert_eq!(tail(Select::All), (own_pages, pages("")), "every page: nothing to select");
        assert_eq!(plan(&Job { pages: pages_step(Select::All, None), ..Job::default() }, &[with_own(&READY)]).expect("t").expected_pages, Some(2));
        // Without the Pages step they count for nothing.
        let repair = Job::default();
        assert!(!words(&plan(&repair, &[with_own(&READY)]).expect("target").args).contains("--pages"));
        let reverse = Job { pages: pages_step(Select::Reverse, None), ..Job::default() };
        assert!(reverse.work_key().own_pages() && !repair.work_key().own_pages());
    }

    #[test]
    fn attachments_are_added_and_removed_with_their_rules() {
        let attach = |add: &[&str], remove_all| Job {
            attachments: Some(Attachments { add: add.iter().map(PathBuf::from).collect(), remove_all }),
            ..Job::default()
        };
        assert_eq!(attach(&[r"C:\a\notes.txt"], false).tag(), "attachments");
        let error = |job: Job| job.check().map_err(|e| e.sentence());
        assert_eq!(error(attach(&[], false)), Err("Attachments: choose files to attach, or Remove all".into()));
        assert_eq!(error(attach(&[r"C:\a\x.txt", r"D:\b\X.TXT"], false)), Err("Attachments: two files are named “X.TXT”".into()));
        assert_eq!(error(attach(&[], true)), Ok(()));
        // Removing needs the file's keys, listed before QPDF runs.
        let remove = attach(&[r"C:\a\new.txt"], true);
        assert!(remove.lists_attachments());
        let keys = [String::from("old 1"), String::from("new.txt")];
        let target = plan(&remove, &[Input { attachments: Some(&keys), ..input(r"C:\Docs\a.pdf", &READY) }]).expect("target");
        assert_eq!(
            words(&target.args),
            r"--add-attachment C:\a\new.txt -- --progress --remove-attachment=old 1 --remove-attachment=new.txt C:\Docs\a.pdf"
        );
        assert_eq!(
            refused(&remove, &READY),
            Verdict::Failed(Failure::Other(t!(FailureAttachmentsUnlisted).into())),
            "never run without knowing them"
        );
        // A merge starts from an empty document: nothing to remove.
        let merging = Job { pages: merge_step(Files::Merge, Select::All), ..attach(&[], true) };
        assert!(!merging.lists_attachments());
        assert!(attach(&[], true).work_key() != attach(&[], false).work_key());
    }

    #[test]
    fn overlays_and_split_results_have_their_arguments_and_names() {
        let under = Job { overlay: Some(Overlay { under: true, ..overlay(READY, Placement::InTurn).expect("o") }), ..Job::default() };
        assert_eq!(under.tag(), "underlaid");
        assert_eq!(under.describe(), r"--underlay C:\stamp.pdf --repeat=1-z -- --progress <input>");
        assert_eq!(Job { overlay: overlay(READY, Placement::InOrder), ..Job::default() }.tag(), "overlaid");

        let split = Job { output: Some(Output { split: Some(5), linearize: false, ..Output::default() }), ..Job::default() };
        assert_eq!(split.tag(), "split");
        let target = target(&split, &READY);
        assert_eq!(target.desired, Path::new(r"C:\Docs\a (split)"), "a folder");
        assert_eq!(target.split_parts.as_deref(), Some("a"));
        assert!(words(&target.args).contains("--split-pages=5"));
        let both = Job { output: Some(Output { split: Some(5), linearize: true, ..Output::default() }), ..Job::default() };
        assert_eq!(both.tag(), "split");
        assert_eq!(Job { output: Some(Output { split: None, linearize: true, ..Output::default() }), ..Job::default() }.tag(), "linearized");
    }

    #[test]
    fn advanced_options_add_their_arguments_only_when_set() {
        let forms = Forms { action: FormAction::RemoveForm, flatten_rotation: true };
        let privacy = Privacy { remove_structure: true, remove_page_labels: true, ..Privacy::default() };
        let job = Job { forms: Some(forms), privacy: Some(privacy), ..Job::default() };
        assert_eq!(
            job.describe(),
            "--generate-appearances --remove-acroform --flatten-rotation --remove-info --remove-metadata \
             --deterministic-id --remove-structure --remove-page-labels --progress <input>"
        );
        let advanced = Optimize {
            images: Images::JpegQuality(50),
            object_streams: ObjectStreams::Disable,
            keep_inline_images: true,
            min_image: (MIN_IMAGE.0, 300),
            ..Optimize::default()
        };
        let describe = |optimize| Job { optimize: Some(optimize), ..Job::default() }.describe();
        assert_eq!(
            describe(advanced),
            "--compress-streams=y --recompress-flate --object-streams=disable --remove-unreferenced-resources=yes \
             --optimize-images --jpeg-quality=50 --keep-inline-images --oi-min-height=300 --oi-min-area=0 --progress <input>"
        );
        // Smaller without a quality: the JPEGs there are stay as they are.
        assert!(describe(Optimize { images: Images::Jpeg, ..advanced }).contains("--optimize-images --keep-inline-images"));
        // The image options belong to Smaller: Lossless re-encodes no image.
        assert!(!describe(Optimize { images: Images::Lossless, ..advanced }).contains("inline"));
        // Advanced alone is enough for Privacy.
        let only_labels = Privacy { remove_info: false, remove_xmp: false, fixed_id: false, remove_structure: false, remove_page_labels: true };
        assert_eq!(Job { privacy: Some(only_labels), ..Job::default() }.check(), Ok(()));
    }

    fn placed(folder: Option<&str>, name: Option<&str>) -> Option<Output> {
        Some(Output { folder: folder.map(PathBuf::from), name: name.map(String::from), ..Output::default() })
    }

    #[test]
    fn the_result_goes_to_the_chosen_folder_under_the_typed_name() {
        let job = Job { output: placed(Some(r"D:\Results"), Some("small")), ..Job::optimize_lossless() };
        assert_eq!(job.check(), Ok(()));
        assert_eq!(job.steps(), [Step::Optimize], "a place is not a step");
        assert_eq!(job.tag(), "optimized");
        assert_eq!(target(&job, &READY).desired, Path::new(r"D:\Results\a (small).pdf"));
        assert!(target(&job, &READY).keep_if_not_smaller.is_some(), "still Optimize alone");
        // Only placed: the file is repaired, into the folder.
        let repaired = Job { output: placed(Some(r"D:\Results"), None), ..Job::default() };
        assert_eq!(target(&repaired, &READY).desired, Path::new(r"D:\Results\a (repaired).pdf"));
        // A merge takes the typed name whole, with or without `.pdf`.
        let merge = |name| Job { pages: merge_step(Files::Merge, Select::All), output: placed(None, Some(name)), ..Job::default() };
        for name in ["Umowa komplet", "Umowa komplet.PDF"] {
            assert_eq!(target(&merge(name), &READY).desired, Path::new(r"C:\Docs\Umowa komplet.pdf"), "{name}");
        }
        // Split: a folder of the name, the parts after the input — or after
        // the merge's name.
        let split = |job: Job| Job { output: Some(Output { split: Some(5), ..job.output.clone().expect("output") }), ..job };
        let per_file = target(&split(Job { output: placed(None, Some("parts")), ..Job::default() }), &READY);
        assert_eq!((per_file.desired.as_path(), per_file.split_parts.as_deref()), (Path::new(r"C:\Docs\a (parts)"), Some("a")));
        let merged = target(&split(merge("Komplet")), &READY);
        assert_eq!((merged.desired.as_path(), merged.split_parts.as_deref()), (Path::new(r"C:\Docs\Komplet"), Some("Komplet")));
        // Another place or name is other work.
        assert!(job.work_key() != Job::optimize_lossless().work_key());
        assert!(job.work_key() != Job { output: placed(Some(r"D:\Other"), Some("small")), ..Job::optimize_lossless() }.work_key());
    }

    #[test]
    fn a_place_or_name_that_cannot_be_used_is_named() {
        let error = |output| Job { output, ..Job::default() }.check().map_err(|e| e.sentence());
        // Output as it starts: next to the original, the step's name ending.
        assert_eq!(error(placed(None, None)), Ok(()));
        assert_eq!(error(placed(Some("Results"), None)), Err(r"Output: the folder must be a full path, e.g. D:\Results".into()));
        assert!(error(placed(None, Some(r"..\up"))).is_err_and(|e| e.starts_with("Output: name: it cannot hold")));
        assert_eq!(error(placed(None, Some("CON"))), Err("Output: name: Windows keeps that name for a device".into()));
    }

    #[test]
    fn the_command_is_qpdf_with_the_same_arguments_quoted_for_cmd() {
        let dir = std::env::temp_dir().join(format!("qd_command_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let (file, exe) = (dir.join("my scan.pdf"), Path::new(r"C:\Program Files\qpdf\bin\qpdf.exe"));
        let file = file.to_str().expect("utf-8").to_string();
        let one = |job: &Job, probe: &Probe| job.command(exe, &[input(&file, probe)]);
        let job = Job { input_password: Some(secret("in")), ..Job::optimize_lossless() };
        let line = one(&job, &Probe::NeedsPassword).expect("a command");
        let result = dir.join("my scan (optimized).pdf");
        assert_eq!(
            line,
            format!(
                "\"{}\" --password=******** --compress-streams=y --recompress-flate --object-streams=generate \
                 --remove-unreferenced-resources=yes --progress \"{file}\" \"{}\"",
                exe.display(),
                result.display()
            )
        );
        // The name Start would take now: never over an existing file.
        std::fs::write(&result, b"older").expect("write");
        assert!(one(&job, &READY).expect("a command").ends_with("my scan (optimized) (2).pdf\""));
        // Split: the folder first.
        let split = Job { output: Some(Output { split: Some(2), ..Output::default() }), ..Job::default() };
        let line = one(&split, &READY).expect("a command");
        let folder = dir.join("my scan (split)");
        assert!(line.starts_with(&format!("mkdir \"{}\" && \"", folder.display())), "{line}");
        assert!(line.ends_with(&format!("\"{}\"", folder.join("my scan.pdf").display())), "{line}");
        // A file that would not run says why.
        assert_eq!(one(&Job::optimize_lossless(), &Probe::NeedsPassword), Err("my scan.pdf: Password required".into()));
        let merge = Job { pages: merge_step(Files::Merge, Select::All), ..Job::default() };
        let locked = Probe::NeedsPassword;
        let refused = merge.command(exe, &[input(r"C:\a.pdf", &READY), input(r"C:\b.pdf", &locked)]);
        assert_eq!(refused, Err("b.pdf: Password required".into()), "the file that stops the merge");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
