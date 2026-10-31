//! `strict_compare A.mid B.mid [--256]` — do two MIDI files hold the same
//! events in the same order, track by track? (v1.0-beta1 B1-0125,
//! `midi::compare`; streaming, for the owner's 6M / 40M-note pairs.)
//! Exit code 0: the same events; 1: they differ; 2: a file cannot be read.

use plumspirit_engine::midi::compare::compare_files;
use plumspirit_engine::model::project::KeyRange;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if files.len() != 2 {
        eprintln!("usage: strict_compare A.mid B.mid [--256]");
        return ExitCode::from(2);
    }
    let keys = if args.iter().any(|a| a == "--256") {
        KeyRange::Extended256
    } else {
        KeyRange::Standard
    };
    let started = std::time::Instant::now();
    match compare_files(Path::new(files[0]), Path::new(files[1]), keys) {
        Ok(r) => {
            for l in r.lines() {
                println!("{l}");
            }
            println!("({:.2} s)", started.elapsed().as_secs_f64());
            if r.same_events {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("{}", e.user_message());
            ExitCode::from(2)
        }
    }
}
