//! Compress every corpus file into a directory (named `<corpus>.raw.zst`) so
//! two builds can be compared byte for byte: `dump_comp dir_a` on each build,
//! then `cmp`. Byte equality is the regression gate for optimizations that
//! must not change the encoder's choices.
use std::fs;

fn main() {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");
    let out_dir = std::env::args().nth(1).expect("out dir");
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".raw") {
            continue;
        }
        let raw = fs::read(&path).unwrap();
        let comp = ruzstd::encoding::compress_slice_to_vec(&raw[..], ruzstd::Level::Fastest);
        fs::write(format!("{out_dir}/{name}.zst"), &comp).unwrap();
    }
}
