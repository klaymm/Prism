use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let kit = manifest_dir.join("../crates/prism-ui/ui");
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_library_paths(HashMap::from([("prism".to_string(), kit)]));
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");

    write_resources(&manifest_dir, &out_dir);
    write_flags(&manifest_dir.join("assets/flags"), &out_dir);
}

fn write_resources(manifest_dir: &Path, out_dir: &Path) {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let numeric = format!("{},{},{},0", parts[0], parts[1], parts[2]);
    let path = |p: &str| manifest_dir.join(p).display().to_string().replace('\\', "/");
    let rc = format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041904B0"
    BEGIN
      VALUE "CompanyName", "klaymm"
      VALUE "FileDescription", "Prism"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "prism"
      VALUE "LegalCopyright", "klaymm, GPL-3.0-or-later"
      VALUE "OriginalFilename", "prism.exe"
      VALUE "ProductName", "Prism"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x419, 1200
  END
END
"#,
        icon = path("icons/icon.ico"),
        manifest = path("prism.exe.manifest"),
    );
    let rc_path = out_dir.join("prism.rc");
    std::fs::write(&rc_path, rc).unwrap();
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=prism.exe.manifest");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
}

fn write_flags(dir: &Path, out_dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".svg"))
        .collect();
    names.sort();
    let mut code = String::from("pub fn svg(code: &str) -> Option<&'static [u8]> {\n    match code {\n");
    for name in names {
        let cc = name.trim_end_matches(".svg");
        let file = dir.join(&name).display().to_string().replace('\\', "/");
        writeln!(code, "        {cc:?} => Some(include_bytes!({file:?})),").unwrap();
    }
    code.push_str("        _ => None,\n    }\n}\n");
    std::fs::write(out_dir.join("flags.rs"), code).unwrap();
}
