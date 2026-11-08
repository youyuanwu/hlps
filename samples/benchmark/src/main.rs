use std::io::{BufWriter, Write};

use anyhow::{Context, Result, bail, ensure};
use hyperlight_benchmarks::measurement::{self, Config, Row};
use hyperlight_benchmarks::{Mode, Paths, cases, modes, report_environment};
use hyperlight_host::Sandbox;
use hyperlight_process::{InProcessRunner, Runner};

const USAGE: &str = "\
Usage: hyperlight-benchmark [options]
  --mode all|kvm|subprocess|inprocess       Default: all
  --workload all|add|echo|callbacks|compute Default: all
  --samples N                             Default: 1000 (1..100000)
  --warmup N                              Default: 100 (0..100000)
  --lifecycle-samples N                    Default: 20 (1..10000)
  --repeats N                              Default: 3 (1..100)
  --guest PATH                            Override VM guest ELF
  --runner PATH                           Override subprocess executable
  --help
CSV goes to stdout; configuration goes to stderr. Use release builds.";

struct Options {
    modes: Vec<Mode>,
    workload: String,
    config: Config,
    repeats: usize,
    paths: Paths,
}

fn options() -> Result<Option<Options>> {
    let mut result = Options {
        modes: Mode::ALL.to_vec(),
        workload: "all".into(),
        config: Config {
            samples: 1000,
            warmup: 100,
            lifecycle_samples: 20,
        },
        repeats: 3,
        paths: Paths::new(std::env::current_exe()?.with_file_name("hyperlight-benchmark-runner")),
    };
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--help" {
            println!("{USAGE}");
            return Ok(None);
        }
        let flag = argument
            .to_str()
            .context("option name is not valid UTF-8")?;
        ensure!(
            matches!(
                flag,
                "--mode"
                    | "--workload"
                    | "--samples"
                    | "--warmup"
                    | "--lifecycle-samples"
                    | "--repeats"
                    | "--guest"
                    | "--runner"
            ),
            "unknown option {flag:?}\n{USAGE}"
        );
        let value = args
            .next()
            .with_context(|| format!("missing value for {flag}"))?;
        if flag == "--guest" {
            result.paths.guest = value.into();
        } else if flag == "--runner" {
            result.paths.runner = value.into();
        } else {
            let value = value.to_str().context("option value is not valid UTF-8")?;
            match flag {
                "--mode" => result.modes = modes(value)?,
                "--workload" => result.workload = value.into(),
                "--samples" => result.config.samples = value.parse().context("invalid samples")?,
                "--warmup" => result.config.warmup = value.parse().context("invalid warmup")?,
                "--lifecycle-samples" => {
                    result.config.lifecycle_samples =
                        value.parse().context("invalid lifecycle-samples")?
                }
                "--repeats" => result.repeats = value.parse().context("invalid repeats")?,
                _ => bail!("unhandled option {flag}"),
            }
        }
    }
    result.config.validate()?;
    ensure!(
        (1..=100).contains(&result.repeats),
        "repeats must be between 1 and 100"
    );
    cases(&result.workload)?;
    Ok(Some(result))
}

fn write_row(output: &mut impl Write, row: Row) -> Result<()> {
    let summary = row.summary;
    ensure!(
        summary.mean_ns > 0.0,
        "timer resolution produced a zero-duration measurement"
    );
    let rate = 1e9 / summary.mean_ns;
    let throughput = row
        .round_trip_bytes
        .map(|bytes| format!("{:.3}", bytes as f64 * rate / (1024.0 * 1024.0)))
        .unwrap_or_default();
    writeln!(
        output,
        "{},{},{},{},{:.3},{},{},{},{:.3},{}",
        row.mode.name(),
        row.workload,
        row.repeat,
        row.samples,
        summary.mean_ns,
        summary.p50_ns,
        summary.p95_ns,
        summary.p99_ns,
        rate,
        throughput
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let Some(mut options) = options()? else {
        return Ok(());
    };
    let cases = cases(&options.workload)?;
    for &mode in &options.modes {
        mode.preflight(&options.paths)?;
    }
    report_environment(&options.paths)?;
    eprintln!(
        "samples={}; warmup={}; lifecycle-samples={}; repeats={}; rotating mode order",
        options.config.samples,
        options.config.warmup,
        options.config.lifecycle_samples,
        options.repeats
    );
    if env!("HL_BENCH_OPT_LEVEL") != "3" {
        eprintln!(
            "WARNING: Rust is not built with release opt-level=3; do not compare these timings"
        );
    }
    let mut output = BufWriter::new(std::io::stdout().lock());
    writeln!(
        output,
        "mode,workload,repeat,samples,mean_ns,p50_ns,p95_ns,p99_ns,operations_per_second,round_trip_mib_per_second"
    )?;
    for repeat in 1..=options.repeats {
        for &mode in &options.modes {
            let rows = match mode {
                Mode::Kvm => {
                    measurement::run::<Sandbox>(&options.paths, &cases, options.config, repeat)?
                }
                Mode::Subprocess => {
                    measurement::run::<Runner>(&options.paths, &cases, options.config, repeat)?
                }
                Mode::InProcess => measurement::run::<InProcessRunner>(
                    &options.paths,
                    &cases,
                    options.config,
                    repeat,
                )?,
            };
            for row in rows {
                write_row(&mut output, row)?;
            }
            output.flush()?;
        }
        options.modes.rotate_left(1);
    }
    Ok(())
}
