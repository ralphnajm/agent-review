//! Embeds the whiteboard's files (`data/whiteboard`, built from `whiteboard/`)
//! in the binary, as `WHITEBOARD`: their paths and contents; and compiles the
//! app's icons (`data/icons/phosphor`) into a resource bundle.

use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

fn main() {
    glib_build_tools::compile_resources(
        &["data/icons/phosphor"],
        "data/icons/phosphor/icons.gresource.xml",
        "icons.gresource",
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/whiteboard");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let mut code = String::from("pub static WHITEBOARD: &[(&str, &[u8])] = &[\n");
    for (path, file) in files {
        writeln!(code, "    ({path:?}, include_bytes!({file:?})),").unwrap();
    }
    code.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("whiteboard.rs"), code).unwrap();
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            files.push((relative, path.to_string_lossy().into_owned()));
        }
    }
}
