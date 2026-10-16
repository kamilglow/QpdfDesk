//! Arguments written for a command line pasted into cmd.exe.
//!
//! Two readers see it: cmd.exe first, which treats `& | < > ^` outside
//! quotes as its own, then the program's C runtime, which splits the line at
//! spaces outside quotes and reads `\"` as a quote. An argument with a space
//! or any of those characters goes in quotes, with the backslashes before a
//! quote — or before the closing one — doubled. (cmd.exe still expands
//! `%name%` inside quotes when such a variable exists; file names rarely
//! hold one, and it cannot be escaped there.)

/// `arg` as one argument of a cmd.exe command line.
pub fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"', '&', '|', '<', '>', '^', '(', ')']) {
        return arg.to_string();
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Each backslash doubled, and one more for the quote itself.
                quoted.extend(std::iter::repeat_n('\\', 2 * backslashes + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    // Before the closing quote: doubled, or they would escape it.
    quoted.extend(std::iter::repeat_n('\\', 2 * backslashes));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn arguments_are_quoted_only_when_they_must_be() {
        assert_eq!(quote("--object-streams=generate"), "--object-streams=generate");
        assert_eq!(quote(r"C:\Docs\a.pdf"), r"C:\Docs\a.pdf", "backslashes alone are plain");
        assert_eq!(quote(r"C:\My Docs\a (optimized).pdf"), r#""C:\My Docs\a (optimized).pdf""#);
        assert_eq!(quote("1-z,x2"), "1-z,x2");
        assert_eq!(quote(""), r#""""#, "an empty argument stays one");
        // cmd.exe's own characters stay inside quotes.
        assert_eq!(quote("--remove-attachment=<key>"), r#""--remove-attachment=<key>""#);
        assert_eq!(quote("R&D.pdf"), r#""R&D.pdf""#);
        assert_eq!(quote("a^b"), r#""a^b""#);
    }

    #[test]
    fn backslashes_before_a_quote_follow_the_runtime_rules() {
        assert_eq!(quote(r"C:\My Results\"), r#""C:\My Results\\""#, "a closing backslash is doubled");
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[link(name = "shell32")]
    extern "system" {
        fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    /// `line` split into arguments by Windows' own rules — those of the C
    /// runtime QPDF starts with, for what `quote` writes.
    fn split(line: &str) -> Vec<String> {
        let wide = crate::platform::wide(line);
        let mut count = 0;
        // SAFETY: `wide` ends in NUL; the array is read within `count` and freed once.
        unsafe {
            let list = CommandLineToArgvW(wide.as_ptr(), &mut count);
            assert!(!list.is_null());
            let args = (0..count as usize)
                .map(|i| {
                    let arg = *list.add(i);
                    let len = (0..).take_while(|&n| *arg.add(n) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(arg, len))
                })
                .collect();
            LocalFree(list.cast());
            args
        }
    }

    #[test]
    fn random_arguments_come_back_as_they_were() {
        let alphabet = crate::fuzz::chars("a \\\"&|<>^()\t%!=:é");
        let mut rng = crate::fuzz::Rng::new(0x5EED_0009);
        for _ in 0..20_000 {
            let args: Vec<String> = (0..1 + rng.below(3)).map(|_| rng.text(&alphabet, 10).replace('\0', "")).collect();
            let line = std::iter::once("qpdf.exe".to_string()).chain(args.iter().map(|arg| quote(arg))).collect::<Vec<_>>().join(" ");
            assert_eq!(split(&line)[1..], args[..], "{line}");
        }
    }
}
