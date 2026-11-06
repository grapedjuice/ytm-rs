// Embed the app icon into the Windows executable (Explorer, taskbar, shortcuts).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon/icon.ico");
    theme_covers();
    theme_presets();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon/icon.ico")
            .set_manifest(MANIFEST)
            .compile()
            .expect("failed to embed Windows icon");
    }
}

// The segment heap returns freed memory to the OS far more readily than the default
// NT heap, which keeps the resident footprint down after bursts (images, JSON).
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <heapType xmlns="http://schemas.microsoft.com/SMI/2020/WindowsSettings">SegmentHeap</heapType>
    </windowsSettings>
  </application>
</assembly>
"#;

// Embed every theme (assets/themes/presets/<id>.json) as a table of id and text.
fn theme_presets() {
    let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/themes/presets");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default();
    entries.sort();
    let mut out = String::from("&[\n");
    for p in entries {
        println!("cargo:rerun-if-changed={}", p.display());
        let id = p.file_stem().unwrap().to_string_lossy();
        out += &format!("    ({id:?}, include_str!({:?})),\n", p.display().to_string());
    }
    out += "]\n";
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("theme_presets.rs"), out).unwrap();
}

// Embed every theme preset's cover thumbnail (assets/themes/covers/<id>.jpg) as a
// table the settings page looks up by preset id.
fn theme_covers() {
    let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/themes/covers");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jpg")).collect())
        .unwrap_or_default();
    entries.sort();
    let mut out = String::from("&[\n");
    for p in entries {
        let id = p.file_stem().unwrap().to_string_lossy();
        out += &format!("    ({id:?}, include_bytes!({:?}).as_slice()),\n", p.display().to_string());
    }
    out += "]\n";
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("theme_covers.rs"), out).unwrap();
}
