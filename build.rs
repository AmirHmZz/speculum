use std::{env, fs, path::Path};

fn collect(root: &Path, dir: &Path, output: &Path, entries: &mut Vec<String>) {
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).expect("read dashboard assets") {
        let path = entry.expect("dashboard asset").path();
        if path.is_dir() {
            collect(root, &path, output, entries);
        } else {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let absolute = output.join(&name);
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::copy(&path, &absolute).expect("snapshot dashboard asset");
            entries.push(format!(
                "{name:?} => Some(include_bytes!({:?})),",
                absolute.to_string_lossy()
            ));
        }
    }
}
fn main() {
    println!("cargo:rerun-if-changed=dashboard/dist");
    println!("cargo:rerun-if-env-changed=SPECULUM_REQUIRE_DASHBOARD");
    let mut entries = Vec::new();
    collect(
        Path::new("dashboard/dist"),
        Path::new("dashboard/dist"),
        &Path::new(&env::var("OUT_DIR").unwrap()).join("dashboard"),
        &mut entries,
    );
    if env::var_os("SPECULUM_REQUIRE_DASHBOARD").is_some() && entries.is_empty() {
        panic!("Build dashboard assets before compiling the release image");
    }
    entries.sort();
    let source = format!(
        "pub fn asset(path: &str) -> Option<&'static [u8]> {{ match path {{ {} _ => None }} }}",
        entries.join("\n")
    );
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("assets.rs"),
        source,
    )
    .unwrap();
}
