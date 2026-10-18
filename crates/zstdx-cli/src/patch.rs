//! `--patch-from=REF`: diff-style (de)compression against a reference
//! file, mirroring zstd 1.5.7's `--patch-from`. REF loads through the
//! same raw-content dictionary path as `-D` (headerless bytes as match
//! history, no dictID in the frame), and the encode window is derived
//! from the source size — overriding `--long` — exactly like fileio.c's
//! `FIO_adjustParamsForPatchFromMode`.

use std::fs;

use zstdx::decoding::DEFAULT_MAX_WINDOW_SIZE;

use crate::{Cli, Input, MAX_WINDOW_LOG, MIN_WINDOW_LOG, Mode, PREFIX};

/// The settings overrides patch mode derives for one invocation.
#[derive(Debug)]
pub(crate) struct Adjustments {
    /// Encode window log forced from the source size; supersedes `--long`
    /// (fileio.c assigns `comprParams.windowLog` unconditionally).
    pub(crate) window_log: Option<u32>,
    /// Decode window bound raised to cover REF (`FIO_adjustMemLimitFor
    /// PatchFromMode`: `memLimit = max(memLimit, dictSize)`); `None` in
    /// compression, which the reference leaves untouched too.
    pub(crate) max_window: Option<u64>,
}

/// Validate the patch invocation against its inputs and derive the
/// window adjustments. `Err` carries the diagnostic, printed by the
/// caller; warnings (the `--long` clamp convention) print here.
pub(crate) fn adjust(
    cli: &Cli,
    mode: Mode,
    inputs: &[Input],
    dict: Option<&[u8]>,
) -> Result<Adjustments, String> {
    // fileio.c: "can't use --patch-from=# on multiple files" — exactly
    // one input, stdin or file.
    if inputs.len() > 1 {
        return Err("can't use --patch-from on multiple files".into());
    }
    let dict_size = dict.map_or(0, <[u8]>::len) as u64;
    if mode == Mode::Compress {
        // The window rule sizes off the largest source (the single input
        // here); a stdin source is unknown without a pledge, which
        // fileio.c rejects outright ("Using --patch-from with stdin
        // requires --stream-size").
        let max_src = match inputs {
            [Input::Stdin] => cli
                .stream_size
                .ok_or("--patch-from with stdin requires --stream-size")?,
            // A missing file reports through open_input below with the
            // same wording as every other input; a zero size keeps the
            // window rule total here.
            [Input::File(path)] => fs::metadata(path).map_or(0, |meta| meta.len()),
            _ => unreachable!("the single input was checked above"),
        };
        let file_window_log = highbit64(max_src) + 1;
        if file_window_log > MAX_WINDOW_LOG {
            eprintln!(
                "{PREFIX}Warning: --patch-from window log {file_window_log} above the engine's \
                 maximum {MAX_WINDOW_LOG}, reduced to {MAX_WINDOW_LOG} (compression ratio will \
                 suffer)"
            );
        }
        if dict_size > 1 << MAX_WINDOW_LOG {
            eprintln!(
                "{PREFIX}Warning: --patch-from reference above the maximum window ({} bytes): its \
                 distant content is unreachable",
                1u64 << MAX_WINDOW_LOG
            );
        }
        Ok(Adjustments {
            window_log: Some(source_window_log(max_src)),
            max_window: None,
        })
    } else {
        // Decode modes (decompress, test): raise the window bound so the
        // reference's history stays in reach, as fileio.c raises memLimit.
        Ok(Adjustments {
            window_log: None,
            max_window: Some(decode_max_window(cli.memory, dict_size)),
        })
    }
}

/// fileio.c: `windowLog = max(WINDOWLOG_MIN, min(WINDOWLOG_MAX,
/// highbit(maxSrcFileSize) + 1))` — the next power of two above the
/// source, clamped to the format/engine bounds.
fn source_window_log(max_src: u64) -> u32 {
    (highbit64(max_src) + 1).clamp(MIN_WINDOW_LOG, MAX_WINDOW_LOG)
}

/// `FIO_adjustMemLimitForPatchFromMode`: `max(memLimit, dictSize)`.
fn decode_max_window(requested: Option<u64>, dict_size: u64) -> u64 {
    requested.unwrap_or(DEFAULT_MAX_WINDOW_SIZE).max(dict_size)
}

/// libzstd's `FIO_highbit64`: floor(log2(v)), with 0 for 0 (the `| 1`
/// maps 0 onto 1's logarithm instead of panicking like a bare `ilog2`).
fn highbit64(v: u64) -> u32 {
    (v | 1).ilog2()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser as _;

    use super::*;

    /// A parsed CLI with `--patch-from=ref` plus any extra flags.
    fn patch_cli(extra: &[&str]) -> Cli {
        let mut args = vec!["zstdx", "--patch-from=ref"];
        args.extend_from_slice(extra);
        Cli::try_parse_from(args).unwrap()
    }

    /// floor(log2(v)) with 0 for 0, and the clamped window table: an
    /// empty source takes the format minimum, powers of two round up to
    /// the next one, and anything past 128 MiB clamps (with a warning).
    #[test]
    fn window_rule_table() {
        assert_eq!(highbit64(0), 0);
        assert_eq!(highbit64(1), 0);
        assert_eq!(highbit64(1023), 9);
        assert_eq!(highbit64(1024), 10);
        assert_eq!(highbit64(1025), 10);
        assert_eq!(highbit64(1 << 63), 63);
        assert_eq!(highbit64(u64::MAX), 63);

        assert_eq!(source_window_log(0), MIN_WINDOW_LOG);
        assert_eq!(source_window_log(1024), 11);
        assert_eq!(source_window_log(1025), 11);
        assert_eq!(source_window_log(1 << 20), 21);
        assert_eq!(source_window_log((1 << 20) - 1), 20);
        assert_eq!(source_window_log(1 << 26), MAX_WINDOW_LOG);
        assert_eq!(source_window_log(1 << 29), MAX_WINDOW_LOG);
        assert_eq!(source_window_log(u64::MAX), MAX_WINDOW_LOG);
    }

    /// The decode bound covers the reference (the reference raises
    /// memLimit to dictSize, never lowering an explicit `-M`).
    #[test]
    fn decode_bound_covers_dict() {
        assert_eq!(decode_max_window(None, 0), DEFAULT_MAX_WINDOW_SIZE);
        assert_eq!(
            decode_max_window(None, DEFAULT_MAX_WINDOW_SIZE + 1),
            DEFAULT_MAX_WINDOW_SIZE + 1
        );
        assert_eq!(
            decode_max_window(Some(1 << 20), 1 << 22),
            1 << 22,
            "the dict wins over a smaller -M"
        );
        assert_eq!(
            decode_max_window(Some(1 << 24), 1 << 20),
            1 << 24,
            "an explicit -M above the dict stays"
        );
    }

    /// Validation shape: multiple inputs and an unpledged stdin reject;
    /// a pledged stdin derives its window from the pledge.
    #[test]
    fn adjust_validates_and_derives() {
        let stdin = [Input::Stdin];

        let cli = patch_cli(&[]);
        assert_eq!(
            adjust(&cli, Mode::Compress, &stdin, None).unwrap_err(),
            "--patch-from with stdin requires --stream-size"
        );

        let cli = patch_cli(&["--stream-size=100K"]);
        let adj = adjust(&cli, Mode::Compress, &stdin, None).unwrap();
        assert_eq!(adj.window_log, Some(17));
        assert_eq!(adj.max_window, None);

        let two = [
            Input::File(PathBuf::from("a")),
            Input::File(PathBuf::from("b")),
        ];
        assert_eq!(
            adjust(&cli, Mode::Compress, &two, None).unwrap_err(),
            "can't use --patch-from on multiple files"
        );

        // decode modes raise the bound to the dict size, never below the
        // engine default, and leave the encode window alone
        let dict = vec![0u8; 4096];
        let cli = patch_cli(&[]);
        let adj = adjust(&cli, Mode::Decompress, &stdin, Some(&dict)).unwrap();
        assert_eq!(adj.window_log, None);
        assert_eq!(adj.max_window, Some(DEFAULT_MAX_WINDOW_SIZE));
    }
}
