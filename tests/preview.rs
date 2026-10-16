//! The command preview with the real QPDF and cmd.exe: the command pasted
//! into cmd makes the very same file as Start (`--deterministic-id`), also for
//! names cmd.exe would otherwise take apart.

mod common;

use common::corpus::Item;
use common::range;
use qpdf_desk::core::job::{Files, Input, Job, Optimize, Output, Pages, Privacy, Select};
use qpdf_desk::core::runner::Context;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;

/// Run `line` as if typed into cmd.exe (by its full path); its exit code.
fn cmd(line: &str) -> Option<i32> {
    // `/s /c "…"`: cmd.exe drops the outer quotes and runs the rest as typed.
    std::process::Command::new(qpdf_desk::platform::system32("cmd.exe"))
        .raw_arg(format!("/d /s /c \"{line}\""))
        .stdout(Stdio::null())
        .status()
        .expect("cmd.exe")
        .code()
}

/// What Start makes of `files` (the inputs in order), and the command shown
/// for them right after.
fn start_and_command(exe: &Path, job: &Job, files: &[(PathBuf, u64)]) -> (PathBuf, String) {
    assert_eq!(job.check(), Ok(()));
    let inputs: Vec<Input> =
        files.iter().map(|(path, size)| Input { path, size: *size, probe: &common::BASE, range: None, attachments: None }).collect();
    let (job_object, cancel) = (common::job_object(), AtomicBool::new(false));
    let done = job.produce(exe, &inputs, &Context::new(&job_object, &cancel)).remove(0);
    assert!(done.verdict.succeeded(), "{done:?}");
    (done.output.expect("a result"), job.command(exe, &inputs).expect("a command"))
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_command_makes_the_same_file_as_start() {
    for exe in common::qpdf_builds() {
        // `&` and parentheses: cmd.exe's own characters, in the folder and the name.
        let dir = common::scratch("preview R&D");
        let file = common::copy_item(Item::Base, &dir, "R&D (draft).pdf");
        let job = Job {
            pages: Some(Pages { select: Select::Keep(range("3,1")), files: Files::Each, rotate: None }),
            privacy: Some(Privacy::default()),
            optimize: Some(Optimize::default()),
            ..Job::default()
        };
        let (started, line) = start_and_command(&exe, &job, &[file]);
        assert_eq!(started, dir.join("R&D (draft) (processed).pdf"));
        // Start's result is there: the command takes the next free name.
        let pasted = dir.join("R&D (draft) (processed) (2).pdf");
        assert!(line.ends_with(&format!("\"{}\"", pasted.display())), "{line}");
        assert_eq!(cmd(&line), Some(0), "{line}");
        assert!(bytes(&started) == bytes(&pasted), "the same bytes: {line}");
    }
}

#[test]
fn a_merged_split_result_is_the_same_part_by_part() {
    let Some(exe) = common::qpdf() else { return };
    let dir = common::scratch("preview merge");
    let files = [common::copy_item(Item::Base, &dir, "a.pdf"), common::copy_item(Item::Base, &dir, "b.pdf")];
    let out = dir.join("out folder");
    std::fs::create_dir(&out).expect("out");
    let job = Job {
        pages: Some(Pages { select: Select::All, files: Files::Merge, rotate: None }),
        privacy: Some(Privacy::default()),
        output: Some(Output { split: Some(4), folder: Some(out.clone()), name: Some("Komplet".into()), ..Output::default() }),
        ..Job::default()
    };
    let (started, line) = start_and_command(&exe, &job, &files);
    assert_eq!(started, out.join("Komplet"));
    let pasted = out.join("Komplet (2)");
    assert!(line.starts_with(&format!("mkdir \"{}\" && ", pasted.display())), "{line}");
    assert_eq!(cmd(&line), Some(0), "{line}");
    let mut parts = common::others(&started, &[]);
    parts.sort();
    assert_eq!(parts, ["Komplet-1-4.pdf", "Komplet-5-6.pdf"]);
    for part in parts {
        assert!(bytes(&started.join(&part)) == bytes(&pasted.join(&part)), "{part}: {line}");
    }
}
