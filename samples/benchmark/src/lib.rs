use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use hyperlight_host::{Sandbox, SandboxBuilder};
use hyperlight_process::func::{ParameterTuple, SupportedReturnType};
use hyperlight_process::{InProcessRunner, InProcessRunnerBuilder, Runner, RunnerBuilder};

pub mod measurement;

pub const SEED: u64 = 0x1234_5678_9abc_def0;
pub const PAYLOAD_SIZES: [usize; 4] = [64, 1024, 4096, 65536];
pub const CALLBACK_COUNTS: [u32; 3] = [1, 10, 100];
pub const COMPUTE_COUNTS: [u32; 3] = [100, 10_000, 1_000_000];
pub const VM_SCRATCH_SIZE: usize = 2 * 1024 * 1024;
pub const VM_BUFFER_SIZE: usize = 128 * 1024;
pub const VM_POOL_PAGES: usize = 128;

unsafe extern "C" {
    fn hyperlight_main();
}

pub fn run_subprocess() -> Result<()> {
    unsafe { hyperlight_process::runtime::run(hyperlight_main) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Kvm,
    Subprocess,
    InProcess,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Kvm, Self::Subprocess, Self::InProcess];

    pub fn name(self) -> &'static str {
        match self {
            Self::Kvm => "kvm",
            Self::Subprocess => "subprocess",
            Self::InProcess => "inprocess",
        }
    }

    pub fn preflight(self, paths: &Paths) -> Result<()> {
        match self {
            Self::Kvm => {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/kvm")
                    .context("KVM benchmarks require read/write access to /dev/kvm")?;
                std::fs::File::open(&paths.guest).context("cannot open the benchmark VM guest")?;
            }
            Self::Subprocess => {
                std::fs::metadata(&paths.runner)
                    .context("benchmark runner is missing; build the package's binaries")?;
            }
            Self::InProcess => {}
        }
        Ok(())
    }
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "kvm" => Ok(Self::Kvm),
            "subprocess" => Ok(Self::Subprocess),
            "inprocess" => Ok(Self::InProcess),
            _ => bail!("unknown mode {value:?}; use kvm, subprocess, inprocess, or all"),
        }
    }
}

pub fn modes(value: &str) -> Result<Vec<Mode>> {
    if value == "all" {
        Ok(Mode::ALL.to_vec())
    } else {
        Ok(vec![value.parse()?])
    }
}

pub struct Paths {
    pub guest: PathBuf,
    pub runner: PathBuf,
}

impl Paths {
    pub fn new(runner: impl Into<PathBuf>) -> Self {
        Self {
            guest: std::env::var_os("HL_BENCH_GUEST")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("HL_BENCH_GUEST"))),
            runner: std::env::var_os("HL_BENCH_RUNNER")
                .map(PathBuf::from)
                .unwrap_or_else(|| runner.into()),
        }
    }
}

pub fn step(mut value: u64) -> u64 {
    value ^= value >> 12;
    value ^= value << 25;
    value ^= value >> 27;
    value.wrapping_mul(2_685_821_657_736_338_717)
}

pub fn checksum(seed: u64, count: u32) -> u64 {
    (0..count).fold(seed, |value, _| step(value))
}

pub fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

pub trait Backend: Sized {
    const MODE: Mode;

    fn create(paths: &Paths) -> Result<Self>;
    fn call<T: SupportedReturnType>(&mut self, name: &str, args: impl ParameterTuple) -> Result<T>;
    fn shutdown(self) -> Result<()>;
}

impl Backend for Sandbox {
    const MODE: Mode = Mode::Kvm;

    fn create(paths: &Paths) -> Result<Self> {
        SandboxBuilder::from_file(&paths.guest)
            .scratch_size(VM_SCRATCH_SIZE)
            .g2h_buffer_size(VM_BUFFER_SIZE)
            .h2g_buffer_size(VM_BUFFER_SIZE)
            .g2h_pool_pages(VM_POOL_PAGES)
            .h2g_pool_pages(VM_POOL_PAGES)
            .host_function("HostStep", step)
            .build()
            .context("creating the benchmark KVM sandbox")
    }

    fn call<T: SupportedReturnType>(&mut self, name: &str, args: impl ParameterTuple) -> Result<T> {
        self.call(name, args)
            .context("calling the benchmark VM guest")
    }

    fn shutdown(self) -> Result<()> {
        drop(self);
        Ok(())
    }
}

impl Backend for Runner {
    const MODE: Mode = Mode::Subprocess;

    fn create(paths: &Paths) -> Result<Self> {
        RunnerBuilder::from_executable(&paths.runner)
            .host_function("HostStep", step)
            .build()
            .context("creating the benchmark subprocess")
    }

    fn call<T: SupportedReturnType>(&mut self, name: &str, args: impl ParameterTuple) -> Result<T> {
        self.call(name, args)
    }

    fn shutdown(self) -> Result<()> {
        Runner::shutdown(self)
    }
}

impl Backend for InProcessRunner {
    const MODE: Mode = Mode::InProcess;

    fn create(_: &Paths) -> Result<Self> {
        // The stateless linked guest uses valid call-scoped pointers and never reenters.
        unsafe {
            InProcessRunnerBuilder::new(hyperlight_main)
                .host_function("HostStep", step)
                .build()
        }
        .context("creating the benchmark in-process runner")
    }

    fn call<T: SupportedReturnType>(&mut self, name: &str, args: impl ParameterTuple) -> Result<T> {
        self.call(name, args)
    }

    fn shutdown(self) -> Result<()> {
        drop(self);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Add,
    Echo(usize),
    Callbacks(u32),
    Compute(u32),
}

impl Case {
    pub fn name(self) -> String {
        match self {
            Self::Add => "add".into(),
            Self::Echo(size) => format!("echo/{size}"),
            Self::Callbacks(count) => format!("callbacks/{count}"),
            Self::Compute(count) => format!("compute/{count}"),
        }
    }

    pub fn round_trip_bytes(self) -> Option<u64> {
        match self {
            Self::Echo(size) => Some(2 * size as u64),
            _ => None,
        }
    }
}

pub fn cases(workload: &str) -> Result<Vec<Case>> {
    let mut result = Vec::new();
    if matches!(workload, "all" | "add") {
        result.push(Case::Add);
    }
    if matches!(workload, "all" | "echo") {
        result.extend(PAYLOAD_SIZES.map(Case::Echo));
    }
    if matches!(workload, "all" | "callbacks") {
        result.extend(CALLBACK_COUNTS.map(Case::Callbacks));
    }
    if matches!(workload, "all" | "compute") {
        result.extend(COMPUTE_COUNTS.map(Case::Compute));
    }
    if result.is_empty() {
        bail!("unknown workload {workload:?}; use all, add, echo, callbacks, or compute");
    }
    Ok(result)
}

pub fn report_environment(paths: &Paths) -> Result<()> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")?;
    let status = std::fs::read_to_string("/proc/self/status")?;
    let cpu = cpuinfo
        .lines()
        .find(|line| line.starts_with("model name"))
        .context("/proc/cpuinfo has no CPU model name")?;
    let affinity = status
        .lines()
        .find(|line| line.starts_with("Cpus_allowed_list"))
        .context("/proc/self/status has no CPU affinity")?;
    eprintln!(
        "{}; {}; Rust opt-level={}; C opt-level=2",
        env!("HL_BENCH_RUSTC"),
        env!("HL_BENCH_CLANG"),
        env!("HL_BENCH_OPT_LEVEL")
    );
    eprintln!(
        "{cpu}; {affinity}; kernel={}",
        std::fs::read_to_string("/proc/sys/kernel/osrelease")?.trim()
    );
    eprintln!(
        "Hyperlight=0.19.0; Vercode=0.7.0; VM scratch={VM_SCRATCH_SIZE}; VM buffers={VM_BUFFER_SIZE}; VM pool pages={VM_POOL_PAGES} each"
    );
    eprintln!(
        "guest={}; runner={}; child inherits host affinity",
        paths.guest.display(),
        paths.runner.display()
    );
    Ok(())
}
