//! Persisted settings: thread count, memory limit, language, window size and
//! last used folder. Never the steps panel: every start begins with no step
//! chosen.
//!
//! Stored as a tiny `key=value` text file in the application's own folder (next
//! to the executable), so a portable copy keeps its settings with it.
//! Any read problem — missing file, corrupt line, bad number — silently falls
//! back to defaults: settings must never crash or nag the user.

use std::path::{Path, PathBuf};

/// Everything remembered between runs.
///
/// The window POSITION is deliberately not persisted: the app always opens
/// centered on the screen under the cursor, which is predictable on a
/// multi-monitor desk and avoids the classic restore-drift (a saved position
/// creeps by the title-bar height on every run).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// How many files are processed at once; `None` = automatic,
    /// see [`Settings::threads`]. Not in the UI; advanced users edit
    /// `settings.txt`.
    pub threads: Option<usize>,
    /// Last window size `(w, h)`.
    pub window_size: Option<(i32, i32)>,
    /// Last folder used in a file/folder/save dialog.
    pub last_folder: Option<PathBuf>,
    /// Combined memory limit of all QPDF processes, in MB;
    /// `None` = automatic, see [`Settings::qpdf_memory_limit`]. Not in the UI.
    pub qpdf_memory_mb: Option<u64>,
    /// The language of the window (`en`…); `None` = the system's.
    pub language: Option<String>,
}

/// The automatic thread count: half the logical processors, at most this
/// — two files at once halve a long batch (measured:
/// 26.6 s → 12.7 s), and QPDF runs below normal priority, so the machine
/// stays usable.
const MAX_AUTO_THREADS: usize = 4;

/// [`MAX_AUTO_THREADS`] at most, half of `processors`, at least one.
pub fn auto_threads(processors: usize) -> usize {
    (processors / 2).clamp(1, MAX_AUTO_THREADS)
}

/// Upper bound for the configured thread count. Anything higher is a typo, and
/// many parallel QPDF processes would only thrash the disk.
pub const MAX_THREADS: usize = 16;

/// Upper bound for a remembered window side, in FLTK units — well past any
/// monitor, far below where the size math could overflow.
const MAX_WINDOW_SIDE: i32 = 16_384;

/// Accepted range for `qpdf_memory_mb`: below 256 MB ordinary files fail,
/// above 1 TB is a typo.
const QPDF_MEMORY_MB: std::ops::RangeInclusive<u64> = 256..=1_048_576;

/// The automatic memory limit: half the installed memory, at most this
///…
const AUTO_MEMORY_MAX: u64 = 8 << 30;

/// …or this, when the installed memory is unknown.
const AUTO_MEMORY_FALLBACK: u64 = 2 << 30;

/// Directory holding the app's data files (`settings.txt`, `app.lock`): the
/// folder containing the executable, so a portable install carries its settings
/// with it. There is no other location — if the folder cannot be written to,
/// the app simply runs with defaults and persists nothing.
///
/// `None` only when the executable's own path cannot be determined.
pub fn data_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
}

/// Full path of the settings file inside `dir`.
fn settings_file(dir: &Path) -> PathBuf {
    dir.join("settings.txt")
}

/// The most of `settings.txt` read: a real one is under 200 bytes.
const MAX_FILE_BYTES: u64 = 64 << 10;

/// The first `limit` bytes of the text file `path`; `None` when it cannot be
/// read or is not UTF-8. A huge file next to the EXE costs no more than that;
/// a character the limit cuts in two is left out, not the whole file.
pub fn read_text(path: &Path, limit: u64) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|file| file.take(limit).read_to_end(&mut bytes)).ok()?;
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        // Only the end is unfinished: the cut.
        Err(e) if e.utf8_error().error_len().is_none() => {
            let whole = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(whole);
            String::from_utf8(bytes).ok()
        }
        Err(_) => None,
    }
}

/// Write `text` as the file `path`: a new file first, then renamed over the
/// old one, so a failed write (a full disk, the power gone) never leaves it
/// half-written.
pub fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let written = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

impl Settings {
    /// Parse the `key=value` format. Unknown keys and malformed lines are
    /// ignored; missing keys keep their defaults.
    pub fn parse(text: &str) -> Settings {
        let mut settings = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                // Empty, out-of-range or unparsable: automatic, rather than
                // letting a typo start hundreds of processes.
                ("threads", v) => settings.threads = v.parse().ok().filter(|n| (1..=MAX_THREADS).contains(n)),
                ("window_size", v) => {
                    let parts: Vec<i32> =
                        v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                    if let [w, h] = parts[..] {
                        // Reject degenerate or absurd sizes — treat as corrupt.
                        // A huge one overflowed the DPI math into a window
                        // 0 px tall (measured in MediaFileValid with i32::MAX).
                        if (200..=MAX_WINDOW_SIDE).contains(&w)
                            && (150..=MAX_WINDOW_SIDE).contains(&h)
                        {
                            settings.window_size = Some((w, h));
                        }
                    }
                }
                ("last_folder", v) if !v.is_empty() => {
                    settings.last_folder = Some(PathBuf::from(v));
                }
                ("qpdf_memory_mb", v) => {
                    settings.qpdf_memory_mb = v.parse().ok().filter(|mb| QPDF_MEMORY_MB.contains(mb));
                }
                ("language", v) if !v.is_empty() => settings.language = Some(v.to_string()),
                _ => {}
            }
        }
        settings
    }

    /// Serialize to the `key=value` format (inverse of [`Settings::parse`]).
    pub fn serialize(&self) -> String {
        // The options found only here are always written — empty means
        // automatic — so a look at the file shows they exist.
        let or_empty = |value: Option<String>| value.unwrap_or_default();
        let mut out = format!("threads={}\n", or_empty(self.threads.map(|n| n.to_string())));
        out.push_str(&format!("qpdf_memory_mb={}\n", or_empty(self.qpdf_memory_mb.map(|mb| mb.to_string()))));
        out.push_str(&format!("language={}\n", or_empty(self.language.clone())));
        if let Some((w, h)) = self.window_size {
            out.push_str(&format!("window_size={w},{h}\n"));
        }
        if let Some(folder) = &self.last_folder {
            out.push_str(&format!("last_folder={}\n", folder.display()));
        }
        out
    }

    /// The combined memory limit for the QPDF job, in bytes:
    /// `qpdf_memory_mb` if set, else half of `total_memory`, at most 8 GB.
    pub fn qpdf_memory_limit(&self, total_memory: Option<u64>) -> u64 {
        match (self.qpdf_memory_mb, total_memory) {
            (Some(mb), _) => mb.saturating_mul(1 << 20),
            (None, Some(total)) => (total / 2).min(AUTO_MEMORY_MAX),
            (None, None) => AUTO_MEMORY_FALLBACK,
        }
    }

    /// Files processed at once: as set, or [`auto_threads`] of this machine.
    pub fn threads(&self) -> usize {
        self.threads.unwrap_or_else(|| auto_threads(std::thread::available_parallelism().map_or(1, |n| n.get())))
    }

    /// How many QPDF processes may run at once: one per
    /// thread for the batch and as many for the probe, which looks at
    /// files `threads` at a time, plus one for the Info window and one for
    /// the overlay file's probe. Verification runs after its job, in the same
    /// thread, so it needs no room of its own.
    pub fn qpdf_process_limit(&self) -> u32 {
        u32::try_from(self.threads()).unwrap_or(1).saturating_mul(2).saturating_add(2)
    }

    /// Load from `dir`, silently defaulting on any error.
    pub fn load(dir: &Path) -> Settings {
        read_text(&settings_file(dir), MAX_FILE_BYTES).map(|text| Settings::parse(&text)).unwrap_or_default()
    }

    /// Save to `dir` (created if needed). Failures are ignored — losing a
    /// preference must never bother the user.
    pub fn save(&self, dir: &Path) {
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = write_text(&settings_file(dir), &self.serialize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_all_fields() {
        let settings = Settings {
            threads: Some(8),
            window_size: Some((900, 600)),
            last_folder: Some(PathBuf::from(r"C:\Documents")),
            qpdf_memory_mb: Some(3000),
            language: Some("en".into()),
        };
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn a_panel_saved_by_an_older_version_is_left_out() {
        let settings = Settings::parse("panel.step.optimize=yes\nprofile=Archive\nthreads=2\n");
        assert_eq!(settings, Settings { threads: Some(2), ..Settings::default() });
        assert!(!settings.serialize().contains("panel.") && !settings.serialize().contains("profile="));
    }

    #[test]
    fn qpdf_memory_limit_is_half_the_ram_at_most_8_gb() {
        const GB: u64 = 1 << 30;
        let auto = Settings::default();
        assert_eq!(auto.qpdf_memory_limit(Some(8 * GB)), 4 * GB);
        assert_eq!(auto.qpdf_memory_limit(Some(64 * GB)), 8 * GB);
        assert_eq!(auto.qpdf_memory_limit(None), 2 * GB);
        let set = Settings::parse("qpdf_memory_mb=1024");
        assert_eq!(set.qpdf_memory_limit(Some(64 * GB)), GB);
        // Out of range or junk: automatic.
        for bad in ["0", "100", "2000000", "-5", "lots", "99999999999999999999999"] {
            assert_eq!(Settings::parse(&format!("qpdf_memory_mb={bad}")).qpdf_memory_mb, None, "{bad}");
        }
    }

    #[test]
    fn qpdf_process_limit_leaves_room_for_the_probe_and_info() {
        assert_eq!(Settings::parse("threads=1").qpdf_process_limit(), 4);
        assert_eq!(Settings::parse("threads=16").qpdf_process_limit(), 34);
    }

    #[test]
    fn thread_count_is_clamped_to_something_sane() {
        assert_eq!(Settings::parse("threads=1").threads, Some(1));
        assert_eq!(Settings::parse("threads=4").threads(), 4);
        assert_eq!(Settings::parse(&format!("threads={MAX_THREADS}")).threads, Some(MAX_THREADS));
        // Zero, absurd values, negatives and junk all mean automatic.
        for bad in ["0", "17", "5000", "-4", "many", ""] {
            assert_eq!(Settings::parse(&format!("threads={bad}")).threads, None, "threads={bad}");
        }
    }

    #[test]
    fn half_the_processors_at_most_four_by_default() {
        assert_eq!(Settings::default().threads, None);
        for (processors, threads) in [(1, 1), (2, 1), (4, 2), (6, 3), (8, 4), (20, 4), (128, 4)] {
            assert_eq!(auto_threads(processors), threads, "{processors} processors");
        }
        assert!((1..=4).contains(&Settings::default().threads()));
        // The options found only in the file are in it, empty = automatic.
        assert!(Settings::default().serialize().starts_with("threads=\nqpdf_memory_mb=\nlanguage=\n"));
    }

    #[test]
    fn defaults_on_garbage_or_empty() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(Settings::parse("not a settings file\n###"), Settings::default());
        assert_eq!(Settings::parse("window_size=abc,def\nthreads=maybe"), Settings::default());
    }

    #[test]
    fn degenerate_window_size_is_rejected() {
        let parsed = Settings::parse("window_size=50,20");
        assert_eq!(parsed.window_size, None, "too small to be a real window");
    }

    #[test]
    fn absurd_window_size_is_rejected() {
        for bad in ["100000,600", "800,100000", "2147483647,2147483647"] {
            let parsed = Settings::parse(&format!("window_size={bad}"));
            assert_eq!(parsed.window_size, None, "window_size={bad}");
        }
        assert_eq!(Settings::parse("window_size=3840,2160").window_size, Some((3840, 2160)));
    }

    #[test]
    fn partial_files_keep_other_defaults() {
        let parsed = Settings::parse("threads=2");
        assert_eq!(parsed.threads, Some(2));
        assert_eq!(parsed.window_size, None);
        assert_eq!(parsed.last_folder, None);
    }

    #[test]
    fn data_dir_is_the_executables_folder() {
        let dir = data_dir().expect("the test binary has a path");
        let exe = std::env::current_exe().expect("current exe");
        assert_eq!(dir, exe.parent().expect("exe parent"));
        assert!(dir.is_dir());
    }

    #[test]
    fn load_missing_dir_gives_defaults() {
        let dir = Path::new("this-directory-does-not-exist-xyz");
        assert_eq!(Settings::load(dir), Settings::default());
    }

    #[test]
    fn save_and_load_roundtrip_on_disk() {
        let dir = std::env::temp_dir().join(format!("qd_settings_{}", std::process::id()));
        let settings = Settings {
            threads: Some(4),
            window_size: Some((800, 500)),
            last_folder: Some(PathBuf::from(r"D:\Scans")),
            ..Settings::default()
        };
        settings.save(&dir);
        assert_eq!(Settings::load(&dir), settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn random_files_give_sane_settings() {
        const KEYS: [&str; 6] = ["threads", "window_size", "last_folder", "qpdf_memory_mb", " threads ", "x"];
        let alphabet = crate::fuzz::chars("0123456789,-= .:\\aZ");
        let mut rng = crate::fuzz::Rng::new(0x5EED_000A);
        for _ in 0..20_000 {
            let text: String = (0..rng.below(6)).map(|_| format!("{}={}\n", rng.pick(&KEYS), rng.text(&alphabet, 24))).collect();
            let settings = Settings::parse(&text);
            assert!(settings.threads.is_none_or(|n| (1..=MAX_THREADS).contains(&n)), "{text:?}");
            assert!(settings.window_size.is_none_or(|(w, h)| (200..=MAX_WINDOW_SIDE).contains(&w) && (150..=MAX_WINDOW_SIDE).contains(&h)));
            assert!(settings.qpdf_memory_mb.is_none_or(|mb| QPDF_MEMORY_MB.contains(&mb)));
            assert_eq!(Settings::parse(&settings.serialize()), settings, "{text:?}");
        }
    }

    #[test]
    fn only_the_start_of_a_huge_file_is_read() {
        let dir = std::env::temp_dir().join(format!("qd_settings_huge_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut text = String::from("threads=3\n");
        text.push_str(&"#".repeat(1 << 20));
        text.push_str("\nthreads=5\n");
        std::fs::write(settings_file(&dir), text).expect("written");
        assert_eq!(Settings::load(&dir).threads, Some(3), "the end, past 64 KB, is never read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_character_cut_by_the_limit_costs_only_itself() {
        let dir = std::env::temp_dir().join(format!("qd_settings_cut_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("cut.txt");
        std::fs::write(&file, "ż".repeat(10)).expect("written");
        assert_eq!(read_text(&file, 5).as_deref(), Some("żż"), "the third ż is cut in two");
        std::fs::write(&file, b"ok\xFFok").expect("written");
        assert_eq!(read_text(&file, 64), None, "not UTF-8 within the limit");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
