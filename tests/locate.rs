//! `core::qpdf::locate` with real programs.

mod common;

use qpdf_desk::core::qpdf::{default_candidates, locate, Missing, MIN_VERSION};

#[test]
fn the_development_copy_of_qpdf_is_found_and_new_enough() {
    if common::qpdf().is_none() {
        return;
    }
    let job = common::job_object();
    let found = locate(&default_candidates(), &job).expect("a usable QPDF");
    assert!(found.version >= MIN_VERSION, "{found:?}");
    assert!(found.exe.is_absolute());
}

#[test]
fn a_program_that_is_not_qpdf_is_unusable() {
    let job = common::job_object();
    let cmd = qpdf_desk::platform::system32("cmd.exe");
    assert_eq!(locate(std::slice::from_ref(&cmd), &job), Err(Missing::Unusable(cmd)));
    assert_eq!(locate(&[], &job), Err(Missing::NotFound));
}
