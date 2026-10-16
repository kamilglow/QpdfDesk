//! Page ranges in QPDF's own syntax, checked here before QPDF
//! ever sees them.
//!
//! The syntax, as QPDF 12.4.2 takes it (measured 2026-10-10):
//! comma-separated groups; a group is a page or `a-b` (counting down when
//! `a > b`); a page is `n`, `r<n>` (n-th from the end) or `z` (the last); a
//! group starting with `x` removes its pages from the group before it (all
//! such exclusions apply to the last plain group), but the first group may
//! not be one; `:odd` or `:even` at the very end keeps every other page of
//! the whole result. Pages may repeat (`1,1` is two pages). Every number —
//! excluded ones too — must be a page of the file.
//!
//! QPDF refuses spaces and capitals; people type them (`1, 3`), so they are
//! removed and lowered first, and the cleaned text is what QPDF gets. The
//! grammar never lets a range start with `-`, `@` or `.`, so it cannot be
//! taken for an option, an argument file or the input.

use crate::t;

/// One page reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Number(u64),
    FromEnd(u64),
    Last,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Group {
    exclude: bool,
    first: Page,
    last: Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parity {
    Odd,
    Even,
}

/// A parsed page range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRange {
    text: String,
    groups: Vec<Group>,
    parity: Option<Parity>,
}

/// Why a range cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeError {
    Empty,
    /// Not QPDF's syntax; the part that is not.
    Syntax(String),
    /// A page the file does not have (`pages` = its page count).
    NoSuchPage { page: String, pages: u64 },
    /// Nothing would be left.
    NoPages,
    /// Deleting works on plain pages and ranges only.
    NotDeletable,
}

impl RangeError {
    pub fn sentence(&self) -> String {
        match self {
            RangeError::Empty => t!(RangeEmpty).into(),
            RangeError::Syntax(part) => t!(RangeSyntax, part = part),
            RangeError::NoSuchPage { page, pages } => t!(RangeNoSuchPage, page = page, pages = pages),
            RangeError::NoPages => t!(RangeNoPages).into(),
            RangeError::NotDeletable => t!(RangeNotDeletable).into(),
        }
    }
}

/// Longest range text accepted — far beyond anything typed by hand, and it
/// keeps one QPDF argument small.
const MAX_TEXT: usize = 1000;

impl PageRange {
    pub fn parse(text: &str) -> Result<PageRange, RangeError> {
        let text: String = text.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
        if text.is_empty() {
            return Err(RangeError::Empty);
        }
        if text.len() > MAX_TEXT {
            return Err(RangeError::Syntax(format!("{}…", text.chars().take(20).collect::<String>())));
        }
        let (body, parity) = match text.split_once(':') {
            None => (text.as_str(), None),
            Some((body, "odd")) => (body, Some(Parity::Odd)),
            Some((body, "even")) => (body, Some(Parity::Even)),
            Some((_, rest)) => return Err(RangeError::Syntax(format!(":{rest}"))),
        };
        let mut groups = Vec::new();
        for part in body.split(',') {
            let group = parse_group(part).ok_or_else(|| RangeError::Syntax(part.to_string()))?;
            if group.exclude && groups.is_empty() {
                return Err(RangeError::Syntax(part.to_string()));
            }
            groups.push(group);
        }
        Ok(PageRange { text, groups, parity })
    }

    /// The cleaned text, as QPDF gets it.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Each plain group as `(first, last)` resolved in a file of `pages`
    /// pages (`first > last` counts down), with the spans its exclusions
    /// remove (the parser never lets an exclusion come first). Every number,
    /// excluded ones too, must be a page of the file.
    fn groups(&self, pages: u64) -> Result<Vec<(Span, Vec<Span>)>, RangeError> {
        let resolve = |page: Page| {
            let number = match page {
                Page::Number(n) => n,
                Page::FromEnd(n) => pages.saturating_add(1).saturating_sub(n),
                Page::Last => pages,
            };
            if (1..=pages).contains(&number) {
                Ok(number)
            } else {
                let page = match page {
                    Page::Number(n) => n.to_string(),
                    Page::FromEnd(n) => format!("r{n}"),
                    Page::Last => "z".into(),
                };
                Err(RangeError::NoSuchPage { page, pages })
            }
        };
        let mut parts: Vec<(Span, Vec<Span>)> = Vec::new();
        for group in &self.groups {
            let (a, b) = (resolve(group.first)?, resolve(group.last)?);
            match parts.last_mut() {
                Some((_, excluded)) if group.exclude => excluded.push((a.min(b), a.max(b))),
                _ => parts.push(((a, b), Vec::new())),
            }
        }
        Ok(parts)
    }

    /// How many pages the range selects in a file of `pages` pages.
    pub fn count(&self, pages: u64) -> Result<u64, RangeError> {
        let total = self
            .groups(pages)?
            .into_iter()
            .map(|((a, b), excluded)| remaining((a.min(b), a.max(b)), excluded))
            .fold(0, u64::saturating_add);
        let total = match self.parity {
            None => total,
            Some(Parity::Odd) => total.div_ceil(2),
            Some(Parity::Even) => total / 2,
        };
        if total == 0 {
            return Err(RangeError::NoPages);
        }
        Ok(total)
    }

    /// The pages the range selects in a file of `pages` pages, in order — as
    /// QPDF takes them (the page view). At most [`MAX_EXPANDED`].
    pub fn pages(&self, pages: u64) -> Result<Vec<u64>, RangeError> {
        if self.count(pages)? > MAX_EXPANDED {
            return Err(RangeError::Syntax(self.text.clone()));
        }
        let all = self.groups(pages)?.into_iter().flat_map(|((a, b), excluded)| {
            let kept: Box<dyn Iterator<Item = u64>> = if a <= b { Box::new(a..=b) } else { Box::new((b..=a).rev()) };
            kept.filter(move |page| !excluded.iter().any(|(from, to)| (from..=to).contains(&page)))
        });
        Ok(match self.parity {
            None => all.collect(),
            Some(Parity::Odd) => all.step_by(2).collect(),
            Some(Parity::Even) => all.skip(1).step_by(2).collect(),
        })
    }

    /// The range of exactly `pages`, in this order: runs up or down become
    /// `a-b` (`1-3,7,5-4`). Fails when the text would be longer than QPDF
    /// should be given (a shuffle of thousands of pages) — as soon as it is.
    pub fn from_pages(pages: &[u64]) -> Result<PageRange, RangeError> {
        let mut text = String::new();
        let mut i = 0;
        while i < pages.len() {
            let start = pages[i];
            let mut end = i;
            if let Some(&next) = pages.get(i + 1) {
                let step: i128 = if next == start.wrapping_add(1) { 1 } else if Some(next) == start.checked_sub(1) { -1 } else { 0 };
                if step != 0 {
                    while pages.get(end + 1).is_some_and(|&p| i128::from(p) == i128::from(pages[end]) + step) {
                        end += 1;
                    }
                }
            }
            if !text.is_empty() {
                text.push(',');
            }
            text.push_str(&if end == i { start.to_string() } else { format!("{start}-{}", pages[end]) });
            if text.len() > MAX_TEXT {
                return Err(RangeError::Syntax(format!("{}…", &text[..20])));
            }
            i = end + 1;
        }
        PageRange::parse(&text)
    }

    /// Every page except these: `1-z,x…` — what "Delete pages" hands to QPDF.
    pub fn complement(&self) -> Result<PageRange, RangeError> {
        if self.parity.is_some() || self.groups.iter().any(|g| g.exclude) {
            return Err(RangeError::NotDeletable);
        }
        let mut text = String::from("1-z");
        let mut groups = vec![Group { exclude: false, first: Page::Number(1), last: Page::Last }];
        for (group, part) in self.groups.iter().zip(self.text.split(',')) {
            text.push_str(",x");
            text.push_str(part);
            groups.push(Group { exclude: true, ..*group });
        }
        Ok(PageRange { text, groups, parity: None })
    }
}

/// First and last page of a group, lowest first.
type Span = (u64, u64);

/// Most pages [`PageRange::pages`] lists — a page view of any real file fits.
pub const MAX_EXPANDED: u64 = 1_000_000;

/// Pages of the span `kept` not inside any of the spans `excluded`.
fn remaining(kept: Span, mut excluded: Vec<Span>) -> u64 {
    excluded.sort_unstable();
    let mut removed = 0u64;
    let mut covered_to = kept.0 - 1; // spans start at page 1
    for (from, to) in excluded {
        let from = from.max(covered_to + 1).max(kept.0);
        let to = to.min(kept.1);
        if from <= to {
            removed += to - from + 1;
            covered_to = to;
        }
    }
    kept.1 - kept.0 + 1 - removed
}

fn parse_group(part: &str) -> Option<Group> {
    let (exclude, rest) = match part.strip_prefix('x') {
        Some(rest) => (true, rest),
        None => (false, part),
    };
    let (first, last) = match rest.split_once('-') {
        Some((a, b)) => (parse_page(a)?, parse_page(b)?),
        None => {
            let page = parse_page(rest)?;
            (page, page)
        }
    };
    Some(Group { exclude, first, last })
}

fn parse_page(text: &str) -> Option<Page> {
    let digits = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    match text {
        "z" => Some(Page::Last),
        _ => match text.strip_prefix('r') {
            Some(n) => digits(n).map(Page::FromEnd),
            None => digits(text).map(Page::Number),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(text: &str, pages: u64) -> Result<u64, RangeError> {
        PageRange::parse(text)?.count(pages)
    }

    #[test]
    fn counts_match_qpdf() {
        // Each measured with QPDF 12.4.2 on its 178-page manual.
        let cases = [
            ("1-10", 10),
            ("1-10,x3-4", 8),
            ("1-10,x3,x5", 8),
            ("1-10,x3,7-8,x8", 10),
            ("1-10:odd", 5),
            ("1-10:even", 5),
            ("1-5,7-10:odd", 5),
            ("z-1", 178),
            ("r3-z", 3),
            ("1-z,x2-r2", 2),
            ("5-3", 3),
            ("1,1,1", 3),
            ("1-z:even", 89),
            ("1-10,x2:odd", 5),
            ("z-r1", 1),
            ("01", 1),
            ("r1-r178", 178),
            ("1-10,x1-10,3", 1),
            ("z-z", 1),
            ("3-r176", 1),
            ("1-10,xr1", 10),
            ("1-10,x20", 10),
        ];
        for (text, expected) in cases {
            assert_eq!(count(text, 178), Ok(expected), "{text}");
        }
    }

    #[test]
    fn what_qpdf_refuses_is_refused() {
        for text in ["1-5:odd,7-10", "1-10:odd:even", "1-2,,3", "1-", "-3", "1--3", "1-10:", "x", "1,x", "x3", "1-3x", "a", "1.5", "@x", "."] {
            assert!(matches!(PageRange::parse(text), Err(RangeError::Syntax(_))), "{text}");
        }
        assert_eq!(PageRange::parse("  "), Err(RangeError::Empty));
        assert!(matches!(PageRange::parse("99999999999999999999999"), Err(RangeError::Syntax(_))));
        assert!(matches!(PageRange::parse(&"1,".repeat(600)), Err(RangeError::Syntax(_))));
        for text in ["0", "200", "r0", "r179", "1-10,x200"] {
            assert!(matches!(count(text, 178), Err(RangeError::NoSuchPage { .. })), "{text}");
        }
        // QPDF writes a file without pages for these; we never do.
        for text in ["1-10,x1-10", "1-10,x10-1", "1:even"] {
            assert_eq!(count(text, 178), Err(RangeError::NoPages), "{text}");
        }
        assert!(matches!(count("z", 0), Err(RangeError::NoSuchPage { .. })));
    }

    #[test]
    fn spaces_and_capitals_are_forgiven() {
        let range = PageRange::parse(" 1, 3 - 5 ,R1,Z :ODD").expect("valid");
        assert_eq!(range.text(), "1,3-5,r1,z:odd");
        assert_eq!(range.count(10), Ok(3));
    }

    #[test]
    fn deleting_is_keeping_the_rest() {
        let range = PageRange::parse("2,5-6,z").expect("valid");
        let rest = range.complement().expect("deletable");
        assert_eq!(rest.text(), "1-z,x2,x5-6,xz");
        assert_eq!(rest.count(10), Ok(6));
        assert_eq!(PageRange::parse("1-z").expect("valid").complement().expect("ok").count(3), Err(RangeError::NoPages));
        assert_eq!(PageRange::parse("1-3:odd").expect("valid").complement(), Err(RangeError::NotDeletable));
        assert_eq!(PageRange::parse("1-3,x2").expect("valid").complement(), Err(RangeError::NotDeletable));
    }

    fn pages(text: &str, total: u64) -> Vec<u64> {
        PageRange::parse(text).expect("valid").pages(total).expect("fits")
    }

    #[test]
    fn a_range_lists_its_pages_in_qpdfs_order() {
        assert_eq!(pages("1-3,7,5-4", 10), [1, 2, 3, 7, 5, 4]);
        assert_eq!(pages("z-r3", 10), [10, 9, 8]);
        assert_eq!(pages("1-10,x3-4,x8", 10), [1, 2, 5, 6, 7, 9, 10]);
        assert_eq!(pages("1-5,7-10,x8", 10), [1, 2, 3, 4, 5, 7, 9, 10], "an exclusion takes from the group before it");
        assert_eq!(pages("1-10:odd", 10), [1, 3, 5, 7, 9]);
        assert_eq!(pages("10-1:even", 10), [9, 7, 5, 3, 1]);
        assert_eq!(pages("1,1", 3), [1, 1]);
        // Every count the parser agrees with QPDF on is the length of the list.
        for text in ["1-10,x3,7-8,x8", "1-z,x2-r2", "3-r176", "1-10,x2:odd", "r1-r178"] {
            let range = PageRange::parse(text).expect("valid");
            assert_eq!(range.pages(178).expect("ok").len() as u64, range.count(178).expect("ok"), "{text}");
        }
        assert!(PageRange::parse("12").expect("valid").pages(3).is_err());
    }

    #[test]
    fn pages_become_the_shortest_range() {
        let text = |list: &[u64]| PageRange::from_pages(list).map(|r| r.text().to_string());
        assert_eq!(text(&[1, 2, 3, 7, 5, 4]), Ok("1-3,7,5-4".into()));
        assert_eq!(text(&[3, 1, 2]), Ok("3,1-2".into()));
        assert_eq!(text(&[4]), Ok("4".into()));
        assert_eq!(text(&[2, 2, 2]), Ok("2,2,2".into()));
        assert_eq!(text(&[]), Err(RangeError::Empty));
        // Round trip: the range lists the same pages again.
        let shuffled = [9, 1, 2, 3, 10, 8, 7, 4];
        assert_eq!(PageRange::from_pages(&shuffled).expect("ok").pages(10).expect("ok"), shuffled);
        // A shuffle too long for one argument is refused, not cut.
        let wild: Vec<u64> = (1..=2000).map(|i| (i * 7919) % 2000 + 1).collect();
        assert!(matches!(PageRange::from_pages(&wild), Err(RangeError::Syntax(_))));
    }

    #[test]
    fn errors_read_like_sentences() {
        assert_eq!(count("12", 3).expect_err("beyond").sentence(), "page 12 does not exist (the file has 3)");
        assert!(PageRange::parse("1-3x").expect_err("bad").sentence().contains("“1-3x”"));
    }

    /// A random range text: mostly the grammar's own tokens, numbers of
    /// every size, now and then any character at all.
    fn random_range(rng: &mut crate::fuzz::Rng) -> String {
        const TOKENS: [&str; 14] = ["-", ",", "x", "r", "z", ":odd", ":even", " ", "R", "Z", "0", "18446744073709551615", "99999999999999999999", ":"];
        let mut text = String::new();
        for _ in 0..rng.below(12) {
            match rng.below(10) {
                0..=4 => text.push_str(&(1 + rng.below(30)).to_string()),
                5 if rng.chance(4) => text.push(rng.any_char()),
                _ => text.push_str(rng.pick(&TOKENS)),
            }
        }
        text
    }

    #[test]
    fn random_ranges_never_panic_and_agree_with_themselves() {
        let mut rng = crate::fuzz::Rng::new(0x5EED_0001);
        let mut valid = 0;
        for _ in 0..30_000 {
            let text = random_range(&mut rng);
            let Ok(range) = PageRange::parse(&text) else { continue };
            valid += 1;
            assert_eq!(PageRange::parse(range.text()).as_ref(), Ok(&range), "{text:?}: the cleaned text means the same");
            let total = rng.below(40);
            let counted = range.count(total);
            match range.pages(total) {
                // Two ways of counting: arithmetic, and listing the pages.
                Ok(listed) => {
                    assert_eq!(counted, Ok(listed.len() as u64), "{text:?} in {total} pages");
                    assert!(listed.iter().all(|page| (1..=total).contains(page)), "{text:?}");
                    if let Ok(again) = PageRange::from_pages(&listed) {
                        assert_eq!(again.pages(total).as_ref(), Ok(&listed), "{text:?} rewritten as {:?}", again.text());
                    }
                }
                Err(error) => assert_eq!(counted, Err(error), "{text:?} in {total} pages"),
            }
            if let Ok(rest) = range.complement() {
                let _ = rest.count(total);
            }
        }
        assert!(valid > 3_000, "the generator must reach the parser's depths: {valid} valid");
    }
}
