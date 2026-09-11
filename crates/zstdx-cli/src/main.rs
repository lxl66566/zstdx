extern crate zstdx;
mod progress;
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::{ContextCompat, WrapErr};
use progress::{ProgressMonitor, fmt_size};
use tracing::info;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use zstdx::Level;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

// TODO: implement a dictionary creation command, and a command for benchmarking
#[derive(Subcommand)]
enum Commands {
    /// Compress a single file. If no output file is specified,
    /// output will be written to <INPUT_FILE>.zst
    Compress {
        /// File to compress
        input_file: PathBuf,
        /// Where the compressed file is written
        /// [default: <INPUT_FILE>.zst]
        output_file: Option<PathBuf>,
        /// How thoroughly the file should be compressed. A higher level will take
        /// more time to compress but result in a smaller file, and vice versa.
        ///
        /// 1-22 follow libzstd's ladder; 0 stores uncompressed.
        #[arg(
            short,
            long,
            value_name = "COMPRESSION_LEVEL",
            default_value_t = 1,
            verbatim_doc_comment
        )]
        level: u8,
    },
    Decompress {
        /// .zst archive to decompress
        input_file: PathBuf,
        /// Where the compressed file is written
        /// [default: <ARCHIVE_NAME>]
        output_file: Option<PathBuf>,
    },
}

fn main() -> color_eyre::Result<()> {
    // Process CLI arguments
    let cli = Cli::parse();
    // Initialize logging (with indicatif integration)
    let indicatif_layer = IndicatifLayer::new();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(indicatif_layer.get_stderr_writer())
                .without_time(),
        )
        .with(indicatif_layer)
        .init();

    let command: Commands = cli.command.wrap_err("no subcommand provided").unwrap();
    match command {
        Commands::Compress {
            input_file,
            output_file,
            level,
        } => {
            let output_file = output_file.unwrap_or_else(|| add_extension(&input_file, ".zst"));
            compress(input_file, output_file, level)?;
        },
        Commands::Decompress {
            input_file,
            output_file,
        } => {
            let output_file = output_file.unwrap_or(
                input_file
                    .file_stem()
                    .expect("input has a file name")
                    .into(),
            );
            decompress(input_file, output_file)?;
        },
    }
    Ok(())
}

fn compress(input: PathBuf, output: PathBuf, level: u8) -> color_eyre::Result<()> {
    info!("compressing {input:?} to {output:?}");
    let compression_level: Level = Level::from_zstd(level as i32);
    let source_file = File::open(input).wrap_err("failed to open input file")?;
    let source_size = source_file.metadata()?.len() as usize;
    let buffered_source = BufReader::new(source_file);
    let encoder_input = ProgressMonitor::new(buffered_source, source_size);
    let output: File = File::create(output).wrap_err("failed to open output file for writing")?;

    zstdx::encoding::compress(encoder_input, &output, compression_level);
    let compressed_size = output.metadata()?.len();
    let compression_ratio = compressed_size as f64 / source_size as f64 * 100.0;
    info!(
        "{} ——> {} ({compression_ratio:.2}%)",
        fmt_size(source_size as f64),
        fmt_size(compressed_size as f64)
    );
    Ok(())
}

fn decompress(input: PathBuf, output: PathBuf) -> color_eyre::Result<()> {
    info!("extracting {input:?} to {output:?}");
    let source_file = File::open(input).wrap_err("failed to open input file")?;
    let source_size = source_file.metadata()?.len() as usize;
    let buffered_source = BufReader::new(source_file);
    let decoder_input = ProgressMonitor::new(buffered_source, source_size);
    let mut output: File =
        File::create(output).wrap_err("failed to open output file for writing")?;

    let mut decoder = zstdx::decoding::StreamingDecoder::new(decoder_input)?;

    std::io::copy(&mut decoder, &mut output)?;

    info!(
        "inflated {} ——> {}",
        fmt_size(source_size as f64),
        fmt_size(output.metadata()?.len() as f64),
    );
    Ok(())
}

/// A temporary utility function that appends a file extension
/// to the provided path buf.
///
/// Pending removal when our MSRV reaches 1.91 so we can use
///
/// <https://doc.rust-lang.org/std/path/struct.PathBuf.html#method.add_extension>
fn add_extension<P: AsRef<Path>>(path: &Path, extension: P) -> PathBuf {
    let mut output = path.to_path_buf().into_os_string();
    output.push(extension.as_ref().as_os_str());

    output.into()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::add_extension;

    #[test]
    fn extension_added() {
        let filename = PathBuf::from("README.md");
        assert_eq!(
            add_extension(&filename, ".zst"),
            PathBuf::from("README.md.zst")
        );
    }
}
