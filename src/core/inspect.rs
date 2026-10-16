//! The read-only tools: what a PDF holds and whether it is
//! sound, from QPDF's own reports.
//!
//! The details come from QPDF's JSON (`--json=2`), read with `serde_json` into
//! a `Value` — no `derive`. A missing key or an unexpected type
//! means "not known", never a panic, and the JSON is limited in size before it
//! is parsed ([`MAX_JSON`]). Only the keys needed are asked for:
//! the whole JSON of a 10 000-page file is megabytes.
//!
//! The Info takes three short runs (measured on 12.4.2):
//! 1. `--json-key=encrypt|attachments|pagelabels` with the trailer
//!    (`--json-key=qpdf --json-object=trailer`): PDF version, encryption,
//!    attachments, page labels, and where the catalog and the document info
//!    are;
//! 2. those two objects (`--json-object=N,G`): form, bookmarks, metadata;
//! 3. `--show-npages --show-linearization`: the pages, and whether the file
//!    is linearized.
//!
//! A file is read without a password first; only when QPDF asks for one is the
//! password for encrypted inputs tried — a file encrypted without an open
//! password refuses a wrong one (measured).
//!
//! Text from the PDF (metadata, attachment names, label prefixes) is passed
//! through [`super::printable`] and cut to [`MAX_TEXT`] characters.

use crate::text::{self, Msg};
use crate::{t, tn};
use std::path::Path;

use serde_json::{Map, Value};

use super::filelist::Probe;
use super::outcome::{self, Failure, Verdict};
use super::runner::{self, Arg, Context, Ending};
use super::secret::Secret;

/// Largest JSON report read: far beyond what the keys asked for give for a
/// real file (thousands of attachments), small enough to hold in memory.
pub const MAX_JSON: u64 = 32 << 20;

/// Longest text from a PDF shown, in characters.
pub const MAX_TEXT: usize = 300;

/// What the Info window shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub pdf_version: Option<String>,
    pub pages: Option<u64>,
    pub linearized: Option<bool>,
    /// `None`: not encrypted.
    pub encryption: Option<Encryption>,
    /// An interactive form (`/AcroForm`).
    pub form: bool,
    pub bookmarks: bool,
    /// The page label ranges, by first page.
    pub labels: Vec<LabelRange>,
    pub attachments: Vec<Attachment>,
    /// The document info (Title, Author…) as `(key, text)`.
    pub metadata: Vec<(String, String)>,
    /// The file opened only with the password for encrypted inputs.
    pub needed_password: bool,
}

impl Info {
    /// The password for encrypted inputs, for further runs on this file
    /// (extracting): [`Probe::password`]'s rule, with what reading the file
    /// found out — only when reading it needed one.
    pub fn password_for(&self, password: Option<Secret>) -> Option<Secret> {
        password.filter(|_| self.needed_password)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encryption {
    /// E.g. `AES-256`.
    pub method: String,
    /// What the restrictions forbid, in words (`printing`, `copying`…).
    pub not_allowed: Vec<&'static str>,
}

/// One embedded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// QPDF's key for it (`--show-attachment=key`), as in the PDF.
    pub key: String,
    /// Its preferred file name, as in the PDF — make it safe before use
    /// ([`super::sanitize::file_name`]).
    pub name: String,
    pub description: Option<String>,
}

/// Page labels from page `first` on (counted from 0): `prefix` and a number in
/// `style` (`D` 1 2 3, `R` I II, `r` i ii, `A` A B, `a` a b; none = prefix
/// only), counting from `start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    pub first: u64,
    pub style: Option<char>,
    pub prefix: String,
    pub start: u64,
}

/// The label of page `index` (from 0), when the file has labels.
pub fn label(ranges: &[LabelRange], index: u64) -> Option<String> {
    // The ranges are sorted by first page: a search, not a scan per page.
    let range = ranges[..ranges.partition_point(|range| range.first <= index)].last()?;
    let n = range.start.saturating_add(index - range.first);
    let number = match range.style {
        Some('D') => n.to_string(),
        Some('R') => roman(n),
        Some('r') => roman(n).to_lowercase(),
        Some('A') => letters(n),
        Some('a') => letters(n).to_lowercase(),
        _ => String::new(),
    };
    Some(format!("{}{number}", range.prefix))
}

/// Above this, roman numerals and letters turn into digits — a label start
/// from a hostile file must not become a megabyte of `M`s.
const MAX_WORDY: u64 = 10_000;

fn roman(n: u64) -> String {
    if n == 0 || n > MAX_WORDY {
        return n.to_string();
    }
    const DIGITS: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let (mut rest, mut out) = (n, String::new());
    for (value, digit) in DIGITS {
        while rest >= value {
            out.push_str(digit);
            rest -= value;
        }
    }
    out
}

/// `A`…`Z`, then `AA`…`ZZ`, `AAA`… (the PDF way: one letter repeated).
fn letters(n: u64) -> String {
    if n == 0 || n > MAX_WORDY {
        return n.to_string();
    }
    let letter = char::from(b'A' + ((n - 1) % 26) as u8);
    std::iter::repeat_n(letter, ((n - 1) / 26 + 1) as usize).collect()
}

/// Text from a PDF for the window: printable, at most [`MAX_TEXT`] characters.
fn shown(text: &str) -> String {
    let clean = super::printable(text);
    match clean.char_indices().nth(MAX_TEXT) {
        Some((cut, _)) => format!("{}…", &clean[..cut]),
        None => clean,
    }
}

/// A PDF value in QPDF's JSON as text: strings are `u:text` (or `b:hex` for
/// binary ones), names `/Name`; `None` for a reference or anything else.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => {
            if let Some(text) = s.strip_prefix("u:") {
                Some(text.to_string())
            } else if s.starts_with("b:") {
                Some(t!(EncryptionBinary).into())
            } else {
                s.strip_prefix('/').map(String::from)
            }
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `2603 0 R` → `2603,0`, the form `--json-object` takes; `None` for anything
/// that is not a reference.
fn object_id(reference: &str) -> Option<String> {
    let mut parts = reference.split(' ');
    let (number, generation, r) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |s: &str| !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit());
    (digits(number) && digits(generation) && r == "R" && parts.next().is_none()).then(|| format!("{number},{generation}"))
}

/// What the first run tells: [`Info`] in part, and the catalog and the
/// document info to ask for next (their references, or the info itself).
#[derive(Debug, Default)]
struct Overview {
    info: Info,
    root: Option<String>,
    document_info: Option<Value>,
}

fn parse_overview(json: &Value) -> Overview {
    let mut info = Info { pdf_version: json["qpdf"][0]["pdfversion"].as_str().map(shown), ..Info::default() };
    let encrypt = &json["encrypt"];
    if encrypt["encrypted"].as_bool() == Some(true) {
        info.encryption = Some(Encryption {
            method: method_name(&encrypt["parameters"]),
            not_allowed: not_allowed(&encrypt["capabilities"]),
        });
    }
    if let Some(attachments) = json["attachments"].as_object() {
        info.attachments = attachments
            .iter()
            .map(|(key, entry)| Attachment {
                key: key.clone(),
                name: entry["preferredname"].as_str().unwrap_or(key).to_string(),
                description: entry["description"].as_str().map(|d| shown(d.strip_prefix("u:").unwrap_or(d))),
            })
            .collect();
    }
    if let Some(labels) = json["pagelabels"].as_array() {
        info.labels = labels
            .iter()
            .filter_map(|entry| {
                let label = &entry["label"];
                Some(LabelRange {
                    first: entry["index"].as_u64()?,
                    style: label["/S"].as_str().and_then(|s| s.strip_prefix('/')).and_then(|s| s.chars().next()),
                    prefix: value_text(&label["/P"]).map(|p| shown(&p)).unwrap_or_default(),
                    start: label["/St"].as_u64().unwrap_or(1),
                })
            })
            .collect();
        info.labels.sort_by_key(|range| range.first);
    }
    let trailer = &json["qpdf"][1]["trailer"]["value"];
    Overview {
        info,
        root: trailer["/Root"].as_str().map(String::from),
        document_info: match &trailer["/Info"] {
            Value::Null => None,
            other => Some(other.clone()),
        },
    }
}

/// `AES-256`, `RC4 128-bit`… from the encryption parameters.
fn method_name(parameters: &Value) -> String {
    let bits = parameters["bits"].as_u64().unwrap_or(0);
    match parameters["method"].as_str().unwrap_or("") {
        "AESv3" => "AES-256".into(),
        "AESv2" => "AES-128".into(),
        "RC4" => t!(EncryptionRc4, bits = bits),
        other => shown(other),
    }
}

/// What the restrictions forbid, from QPDF's `capabilities`.
fn not_allowed(capabilities: &Value) -> Vec<&'static str> {
    let no = |key: &str| capabilities[key].as_bool() == Some(false);
    let mut list = Vec::new();
    if no("printlow") {
        list.push(t!(ForbiddenPrinting));
    } else if no("printhigh") {
        list.push(t!(ForbiddenPrintingHigh));
    }
    for (key, what) in [
        ("extract", t!(ForbiddenCopying)),
        ("modifyother", t!(ForbiddenEditing)),
        ("modifyannotations", t!(ForbiddenAnnotating)),
        ("modifyforms", t!(ForbiddenForms)),
        ("modifyassembly", t!(ForbiddenAssembling)),
        ("accessibility", t!(ForbiddenAccessibility)),
    ] {
        if no(key) {
            list.push(what);
        }
    }
    list
}

/// The keys of the document info shown first, in this order, with their
/// names in the language shown.
const METADATA_ORDER: [(&str, Msg); 8] = [
    ("Title", Msg::MetaTitle),
    ("Author", Msg::MetaAuthor),
    ("Subject", Msg::MetaSubject),
    ("Keywords", Msg::MetaKeywords),
    ("Creator", Msg::MetaCreator),
    ("Producer", Msg::MetaProducer),
    ("CreationDate", Msg::MetaCreated),
    ("ModDate", Msg::MetaModified),
];

/// The name of a document info key in the language shown; a key of the
/// file's own as it is.
pub fn metadata_name(key: &str) -> &str {
    METADATA_ORDER.iter().find(|(k, _)| *k == key).map_or(key, |(_, name)| text::get(*name))
}

/// The document info as `(key, text)`: the usual keys first, then the rest by
/// name; empty values, references and anything unreadable left out.
fn metadata(dictionary: &Map<String, Value>) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = dictionary
        .iter()
        .filter_map(|(key, value)| {
            let key = key.strip_prefix('/')?;
            let text = value_text(value)?;
            let text = if key.ends_with("Date") { pdf_date(&text).unwrap_or(text) } else { text };
            let text = shown(text.trim());
            (!text.is_empty()).then(|| (shown(key), text))
        })
        .collect();
    let rank = |key: &str| METADATA_ORDER.iter().position(|(k, _)| *k == key).unwrap_or(METADATA_ORDER.len());
    entries.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    entries
}

/// `D:20260926213726+02'00'` → `2026-09-26 21:37:26 +02:00` (`Z` → `UTC`).
fn pdf_date(text: &str) -> Option<String> {
    let digits = text.strip_prefix("D:").unwrap_or(text);
    let field = |from: usize, len: usize| digits.get(from..from + len).filter(|s| s.bytes().all(|b| b.is_ascii_digit()));
    let year = field(0, 4)?;
    let mut out = year.to_string();
    for (from, sep) in [(4, "-"), (6, "-"), (8, " "), (10, ":"), (12, ":")] {
        match field(from, 2) {
            Some(part) => {
                out.push_str(sep);
                out.push_str(part);
            }
            None => return Some(out),
        }
    }
    match digits.get(14..) {
        Some("Z") | Some("Z00'00'") => out.push_str(" UTC"),
        Some(zone) if zone.starts_with(['+', '-']) => {
            let zone: String = zone.chars().filter(|c| *c != '\'').collect();
            match (zone.get(..3), zone.get(3..5)) {
                (Some(hours), Some(minutes)) => out.push_str(&format!(" {hours}:{minutes}")),
                (Some(hours), None) => out.push_str(&format!(" {hours}:00")),
                _ => {}
            }
        }
        _ => {}
    }
    Some(out)
}

/// Run `args` and read stdout as JSON.
fn json(qpdf: &Path, args: &[Arg], ctx: &Context) -> Result<Value, Verdict> {
    let mut out = Vec::new();
    let (done, copied) = runner::run_into(qpdf, args, ctx, &mut out, MAX_JSON)?;
    if copied.over_limit {
        return Err(Verdict::Failed(Failure::Other(t!(FailureReportTooLarge).into())));
    }
    let verdict = outcome::judge(&done);
    if !verdict.succeeded() {
        return Err(verdict);
    }
    serde_json::from_slice(&out).map_err(|_| Verdict::Failed(Failure::Other(t!(FailureReportUnreadable).into())))
}

/// `run` with the password for encrypted inputs only where it is needed: at
/// once when the probe says so, otherwise first without it and again with it
/// only when the answer `asks` for one — a file encrypted without an open
/// password refuses a wrong one. Returns which was used.
fn with_password<'p, T>(
    password: Option<&'p Secret>,
    probe: &Probe,
    mut run: impl FnMut(Option<&Secret>) -> T,
    asks: impl Fn(&T) -> bool,
) -> (T, Option<&'p Secret>) {
    if let Some(password) = probe.password(password) {
        return (run(Some(password)), Some(password));
    }
    let first = run(None);
    match password {
        Some(password) if asks(&first) => (run(Some(password)), Some(password)),
        _ => (first, None),
    }
}

/// [`with_password`] for a run that fails with [`Verdict::PasswordRequired`].
fn json_with_password<'p>(
    password: Option<&'p Secret>,
    probe: &Probe,
    run: impl FnMut(Option<&Secret>) -> Result<Value, Verdict>,
) -> Result<(Value, Option<&'p Secret>), Verdict> {
    let (json, used) = with_password(password, probe, run, |found| *found == Err(Verdict::PasswordRequired));
    Ok((json?, used))
}

/// Everything the Info window shows about `path` — or why it cannot be
/// read. `probe` is what the probe found, `password` the password for
/// encrypted inputs.
pub fn inspect(qpdf: &Path, path: &Path, probe: &Probe, password: Option<&Secret>, ctx: &Context) -> Result<Info, Verdict> {
    let (overview, password) = json_with_password(password, probe, |password| {
        let keys = ["--json=2", "--json-key=encrypt", "--json-key=attachments", "--json-key=pagelabels", "--json-key=qpdf", "--json-object=trailer"];
        json(qpdf, &Arg::on_file(password, &keys, path), ctx)
    })?;
    let Overview { mut info, root, document_info } = parse_overview(&overview);
    info.needed_password = password.is_some();

    // The catalog and the document info, unless the trailer holds the info
    // itself.
    let wanted: Vec<String> = [root.as_deref(), document_info.as_ref().and_then(Value::as_str)]
        .into_iter()
        .flatten()
        .filter_map(object_id)
        .map(|id| format!("--json-object={id}"))
        .collect();
    let objects = if wanted.is_empty() {
        Value::Null
    } else {
        let mut options = vec!["--json=2", "--json-key=qpdf"];
        options.extend(wanted.iter().map(String::as_str));
        json(qpdf, &Arg::on_file(password, &options, path), ctx)?
    };
    let object = |reference: Option<&str>| reference.map(|r| &objects["qpdf"][1][format!("obj:{r}")]["value"]);
    if let Some(catalog) = object(root.as_deref()).and_then(Value::as_object) {
        info.form = catalog.contains_key("/AcroForm");
        info.bookmarks = catalog.contains_key("/Outlines");
    }
    let dictionary = match &document_info {
        Some(Value::String(reference)) => object(Some(reference.as_str())).and_then(Value::as_object),
        Some(direct) => direct.as_object(),
        None => None,
    };
    if let Some(dictionary) = dictionary {
        info.metadata = metadata(dictionary);
    }

    // Pages and linearization: whole lines of QPDF's own, about the path as
    // given (`<path> is not linearized`, `<path>: linearization data:`).
    let done = runner::run(qpdf, &Arg::on_file(password, &["--show-npages", "--show-linearization"], path), ctx)?;
    if let (true, Ok(text)) = (outcome::judge(&done).succeeded(), runner::check_path(path)) {
        let lines = &done.stdout.lines;
        info.pages = lines.first().and_then(|line| line.trim().parse().ok());
        info.linearized = if lines.iter().any(|line| *line == format!("{text}: linearization data:")) {
            Some(true)
        } else if lines.iter().any(|line| *line == format!("{text} is not linearized")) {
            Some(false)
        } else {
            None
        };
    }
    Ok(info)
}

/// What the page view needs: the page labels and the page count — one
/// short run when the probe knows the count, not the three of [`inspect`]
/// (`--show-linearization` alone prints a line per page of a linearized file).
pub fn page_labels(qpdf: &Path, path: &Path, probe: &Probe, password: Option<&Secret>, ctx: &Context) -> Result<Info, Verdict> {
    let (labels, password) = json_with_password(password, probe, |password| {
        json(qpdf, &Arg::on_file(password, &["--json=2", "--json-key=pagelabels"], path), ctx)
    })?;
    let mut info = parse_overview(&labels).info;
    info.needed_password = password.is_some();
    info.pages = match probe.pages() {
        Some(pages) => Some(pages),
        None => {
            let done = runner::run(qpdf, &Arg::on_file(password, &["--show-npages"], path), ctx)?;
            done.stdout.last_line().and_then(|line| line.trim().parse().ok())
        }
    };
    Ok(info)
}

/// The keys of the file's attachments, for "Remove all attachments".
pub fn attachment_keys(qpdf: &Path, path: &Path, probe: &Probe, password: Option<&Secret>, ctx: &Context) -> Result<Vec<String>, Verdict> {
    let (json, _) = json_with_password(password, probe, |password| {
        json(qpdf, &Arg::on_file(password, &["--json=2", "--json-key=attachments"], path), ctx)
    })?;
    Ok(json["attachments"].as_object().map(|keys| keys.keys().cloned().collect()).unwrap_or_default())
}

/// What `qpdf --check` said about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Done = OK, DoneWithWarnings, PasswordRequired, Failed (damaged, not a
    /// PDF, unreadable…), Cancelled.
    pub verdict: Verdict,
    /// QPDF's lines: what it checked, then its warnings and errors.
    pub details: Vec<String>,
}

impl CheckReport {
    /// The kind of verdict in a word or two — what the counts count (short
    /// enough for the status line).
    pub fn kind(&self) -> Msg {
        match &self.verdict {
            // A check never finds a file "not smaller".
            Verdict::Done | Verdict::NotSmaller => Msg::CheckOk,
            Verdict::DoneWithWarnings => Msg::CheckWarnings,
            Verdict::PasswordRequired => Msg::CheckPassword,
            Verdict::Cancelled => Msg::CheckCancelled,
            Verdict::Failed(Failure::NotPdf) => Msg::CheckNotPdf,
            Verdict::Failed(Failure::Damaged) => Msg::CheckDamaged,
            Verdict::Failed(Failure::AccessDenied | Failure::InUse | Failure::NotFound | Failure::PathTooLong) => Msg::CheckUnreadable,
            Verdict::Failed(_) => Msg::CheckFailed,
        }
    }

    /// The verdict as the Check shows it: its kind, and why when it says more.
    pub fn health(&self) -> String {
        let kind = text::get(self.kind());
        match &self.verdict {
            Verdict::PasswordRequired => t!(VerdictPassword).into(),
            Verdict::Failed(failure) if !matches!(failure, Failure::NotPdf | Failure::Damaged) => t!(CheckHealth, kind = kind, why = failure.sentence()),
            _ => kind.to_string(),
        }
    }
}

/// `qpdf --check` on `path`.
pub fn check(qpdf: &Path, path: &Path, probe: &Probe, password: Option<&Secret>, ctx: &Context) -> CheckReport {
    let run = |password: Option<&Secret>| match runner::run(qpdf, &Arg::on_file(password, &["--check"], path), ctx) {
        Err(e) => CheckReport { verdict: e.into(), details: Vec::new() },
        Ok(done) => {
            let verdict = match outcome::judge(&done) {
                // Errors `--check` found, told in QPDF's own words below.
                Verdict::Failed(Failure::Other(_)) if done.ending == Ending::Exited(2) => Verdict::Failed(Failure::Damaged),
                verdict => verdict,
            };
            let mut details = done.stdout.lines;
            details.extend(done.stderr.lines);
            let omitted = done.stdout.omitted + done.stderr.omitted;
            if omitted > 0 {
                details.push(tn!(CheckOmitted, omitted));
            }
            CheckReport { verdict, details }
        }
    };
    with_password(password, probe, run, |report| report.verdict == Verdict::PasswordRequired).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    /// Real QPDF 12.4.2 output for the corpus file with hostile attachment
    /// names (shortened).
    const OVERVIEW: &str = r#"{
      "version": 2,
      "pagelabels": [
        { "index": 2, "label": { "/S": "/D", "/St": 1 } },
        { "index": 0, "label": { "/S": "/r", "/St": 1, "/P": "u:p-" } }
      ],
      "qpdf": [
        { "jsonversion": 2, "pdfversion": "1.7", "maxobjectid": 14 },
        { "trailer": { "value": { "/Info": "2603 0 R", "/Root": "1 0 R", "/Size": 15 } } }
      ],
      "attachments": {
        "a00": { "description": null, "filespec": "16 0 R", "preferredname": "..\\..\\outside.txt" },
        "a07": { "description": "u:a note", "preferredname": "zażółć.txt" },
        "k": { "preferredname": 5 }
      },
      "encrypt": {
        "capabilities": { "extract": false, "printhigh": false, "printlow": true, "modify": true },
        "encrypted": true,
        "parameters": { "P": -2056, "R": 6, "V": 5, "bits": 256, "method": "AESv3" }
      }
    }"#;

    #[test]
    fn the_overview_is_read_from_qpdfs_json() {
        let Overview { info, root, document_info } = parse_overview(&parse(OVERVIEW));
        assert_eq!(info.pdf_version.as_deref(), Some("1.7"));
        assert_eq!(root.as_deref(), Some("1 0 R"));
        assert_eq!(document_info, Some(Value::String("2603 0 R".into())));
        assert_eq!(
            info.encryption,
            Some(Encryption { method: "AES-256".into(), not_allowed: vec!["printing in high quality", "copying"] })
        );
        let names: Vec<(&str, &str)> = info.attachments.iter().map(|a| (a.key.as_str(), a.name.as_str())).collect();
        assert_eq!(names, [("a00", r"..\..\outside.txt"), ("a07", "zażółć.txt"), ("k", "k")], "a name that is not text: the key");
        assert_eq!(info.attachments[1].description.as_deref(), Some("a note"));
        assert_eq!(info.labels.iter().map(|l| l.first).collect::<Vec<_>>(), [0, 2], "sorted by page");
        assert_eq!(info.labels[0].prefix, "p-");
    }

    #[test]
    fn missing_or_hostile_json_means_not_known() {
        for text in ["{}", "[]", "null", r#"{"qpdf": 5, "encrypt": [], "attachments": "x", "pagelabels": {}}"#] {
            let overview = parse_overview(&parse(text));
            assert_eq!(overview.info, Info::default(), "{text}");
            assert!(overview.root.is_none());
        }
        // Deep nesting is stopped by serde_json's limit, not by the stack.
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(serde_json::from_str::<Value>(&deep).is_err());
        let unencrypted = parse(r#"{"encrypt": {"encrypted": false, "parameters": {"method": "none"}}}"#);
        assert_eq!(parse_overview(&unencrypted).info.encryption, None);
    }

    #[test]
    fn page_labels_follow_the_pdf_rules() {
        let ranges = [
            LabelRange { first: 0, style: Some('r'), prefix: String::new(), start: 1 },
            LabelRange { first: 4, style: Some('D'), prefix: String::new(), start: 1 },
            LabelRange { first: 7, style: Some('A'), prefix: "App-".into(), start: 26 },
            LabelRange { first: 9, style: None, prefix: "Cover".into(), start: 1 },
        ];
        let labels: Vec<String> = (0..10).map(|i| label(&ranges, i).expect("labelled")).collect();
        assert_eq!(labels, ["i", "ii", "iii", "iv", "1", "2", "3", "App-Z", "App-AA", "Cover"]);
        assert_eq!(label(&[], 3), None, "no labels: the number speaks");
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(letters(53), "AAA");
        // A hostile start does not explode into text.
        let huge = [LabelRange { first: 0, style: Some('R'), prefix: String::new(), start: u64::MAX - 1 }];
        assert_eq!(label(&huge, 5), Some(u64::MAX.to_string()));
    }

    #[test]
    fn metadata_keys_get_their_names() {
        assert_eq!(metadata_name("CreationDate"), "Created");
        assert_eq!(metadata_name("Title"), "Title");
        assert_eq!(metadata_name("PTEX.Fullbanner"), "PTEX.Fullbanner", "a key of the file's own");
    }

    #[test]
    fn metadata_reads_as_text_with_dates() {
        let info = parse(
            r#"{ "/Author": "u:Jay", "/CreationDate": "u:D:20260926213726Z", "/ModDate": "u:D:20261001120000+02'00'",
                 "/Keywords": "u:", "/Title": "u:qpdf\u0007 manual", "/Trapped": "/False", "/Custom": "b:00ff",
                 "/Pages": "3 0 R", "/Count": 7 }"#,
        );
        let entries = metadata(info.as_object().expect("dictionary"));
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["Title", "Author", "CreationDate", "ModDate", "Count", "Custom", "Trapped"]);
        assert_eq!(entries[0].1, "qpdf manual", "control characters dropped");
        assert_eq!(entries[2].1, "2026-09-26 21:37:26 UTC");
        assert_eq!(entries[3].1, "2026-10-01 12:00:00 +02:00");
        assert_eq!(entries[5].1, "(binary)");
        assert_eq!(pdf_date("D:2026"), Some("2026".into()));
        assert_eq!(pdf_date("yesterday"), None);
        let long = parse(&format!(r#"{{"/Title": "u:{}"}}"#, "x".repeat(5000)));
        assert_eq!(metadata(long.as_object().expect("dictionary"))[0].1.chars().count(), MAX_TEXT + 1);
    }

    #[test]
    fn only_real_references_become_object_ids() {
        assert_eq!(object_id("2603 0 R").as_deref(), Some("2603,0"));
        for bad in ["2603 0", "a 0 R", "1 0 R x", "", "-1 0 R", "1  0 R", "99999999999 0 R"] {
            assert_eq!(object_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn check_verdicts_read_as_in_f11() {
        let report = |verdict| CheckReport { verdict, details: Vec::new() }.health();
        assert_eq!(report(Verdict::Done), "OK");
        assert_eq!(report(Verdict::DoneWithWarnings), "Warnings");
        assert_eq!(report(Verdict::Failed(Failure::Damaged)), "Damaged");
        assert_eq!(report(Verdict::Failed(Failure::NotPdf)), "Not a PDF");
        assert_eq!(report(Verdict::Failed(Failure::InUse)), "Unreadable: File is in use by another program");
        assert_eq!(report(Verdict::Failed(Failure::Hung)), "Failed: QPDF stopped responding");
    }

    /// A random JSON value shaped like QPDF's: its keys, its `u:`/`b:`/`/`
    /// strings, numbers of every size, nested up to `depth`.
    fn random_json(rng: &mut crate::fuzz::Rng, depth: u32) -> Value {
        const KEYS: [&str; 26] = [
            "version", "qpdf", "pdfversion", "trailer", "value", "/Root", "/Info", "encrypt", "encrypted", "parameters",
            "method", "bits", "capabilities", "printlow", "extract", "attachments", "preferredname", "description",
            "pagelabels", "index", "label", "/S", "/P", "/St", "/Title", "/CreationDate",
        ];
        const STRINGS: [&str; 12] =
            ["u:", "b:00ff", "/D", "/R", "/a", "AESv3", "RC4", "1 0 R", "D:20260926213726+02'00'", "D:2026+\u{17c}", "u:\u{202E}x\n", "obj:1 0 R"];
        let alphabet = crate::fuzz::chars("aZ09:/'+-Dz ");
        match rng.below(if depth == 0 { 4 } else { 6 }) {
            0 => Value::Null,
            1 => Value::Bool(rng.chance(2)),
            2 => match rng.below(3) {
                0 => Value::from(rng.next()),
                1 => Value::from(-(rng.below(1000) as i64)),
                _ => Value::from(rng.below(1000) as f64 / 7.0),
            },
            3 => Value::String(if rng.chance(2) { rng.pick(&STRINGS).to_string() } else { format!("{}{}", rng.pick(&STRINGS), rng.text(&alphabet, 400)) }),
            4 => Value::Array((0..rng.below(5)).map(|_| random_json(rng, depth - 1)).collect()),
            _ => Value::Object(
                (0..rng.below(6)).map(|_| (rng.pick(&KEYS).to_string(), random_json(rng, depth - 1))).collect(),
            ),
        }
    }

    #[test]
    fn random_json_from_qpdf_never_panics_and_shows_only_clean_text() {
        let mut rng = crate::fuzz::Rng::new(0x5EED_0006);
        let clean = |text: &str| crate::core::printable(text) == text && text.chars().count() <= MAX_TEXT + 1;
        for _ in 0..20_000 {
            // The shape QPDF gives, with random parts inside it.
            let mut json = serde_json::json!({ "qpdf": [{ "pdfversion": random_json(&mut rng, 1) }, { "trailer": { "value": random_json(&mut rng, 2) } }] });
            for key in ["encrypt", "attachments", "pagelabels"] {
                json[key] = if rng.chance(2) { random_json(&mut rng, 3) } else { Value::Array((0..3).map(|_| random_json(&mut rng, 3)).collect()) };
            }
            let overview = parse_overview(&json);
            let info = &overview.info;
            assert!(info.pdf_version.as_deref().is_none_or(clean));
            assert!(info.encryption.as_ref().is_none_or(|e| clean(&e.method)));
            assert!(info.attachments.iter().all(|a| a.description.as_deref().is_none_or(clean)));
            for index in [0, 1, rng.below(20_000), rng.next(), u64::MAX] {
                if let Some(text) = label(&info.labels, index) {
                    assert!(!text.chars().any(char::is_control) && text.len() < 2_000, "{text:?}");
                }
            }
            if let Some(dictionary) = random_json(&mut rng, 2).as_object() {
                assert!(metadata(dictionary).iter().all(|(key, text)| clean(key) && clean(text)));
            }
            let _ = overview.root.as_deref().map(object_id);
        }
    }
}
