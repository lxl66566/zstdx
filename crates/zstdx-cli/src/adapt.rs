//! `--adapt`: dynamic compression-level adaptation, zstd 1.5.7's fileio.c
//! engine (`FIO_compressZstdFrame`) translated from per-MT-job
//! frame-progression counters to per-chunk phase timings. The library fixes
//! `EncoderOptions` at construction (no mid-frame re-parametering), so the
//! input is cut into 8 MiB chunks (the reference's
//! `ADAPT_WINDOWLOG_DEFAULT`) and every chunk compresses as its own frame;
//! a level step applies to every chunk after the one that triggered it.
//! The verdicts keep the reference's thresholds: step up when output
//! cannot keep up with production (`produced > flushed*9/8`) or when input
//! cannot feed compression (input never blocked), step down when
//! compression lags input while output keeps up (the 33/32 margins), and
//! evaluate at most once per `REFRESH_RATE` (1/6 s). Decisions trace at
//! `-vv`, the reference's DISPLAYLEVEL(6) equivalent.

use std::{
    io::{ErrorKind, Read, Write},
    time::{Duration, Instant},
};

use tracing::trace;
use zstdx::{EncoderDictionary, EncoderOptions, InputShape, Level};

use crate::{AnyResult, Output, Settings};

/// The reference's `REFRESH_RATE` (fileio_common.h): one adaptation check
/// per 1/6 s at most; a check evaluates the last chunk's timings and steps
/// the level by at most one.
const REFRESH_RATE: Duration = Duration::from_nanos(1_000_000_000 / 6);
/// The reference's `ADAPT_WINDOWLOG_DEFAULT` (fileio.c): 8 MiB. There it
/// caps the frame window; here it is also the chunk (frame) size the level
/// walks on, and the window forced on each chunk frame when `--long` is
/// absent.
const ADAPT_WINDOW_LOG: u32 = 23;
/// The engine's level bounds: no negative rungs exist (`Level::from_zstd`
/// clamps them to 1) and 0 is the raw-store row, so the walk lives on
/// 1..=22. The reference seeds its bounds with `ZSTD_minCLevel()` (about
/// -131072) and `ZSTD_maxCLevel()` (22); the effective floor is the one
/// the engine enforces, which also makes the reference's "skip 0" rule
/// vacuous here (a step below 1 clamps back to the floor).
const MIN_LEVEL: i32 = 1;
const MAX_LEVEL: i32 = 22;
/// The chunk (and buffer) size of the pipeline.
const CHUNK: usize = 1 << ADAPT_WINDOW_LOG;

/// The `--adapt=min=..,max=..` bounds as written (validated `min <= max`,
/// not yet clamped to the engine's range — the reference compares these raw
/// values against the starting level too).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct AdaptRange {
    pub(crate) min: i32,
    pub(crate) max: i32,
}

/// Parse `min=N` and/or `max=N`, comma-separated, either order — the
/// reference's `parseAdaptParameters` (zstdcli.c): a key must lead every
/// comma-separated segment, nothing may trail, and `min > max` is
/// incoherent. Missing keys keep the engine-bounds default.
pub(crate) fn parse_range(text: &str) -> Result<AdaptRange, String> {
    let mut range = AdaptRange {
        min: MIN_LEVEL,
        max: MAX_LEVEL,
    };
    let mut rest = text;
    loop {
        let (slot, tail) = if let Some(tail) = rest.strip_prefix("min=") {
            (&mut range.min, tail)
        } else if let Some(tail) = rest.strip_prefix("max=") {
            (&mut range.max, tail)
        } else {
            return Err(format!(
                "invalid compression parameter {text:?} (expected min=N and/or max=N)"
            ));
        };
        let (value, tail) = split_i32(tail, text)?;
        *slot = value;
        if let Some(next) = tail.strip_prefix(',') {
            rest = next;
        } else {
            // no separator: end of the string; the leftover must be empty
            // (checked below, like the reference's end-of-string test)
            rest = tail;
            break;
        }
    }
    if !rest.is_empty() || text.is_empty() {
        return Err(format!(
            "invalid compression parameter {text:?} (expected min=N and/or max=N)"
        ));
    }
    if range.min > range.max {
        return Err(format!(
            "incoherent adaptation limits {text:?} (min must not exceed max)"
        ));
    }
    Ok(range)
}

/// Split a leading `-?[0-9]+` off `rest` (the reference's signed
/// `readIntFromChar`).
fn split_i32<'a>(rest: &'a str, whole: &str) -> Result<(i32, &'a str), String> {
    let digits_start = usize::from(rest.starts_with('-'));
    let end = rest[digits_start..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(rest.len(), |offset| digits_start + offset);
    rest[..end]
        .parse::<i32>()
        .map(|value| (value, &rest[end..]))
        .map_err(|_| format!("invalid compression parameter {whole:?} (bad level)"))
}

/// One chunk's verdict, mirroring which reference branch fired.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Verdict {
    /// Output cannot keep up with production (the reference's `produced >
    /// flushed*9/8` and its stalled-compression check): step up, invest the
    /// pipeline's slack in ratio.
    OutputBound,
    /// Input cannot feed compression (the reference's "input is never
    /// blocked"): step up, the encoder idles either way.
    InputBound,
    /// Compression lags input while output keeps up (the reference's
    /// "recommend faster" with its 33/32 margins): step down.
    EncodeBound,
    /// Inside the band between the thresholds: keep the level.
    Hold,
}

impl Verdict {
    /// Decide on one chunk's phase timings (`read` fills the chunk, `encode`
    /// compresses it, `output` waits inside output writes). Nanosecond
    /// magnitudes compare in u128 so the products cannot overflow.
    fn of(read: Duration, encode: Duration, output: Duration) -> Self {
        let (r, c, w) = (read.as_nanos(), encode.as_nanos(), output.as_nanos());
        if w * 8 > c * 9 {
            Self::OutputBound
        } else if r >= c {
            Self::InputBound
        } else if w * 32 <= c * 33 {
            Self::EncodeBound
        } else {
            Self::Hold
        }
    }
}

/// The walking level: which level the next chunk takes. Pure decision
/// logic, separated from the pipeline for testing.
pub(crate) struct Policy {
    level: i32,
    /// Effective bounds: the parsed range clamped to the engine's levels.
    min: i32,
    max: i32,
    /// When the last check ran; the reference throttles the whole
    /// measurement-plus-step block to one per REFRESH_RATE.
    last_check: Instant,
}

/// The result of feeding one chunk's timings to the policy; a step carries
/// the new level and the verdict that drove it (for the `-vv` lines).
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    /// Within REFRESH_RATE of the previous check.
    Throttled,
    /// Evaluated without a level change (hold, or a bound held the level).
    Held,
    /// Stepped to the level for the verdict.
    Stepped(i32, Verdict),
}

impl Policy {
    /// Clamp the starting level into the effective bounds (the CLI already
    /// clamped it into the raw range, mirroring zstdcli.c).
    pub(crate) fn new(start: i32, range: AdaptRange) -> Self {
        let min = range.min.clamp(MIN_LEVEL, MAX_LEVEL);
        let max = range.max.clamp(MIN_LEVEL, MAX_LEVEL);
        Self {
            level: start.clamp(min, max),
            min,
            max,
            last_check: Instant::now(),
        }
    }

    pub(crate) fn level(&self) -> i32 {
        self.level
    }

    /// Feed one chunk's phase timings; `now` clocks the throttle.
    pub(crate) fn observe(
        &mut self,
        read: Duration,
        encode: Duration,
        output: Duration,
        now: Instant,
    ) -> Outcome {
        if now.duration_since(self.last_check) < REFRESH_RATE {
            return Outcome::Throttled;
        }
        self.last_check = now;
        let verdict = Verdict::of(read, encode, output);
        let next = match verdict {
            Verdict::OutputBound | Verdict::InputBound => {
                (self.level + 1).min(MAX_LEVEL).min(self.max)
            },
            Verdict::EncodeBound => (self.level - 1).max(self.min),
            Verdict::Hold => return Outcome::Held,
        };
        if next == self.level {
            return Outcome::Held;
        }
        self.level = next;
        Outcome::Stepped(next, verdict)
    }
}

/// Compress with the adaptive level: chunked frames at the walking level.
pub(crate) fn compress(
    mut reader: Box<dyn Read>,
    output: &Output,
    settings: &Settings,
    dict: Option<&[u8]>,
) -> AnyResult<u64> {
    let (writer, _file) = crate::open_output(output)?;
    let mut out = ChunkOutput::new(writer);
    let result = pipeline(&mut reader, &mut out, settings, dict, CHUNK);
    if result.is_err() {
        crate::remove_partial_output(output);
    }
    result?;
    Ok(out.count)
}

/// The chunk pipeline proper; `chunk` is a parameter so tests can walk
/// several chunks cheaply.
fn pipeline<W: Write>(
    reader: &mut dyn Read,
    out: &mut ChunkOutput<W>,
    settings: &Settings,
    dict: Option<&[u8]>,
    chunk: usize,
) -> AnyResult<()> {
    let range = settings.adapt.expect("caller checked adapt is set");
    let mut policy = Policy::new(settings.level, range);
    let dictionary = match dict {
        Some(raw) => Some(EncoderDictionary::parse(raw)?),
        None => None,
    };
    let mut bufs = [vec![0u8; chunk], vec![0u8; chunk]];
    let mut filled = fill(reader, &mut bufs[0])?;
    let mut slot = 0;
    while filled.len > 0 {
        let level = policy.level();
        let started = Instant::now();
        let mut encoder = zstdx::stream::write::Encoder::with_options(
            &mut *out,
            chunk_options(settings, level, filled.len, dictionary.as_ref()),
        )?;
        encoder.write_all(&bufs[slot][..filled.len])?;
        // Fill the next chunk while the workers compress this one; the read
        // time is subtracted back out of the encode phase below.
        let next = if filled.eof {
            Filled::EOF
        } else {
            fill(reader, &mut bufs[slot ^ 1])?
        };
        encoder.finish()?;
        let output_wait = out.take_wait();
        let encode = started
            .elapsed()
            .saturating_sub(next.read)
            .saturating_sub(output_wait);
        report(&mut policy, filled.read, encode, output_wait, level);
        filled = next;
        slot ^= 1;
    }
    // An empty input still produces one (empty) frame, like every other
    // compression path.
    if out.count == 0 {
        let encoder = zstdx::stream::write::Encoder::with_options(
            &mut *out,
            chunk_options(settings, policy.level(), 0, dictionary.as_ref()),
        )?;
        encoder.finish()?;
    }
    Ok(())
}

/// Options for one chunk frame: the walking level, an exact per-chunk
/// pledge, the reference's 8 MiB window unless `--long`/`--patch-from`
/// forced one, and the shared knobs.
fn chunk_options(
    settings: &Settings,
    level: i32,
    len: usize,
    dictionary: Option<&EncoderDictionary>,
) -> EncoderOptions {
    let mut options = EncoderOptions::new(Level::from_zstd(level))
        .checksum(settings.checksum)
        .pledged_size(Some(len as u64))
        .with_input_shape(
            InputShape::default().with_window_log(settings.window_log.unwrap_or(ADAPT_WINDOW_LOG)),
        );
    if settings.threads > 1 {
        options = options.workers(settings.threads);
    }
    if let Some(dictionary) = dictionary {
        options = options.parsed_dictionary(dictionary);
    }
    options
}

/// One filled chunk: its length, the summed time of its read calls, and
/// whether the reader hit end of input.
struct Filled {
    len: usize,
    read: Duration,
    eof: bool,
}

impl Filled {
    const EOF: Self = Self {
        len: 0,
        read: Duration::ZERO,
        eof: true,
    };
}

/// Fill `buf` from `reader`, timing only the read calls (the encode phase
/// runs elsewhere). Interrupted reads retry, like `read_exact`.
fn fill(reader: &mut dyn Read, buf: &mut [u8]) -> AnyResult<Filled> {
    let mut len = 0;
    let mut nanos = 0u128;
    let mut eof = false;
    while len < buf.len() {
        let started = Instant::now();
        match reader.read(&mut buf[len..]) {
            Ok(0) => {
                eof = true;
                break;
            },
            Ok(n) => {
                len += n;
                nanos += started.elapsed().as_nanos();
            },
            Err(err) if err.kind() == ErrorKind::Interrupted => {},
            Err(err) => return Err(err.into()),
        }
    }
    Ok(Filled {
        len,
        read: Duration::from_nanos(nanos as u64),
        eof,
    })
}

/// Log one evaluated chunk at `-vv` (the reference's DISPLAYLEVEL(6)
/// measurement and decision lines).
fn report(policy: &mut Policy, read: Duration, encode: Duration, output: Duration, from: i32) {
    let outcome = policy.observe(read, encode, output, Instant::now());
    if outcome == Outcome::Throttled {
        return;
    }
    trace!(
        "adapt: read {} µs, encode {} µs, output {} µs",
        read.as_micros(),
        encode.as_micros(),
        output.as_micros()
    );
    if let Outcome::Stepped(to, verdict) = outcome {
        match verdict {
            Verdict::OutputBound => trace!("compression is output-bound => slow down"),
            Verdict::InputBound => trace!("input is slower than compression => slow down"),
            Verdict::EncodeBound => trace!("compression is slower than input => speed up"),
            Verdict::Hold => unreachable!("a step implies a verdict that steps"),
        }
        // the reference's decision lines, with the level it never prints
        if to > from {
            trace!("slower speed, higher compression (level {to})");
        } else {
            trace!("faster speed, lighter compression (level {to})");
        }
    }
}

/// The chunk pipeline's output sink: counts bytes (there may be no file to
/// stat) and tallies the time spent inside write calls — the output-bound
/// signal, the reference's produced-vs-flushed comparison in time form.
struct ChunkOutput<W: Write> {
    inner: W,
    count: u64,
    wait_nanos: u128,
}

impl<W: Write> ChunkOutput<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            count: 0,
            wait_nanos: 0,
        }
    }

    /// Drain the accumulated output wait.
    fn take_wait(&mut self) -> Duration {
        Duration::from_nanos(self.wait_nanos as u64)
    }
}

impl<W: Write> Write for ChunkOutput<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let started = Instant::now();
        let written = self.inner.write(buf)?;
        self.wait_nanos += started.elapsed().as_nanos();
        self.count += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, Read as _},
        time::{Duration, Instant},
    };

    use zstdx::DecoderOptions;

    use super::{AdaptRange, CHUNK, MAX_LEVEL, MIN_LEVEL, Outcome, Policy, Verdict};
    use crate::Settings;

    /// Millisecond durations, exact in nanoseconds.
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn settings(level: i32, range: AdaptRange) -> Settings {
        Settings {
            level,
            threads: 1,
            checksum: true,
            window_log: None,
            stream_size: None,
            size_hint: None,
            max_window: None,
            adapt: Some(range),
        }
    }

    #[test]
    fn parse_accepts_reference_forms() {
        use super::parse_range;
        let full = AdaptRange { min: 1, max: 19 };
        assert_eq!(parse_range("min=1,max=19").unwrap(), full);
        assert_eq!(parse_range("max=19,min=1").unwrap(), full, "either order");
        assert_eq!(
            parse_range("min=1").unwrap(),
            AdaptRange {
                min: 1,
                max: MAX_LEVEL
            },
            "missing max keeps the engine default"
        );
        assert_eq!(parse_range("max=10").unwrap(), AdaptRange {
            min: MIN_LEVEL,
            max: 10
        });
        // negative bounds parse like the reference's readIntFromChar
        assert_eq!(parse_range("min=-5,max=8").unwrap(), AdaptRange {
            min: -5,
            max: 8
        });
    }

    #[test]
    fn parse_rejects_malformed() {
        use super::parse_range;
        for bad in [
            "",
            "bogus=1",
            "min=",
            "min=x",
            "min=1,max=2,x=3",
            "min=1,max=2,",
            "min=1x",
            "min 1",
        ] {
            assert!(parse_range(bad).is_err(), "{bad:?} must not parse");
        }
        // min > max is incoherent (zstdcli.c), not silently swapped
        assert!(parse_range("min=19,max=1").is_err());
        assert!(parse_range("min=-1,max=-9").is_err());
    }

    /// The verdict thresholds, exactly the reference's: output-bound at
    /// 9/8, input-bound at bare `read >= encode`, encode-bound needs output
    /// within 33/32, and the gap between them holds.
    #[test]
    fn verdict_thresholds_mirror_fileio() {
        // output wait past encode * 9/8 (80 -> 91 crosses 90)
        assert_eq!(Verdict::of(ms(100), ms(80), ms(91)), Verdict::OutputBound);
        // just under the margin falls through to the input compare
        assert_eq!(Verdict::of(ms(100), ms(80), ms(89)), Verdict::InputBound);
        // input cannot feed compression: read >= encode, boundary included
        assert_eq!(Verdict::of(ms(100), ms(100), ms(1)), Verdict::InputBound);
        // compression lags input while output keeps up
        assert_eq!(Verdict::of(ms(80), ms(100), ms(50)), Verdict::EncodeBound);
        // compression lags input but output lags past 33/32 (100 -> 104):
        // neither direction fires, hold
        assert_eq!(Verdict::of(ms(80), ms(100), ms(104)), Verdict::Hold);
    }

    #[test]
    fn start_level_clamps_into_range() {
        let range = AdaptRange { min: 5, max: 12 };
        assert_eq!(Policy::new(3, range).level(), 5, "pulled to min");
        assert_eq!(Policy::new(19, range).level(), 12, "pulled to max");
        assert_eq!(Policy::new(9, range).level(), 9);
        // out-of-engine bounds clamp to the engine's range
        let wide = AdaptRange { min: -50, max: 30 };
        assert_eq!(Policy::new(25, wide).level(), MAX_LEVEL);
        assert_eq!(Policy::new(-9, wide).level(), MIN_LEVEL);
    }

    /// Fast input (compression cannot keep up) walks the level down to the
    /// floor; slow input walks it up to the ceiling; both then clamp.
    #[test]
    fn stepping_follows_io_balance() {
        let mut policy = Policy::new(10, AdaptRange { min: 5, max: 12 });
        // a clock advancing one REFRESH_RATE per observation
        let mut now = Instant::now();
        let mut advance = || {
            now += super::REFRESH_RATE;
            now
        };
        // compression lags input 10:1 => step down to the floor
        for expect in (5..10).rev() {
            assert_eq!(
                policy.observe(ms(10), ms(100), ms(1), advance()),
                Outcome::Stepped(expect, Verdict::EncodeBound)
            );
        }
        assert_eq!(
            policy.observe(ms(10), ms(100), ms(1), advance()),
            Outcome::Held
        );
        // input lags compression 10:1 => climb back to the ceiling
        for expect in 6..=12 {
            assert_eq!(
                policy.observe(ms(100), ms(10), ms(1), advance()),
                Outcome::Stepped(expect, Verdict::InputBound)
            );
        }
        assert_eq!(
            policy.observe(ms(100), ms(10), ms(1), advance()),
            Outcome::Held
        );
    }

    /// The throttle: no two checks within one REFRESH_RATE, mirroring the
    /// reference's lastAdaptTime gate (including the first check after
    /// construction).
    #[test]
    fn refresh_rate_throttles_checks() {
        let mut policy = Policy::new(3, AdaptRange { min: 1, max: 22 });
        let now = Instant::now();
        let slow_input = (ms(100), ms(10), ms(1));
        // inside the first refresh window: throttled even on a clear signal
        assert_eq!(
            policy.observe(slow_input.0, slow_input.1, slow_input.2, now),
            Outcome::Throttled
        );
        // past it: the same signal steps
        let late = now + super::REFRESH_RATE;
        assert_eq!(
            policy.observe(slow_input.0, slow_input.1, slow_input.2, late),
            Outcome::Stepped(4, Verdict::InputBound)
        );
        // and immediately after a step: throttled again
        let just_after = late + super::REFRESH_RATE / 2;
        assert_eq!(
            policy.observe(slow_input.0, slow_input.1, slow_input.2, just_after),
            Outcome::Throttled
        );
    }

    /// The pipeline over several chunk frames: the output decodes back to
    /// the input (the decoder must consume every concatenated frame), and
    /// an empty input still yields one frame.
    #[test]
    fn chunked_output_decodes_multi_frame() {
        let pattern: Vec<u8> = (0..u8::MAX).cycle().take(200_000).collect();
        let mut out = super::ChunkOutput::new(Vec::new());
        super::pipeline(
            &mut Cursor::new(pattern.clone()),
            &mut out,
            &settings(3, AdaptRange { min: 1, max: 12 }),
            None,
            64 * 1024,
        )
        .unwrap();
        let compressed = out.inner;
        assert!(!compressed.is_empty());
        let frames = compressed
            .windows(4)
            .filter(|w| w == &[0x28, 0xb5, 0x2f, 0xfd])
            .count();
        assert_eq!(frames, 4, "200k bytes over 64k chunks: four frames");
        let mut decoder = zstdx::stream::read::Decoder::with_options(
            Cursor::new(compressed),
            DecoderOptions::new(),
        )
        .unwrap();
        let mut restored = Vec::new();
        decoder.read_to_end(&mut restored).unwrap();
        assert_eq!(restored, pattern);
    }

    #[test]
    fn empty_input_emits_one_frame() {
        let mut out = super::ChunkOutput::new(Vec::new());
        super::pipeline(
            &mut Cursor::new([]),
            &mut out,
            &settings(3, AdaptRange { min: 1, max: 12 }),
            None,
            64 * 1024,
        )
        .unwrap();
        let compressed = out.inner;
        assert!(!compressed.is_empty(), "empty input still frames");
        let mut decoder = zstdx::stream::read::Decoder::with_options(
            Cursor::new(compressed),
            DecoderOptions::new(),
        )
        .unwrap();
        let mut restored = Vec::new();
        decoder.read_to_end(&mut restored).unwrap();
        assert!(restored.is_empty());
    }

    /// The chunk size is the reference's 8 MiB window.
    #[test]
    fn chunk_is_the_reference_window() {
        assert_eq!(CHUNK, 8 * 1024 * 1024);
    }
}
