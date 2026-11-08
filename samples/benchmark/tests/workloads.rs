use std::process::Command;

use anyhow::{Result, ensure};
use hyperlight_benchmarks::{
    Backend, CALLBACK_COUNTS, COMPUTE_COUNTS, PAYLOAD_SIZES, Paths, SEED, checksum, payload,
};
use hyperlight_host::Sandbox;
use hyperlight_process::{InProcessRunner, Runner};

fn paths() -> Paths {
    Paths::new(env!("CARGO_BIN_EXE_hyperlight-benchmark-runner"))
}

fn check_workloads<R: Backend>() -> Result<()> {
    let mut guest = R::create(&paths())?;
    for (left, right, expected) in [
        (20, 22, 42),
        (i32::MAX, i32::MAX, 4_294_967_294),
        (i32::MIN, i32::MIN, -4_294_967_296),
    ] {
        assert_eq!(guest.call::<i64>("Add", (left, right))?, expected);
    }
    for size in [0, 1].into_iter().chain(PAYLOAD_SIZES) {
        let input = payload(size);
        assert_eq!(guest.call::<Vec<u8>>("EchoBytes", input.clone())?, input);
    }
    for seed in [0, SEED, u64::MAX] {
        for count in [0].into_iter().chain(CALLBACK_COUNTS) {
            assert_eq!(
                guest.call::<u64>("CallHost", (seed, count))?,
                checksum(seed, count)
            );
        }
        for count in [0].into_iter().chain(COMPUTE_COUNTS) {
            assert_eq!(
                guest.call::<u64>("Compute", (seed, count))?,
                checksum(seed, count)
            );
        }
    }
    guest.shutdown()
}

#[test]
fn inprocess_workloads() -> Result<()> {
    check_workloads::<InProcessRunner>()
}

#[test]
fn subprocess_workloads() -> Result<()> {
    check_workloads::<Runner>()
}

#[test]
#[ignore = "requires read/write access to /dev/kvm"]
fn kvm_workloads() -> Result<()> {
    check_workloads::<Sandbox>()
}

#[test]
fn latency_cli_outputs_all_cases_and_lifecycle_rows() -> Result<()> {
    for mode in ["inprocess", "subprocess"] {
        let output = Command::new(env!("CARGO_BIN_EXE_hyperlight-benchmark"))
            .args([
                "--mode",
                mode,
                "--samples",
                "2",
                "--warmup",
                "1",
                "--lifecycle-samples",
                "2",
                "--repeats",
                "1",
            ])
            .output()?;
        ensure!(
            output.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let csv = String::from_utf8(output.stdout)?;
        check_csv_metrics(&csv)?;
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(lines.len(), 15);
        assert!(lines[0].starts_with("mode,workload,repeat,samples,mean_ns"));
        for line in &lines[1..] {
            let fields: Vec<_> = line.split(',').collect();
            assert_eq!(fields.len(), 10);
            assert_eq!(fields[0], mode);
            assert_eq!(fields[2], "1");
            assert_eq!(fields[3], "2");
            assert!(fields[4].parse::<f64>()? > 0.0);
            if fields[1].starts_with("echo/") {
                assert!(fields[9].parse::<f64>()? > 0.0);
            } else {
                assert!(fields[9].is_empty());
            }
        }
    }
    Ok(())
}

#[test]
fn latency_cli_rejects_invalid_options() -> Result<()> {
    for args in [
        vec!["--mode", "missing"],
        vec!["--workload", "missing"],
        vec!["--samples", "0"],
        vec!["--samples", "100001"],
        vec!["--repeats", "0"],
        vec!["--warmup"],
        vec!["--unknown", "value"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_hyperlight-benchmark"))
            .args(args)
            .output()?;
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        assert!(output.stdout.is_empty());
    }
    Ok(())
}

fn check_csv_metrics(csv: &str) -> Result<()> {
    for line in csv.lines().skip(1) {
        let fields: Vec<_> = line.split(',').collect();
        assert_eq!(fields.len(), 10);
        let mean = fields[4].parse::<f64>()?;
        let p50 = fields[5].parse::<u128>()?;
        let p95 = fields[6].parse::<u128>()?;
        let p99 = fields[7].parse::<u128>()?;
        let rate = fields[8].parse::<f64>()?;
        assert!(mean > 0.0 && p50 > 0 && p50 <= p95 && p95 <= p99);
        assert!((rate * mean / 1e9 - 1.0).abs() < 0.0001);
        if let Some(size) = fields[1].strip_prefix("echo/") {
            let expected = 2.0 * size.parse::<f64>()? * rate / (1024.0 * 1024.0);
            assert!((fields[9].parse::<f64>()? - expected).abs() < 0.01);
        } else {
            assert!(fields[9].is_empty());
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires read/write access to /dev/kvm"]
fn all_modes_cli_rotates_order_and_separates_sample_counts() -> Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_hyperlight-benchmark"))
        .args([
            "--mode",
            "all",
            "--workload",
            "add",
            "--samples",
            "3",
            "--warmup",
            "1",
            "--lifecycle-samples",
            "2",
            "--repeats",
            "3",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let csv = String::from_utf8(output.stdout)?;
    check_csv_metrics(&csv)?;
    let rows: Vec<Vec<_>> = csv
        .lines()
        .skip(1)
        .map(|line| line.split(',').collect())
        .collect();
    assert_eq!(rows.len(), 36);
    for (repeat, order) in [
        ["kvm", "subprocess", "inprocess"],
        ["subprocess", "inprocess", "kvm"],
        ["inprocess", "kvm", "subprocess"],
    ]
    .iter()
    .enumerate()
    {
        for (position, mode) in order.iter().enumerate() {
            let start = repeat * 12 + position * 4;
            for (index, row) in rows[start..start + 4].iter().enumerate() {
                assert_eq!(row[0], *mode);
                assert_eq!(row[2], (repeat + 1).to_string());
                assert_eq!(row[3], if index == 3 { "3" } else { "2" });
            }
        }
    }
    Ok(())
}
