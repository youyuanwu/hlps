use std::fmt::Debug;
use std::hint::black_box;
use std::time::Duration;

use anyhow::{Result, ensure};
use criterion::{BenchmarkGroup, Criterion, Throughput, measurement::WallTime};
use hyperlight_benchmarks::{
    Backend, Case, Mode, Paths, SEED, cases, checksum, modes, payload, report_environment,
};
use hyperlight_host::Sandbox;
use hyperlight_process::{InProcessRunner, Runner};

fn bench_case<R: Backend, T: PartialEq + Debug>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    guest: &mut R,
    case: Case,
    mut call: impl FnMut(&mut R) -> Result<T>,
    expected: T,
) -> Result<()> {
    ensure!(call(guest)? == expected, "incorrect result for {case:?}");
    group.throughput(match case.round_trip_bytes() {
        Some(bytes) => Throughput::Bytes(bytes),
        None => Throughput::Elements(1),
    });
    group.bench_function(case.name(), |bencher| {
        bencher.iter(|| black_box(call(guest).expect("benchmark guest call failed")));
    });
    ensure!(call(guest)? == expected, "incorrect result for {case:?}");
    Ok(())
}

fn bench_mode<R: Backend>(criterion: &mut Criterion, paths: &Paths) -> Result<()> {
    let mut guest = R::create(paths)?;
    let mut group = criterion.benchmark_group(R::MODE.name());
    for case in cases("all")? {
        match case {
            Case::Add => bench_case(
                &mut group,
                &mut guest,
                case,
                |guest| guest.call::<i64>("Add", (black_box(20_i32), black_box(22_i32))),
                42,
            )?,
            Case::Echo(size) => {
                let input = payload(size);
                let expected = input.clone();
                bench_case(
                    &mut group,
                    &mut guest,
                    case,
                    |guest| guest.call::<Vec<u8>>("EchoBytes", black_box(input.clone())),
                    expected,
                )?;
            }
            Case::Callbacks(count) => bench_case(
                &mut group,
                &mut guest,
                case,
                |guest| guest.call::<u64>("CallHost", (black_box(SEED), black_box(count))),
                checksum(SEED, count),
            )?,
            Case::Compute(count) => bench_case(
                &mut group,
                &mut guest,
                case,
                |guest| guest.call::<u64>("Compute", (black_box(SEED), black_box(count))),
                checksum(SEED, count),
            )?,
        }
    }
    group.finish();
    guest.shutdown()
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    let selected = match std::env::var("HL_BENCH_MODE") {
        Ok(value) => modes(&value)?,
        Err(std::env::VarError::NotPresent) => Mode::ALL.to_vec(),
        Err(error) => return Err(error.into()),
    };
    let paths = Paths::new(env!("CARGO_BIN_EXE_hyperlight-benchmark-runner"));
    for &mode in &selected {
        mode.preflight(&paths)?;
    }
    report_environment(&paths)?;
    for mode in selected {
        match mode {
            Mode::Kvm => bench_mode::<Sandbox>(&mut criterion, &paths)?,
            Mode::Subprocess => bench_mode::<Runner>(&mut criterion, &paths)?,
            Mode::InProcess => bench_mode::<InProcessRunner>(&mut criterion, &paths)?,
        }
    }
    criterion.final_summary();
    Ok(())
}
