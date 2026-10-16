//! Accepted file type and folder traversal.
//!
//! A file joins the list only if its extension is `.pdf` (any letter case).
//! This is a *filter*, not a verdict: whether the file really is a sound PDF is
//! decided later by QPDF. Checking an extension costs nothing, while probing
//! every file in a folder would mean one process launch per file (~64 ms each,
//! measured).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// Whether `path` has the `.pdf` extension, in any letter case.
pub fn is_pdf_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// The first `limit` PDF files in natural order ([`natural_path_cmp`]) of
/// however many are found — each with its size — and how many came after
/// them. Never more than twice `limit` are held at once: a whole drive
/// dropped by mistake must not fill the memory with its paths.
pub struct FirstPdfs {
    pdfs: Vec<(PathBuf, u64)>,
    beyond: usize,
    limit: usize,
}

impl FirstPdfs {
    pub fn new(limit: usize) -> FirstPdfs {
        FirstPdfs { pdfs: Vec::new(), beyond: 0, limit }
    }

    pub fn push(&mut self, pdf: (PathBuf, u64)) {
        self.pdfs.push(pdf);
        if self.pdfs.len() >= self.limit.saturating_mul(2).max(1) {
            self.cut();
        }
    }

    /// Keep the first `limit`, in no particular order, counting the rest.
    fn cut(&mut self) {
        if self.pdfs.len() > self.limit {
            self.pdfs.select_nth_unstable_by(self.limit, |a, b| natural_path_cmp(&a.0, &b.0));
            self.beyond = self.beyond.saturating_add(self.pdfs.len() - self.limit);
            self.pdfs.truncate(self.limit);
        }
    }

    /// The files kept, in natural order, and how many came after them.
    pub fn sorted(mut self) -> (Vec<(PathBuf, u64)>, usize) {
        self.cut();
        sort_naturally(&mut self.pdfs, |(path, _)| path);
        (self.pdfs, self.beyond)
    }
}

/// Walk `root` and its subfolders, putting the PDF files into `pdfs`; returns
/// how many other files were skipped. Stops as soon as `stop` is set (the
/// user cancelled the add). Unreadable directories are silently ignored — a
/// permission error must never abort the whole add.
///
/// The file system returns names in its own order — on NTFS `Skan1, skan10,
/// skan2` (measured) — so [`FirstPdfs`] keeps the batch in natural order.
///
/// Depth-first with its own stack rather than recursion: the UI thread that
/// runs it has a small stack, and a folder tree can be deeper than any sane
/// recursion budget. Links (symlinks, junctions) are never entered —
/// `DirEntry::file_type` does not follow them — so a looping junction cannot
/// trap the walk.
pub fn scan_folder(root: &Path, pdfs: &mut FirstPdfs, stop: &AtomicBool) -> usize {
    let mut skipped = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if stop.load(AtomicOrdering::Relaxed) {
                return skipped;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                if is_pdf_file(&path) {
                    // `DirEntry::metadata` is served from the directory listing
                    // the walk already read; `fs::metadata` would reopen every file.
                    let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                    pdfs.push((path, size));
                } else {
                    skipped += 1;
                }
            }
        }
    }
    skipped
}

/// Order paths the way a person reads a file list: folder by folder, letters
/// without regard to case, and numbers by value — `scan 2` before `scan 10`.
///
/// Paths are compared component by component, so a folder's files stay
/// together (`Invoices\b.pdf` before `Invoices 2\a.pdf`); a plain string compare
/// would put the space of `Invoices 2` before the backslash and interleave them.
pub fn natural_path_cmp(a: &Path, b: &Path) -> Ordering {
    // Equal apart from case: stay deterministic.
    natural_parts_cmp(parts(a), parts(b)).then_with(|| a.cmp(b))
}

/// Sort `items` by their `path`, in [`natural_path_cmp`] order, splitting
/// each path once: 10 000 paths split anew for every comparison took up to
/// 49 ms on the UI thread (measured); once, about half.
pub fn sort_naturally<T>(items: &mut Vec<T>, path: impl Fn(&T) -> &Path) {
    let order = natural_order(&items.iter().map(path).collect::<Vec<_>>());
    let mut slots: Vec<Option<T>> = std::mem::take(items).into_iter().map(Some).collect();
    *items = order.into_iter().filter_map(|index| slots[index].take()).collect();
}

/// The indices of `paths` in natural order — not generic, so the sort is
/// compiled once for every kind of item (a copy per kind cost 4.5 KB of EXE, measured).
fn natural_order(paths: &[&Path]) -> Vec<usize> {
    let split: Vec<Vec<String>> = paths.iter().map(|path| parts(path).map(String::from).collect()).collect();
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by(|&a, &b| natural_parts_cmp(split[a].iter(), split[b].iter()).then_with(|| paths[a].cmp(paths[b])));
    order
}

/// The text of each component of `path`.
fn parts(path: &Path) -> impl Iterator<Item = std::borrow::Cow<'_, str>> {
    path.components().map(|part| part.as_os_str().to_string_lossy())
}

/// [`natural_path_cmp`] of two paths' [`parts`], without its last resort.
fn natural_parts_cmp<S: AsRef<str>>(mut a: impl Iterator<Item = S>, mut b: impl Iterator<Item = S>) -> Ordering {
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            // Folders the two paths share are byte-identical: skip them before
            // any case-folding. (A sort compares mostly siblings.)
            (Some(x), Some(y)) if x.as_ref() == y.as_ref() => {}
            (Some(x), Some(y)) => {
                let order = natural_cmp(x.as_ref(), y.as_ref());
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Case-insensitive compare in which runs of digits compare by numeric value.
///
/// Allocation-free — a sort of 20,000 paths runs it ~300,000 times, on the UI
/// thread: digit runs are slices of the input, and ASCII letters (nearly all
/// of them in file names) are folded without the general Unicode path.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_from(a, b, shared_start(a, b))
}

/// Bytes at the start of `a` and `b` that compare equal as they are — names
/// in one folder often share a long start, and folding its case character
/// by character was most of a 10 000-file sort (measured) — but never into
/// a digit run, which counts by its whole value, nor into a character.
fn shared_start(a: &str, b: &str) -> usize {
    let mut start = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    while start > 0 && (!a.is_char_boundary(start) || a.as_bytes()[start - 1].is_ascii_digit()) {
        start -= 1;
    }
    start
}

/// [`natural_cmp`] of `a` and `b` after their first `start` bytes, the same
/// in both.
fn natural_cmp_from(a: &str, b: &str, start: usize) -> Ordering {
    let (mut i, mut j) = (start, start);
    loop {
        let (Some(x), Some(y)) = (a[i..].chars().next(), b[j..].chars().next()) else {
            // One side ran out: the shorter remainder sorts first.
            return (a.len() - i).cmp(&(b.len() - j));
        };
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let (da, db) = (digit_run(&a[i..]), digit_run(&b[j..]));
            i += da.len();
            j += db.len();
            // By value without parsing (no overflow on 40-digit names): fewer
            // significant digits is smaller; then digit by digit; then the one
            // with fewer leading zeros first, so `7` and `007` still differ.
            let (sa, sb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
            sa.len().cmp(&sb.len()).then_with(|| sa.cmp(sb)).then_with(|| da.len().cmp(&db.len()))
        } else {
            i += x.len_utf8();
            j += y.len_utf8();
            if x.is_ascii() && y.is_ascii() {
                x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase())
            } else {
                x.to_lowercase().cmp(y.to_lowercase())
            }
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// The run of ASCII digits at the start of `s`.
fn digit_run(s: &str) -> &str {
    &s[..s.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(s.len())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("qd_scandir_{tag}_{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every PDF under `root`, and the files skipped.
    fn scan(root: &Path) -> (Vec<(PathBuf, u64)>, usize) {
        let mut pdfs = FirstPdfs::new(usize::MAX);
        let skipped = scan_folder(root, &mut pdfs, &AtomicBool::new(false));
        (pdfs.sorted().0, skipped)
    }

    #[test]
    fn only_the_first_files_are_kept_and_never_many_more_held() {
        let mut pdfs = FirstPdfs::new(3);
        for n in (1..=20).rev() {
            pdfs.push((PathBuf::from(format!("scan{n}.pdf")), n));
            assert!(pdfs.pdfs.len() < 6, "at most twice the limit held");
        }
        let (kept, beyond) = pdfs.sorted();
        let names: Vec<String> = kept.iter().map(|(path, _)| path.display().to_string()).collect();
        assert_eq!((names, beyond), (vec!["scan1.pdf".into(), "scan2.pdf".into(), "scan3.pdf".into()], 17));
    }

    #[test]
    fn a_stopped_walk_ends_at_once() {
        let dir = temp_dir("stop");
        fs::write(dir.join("a.pdf"), b"x").unwrap();
        let mut pdfs = FirstPdfs::new(10);
        assert_eq!(scan_folder(&dir, &mut pdfs, &AtomicBool::new(true)), 0);
        assert!(pdfs.sorted().0.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recognizes_pdf_in_any_letter_case() {
        assert!(is_pdf_file(Path::new("contract.pdf")));
        assert!(is_pdf_file(Path::new("SCAN.PDF")));
        assert!(is_pdf_file(Path::new("mixed.Pdf")));
        assert!(is_pdf_file(Path::new(r"C:\a b\ząb.pdf")));
    }

    #[test]
    fn rejects_everything_else() {
        for name in [
            "a.docx", "a.txt", "a.pdfx", "a.pd", "a.pdf.exe", "a.fdf", "a.xps", "a.ps", "pdf",
            ".pdf.lnk", "no_extension", "a.pdf.",
        ] {
            assert!(!is_pdf_file(Path::new(name)), "{name} must be rejected");
        }
    }

    #[test]
    fn a_junction_that_loops_back_is_not_followed() {
        let root = temp_dir("junction");
        fs::write(root.join("doc.pdf"), b"x").unwrap();
        // A junction needs no admin rights, unlike a symlink.
        let made = std::process::Command::new(crate::platform::system32("cmd.exe"))
            .args(["/C", "mklink", "/J"])
            .arg(root.join("loop"))
            .arg(&root)
            .output()
            .is_ok_and(|out| out.status.success());
        if made {
            let (pdfs, _) = scan(&root);
            assert_eq!(pdfs.len(), 1, "the file once, not once per lap");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_subfolders_filtering_and_sizing_each_file() {
        let dir = temp_dir("walk");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("a.pdf"), b"1234").unwrap();
        fs::write(dir.join("B.PDF"), b"x").unwrap();
        fs::write(dir.join("photo.jpg"), b"x").unwrap();
        fs::write(dir.join("readme.txt"), b"x").unwrap();
        fs::write(sub.join("c.pdf"), b"x").unwrap();
        fs::write(sub.join("cover.png"), b"x").unwrap();

        let (pdfs, skipped) = scan(&dir);
        assert_eq!(pdfs.len(), 3, "PDFs from this folder and its subfolder");
        assert_eq!(skipped, 3, "jpg + txt + png skipped");
        // Sizes come straight from the walk, so no file has to be reopened.
        let sizes: Vec<u64> = pdfs.iter().map(|(_, size)| *size).collect();
        assert!(sizes.contains(&4), "a.pdf is 4 bytes: {sizes:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Sort `names` with [`natural_path_cmp`] and return them in that order.
    fn sorted(names: &[&str]) -> Vec<String> {
        let mut paths: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
        paths.sort_by(|a, b| natural_path_cmp(a, b));
        paths.iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn numbers_sort_by_value_not_by_character() {
        // The order NTFS returns (measured) versus the one a person expects.
        assert_eq!(
            sorted(&["Skan1.pdf", "skan10.pdf", "skan2.pdf"]),
            ["Skan1.pdf", "skan2.pdf", "skan10.pdf"]
        );
        // Longer than u64 — must not overflow or panic.
        assert_eq!(
            sorted(&["x99999999999999999999999.pdf", "x100000000000000000000000.pdf"]),
            ["x99999999999999999999999.pdf", "x100000000000000000000000.pdf"]
        );
    }

    #[test]
    fn letters_ignore_case() {
        assert_eq!(sorted(&["b.pdf", "C.pdf", "a.pdf"]), ["a.pdf", "b.pdf", "C.pdf"]);
    }

    #[test]
    fn a_folders_files_stay_together() {
        // A string compare would put "Invoices 2" (space) before "Invoices\" and
        // split the first folder's files around it.
        assert_eq!(
            sorted(&[r"D:\Invoices 2\a.pdf", r"D:\Invoices\b.pdf", r"D:\Invoices\a.pdf"]),
            [r"D:\Invoices\a.pdf", r"D:\Invoices\b.pdf", r"D:\Invoices 2\a.pdf"]
        );
    }

    #[test]
    fn ordering_is_total_and_deterministic() {
        // Case-only and leading-zero-only differences still get a fixed order,
        // so repeated sorts never shuffle equal-looking rows.
        let mut once = sorted(&["A.pdf", "a.pdf", "7.pdf", "007.pdf"]);
        let twice = sorted(&["a.pdf", "007.pdf", "A.pdf", "7.pdf"]);
        assert_eq!(once, twice);
        once.dedup();
        assert_eq!(once.len(), 4, "no two distinct paths compare equal");
    }

    #[test]
    fn missing_folder_yields_empty() {
        let (pdfs, skipped) = scan(Path::new("this-does-not-exist-xyz"));
        assert!(pdfs.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn random_names_are_in_one_consistent_order() {
        // A sort with an inconsistent order may panic (Rust 1.81+) — on the UI
        // thread. Letters that fold to several (İ, ß) and digit runs with zeros
        // are where it would break.
        let alphabet = crate::fuzz::chars("aAbB0017 9.-_İiẞßÅå{307}\\");
        let mut rng = crate::fuzz::Rng::new(0x5EED_0002);
        let random_path = |rng: &mut crate::fuzz::Rng| PathBuf::from(format!(r"C:{}", rng.text(&alphabet, 8)));
        for _ in 0..20_000 {
            let (a, b, c) = (random_path(&mut rng), random_path(&mut rng), random_path(&mut rng));
            let ab = natural_path_cmp(&a, &b);
            assert_eq!(ab, natural_path_cmp(&b, &a).reverse(), "{a:?} {b:?}");
            assert_eq!(natural_path_cmp(&a, &a), Ordering::Equal);
            if ab != Ordering::Greater && natural_path_cmp(&b, &c) != Ordering::Greater {
                assert_ne!(natural_path_cmp(&a, &c), Ordering::Greater, "{a:?} <= {b:?} <= {c:?}");
            }
        }
        let mut many: Vec<PathBuf> = (0..2_000).map(|_| random_path(&mut rng)).collect();
        let mut split_once = many.clone();
        many.sort_by(|a, b| natural_path_cmp(a, b));
        assert!(many.windows(2).all(|pair| natural_path_cmp(&pair[0], &pair[1]) != Ordering::Greater));
        // Skipping the start two names share changes no order.
        for _ in 0..20_000 {
            let (a, b) = (rng.text(&alphabet, 10), rng.text(&alphabet, 10));
            let shared = format!("{}{}", rng.text(&alphabet, 6), rng.text(&alphabet, 2));
            let (a, b) = (format!("{shared}{a}"), format!("{shared}{b}"));
            assert_eq!(natural_cmp(&a, &b), natural_cmp_from(&a, &b, 0), "{a:?} {b:?}");
        }
        // A sort that splits each path once puts them in the very same order.
        sort_naturally(&mut split_once, |path| path);
        assert_eq!(split_once, many);
    }
}
