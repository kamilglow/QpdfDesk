//! While QPDF works with passwords,
//! its command line — what any other process can read — is only
//! `"…\qpdf.exe" @-`. In a file of its own: it looks at every `qpdf.exe`
//! this test process starts, so no other test may run QPDF beside it (in
//! runner.rs a parallel test's QPDF made it fail once).

mod common;

use common::corpus::{self, Item};
use common::{job_object as job, secret};
use qpdf_desk::core::runner::{run, Arg, Context, Ending};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Command lines of the running `qpdf.exe` children of this process — read the
/// way Process Explorer reads them (`ProcessCommandLineInformation`).
fn qpdf_command_lines() -> Vec<String> {
    type Handle = *mut core::ffi::c_void;
    #[repr(C)]
    struct ProcessEntry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(process: Handle, class: u32, info: *mut u8, len: u32, returned: *mut u32) -> i32;
    }
    let mut lines = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(0x2, 0);
        let mut entry: ProcessEntry = std::mem::zeroed();
        entry.size = std::mem::size_of::<ProcessEntry>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let name_len = entry.exe.iter().position(|&c| c == 0).unwrap_or(260);
            let name = String::from_utf16_lossy(&entry.exe[..name_len]);
            if entry.parent == std::process::id() && name.eq_ignore_ascii_case("qpdf.exe") {
                let process = OpenProcess(0x1000, 0, entry.pid);
                let mut buf = vec![0u8; 65_536];
                let status = NtQueryInformationProcess(process, 60, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut());
                assert_eq!(status, 0, "NtQueryInformationProcess");
                // UNICODE_STRING { Length, MaximumLength, Buffer } at the start.
                let len = u16::from_ne_bytes([buf[0], buf[1]]) as usize / 2;
                let text = *(buf.as_ptr().add(8) as *const *const u16);
                lines.push(String::from_utf16_lossy(std::slice::from_raw_parts(text, len)));
                CloseHandle(process);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    lines
}

#[test]
fn no_password_is_visible_on_the_command_line() {
    // While QPDF works with passwords, its command line —
    // what any other process can read — is only `qpdf.exe @-`.
    let Some(exe) = common::qpdf() else { return };
    let (job, cancel) = (job(), AtomicBool::new(false));
    let ctx = Context::new(&job, &cancel);
    let out = common::scratch("command-line").join("out.pdf");
    let args = [
        Arg::plain("--encrypt"),
        Arg::Secret("--user-password", secret("TopSecretUser")),
        Arg::Secret("--owner-password", secret("TopSecretOwner")),
        Arg::plain("--bits=256"),
        Arg::plain("--"),
        Arg::path(corpus::item(Item::Heavy)),
        Arg::path(&out),
    ];
    let seen = std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            let mut seen = Vec::new();
            let until = Instant::now() + Duration::from_secs(10);
            while seen.is_empty() && Instant::now() < until {
                seen = qpdf_command_lines();
            }
            cancel.store(true, Ordering::Relaxed);
            seen
        });
        let done = run(&exe, &args, &ctx).expect("run");
        assert_eq!(done.ending, Ending::Cancelled, "QPDF finished before it was looked at");
        watcher.join().expect("watcher")
    });
    assert_eq!(seen, [format!("\"{}\" @-", exe.display())]);
}
