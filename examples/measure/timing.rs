//! Runs read from their output, the commands kept and timed, and the records
//! that summarize their runs.

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::process::Output;

/// Commands timed per verb, each from a different transcript.
const PER_VERB: usize = 8;

/// What one run of a command did, read from its exit status and output.
#[derive(Clone)]
pub struct Run {
    seconds: f64,
    exit: i32,
    /// SHA-256 of the data records, everything before `end`.
    data: [u8; 32],
    /// `end.complete`.
    complete: bool,
    stale: bool,
    files_changed: u64,
    records_indexed: u64,
}

/// The run read from a finished command's output, or None without an `end` record.
pub fn finished(seconds: f64, out: &Output) -> Option<Run> {
    let stdout = std::str::from_utf8(&out.stdout).ok()?.trim_end();
    let (data, last) = stdout.rsplit_once('\n').unwrap_or(("", stdout));
    let end = serde_json::from_str::<Value>(last)
        .ok()
        .filter(|v| v["type"] == "end")?;
    let refresh = &end["refresh"];
    Some(Run {
        seconds,
        exit: out.status.code()?,
        data: Sha256::digest(data).into(),
        complete: end["complete"] == true,
        stale: end["stale"] == true,
        files_changed: refresh["files_changed"].as_u64().unwrap_or(0),
        records_indexed: refresh["records"].as_u64().unwrap_or(0),
    })
}

/// A binary refused the invocation: it exited 2 before the query could have
/// run out of its budget.
fn refusal(run: &Run) -> bool {
    run.exit == 2 && run.seconds < sessidx::BUDGET.as_secs_f64()
}

/// Keeps the first `PER_VERB` commands that no binary refuses, running each
/// once per binary untimed, which also warms it. Returns the kept commands and
/// how many each binary refused.
pub fn validate(
    binaries: usize,
    commands: Vec<Vec<String>>,
    mut run: impl FnMut(usize, &[String]) -> Result<Run>,
) -> Result<(Vec<Vec<String>>, Vec<usize>)> {
    let (mut kept, mut refused) = (Vec::new(), vec![0; binaries]);
    for argv in commands {
        if kept.len() == PER_VERB {
            break;
        }
        let runs = (0..binaries)
            .map(|b| run(b, &argv))
            .collect::<Result<Vec<_>>>()?;
        for (n, r) in refused.iter_mut().zip(&runs) {
            *n += usize::from(refusal(r));
        }
        if !runs.iter().any(refusal) {
            kept.push(argv);
        }
    }
    Ok((kept, refused))
}

/// Runs every command `runs` times per binary, calling `before` ahead of each
/// pair and alternating which binary goes first. Returns `[binary][command][run]`.
pub fn time(
    binaries: usize,
    commands: &[Vec<String>],
    runs: u32,
    mut before: impl FnMut() -> Result<()>,
    mut run: impl FnMut(usize, &[String]) -> Result<Run>,
) -> Result<Vec<Vec<Vec<Run>>>> {
    let mut out = vec![vec![Vec::new(); commands.len()]; binaries];
    for (c, argv) in commands.iter().enumerate() {
        for i in 0..runs as usize {
            before()?;
            for k in 0..binaries {
                let b = (k + i) % binaries;
                out[b][c].push(run(b, argv)?);
            }
        }
    }
    Ok(out)
}

pub fn timing(condition: &str, verb: &str, binary: &str, per_command: &[Vec<Run>]) -> Value {
    let sorted = |runs: &[&Run]| {
        let mut seconds: Vec<f64> = runs.iter().map(|r| r.seconds).collect();
        seconds.sort_by(f64::total_cmp);
        seconds
    };
    let all: Vec<&Run> = per_command.iter().flatten().collect();
    let seconds = sorted(&all);
    let count = |f: fn(&Run) -> bool| all.iter().filter(|r| f(r)).count();
    json!({
        "type": "timing",
        "condition": condition,
        "verb": verb,
        "binary": binary,
        "runs": all.len(),
        "p50_seconds": rank(&seconds, 0.5),
        "p95_seconds": rank(&seconds, 0.95),
        "max_seconds": seconds.last(),
        "command_p50_seconds": per_command
            .iter()
            .map(|c| rank(&sorted(&c.iter().collect::<Vec<_>>()), 0.5))
            .collect::<Vec<_>>(),
        "exit_0": count(|r| r.exit == 0),
        "exit_1": count(|r| r.exit == 1),
        "exit_2": count(|r| r.exit == 2),
        "incomplete": count(|r| !r.complete),
        "stale": count(|r| r.stale),
        "refreshed": count(|r| r.files_changed > 0),
        "records_indexed": all.iter().map(|r| r.records_indexed).sum::<u64>(),
    })
}

/// Nearest-rank quantile of sorted values.
fn rank(sorted: &[f64], q: f64) -> Option<f64> {
    sorted
        .get(((q * sorted.len() as f64).ceil() as usize).max(1) - 1)
        .copied()
}

/// Compares each command's warm runs on the two binaries pair by pair, the
/// i-th run of each being a pair.
pub fn agreement(verb: &str, candidate: &[Vec<Run>], baseline: &[Vec<Run>]) -> Value {
    let (mut differ, mut less_complete) = (0, 0);
    for (c, b) in candidate.iter().zip(baseline) {
        let most = |f: fn(&Run, &Run) -> bool| {
            2 * c.iter().zip(b).filter(|(c, b)| f(c, b)).count() > c.len()
        };
        differ += usize::from(most(|c, b| c.data != b.data));
        less_complete += usize::from(most(|c, b| b.complete && !c.complete));
    }
    json!({
        "type": "agreement",
        "verb": verb,
        "commands": candidate.len(),
        "differ": differ,
        "less_complete": less_complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn run_of(seconds: f64, exit: i32) -> Run {
        Run {
            seconds,
            exit,
            data: [0; 32],
            complete: true,
            stale: false,
            files_changed: 0,
            records_indexed: 0,
        }
    }

    #[test]
    fn a_fast_exit_2_is_a_refusal_and_one_past_the_budget_ran_out_of_it() {
        assert!(refusal(&run_of(0.01, 2)));
        assert!(!refusal(&run_of(2.3, 2)));
        assert!(!refusal(&run_of(0.01, 1)));
    }

    #[test]
    fn a_command_any_binary_refuses_is_dropped_and_the_rest_kept_up_to_the_limit() {
        let commands: Vec<Vec<String>> = (0..PER_VERB + 2)
            .map(|i| vec!["search".to_owned(), i.to_string()])
            .collect();
        let mut ran = HashSet::new();
        let (kept, refused) = validate(2, commands.clone(), |b, argv| {
            ran.insert(argv.to_vec());
            Ok(match (b, argv[1].as_str()) {
                (1, "1") => run_of(0.01, 2),
                (0, "2") => run_of(2.3, 2),
                _ => run_of(0.1, 0),
            })
        })
        .unwrap();
        assert_eq!(refused, [0, 1]);
        assert_eq!(kept.len(), PER_VERB);
        assert!(!kept.contains(&commands[1]));
        assert!(kept.contains(&commands[2]));
        assert!(!ran.contains(&commands[PER_VERB + 1]));
    }

    #[test]
    fn quantiles_take_the_nearest_rank() {
        let twenty: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(rank(&twenty, 0.5), Some(10.0));
        assert_eq!(rank(&twenty, 0.95), Some(19.0));
        let odd: Vec<f64> = (1..=63).map(f64::from).collect();
        assert_eq!(rank(&odd, 0.95), Some(60.0));
        assert_eq!(rank(&[7.0], 0.95), Some(7.0));
        assert_eq!(rank(&[], 0.95), None);
    }

    #[test]
    fn binaries_alternate_which_goes_first_and_before_runs_ahead_of_each_pair() {
        let mut calls = Vec::new();
        let mut befores = 0;
        let out = time(
            2,
            &[vec!["search".to_owned(), "x".to_owned()]],
            3,
            || {
                befores += 1;
                Ok(())
            },
            |b, _| {
                calls.push(b);
                Ok(run_of(0.0, 0))
            },
        )
        .unwrap();
        assert_eq!(calls, [0, 1, 1, 0, 0, 1]);
        assert_eq!(befores, 3);
        assert_eq!((out[0][0].len(), out[1][0].len()), (3, 3));
    }

    #[test]
    fn timing_pools_every_run_and_partitions_runs_by_exit() {
        let mut cut = run_of(0.3, 0);
        cut.complete = false;
        cut.stale = true;
        cut.files_changed = 1;
        cut.records_indexed = 20;
        let per_command = [
            vec![run_of(0.1, 0), run_of(0.2, 0), cut],
            vec![run_of(1.0, 0), run_of(2.0, 1), run_of(3.0, 2)],
        ];
        let t = timing("warm", "search", "candidate", &per_command);
        assert_eq!(t["runs"], 6);
        assert_eq!(
            (
                t["exit_0"].clone(),
                t["exit_1"].clone(),
                t["exit_2"].clone()
            ),
            (json!(4), json!(1), json!(1))
        );
        assert_eq!(t["p50_seconds"], 0.3);
        assert_eq!(t["p95_seconds"], 3.0);
        assert_eq!(t["command_p50_seconds"], json!([0.2, 2.0]));
        assert_eq!(t["incomplete"], 1);
        assert_eq!(t["stale"], 1);
        assert_eq!(t["refreshed"], 1);
        assert_eq!(t["records_indexed"], 20);
    }

    #[test]
    fn agreement_counts_what_holds_in_most_run_pairs() {
        let runs = |rs: [(u8, bool); 4]| {
            rs.map(|(data, complete)| {
                let mut r = run_of(0.0, 0);
                r.data = [data; 32];
                r.complete = complete;
                r
            })
            .to_vec()
        };
        let (t, f) = (true, false);
        let same = runs([(1, t); 4]);
        let pairs = [
            (runs([(1, t); 4]), same.clone()),
            // One run that ran out of the budget, as first observed.
            (runs([(0, f), (1, t), (1, t), (1, t)]), same.clone()),
            (runs([(1, t), (2, t), (1, t), (2, t)]), same.clone()),
            (runs([(2, t), (2, t), (1, t), (2, t)]), same.clone()),
            (
                runs([(1, t), (2, t), (1, t), (2, t)]),
                runs([(2, t), (1, t), (2, t), (1, t)]),
            ),
            (runs([(1, f), (1, f), (1, f), (1, t)]), same.clone()),
            // Pages cut by a limit, as most `grep` pages are, still compare.
            (runs([(2, f); 4]), runs([(1, f); 4])),
            (runs([(1, t); 4]), runs([(1, f); 4])),
        ];
        let (candidate, baseline): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        assert_eq!(
            agreement("sql", &candidate, &baseline),
            json!({"type": "agreement", "verb": "sql", "commands": 8, "differ": 3, "less_complete": 1})
        );
    }

    #[test]
    fn a_run_reads_its_completeness_and_hashes_its_data_without_end() {
        use std::os::unix::process::ExitStatusExt;
        let out = |stdout: &str| Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        let paged = finished(0.1, &out("{\"type\":\"session\"}\n{\"type\":\"end\",\"complete\":false,\"next\":\"c\",\"refresh\":{\"ms\":1}}\n")).unwrap();
        let refreshed = finished(0.1, &out("{\"type\":\"session\"}\n{\"type\":\"end\",\"complete\":false,\"next\":\"c\",\"refresh\":{\"ms\":2}}\n")).unwrap();
        let other = finished(
            0.1,
            &out("{\"type\":\"row\"}\n{\"type\":\"end\",\"complete\":true}\n"),
        )
        .unwrap();
        assert!(!paged.complete);
        assert!(other.complete);
        assert_eq!(paged.data, refreshed.data);
        assert_ne!(paged.data, other.data);
        assert!(finished(0.1, &out("{\"type\":\"session\"}\n")).is_none());
    }
}
