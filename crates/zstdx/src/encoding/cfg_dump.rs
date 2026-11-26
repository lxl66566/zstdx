//! Encoder configuration dump for dev tooling (`cfg_dump` dev feature):
//! the resolved matcher configuration at every state reset, the
//! frame-continuous entries' head-verdict flips and the MT frame's job
//! geometry, appended to one process-global log so worker threads' job
//! resets are captured too. `zstdx-bench stmdiff` drains the log to line
//! the ST bulk entry's configuration up against an MT job's on the same
//! input. Compiled out entirely unless the feature is on.

use alloc::vec::Vec;
use std::sync::{Mutex, OnceLock};

/// The matcher's LDM state at a reset.
#[derive(Clone, Copy, Debug)]
pub struct LdmCfg {
    pub buckets: usize,
    pub ents_per_bucket: usize,
    pub window: u64,
    /// Absolute position the rolling hash has been fed to.
    pub fed: u64,
    /// Entry floor: the last (re)arm's window start.
    pub arm: u64,
    pub stale: bool,
}

/// The resolved strategy and its table geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrategyTag {
    Fast,
    /// Payload: the short-table log.
    Dfast(u32),
    /// Payload: the chain-table log.
    Chain(u32),
    /// Payload: the row log.
    Row(u32),
    Opt {
        bt_log: u32,
        hash3_log: u32,
    },
    BtLazy {
        bt_log: u32,
        hash3_log: u32,
    },
}

/// The balanced row's storage verdict (see `BalancedStore`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreTag {
    Chain,
    Row,
    RowHead,
}

/// The driver's LDM arming context (see `LdmArming`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmingTag {
    Frame,
    FrameScreened,
    Job,
    JobPrefix,
    FarDead,
    ProbeKeep,
}

/// The shared prefix fill's plan variant an MT frame engaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpfTag {
    None,
    Whole,
    Windowed,
    SelfFill,
}

/// The matcher's live configuration at the record site.
#[derive(Clone, Copy, Debug)]
pub struct DriverCfg {
    pub strategy: StrategyTag,
    pub hash_log: u32,
    pub window: usize,
    pub chain_reach: Option<usize>,
    pub search_depth: u32,
    pub lazy_depth: u32,
    pub min_match: u32,
    pub small_src: bool,
    /// The row-band attempts verdict resolved so far.
    pub row_attempts_light: bool,
    pub store: StoreTag,
    /// The small-src 9/10 row verdict (`false` keeps the btlazy swap).
    pub small_src_row_stock: bool,
    pub ldm: Option<LdmCfg>,
    /// Literal-offset sequences still owed before repcodes decode (the
    /// MT job gate).
    pub rep_pending: u8,
    /// The job-start periodic-repeat seed (0 = inactive).
    pub seed_offset: u32,
}

/// A state reset (`reset_slice_state_mode`), the one config point every
/// ST entry and every MT job passes through.
#[derive(Clone, Copy, Debug)]
pub struct ResetRecord {
    pub level: i32,
    pub len: Option<u64>,
    pub window_log: Option<u32>,
    pub reach_keep: bool,
    pub arming: ArmingTag,
    pub cfg: DriverCfg,
}

/// A mid-frame configuration flip (the first-matching-entry and
/// entry-point head verdicts).
#[derive(Clone, Copy, Debug)]
pub struct FlipRecord {
    pub site: &'static str,
    pub cfg: DriverCfg,
}

/// The ST bulk entry's frame-level resolution (see
/// `compress_slice_shaped_ldm`).
#[derive(Clone, Copy, Debug)]
pub struct StEntryRecord {
    pub level: i32,
    /// The balanced row's storage substitution engaged (`BalancedStore`
    /// sampled non-Chain before the first block).
    pub bal_row: bool,
    pub reach_keep: bool,
    pub arming: ArmingTag,
}

/// One MT job's start gates (`prepare_job_state`), with the driver's
/// post-gate configuration inline: worker threads interleave records, so
/// each record must stand alone.
#[derive(Clone, Copy, Debug)]
pub struct JobRecord {
    pub start: u64,
    pub rep_gated: bool,
    pub ramp_depth: u64,
    pub cfg: DriverCfg,
}

/// The MT frame's job geometry (`compress_slice_mt`'s resolution).
#[derive(Clone, Copy, Debug)]
pub struct FrameRecord {
    pub strip: u64,
    pub job_size: u64,
    pub n_jobs: u64,
    /// The history span below its start a job's matcher may reference.
    pub job_overlap: u64,
    pub fast_window: Option<u64>,
    pub capture_window: Option<u64>,
    pub deep_ldm: Option<u64>,
    pub opt_feed: Option<u64>,
    /// Jobs run `LdmArming::JobPrefix` (the capture class) rather than
    /// `Job`.
    pub job_prefix_ldm: bool,
    pub header_window: u64,
    pub reach_keep: bool,
    /// The reach probe donated job zero's span.
    pub donated: bool,
    pub spf: SpfTag,
    /// The jobs' storage verdict (see `plain_job_balanced_store`): the
    /// balanced row's twin when the frame armed the substitution.
    pub bal_store: StoreTag,
    /// The reach-band shared strip armed: the jobs adopt the calling
    /// thread's shared row-strip build in place of the per-job
    /// clear-plus-fill, and `job_overlap` carries the twin's 16 MiB
    /// reach.
    pub shared_row_strip: bool,
}

#[derive(Debug)]
pub enum Record {
    Reset(ResetRecord),
    Flip(FlipRecord),
    StEntry(StEntryRecord),
    Job(JobRecord),
    Frame(FrameRecord),
}

/// Cap on retained records: streaming runs could otherwise grow the log
/// without bound; bulk frames land three orders below it.
const CAP: usize = 1 << 16;

pub(crate) use super::match_generator::LdmArming;
use super::reach_probe::ReachChoice;

pub(crate) fn arming_tag(a: LdmArming) -> ArmingTag {
    match a {
        LdmArming::Frame => ArmingTag::Frame,
        LdmArming::FrameScreened => ArmingTag::FrameScreened,
        #[cfg(feature = "std")]
        LdmArming::Job => ArmingTag::Job,
        #[cfg(feature = "std")]
        LdmArming::JobPrefix => ArmingTag::JobPrefix,
        #[cfg(feature = "std")]
        LdmArming::FarDead => ArmingTag::FarDead,
        LdmArming::ProbeKeep => ArmingTag::ProbeKeep,
    }
}

pub(crate) fn reach_keep(c: ReachChoice) -> bool {
    c == ReachChoice::Keep
}

fn log() -> &'static Mutex<Vec<Record>> {
    static LOG: OnceLock<Mutex<Vec<Record>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

/// Append a record; at the cap the log stays at its head (a probe that
/// overflows is reading trends, not the tail).
pub(crate) fn record(r: Record) {
    let mut log = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if log.len() < CAP {
        log.push(r);
    }
}

/// Drain the recorded configuration events.
pub fn take() -> Vec<Record> {
    let mut log = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    core::mem::take(&mut *log)
}
