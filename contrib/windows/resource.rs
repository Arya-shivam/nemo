//! Build script for both programs: on Windows it gives the .exe the Nemo
//! icon and the name and version Explorer, Task Manager and Apps show. gpui
//! loads icon 1 from the running program for its window, so that's set too.
//!
//! The resource script is written out here with absolute paths rather than
//! kept as a file, because llvm-rc (building from Linux) and rc.exe look for
//! relative paths in different places.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=../../assets/gyotaku.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let env = |key: &str| std::env::var(key).unwrap();
    let icon = PathBuf::from(env("CARGO_MANIFEST_DIR")).join("../../assets/gyotaku.ico");
    let version = env("CARGO_PKG_VERSION");
    let numbers = format!(
        "{},{},{},0",
        env("CARGO_PKG_VERSION_MAJOR"),
        env("CARGO_PKG_VERSION_MINOR"),
        env("CARGO_PKG_VERSION_PATCH"),
    );
    let name = env("CARGO_PKG_NAME");
    let script = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Nemo"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{name}"
      VALUE "LegalCopyright", "{license}"
      VALUE "OriginalFilename", "{name}.exe"
      VALUE "ProductName", "Nemo"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        // Forward slashes work for both compilers and need no escaping.
        icon = icon.display().to_string().replace('\\', "/"),
        description = env("CARGO_PKG_DESCRIPTION"),
        license = env("CARGO_PKG_LICENSE"),
    );
    let rc = PathBuf::from(env("OUT_DIR")).join("gyotaku.rc");
    std::fs::write(&rc, script).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
