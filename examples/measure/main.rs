//! Measures sessidx on this machine's own session logs, for a real-corpus claim
//! in a pull request or a release:
//!
//! ```sh
//! cargo build --release
//! cargo run --release --example measure -- --candidate target/release/sessidx \
//!     --baseline "$BASE/target/release/sessidx" --scratch "$(mktemp -d)" > "$OUT/measure.jsonl"
//! ```
//!
//! where `$BASE` is a release build of the base branch for a pull request, or
//! the previous release for a release. A full history takes tens of minutes,
//! longer than many agents' default shell timeout, so run it in the background.
//! The scratch directory then holds two full indexes of the real logs, redacted
//! text included; delete it when done.
//!
//! Each binary builds its own scratch index from the real roots under
//! `/usr/bin/time -l`, the candidate first. The workload is each verb's most
//! recent invocations that this index itself recorded with a tool result that
//! is not a recognized denial, replayed unchanged, so it follows how sessidx is
//! used rather than what a measurer picks; a result shows that the shell line
//! ran, not that this invocation in it did. Each transcript supplies at most
//! one, its most recent that a more recent transcript has not already
//! supplied, since a session that retries a query with small changes would
//! otherwise fill a verb's sample by itself. An untimed run of each command per
//! binary drops the command when a binary exits 2 within the two-second query
//! budget, having refused the invocation before doing the work, as for an
//! option renamed since it was recorded. A run that exits 2 at or past the
//! budget ran out of it, so its command is kept and timed. A refresh that
//! spends its own budget first, as when another process writes session logs,
//! can make a refused invocation look like one that ran out; `exit_2` then
//! shows it.
//!
//! Each command runs `--runs` times per binary, the binaries alternating so that
//! drift falls on both. In the `warm` condition every command runs with nothing
//! new to index; in the `append` condition the most recent command of each
//! refreshing verb runs with records appended to a synthetic transcript before
//! every pair, as when an agent queries while its own transcript grows. Run it
//! from an ordinary shell on an otherwise idle machine: agent sandboxes break
//! `/usr/bin/time -l`, and anything else writing session logs shows up in the
//! refresh tallies.
//!
//! The JSON Lines report never holds a command's text, a path, or a session ID,
//! so it can be pasted into a pull request:
//!
//! - `header`: the machine, the run's settings, and each binary's version and
//!   SHA-256.
//! - `build` per binary: transcripts, records, database bytes, and the wall
//!   seconds and peak RSS of one full build. The transcripts and records differ
//!   between the binaries only by what was written in between, unless one
//!   indexes differently. The seconds and peak RSS are single samples to judge
//!   against the targets, not against each other: the second build runs on a
//!   page cache the first warmed, and peak RSS counts resident file pages, so
//!   the same binary's can differ widely between two builds. Swap `--candidate`
//!   and `--baseline` to see the order's effect.
//! - `workload` per verb: the commands timed, one per transcript; the
//!   transcripts and invocations recorded in the window, of which `unreplayable`
//!   invocations cannot be replayed faithfully; and how many commands each
//!   binary refused, a command any binary refused being dropped.
//! - `timing` per condition, verb, and binary: p50, nearest-rank p95, and
//!   maximum seconds over every run of the verb's commands, runs that ran out
//!   of the budget included; each command's p50 in workload order, the same
//!   order for every binary; the runs that exited 0, 1, and 2; the runs whose
//!   answer was not complete, a page with a cursor to continue included, since
//!   `grep` returns one both at its limit and at its deadline; the stale runs;
//!   and the runs that refreshed something, with the records they indexed.
//! - `agreement` per verb when a baseline is given: over the warm runs, paired
//!   by run number so that each pair ran back to back, the commands whose data
//!   records differed in more than half of the pairs, and those the baseline
//!   answered completely and the candidate did not in more than half, so that
//!   doing less cannot pass for being faster while one run that ran out of the
//!   budget, which `exit_2` and `incomplete` show, does not pass for a
//!   difference. A difference is a lead, not a verdict: a `sql` query without
//!   `ORDER BY`, or a page both binaries cut at their deadline, can
//!   legitimately differ.
//! - `end`: the load average and the run's seconds.

mod timing;
mod workload;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;
use timing::{Run, agreement, finished, time, timing, validate};
use workload::workload;

/// Synthetic records appended before each pair of runs in the `append` condition.
const APPENDED: usize = 20;

#[derive(Parser)]
struct Args {
    /// Binary to measure
    #[arg(long, value_name = "PATH")]
    candidate: PathBuf,
    /// Binary to compare with, run alternately with the candidate on its own index
    #[arg(long, value_name = "PATH")]
    baseline: Option<PathBuf>,
    /// Empty directory for the scratch indexes and the synthetic transcript
    #[arg(long, value_name = "DIR")]
    scratch: PathBuf,
    /// Timed runs of each command per binary
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(20..))]
    runs: u32,
    /// Take commands recorded at or after this UTC date or RFC 3339 timestamp
    #[arg(long, value_name = "TIME", value_parser = sessidx::query::date)]
    since: Option<DateTime<Utc>>,
    /// Take commands recorded before this UTC date or RFC 3339 timestamp, such as a window a change was not tuned on
    #[arg(long, value_name = "TIME", value_parser = sessidx::query::date)]
    until: Option<DateTime<Utc>>,
    /// Session root to index instead of the defaults; repeat for each root
    #[arg(long = "root", value_name = "AGENT=PATH")]
    roots: Vec<String>,
}

struct Binary {
    name: &'static str,
    path: PathBuf,
    db: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        fs::read_dir(&args.scratch)
            .with_context(|| format!("cannot read {}", args.scratch.display()))?
            .next()
            .is_none(),
        "{} is not empty",
        args.scratch.display()
    );
    let synthetic = args.scratch.join("synthetic");
    fs::create_dir(&synthetic)?;
    let transcript = synthetic.join("measure.jsonl");
    let mut appended = 0;
    append(&transcript, &mut appended, 1)?;
    let mut roots = if args.roots.is_empty() {
        let home = std::env::home_dir().context("home directory unknown")?;
        sessidx::discovery::defaults(&home)
            .into_iter()
            .map(|r| format!("{}={}", sessidx::model::name(r.agent), r.path.display()))
            .collect()
    } else {
        args.roots.clone()
    };
    roots.push(format!("claude={}", synthetic.display()));

    let mut binaries = vec![Binary {
        name: "candidate",
        path: args.candidate.clone(),
        db: args.scratch.join("candidate.db"),
    }];
    if let Some(path) = &args.baseline {
        binaries.push(Binary {
            name: "baseline",
            path: path.clone(),
            db: args.scratch.join("baseline.db"),
        });
    }
    let started = Instant::now();
    let mut identities = serde_json::Map::new();
    for b in &binaries {
        identities.insert(
            b.name.into(),
            json!({
                "version": output(&b.path, &["--version"])?,
                "sha256": hex::encode(Sha256::digest(fs::read(&b.path)?)),
            }),
        );
    }
    emit(json!({
        "type": "header",
        "date": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "commit": output("git", &["rev-parse", "HEAD"])?,
        "binaries": identities,
        "chip": output("sysctl", &["-n", "machdep.cpu.brand_string"])?,
        "cores": output("sysctl", &["-n", "hw.ncpu"])?.parse::<u32>()?,
        "memory_bytes": output("sysctl", &["-n", "hw.memsize"])?.parse::<u64>()?,
        "load_1m": load()?,
        "since": args.since,
        "until": args.until,
        "runs": args.runs,
        "appended_per_pair": APPENDED,
    }));

    for b in &binaries {
        emit(build(b, &roots)?);
    }

    let candidates = workload(&binaries[0].db, args.since, args.until)?;
    // Bring each index up to the logs written while the binaries built, outside
    // the refresh budget, so that the timed runs start from the same state.
    for b in &binaries {
        check(
            b,
            "index",
            &command(&b.path)
                .args(invocation(b, &roots, &["index".into()]))
                .output()?,
        )?;
    }
    let mut workload: Vec<(&str, Vec<Vec<String>>)> = Vec::new();
    for (verb, list) in candidates {
        let (kept, refused) = validate(binaries.len(), list.replayable, |b, argv| {
            run(&binaries[b], &roots, argv)
        })?;
        emit(json!({
            "type": "workload",
            "verb": verb,
            "commands": kept.len(),
            "transcripts": list.transcripts,
            "invocations": list.invocations,
            "unreplayable": list.unreplayable,
            "refused": binaries
                .iter()
                .zip(refused)
                .map(|(b, n)| (b.name.to_owned(), n.into()))
                .collect::<serde_json::Map<_, _>>(),
        }));
        workload.push((verb, kept));
    }

    for (verb, commands) in &workload {
        let runs = time(
            binaries.len(),
            commands,
            args.runs,
            || Ok(()),
            |b, argv| run(&binaries[b], &roots, argv),
        )?;
        for (b, per_command) in binaries.iter().zip(&runs) {
            emit(timing("warm", verb, b.name, per_command));
        }
        if let [candidate, baseline] = &runs[..] {
            emit(agreement(verb, candidate, baseline));
        }
    }
    for (verb, commands) in &workload {
        if *verb == "sql" || commands.is_empty() {
            continue;
        }
        let runs = time(
            binaries.len(),
            &commands[..1],
            args.runs,
            || append(&transcript, &mut appended, APPENDED),
            |b, argv| run(&binaries[b], &roots, argv),
        )?;
        for (b, per_command) in binaries.iter().zip(&runs) {
            emit(timing("append", verb, b.name, per_command));
        }
    }
    emit(json!({
        "type": "end",
        "load_1m": load()?,
        "seconds": started.elapsed().as_secs_f64(),
    }));
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the harness spawns the binaries it measures and runs alone"
)]
fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

fn output(program: impl AsRef<OsStr>, args: &[&str]) -> Result<String> {
    let program = program.as_ref();
    let out = command(program).args(args).output()?;
    ensure!(
        out.status.success(),
        "{} failed: {}",
        program.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// The 1-minute load average, context for a reader rather than proof of an idle machine.
fn load() -> Result<f64> {
    let text = output("sysctl", &["-n", "vm.loadavg"])?;
    let first = text.trim_start_matches('{').split_whitespace().next();
    Ok(first.context("unexpected vm.loadavg")?.parse()?)
}

fn emit(record: Value) {
    println!("{record}");
}

/// The arguments that run `args` on `binary`'s scratch index.
fn invocation(binary: &Binary, roots: &[String], args: &[String]) -> Vec<OsString> {
    let mut argv = vec!["--db".into(), binary.db.clone().into()];
    for r in roots {
        argv.extend(["--root".into(), r.into()]);
    }
    argv.extend(args.iter().map(Into::into));
    argv
}

/// Fails a maintenance command that did not exit 0 with its `end` record, or
/// with its stderr when it printed none.
fn check(binary: &Binary, what: &str, out: &Output) -> Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let detail = record(&String::from_utf8_lossy(&out.stdout), "end").map_or_else(
        |_| String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        |end| end.to_string(),
    );
    bail!("{} {what} failed ({}): {detail}", binary.name, out.status)
}

fn build(binary: &Binary, roots: &[String]) -> Result<Value> {
    let out = command("/usr/bin/time")
        .arg("-l")
        .arg(&binary.path)
        .args(invocation(
            binary,
            roots,
            &["index".into(), "--full".into()],
        ))
        .output()?;
    check(binary, "index --full", &out)?;
    let Some((wall, rss)) = time_stats(&String::from_utf8_lossy(&out.stderr)) else {
        bail!("/usr/bin/time -l reported no wall time or peak RSS; run outside any agent sandbox");
    };
    let out = command(&binary.path)
        .args(invocation(binary, roots, &["doctor".into()]))
        .output()?;
    check(binary, "doctor", &out)?;
    let doctor = record(std::str::from_utf8(&out.stdout)?, "doctor")
        .with_context(|| format!("{} doctor", binary.name))?;
    Ok(json!({
        "type": "build",
        "binary": binary.name,
        "transcripts": doctor["files"],
        "records": doctor["records"],
        "database_bytes": fs::metadata(&binary.db)?.len(),
        "seconds": wall,
        "peak_rss_bytes": rss,
    }))
}

/// Wall seconds and peak RSS bytes from the stderr of macOS `/usr/bin/time -l`.
fn time_stats(stderr: &str) -> Option<(f64, u64)> {
    let wall = stderr.lines().find_map(|l| {
        let mut words = l.split_whitespace();
        let value = words.next()?;
        (words.next()? == "real")
            .then_some(value)?
            .parse::<f64>()
            .ok()
    });
    let rss = stderr.lines().find_map(|l| {
        l.trim()
            .strip_suffix("maximum resident set size")?
            .trim()
            .parse::<u64>()
            .ok()
    });
    Some((wall?, rss?))
}

/// The record of type `kind` in a command's output, searched from the end.
fn record(stdout: &str, kind: &str) -> Result<Value> {
    stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == kind)
        .with_context(|| format!("no {kind} record"))
}

fn run(binary: &Binary, roots: &[String], args: &[String]) -> Result<Run> {
    let mut cmd = command(&binary.path);
    cmd.args(invocation(binary, roots, args));
    let start = Instant::now();
    let out = cmd.output()?;
    let seconds = start.elapsed().as_secs_f64();
    finished(seconds, &out).with_context(|| {
        format!(
            "{} printed no end record ({}): {}",
            binary.name,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })
}

/// Appends `n` user messages with fresh identifiers to the synthetic Claude transcript.
fn append(path: &Path, appended: &mut usize, n: usize) -> Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    for _ in 0..n {
        *appended += 1;
        let record = json!({
            "type": "user",
            "uuid": format!("measure-{appended}"),
            "sessionId": "measure",
            "timestamp": ts,
            "message": {"role": "user", "content": format!("measure append {appended}: the quick brown fox jumps over the lazy dog while the index refreshes")},
        });
        writeln!(file, "{record}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_l_output_gives_wall_seconds_and_peak_rss() {
        let stderr = "      331.51 real       900.10 user        40.20 sys\n           236290048  maximum resident set size\n                   0  average shared memory size\n";
        assert_eq!(time_stats(stderr), Some((331.51, 236290048)));
        assert_eq!(
            time_stats("time: sysctl kern.clockrate: Operation not permitted\n"),
            None
        );
    }

    #[test]
    fn a_run_without_an_end_record_reports_how_the_binary_exited() {
        let binary = Binary {
            name: "candidate",
            path: "/usr/bin/false".into(),
            db: "unused.db".into(),
        };
        let error = run(&binary, &[], &[]).err().unwrap().to_string();
        assert!(error.contains("exit status: 1"), "{error}");
    }
}
