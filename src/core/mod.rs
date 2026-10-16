//! The core: the file list's building blocks (PDF filter, folder walk, natural
//! order, duplicates) and QPDF itself — finding it, running it, reading its
//! verdict and committing its output.
//!
//! No dependency on the UI layer — everything here is testable headless.

pub mod attachments;
pub mod cmdline;
pub mod dedup;
pub mod filelist;
pub mod inspect;
pub mod job;
pub mod outcome;
pub mod output;
pub mod qpdf;
pub mod ranges;
pub mod runner;
pub mod sanitize;
pub mod scandir;
pub mod secret;

/// `text` without control characters: text from file names
/// and from QPDF reaches the window only through here — a line break or an
/// escape would break the list's columns and could fake text. The marks that
/// turn text right-to-left go too: `invoice\u{202E}fdp.exe` would read as
/// `invoiceexe.pdf`.
pub fn printable(text: &str) -> String {
    text.chars().filter(|&c| !c.is_control() && !is_bidi_control(c)).collect()
}

/// A Unicode mark that changes the direction of the text after it.
pub fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{061C}')
}

#[cfg(test)]
mod tests {
    #[test]
    fn printable_text_loses_control_characters() {
        assert_eq!(super::printable("tab\there\u{1b}[31m\r\n.pdf"), "tabhere[31m.pdf");
        assert_eq!(super::printable("zażółć"), "zażółć");
        assert_eq!(super::printable("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
    }
}
