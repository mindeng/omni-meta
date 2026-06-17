//! 种子语料生成器：把 omni-meta-fixtures 的命名 fixture 写入各 target 的 corpus 目录，
//! 使模糊器从「近合法」输入起步。用法：`cargo +nightly run --bin seeds`。

use std::fs;
use std::path::Path;

fn write_seeds(target: &str, seeds: &[(&'static str, Vec<u8>)]) {
    let dir = Path::new("corpus").join(target);
    fs::create_dir_all(&dir).expect("create corpus dir");
    for (name, bytes) in seeds {
        let path = dir.join(format!("{name}.bin"));
        fs::write(&path, bytes).expect("write seed");
    }
    println!("{target}: {} 个种子 → {}", seeds.len(), dir.display());
}

fn main() {
    use omni_meta_fixtures as f;
    let files = f::file_corpus();
    write_seeds("differential", &files);
    write_seeds("read_slice_bounded", &files);
    write_seeds("isobmff", &f::bmff_corpus());
    write_seeds("ebml", &f::ebml_corpus());
    write_seeds("exif", &f::tiff_corpus());
    write_seeds("xmp", &f::xmp_corpus());
}
