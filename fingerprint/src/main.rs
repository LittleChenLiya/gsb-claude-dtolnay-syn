//! Command-line front end for the api-fingerprint library.
//!
//!   api-fingerprint <file.rs>                print the interface report
//!   api-fingerprint diff <old.rs> <new.rs>   show the interface diff
//!
//! Example (from the repository root, well under 90 seconds):
//!
//!   cargo run -p api-fingerprint -- diff fingerprint/fixtures/v1.rs fingerprint/fixtures/v2.rs

use std::process::ExitCode;

const USAGE: &str = "\
usage:
  api-fingerprint <file.rs>                print the interface report and SHA-256 digest
  api-fingerprint diff <old.rs> <new.rs>   show the interface diff between two versions";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [file] => run_report(file),
        [cmd, old, new] if cmd == "diff" => run_diff(old, new),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn fingerprint_path(path: &str) -> Result<api_fingerprint::Fingerprint, String> {
    let source =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    api_fingerprint::fingerprint_source(&source)
        .map_err(|e| format!("cannot parse {path}: {e}"))
}

fn run_report(path: &str) -> ExitCode {
    match fingerprint_path(path) {
        Ok(fp) => {
            print!("{}", fp.report());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_diff(old_path: &str, new_path: &str) -> ExitCode {
    let old = match fingerprint_path(old_path) {
        Ok(fp) => fp,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    let new = match fingerprint_path(new_path) {
        Ok(fp) => fp,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    println!("old digest: {}", old.digest);
    println!("new digest: {}", new.digest);
    println!();
    let diff = api_fingerprint::diff(&old, &new);
    print!("{diff}");
    if diff.is_definitely_compatible() {
        ExitCode::SUCCESS
    } else {
        // Either the interface changed, or unknown boundaries (macros,
        // external re-exports, ...) prevent a definite compatibility claim.
        ExitCode::from(1)
    }
}
