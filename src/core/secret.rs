//! Passwords kept only in memory.
//!
//! [`Secret`] has no `Debug`, `Display` or `Clone`, so a password cannot end up
//! in a log line, a report, the command preview or the clipboard by accident;
//! its bytes are overwritten when it is dropped. The only way out is
//! [`Secret::expose`], used by the runner to write QPDF's stdin.

/// A password: valid for QPDF's `@-` argument list, wiped on drop.
pub struct Secret(String);

/// Why a password was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum SecretError {
    /// A line break or NUL — the `@-` list is newline-separated, so such a
    /// character would split the password into a second argument.
    ForbiddenCharacter,
}

impl Secret {
    /// Take ownership of `password`; refused if it could break the argument
    /// list. A refused value is wiped too.
    pub fn new(password: String) -> Result<Secret, SecretError> {
        let secret = Secret(password);
        if secret.0.contains(['\n', '\r', '\0']) {
            return Err(SecretError::ForbiddenCharacter);
        }
        Ok(secret)
    }

    /// The password text, for the runner and for comparing two entries.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// A second copy, for one more QPDF run — explicit, unlike `Clone`, so
    /// every copy is a decision. Wiped on drop like the first.
    pub fn duplicate(&self) -> Secret {
        Secret(self.0.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(unsafe { self.0.as_mut_vec() });
    }
}

/// Overwrite `bytes` (the whole allocation, not just the used part) with
/// zeros in a way the optimizer may not remove.
pub fn wipe(bytes: &mut Vec<u8>) {
    let capacity = bytes.capacity();
    let ptr = bytes.as_mut_ptr();
    for i in 0..capacity {
        // SAFETY: `i` is within the allocation; volatile so the store is kept
        // even though the memory is about to be freed.
        unsafe { std::ptr::write_volatile(ptr.add(i), 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_passwords_are_accepted_as_they_are() {
        for pw in ["", "secret", "with spaces", "zażółć gęślą jaźń", "--looks-like-an-option"] {
            let secret = Secret::new(pw.to_string()).expect(pw);
            assert_eq!(secret.expose(), pw);
        }
    }

    #[test]
    fn line_breaks_and_nul_are_refused() {
        for bad in ["a\nb", "a\rb", "trailing\n", "a\0b"] {
            assert!(Secret::new(bad.to_string()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn wipe_zeroes_the_whole_allocation() {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(b"hunter2");
        wipe(&mut bytes);
        assert_eq!(bytes, vec![0u8; 7]);
        // The spare capacity was written too; reading it back needs `set_len`.
        unsafe { bytes.set_len(32) };
        assert!(bytes.iter().all(|&b| b == 0));
    }
}
