//! Command line front-end for the `api-fingerprint` library.
//!
//! ```text
//! api-fingerprint <file.rs>                print the interface report
//! api-fingerprint diff <old.rs> <new.rs>   diff two interface versions
//! ```
//!
//! Exit codes: 0 = no known changes and no unknown boundaries,
//! 1 = known interface changes detected, 2 = unknown boundaries present
//! (compatibility cannot be certified).

use api_fingerprint::{diff, fingerprint_source, ChangeKind, Report};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [file] => match load(file) {
            Ok(report) => {
                print_report(&report);
                if report.unknown().next().is_some() {
                    eprintln!(
                        "note: {} unknown export boundar(y/ies); interface is not fully determined",
                        report.unknown().count()
                    );
                    ExitCode::from(2)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
        [cmd, old, new] if cmd == "diff" => match (load(old), load(new)) {
            (Ok(old_report), Ok(new_report)) => {
                let d = diff(&old_report, &new_report);
                print_diff(old, new, &d);
                if !d.unknowns.is_empty() {
                    ExitCode::from(2)
                } else if !d.changes.is_empty() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            (Err(msg), _) | (_, Err(msg)) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage:");
            eprintln!("  api-fingerprint <file.rs>              print the interface report");
            eprintln!("  api-fingerprint diff <old.rs> <new.rs> diff two interface versions");
            ExitCode::FAILURE
        }
    }
}

fn load(path: &str) -> Result<Report, String> {
    let source =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    fingerprint_source(&source).map_err(|e| format!("cannot parse {path}: {e}"))
}

fn print_report(report: &Report) {
    for e in &report.entries {
        let mark = if e.is_unknown() { "!" } else { " " };
        println!(
            "{} [{:<7}] {:<44} {}  @{}:{}",
            mark,
            e.kind.to_string(),
            e.path,
            &e.digest[..16],
            e.line,
            e.column
        );
        println!("      {}", e.signature);
        if let api_fingerprint::Status::Unknown(reason) = e.status {
            println!("      ^ unknown: {reason}");
        }
    }
    println!(
        "-- {} entries ({} known, {} unknown)",
        report.entries.len(),
        report.known().count(),
        report.unknown().count()
    );
}

fn print_diff(old_path: &str, new_path: &str, d: &api_fingerprint::Diff) {
    println!("--- {old_path}");
    println!("+++ {new_path}");
    for change in &d.changes {
        let e = change.entry();
        match change.kind {
            ChangeKind::Added => {
                println!("+ [{}] {}  @{}:{}", e.kind, e.path, e.line, e.column);
                println!("    {}", e.signature);
            }
            ChangeKind::Removed => {
                println!("- [{}] {}  @{}:{}", e.kind, e.path, e.line, e.column);
                println!("    {}", e.signature);
            }
            ChangeKind::Changed => {
                let (o, n) = (change.old.unwrap(), change.new.unwrap());
                println!(
                    "~ [{}] {}  ({} -> {})  old@{}:{} new@{}:{}",
                    e.kind,
                    e.path,
                    &o.digest[..12],
                    &n.digest[..12],
                    o.line,
                    o.column,
                    n.line,
                    n.column
                );
                println!("    - {}", o.signature);
                println!("    + {}", n.signature);
            }
        }
    }
    if !d.unknowns.is_empty() {
        println!("! unknown export boundaries (compatibility cannot be certified):");
        for e in &d.unknowns {
            if let api_fingerprint::Status::Unknown(reason) = e.status {
                println!("! [{}] {}  @{}:{}  {}", e.kind, e.path, e.line, e.column, reason);
            }
        }
    }
    let (mut added, mut removed, mut changed) = (0, 0, 0);
    for c in &d.changes {
        match c.kind {
            ChangeKind::Added => added += 1,
            ChangeKind::Removed => removed += 1,
            ChangeKind::Changed => changed += 1,
        }
    }
    println!(
        "-- {added} added, {removed} removed, {changed} changed, {} unknown",
        d.unknowns.len()
    );
    if d.is_clean() {
        println!("result: IDENTICAL (public interface unchanged)");
    } else if d.changes.is_empty() {
        println!("result: UNKNOWN (no known changes, but unknown boundaries exist)");
    } else {
        println!("result: CHANGED");
    }
}
