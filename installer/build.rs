use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let kit = manifest_dir.join("../crates/prism-ui/ui");
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_library_paths(HashMap::from([("prism".to_string(), kit)]));
    slint_build::compile_with_config("ui/setup.slint", config).expect("Slint build failed");

    write_resources(&manifest_dir, &out_dir);
}

fn write_resources(manifest_dir: &Path, out_dir: &Path) {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let numeric = format!("{},{},{},0", parts[0], parts[1], parts[2]);
    let path = |p: &str| manifest_dir.join(p).display().to_string().replace('\\', "/");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
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
      VALUE "FileDescription", "Установка Prism"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "prism-setup"
      VALUE "LegalCopyright", "klaymm, GPL-3.0-or-later"
      VALUE "OriginalFilename", "prism-setup.exe"
      VALUE "ProductName", "Prism Setup"
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
        manifest = path("prism-setup.exe.manifest"),
    );
    let rc_path = out_dir.join("prism-setup.rc");
    std::fs::write(&rc_path, rc).unwrap();
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=prism-setup.exe.manifest");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
}
