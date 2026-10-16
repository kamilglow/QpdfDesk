//! Windows process helpers shared by the core and the app.
//!
//! One place for the `kernel32` process API and the "no console window" spawn
//! rule, so the same declarations are not repeated (and cannot drift) across
//! modules. UI-only Win32 calls (window placement, theming) stay in the binary
//! crate; nothing here depends on `fltk`.
//!
//! Only API present on Windows 10 belongs here.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

mod sys {
    /// `PROCESS_QUERY_LIMITED_INFORMATION` — succeeds only for a live process.
    pub const QUERY_LIMITED: u32 = 0x1000;
    pub const ERROR_ACCESS_DENIED: u32 = 5;
    /// `STILL_ACTIVE` — the exit code reported by a process that is running.
    pub const STILL_ACTIVE: u32 = 259;

    /// Keep the declared requirement in force until it is cleared again.
    pub const ES_CONTINUOUS: u32 = 0x8000_0000;
    /// The machine must not enter idle sleep.
    pub const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

    /// `LOAD_LIBRARY_SEARCH_SYSTEM32` — resolve DLLs from System32 only.
    pub const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    pub const WAIT_OBJECT_0: u32 = 0;

    /// `MOVEFILE_WRITE_THROUGH` — return only after the move is on disk.
    pub const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    /// Job object limit flags (`JOBOBJECT_BASIC_LIMIT_INFORMATION::LimitFlags`).
    pub const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0008;
    pub const JOB_OBJECT_LIMIT_JOB_MEMORY: u32 = 0x0200;
    pub const JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0400;
    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    /// `JobObjectExtendedLimitInformation` (JOBOBJECTINFOCLASS).
    pub const EXTENDED_LIMIT_INFORMATION: i32 = 9;
    /// `JobObjectAssociateCompletionPortInformation` (JOBOBJECTINFOCLASS).
    pub const ASSOCIATE_COMPLETION_PORT_INFORMATION: i32 = 7;
    /// `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` — the message's "overlapped" value is
    /// the ID of the process that ran into the job's memory limit.
    pub const JOB_OBJECT_MSG_JOB_MEMORY_LIMIT: u32 = 10;
    pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

    /// `JOBOBJECT_ASSOCIATE_COMPLETION_PORT`.
    #[repr(C)]
    pub struct AssociateCompletionPort {
        pub completion_key: usize,
        pub completion_port: Handle,
    }

    pub type Handle = *mut core::ffi::c_void;

    /// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    pub struct BasicLimitInformation {
        pub per_process_user_time_limit: i64,
        pub per_job_user_time_limit: i64,
        pub limit_flags: u32,
        pub minimum_working_set_size: usize,
        pub maximum_working_set_size: usize,
        pub active_process_limit: u32,
        pub affinity: usize,
        pub priority_class: u32,
        pub scheduling_class: u32,
    }

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` (`IO_COUNTERS` flattened).
    #[repr(C)]
    #[derive(Default)]
    pub struct ExtendedLimitInformation {
        pub basic: BasicLimitInformation,
        pub io_counters: [u64; 6],
        pub process_memory_limit: usize,
        pub job_memory_limit: usize,
        pub peak_process_memory_used: usize,
        pub peak_job_memory_used: usize,
    }

    /// `SYSTEMTIME`.
    #[repr(C)]
    #[derive(Default)]
    pub struct SystemTime {
        pub year: u16,
        pub month: u16,
        pub day_of_week: u16,
        pub day: u16,
        pub hour: u16,
        pub minute: u16,
        pub second: u16,
        pub milliseconds: u16,
    }

    /// `MEMORYSTATUSEX` (the fields after `ullTotalPhys` are not needed).
    #[repr(C)]
    pub struct MemoryStatus {
        pub length: u32,
        pub memory_load: u32,
        pub total_phys: u64,
        pub rest: [u64; 6],
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetUserDefaultUILanguage() -> u16;
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        pub fn CloseHandle(handle: Handle) -> i32;
        pub fn GetExitCodeProcess(handle: Handle, code: *mut u32) -> i32;
        pub fn GetLastError() -> u32;
        pub fn QueryFullProcessImageNameW(
            handle: Handle,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        pub fn SetThreadExecutionState(flags: u32) -> u32;
        pub fn SetDefaultDllDirectories(flags: u32) -> i32;
        pub fn CreateJobObjectW(attributes: *mut core::ffi::c_void, name: *const u16) -> Handle;
        pub fn SetInformationJobObject(
            job: Handle,
            class: i32,
            info: *const core::ffi::c_void,
            len: u32,
        ) -> i32;
        pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        pub fn CreateIoCompletionPort(file: Handle, existing: Handle, key: usize, threads: u32) -> Handle;
        pub fn GetQueuedCompletionStatus(
            port: Handle,
            bytes: *mut u32,
            key: *mut usize,
            overlapped: *mut usize,
            milliseconds: u32,
        ) -> i32;
        pub fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        pub fn GetProcessTimes(
            process: Handle,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
        pub fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
        pub fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        pub fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
        pub fn GetLocalTime(time: *mut SystemTime);
    }
}

/// `text` (a path too) as a NUL-terminated UTF-16 string for the `W`
/// functions.
pub fn wide(text: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Make every DLL this process loads at RUN time come from System32 only.
///
/// The link-time imports are already covered by `/DEPENDENTLOADFLAG:0x800`
/// from `build.rs`; this closes the other door — a `LoadLibrary("x.dll")` by
/// name, made by the app or by a system component on its behalf, would
/// otherwise look in the app's folder first, and a portable app typically runs
/// from Downloads, where anything can land. Call it first thing in `main`.
///
/// Best effort: on failure the process keeps the standard search order.
pub fn harden_dll_search() -> bool {
    unsafe { sys::SetDefaultDllDirectories(sys::LOAD_LIBRARY_SEARCH_SYSTEM32) != 0 }
}

/// Keeps the computer from falling asleep on its own while this guard is alive.
///
/// Held only while a job runs: a long batch can take hours, and Windows would
/// otherwise hit its idle timer and suspend the machine mid-write. The DISPLAY
/// is deliberately left alone — the screen may blank as usual, only the system
/// stays awake.
///
/// This is a request, not a lock: an explicit Sleep from the Start menu, a
/// closing laptop lid or a critical battery still put the machine down.
///
/// Windows tracks the request per thread and drops it when that thread exits,
/// so the guard must be created and released on the same thread, and that
/// thread has to outlive the work being protected.
///
/// The private field keeps [`StayAwake::begin`] the only way to make one, so a
/// guard can never be dropped without having taken the request first.
pub struct StayAwake(());

impl StayAwake {
    /// Ask the system to stay awake until the returned guard is dropped.
    pub fn begin() -> Self {
        unsafe {
            sys::SetThreadExecutionState(sys::ES_CONTINUOUS | sys::ES_SYSTEM_REQUIRED);
        }
        StayAwake(())
    }
}

impl Drop for StayAwake {
    fn drop(&mut self) {
        // `ES_CONTINUOUS` alone clears the requirement and restores normal
        // power management.
        unsafe {
            sys::SetThreadExecutionState(sys::ES_CONTINUOUS);
        }
    }
}

/// The Windows folder (`%SystemRoot%`, else `C:\Windows`): a program of
/// Windows is started by its full path under it, never by a bare name.
pub fn windows_dir() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// The full path of `program` in System32.
pub fn system32(program: &str) -> PathBuf {
    windows_dir().join("System32").join(program)
}

/// A `Command` for a background helper (QPDF): no console window ever flashes
/// and the process runs below normal priority, so a long batch never makes the
/// desktop sluggish. `program` must be a full path: a bare name is looked
/// up in the app's own folder first.
pub fn background_command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

/// Owns a child process so it is always killed and reaped, even on an early
/// return (cancellation / error) — no orphan processes are left.
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait up to `milliseconds` for `child` to end; `true` once it has.
///
/// Unlike polling `try_wait` with a sleep, a quick process (a ~64 ms probe,
/// is noticed the moment it exits.
pub fn wait_for_exit(child: &Child, milliseconds: u32) -> bool {
    unsafe { sys::WaitForSingleObject(child.as_raw_handle(), milliseconds) == sys::WAIT_OBJECT_0 }
}

/// Processor time (kernel + user) `child` has used so far, in 100 ns units;
/// `None` if it cannot be read. Feeds the hang watchdog.
pub fn cpu_time(child: &Child) -> Option<u64> {
    let (mut creation, mut exit, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
    let ok = unsafe {
        sys::GetProcessTimes(child.as_raw_handle(), &mut creation, &mut exit, &mut kernel, &mut user)
    };
    (ok != 0).then(|| kernel.saturating_add(user))
}

/// Installed physical memory in bytes; `None` if Windows does not say.
pub fn total_memory() -> Option<u64> {
    let mut status = sys::MemoryStatus {
        length: std::mem::size_of::<sys::MemoryStatus>() as u32,
        memory_load: 0,
        total_phys: 0,
        rest: [0; 6],
    };
    let ok = unsafe { sys::GlobalMemoryStatusEx(&mut status) };
    (ok != 0 && status.total_phys > 0).then_some(status.total_phys)
}

/// The language of the Windows user interface, as a LANGID (`0x0415` Polish,
/// `0x0409` US English): what the window speaks unless `settings.txt` says
/// otherwise.
pub fn ui_language() -> u16 {
    // SAFETY: no arguments, no failure.
    unsafe { sys::GetUserDefaultUILanguage() }
}

/// Bytes free for this user on the volume holding `dir`; `None` when
/// unknown — then the caller does not block the work on a guess.
pub fn free_disk_space(dir: &Path) -> Option<u64> {
    let mut free = 0u64;
    let ok = unsafe {
        sys::GetDiskFreeSpaceExW(
            wide(dir).as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

/// Rename `from` to `to`, refusing to replace an existing `to`.
///
/// `std::fs::rename` cannot be used: on Windows it always replaces. Here an
/// existing target fails with `ErrorKind::AlreadyExists`, so a file that
/// appeared after the name was chosen is never overwritten.
pub fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let ok = unsafe {
        sys::MoveFileExW(wide(from).as_ptr(), wide(to).as_ptr(), sys::MOVEFILE_WRITE_THROUGH)
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Mark `path` as coming from the Internet — the Mark-of-the-Web, a
/// `Zone.Identifier` stream with `ZoneId=3` — so Windows and Office warn
/// before opening it (an attachment pulled out of a PDF). Fails on a volume
/// without alternate streams (FAT, exFAT).
pub fn mark_of_the_web(path: &Path) -> io::Result<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(stream, b"[ZoneTransfer]\r\nZoneId=3\r\n")
}

/// The local date and time as `2026-10-10 17:45` (for a report).
pub fn local_time() -> String {
    let mut now = sys::SystemTime::default();
    unsafe { sys::GetLocalTime(&mut now) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", now.year, now.month, now.day, now.hour, now.minute)
}

/// The one Job Object holding every QPDF process of the app:
///
/// - a COMBINED memory limit for all of them;
/// - a cap on how many may run at once — an extra one is not let in, and QPDF
///   cannot start anything further;
/// - closing the job (dropping this, or the app ending or crashing) kills them;
/// - a crash ends the process at once instead of opening the Windows error
///   dialog, which would keep it alive until someone clicks it away.
///
/// Running out of the memory limit cannot be told from QPDF's own output: a
/// failed heap allocation is reported as a damaged file ("unable to find
/// trailer dictionary while recovering damaged file", exit 2), a failed stack
/// one as a crash (0xC00000FD), both without a word about memory (measured on
/// 12.4.2). So the job reports it through a completion port, naming the
/// process — see [`JobObject::hit_memory_limit`].
pub struct JobObject {
    handle: sys::Handle,
    port: sys::Handle,
    /// Processes reported to have hit the memory limit, not yet asked about.
    memory_hits: std::sync::Mutex<Vec<u32>>,
}

// The handle is only ever passed to thread-safe kernel calls.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    /// A new job with a combined `memory_limit` (bytes) and room for
    /// `max_processes` running at once.
    pub fn new(memory_limit: u64, max_processes: u32) -> io::Result<JobObject> {
        let handle = unsafe { sys::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Owned from here on, so every early return closes the handles.
        let mut job = JobObject {
            handle,
            port: std::ptr::null_mut(),
            memory_hits: std::sync::Mutex::new(Vec::new()),
        };
        job.port = unsafe {
            sys::CreateIoCompletionPort(sys::INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1)
        };
        if job.port.is_null() {
            return Err(io::Error::last_os_error());
        }
        let association = sys::AssociateCompletionPort { completion_key: 0, completion_port: job.port };
        let ok = unsafe {
            sys::SetInformationJobObject(
                job.handle,
                sys::ASSOCIATE_COMPLETION_PORT_INFORMATION,
                (&association as *const sys::AssociateCompletionPort).cast(),
                std::mem::size_of::<sys::AssociateCompletionPort>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut info = sys::ExtendedLimitInformation::default();
        info.basic.limit_flags = sys::JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | sys::JOB_OBJECT_LIMIT_JOB_MEMORY
            | sys::JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | sys::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.basic.active_process_limit = max_processes.max(1);
        info.job_memory_limit = usize::try_from(memory_limit).unwrap_or(usize::MAX);
        let ok = unsafe {
            sys::SetInformationJobObject(
                job.handle,
                sys::EXTENDED_LIMIT_INFORMATION,
                (&info as *const sys::ExtendedLimitInformation).cast(),
                std::mem::size_of::<sys::ExtendedLimitInformation>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Put `child` into the job. Fails — and Windows ends the process — when
    /// the job already runs its maximum number of processes.
    pub fn assign(&self, child: &Child) -> io::Result<()> {
        if unsafe { sys::AssignProcessToJobObject(self.handle, child.as_raw_handle()) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Whether process `pid` ran into the memory limit. Ask once, after the
    /// process has ended: the answer is forgotten, so a reused PID starts
    /// clean.
    pub fn hit_memory_limit(&self, pid: u32) -> bool {
        let mut hits = self.memory_hits.lock().unwrap_or_else(|e| e.into_inner());
        // Drain everything queued so far; other messages (process started,
        // ended…) are of no interest.
        loop {
            let (mut message, mut key, mut value) = (0u32, 0usize, 0usize);
            let got = unsafe {
                sys::GetQueuedCompletionStatus(self.port, &mut message, &mut key, &mut value, 0)
            };
            if got == 0 {
                break;
            }
            if message == sys::JOB_OBJECT_MSG_JOB_MEMORY_LIMIT {
                hits.push(value as u32);
            }
        }
        let hit = hits.contains(&pid);
        hits.retain(|&p| p != pid);
        hit
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        unsafe {
            sys::CloseHandle(self.handle);
            if !self.port.is_null() {
                sys::CloseHandle(self.port);
            }
        }
    }
}

/// Whether `pid` is a running copy of THIS program.
///
/// A PID alone is not enough: `app.lock` outlives a crash or a power cut, and
/// after a restart its PID is soon reused by some other program — which would
/// keep the app from starting. So the process must also run our executable.
pub fn pid_is_this_app(pid: u32) -> bool {
    use std::os::windows::ffi::OsStringExt;
    unsafe {
        let handle = sys::OpenProcess(sys::QUERY_LIMITED, 0, pid);
        if handle.is_null() {
            // Access denied means the process EXISTS but is protected; it cannot
            // be inspected, so it is conservatively taken for a live instance.
            return sys::GetLastError() == sys::ERROR_ACCESS_DENIED;
        }
        // Opened, but it may be a zombie: only STILL_ACTIVE means running.
        let mut code = 0u32;
        let running = sys::GetExitCodeProcess(handle, &mut code) != 0 && code == sys::STILL_ACTIVE;
        let mut name = vec![0u16; 32_768];
        let mut len = name.len() as u32;
        let image = (sys::QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut len) != 0)
            .then(|| std::ffi::OsString::from_wide(&name[..len as usize]));
        sys::CloseHandle(handle);
        // Anything that cannot be compared counts as ours (the safe side).
        let ours = match (image, std::env::current_exe()) {
            (Some(image), Ok(exe)) => {
                image.to_string_lossy().to_lowercase()
                    == exe.as_os_str().to_string_lossy().to_lowercase()
            }
            _ => true,
        };
        running && ours
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_process_is_alive() {
        assert!(pid_is_this_app(std::process::id()));
    }

    #[test]
    fn windows_names_its_language() {
        // A primary language id is never 0 (LANG_NEUTRAL) on a real desktop.
        assert_ne!(ui_language() & 0x3ff, 0);
    }

    #[test]
    fn an_impossible_pid_is_not_alive() {
        // Never a real PID, so this must not report "running".
        assert!(!pid_is_this_app(u32::MAX - 1));
    }

    #[test]
    fn stay_awake_can_be_taken_and_released() {
        // The effect cannot be asserted from inside the process, but the FFI
        // round-trip and the release-on-drop path must both be exercised.
        let guard = StayAwake::begin();
        drop(guard);
        // Taking it twice in a row must also be harmless.
        let _first = StayAwake::begin();
        let _second = StayAwake::begin();
    }

    #[test]
    fn job_structures_match_the_windows_x64_layout() {
        // Sizes from the Windows SDK headers; a wrong layout would silently
        // set the wrong limits.
        assert_eq!(std::mem::size_of::<sys::BasicLimitInformation>(), 64);
        assert_eq!(std::mem::size_of::<sys::ExtendedLimitInformation>(), 144);
        assert_eq!(std::mem::size_of::<sys::AssociateCompletionPort>(), 16);
        assert_eq!(std::mem::size_of::<sys::MemoryStatus>(), 64);
        assert_eq!(std::mem::size_of::<sys::SystemTime>(), 16);
    }

    #[test]
    fn a_job_object_can_be_made_and_asked() {
        let job = JobObject::new(1 << 30, 2).expect("job");
        assert!(!job.hit_memory_limit(std::process::id()));
    }

    #[test]
    fn memory_and_disk_space_are_known() {
        assert!(total_memory().is_some_and(|bytes| bytes > 512 << 20));
        assert!(free_disk_space(&std::env::temp_dir()).is_some());
        assert!(free_disk_space(Path::new(r"Q:\no\such\drive")).is_none());
    }

    #[test]
    fn move_no_replace_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("qd_move_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let (from, to) = (dir.join("from.tmp"), dir.join("to.pdf"));
        std::fs::write(&from, b"new").expect("write");
        std::fs::write(&to, b"precious").expect("write");
        let err = move_no_replace(&from, &to).expect_err("must refuse");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&to).expect("read"), b"precious");
        std::fs::remove_file(&to).expect("remove");
        move_no_replace(&from, &to).expect("free name");
        assert_eq!(std::fs::read(&to).expect("read"), b"new");
        assert!(!from.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mark_of_the_web_is_an_alternate_stream() {
        let dir = std::env::temp_dir().join(format!("qd_motw_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"content").expect("write");
        mark_of_the_web(&file).expect("NTFS");
        let zone = std::fs::read_to_string(dir.join("a.txt:Zone.Identifier")).expect("stream");
        assert_eq!(zone, "[ZoneTransfer]\r\nZoneId=3\r\n");
        assert_eq!(std::fs::read(&file).expect("read"), b"content", "the file itself is untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_time_reads_like_a_date() {
        let now = local_time();
        assert_eq!(now.len(), "2026-10-10 17:45".len(), "{now}");
        assert!(now.starts_with("20") && now.as_bytes()[4] == b'-' && now.as_bytes()[10] == b' ', "{now}");
    }

    #[test]
    fn dll_search_can_be_restricted_to_system32() {
        assert!(harden_dll_search(), "SetDefaultDllDirectories is present since Windows 8");
    }
}
