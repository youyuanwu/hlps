use std::fmt::Debug;
use std::hint::black_box;
use std::time::Instant;

use anyhow::{Result, ensure};

use crate::{Backend, Case, Mode, Paths, SEED, checksum, payload};

#[derive(Clone, Copy)]
pub struct Config {
    pub samples: usize,
    pub warmup: usize,
    pub lifecycle_samples: usize,
}

impl Config {
    pub fn validate(self) -> Result<()> {
        ensure!(
            (1..=100_000).contains(&self.samples),
            "samples must be between 1 and 100000"
        );
        ensure!(self.warmup <= 100_000, "warmup must not exceed 100000");
        ensure!(
            (1..=10_000).contains(&self.lifecycle_samples),
            "lifecycle-samples must be between 1 and 10000"
        );
        Ok(())
    }
}

#[derive(Debug)]
pub struct Summary {
    pub mean_ns: f64,
    pub p50_ns: u128,
    pub p95_ns: u128,
    pub p99_ns: u128,
}

pub fn summarize(samples: &mut [u128]) -> Result<Summary> {
    ensure!(!samples.is_empty(), "cannot summarize empty samples");
    samples.sort_unstable();
    let percentile = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
    Ok(Summary {
        mean_ns: samples.iter().map(|&value| value as f64).sum::<f64>() / samples.len() as f64,
        p50_ns: percentile(50),
        p95_ns: percentile(95),
        p99_ns: percentile(99),
    })
}

pub struct Row {
    pub mode: Mode,
    pub workload: String,
    pub repeat: usize,
    pub samples: usize,
    pub summary: Summary,
    pub round_trip_bytes: Option<u64>,
}

impl Row {
    fn new(
        mode: Mode,
        workload: String,
        repeat: usize,
        mut samples: Vec<u128>,
        bytes: Option<u64>,
    ) -> Result<Self> {
        Ok(Self {
            mode,
            workload,
            repeat,
            samples: samples.len(),
            summary: summarize(&mut samples)?,
            round_trip_bytes: bytes,
        })
    }
}

fn measure<R: Backend, T: PartialEq + Debug>(
    paths: &Paths,
    config: Config,
    repeat: usize,
    case: Case,
    mut call: impl FnMut(&mut R) -> Result<T>,
    expected: T,
) -> Result<Row> {
    let mut guest = R::create(paths)?;
    ensure!(
        call(&mut guest)? == expected,
        "incorrect result for {case:?}"
    );
    for _ in 0..config.warmup {
        drop(black_box(call(&mut guest)?));
    }
    let mut samples = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let start = Instant::now();
        drop(black_box(call(&mut guest)?));
        samples.push(start.elapsed().as_nanos());
    }
    ensure!(
        call(&mut guest)? == expected,
        "incorrect result for {case:?}"
    );
    guest.shutdown()?;
    Row::new(
        R::MODE,
        case.name(),
        repeat,
        samples,
        case.round_trip_bytes(),
    )
}

pub fn run<R: Backend>(
    paths: &Paths,
    cases: &[Case],
    config: Config,
    repeat: usize,
) -> Result<Vec<Row>> {
    config.validate()?;
    let mut creation = Vec::with_capacity(config.lifecycle_samples);
    let mut first_call = Vec::with_capacity(config.lifecycle_samples);
    let mut teardown = Vec::with_capacity(config.lifecycle_samples);
    for _ in 0..config.lifecycle_samples {
        let start = Instant::now();
        let mut guest = R::create(paths)?;
        creation.push(start.elapsed().as_nanos());
        let start = Instant::now();
        let value = black_box(guest.call::<i64>("Add", (black_box(20_i32), black_box(22_i32)))?);
        first_call.push(start.elapsed().as_nanos());
        ensure!(value == 42, "incorrect first-call result");
        let start = Instant::now();
        guest.shutdown()?;
        teardown.push(start.elapsed().as_nanos());
    }
    let mut rows = Vec::new();
    for (name, samples) in [
        ("create", creation),
        ("first_call/add", first_call),
        ("teardown", teardown),
    ] {
        rows.push(Row::new(R::MODE, name.into(), repeat, samples, None)?);
    }
    for &case in cases {
        let row = match case {
            Case::Add => measure::<R, _>(
                paths,
                config,
                repeat,
                case,
                |guest| guest.call::<i64>("Add", (black_box(20_i32), black_box(22_i32))),
                42,
            )?,
            Case::Echo(size) => {
                let input = payload(size);
                let expected = input.clone();
                measure::<R, _>(
                    paths,
                    config,
                    repeat,
                    case,
                    |guest| guest.call::<Vec<u8>>("EchoBytes", black_box(input.clone())),
                    expected,
                )?
            }
            Case::Callbacks(count) => measure::<R, _>(
                paths,
                config,
                repeat,
                case,
                |guest| guest.call::<u64>("CallHost", (black_box(SEED), black_box(count))),
                checksum(SEED, count),
            )?,
            Case::Compute(count) => measure::<R, _>(
                paths,
                config,
                repeat,
                case,
                |guest| guest.call::<u64>("Compute", (black_box(SEED), black_box(count))),
                checksum(SEED, count),
            )?,
        };
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() -> Result<()> {
        let mut samples: Vec<_> = (1..=100).rev().collect();
        let result = summarize(&mut samples)?;
        assert_eq!(result.mean_ns, 50.5);
        assert_eq!((result.p50_ns, result.p95_ns, result.p99_ns), (50, 95, 99));
        let result = summarize(&mut [7])?;
        assert_eq!((result.p50_ns, result.p95_ns, result.p99_ns), (7, 7, 7));
        assert!(summarize(&mut []).is_err());
        Ok(())
    }

    #[test]
    fn sample_counts_are_bounded() {
        for (samples, warmup, lifecycle_samples) in [
            (0, 0, 1),
            (100_001, 0, 1),
            (1, 100_001, 1),
            (1, 0, 0),
            (1, 0, 10_001),
        ] {
            assert!(
                Config {
                    samples,
                    warmup,
                    lifecycle_samples
                }
                .validate()
                .is_err()
            );
        }
    }
}
