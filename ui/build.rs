fn main() {
    let ui_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dist = ui_root.join("dist");
    let _ = std::fs::create_dir_all(&dist);
    for file in &["index.html", "tray.html"] {
        let src = ui_root.join(file);
        let dst = dist.join(file);
        if src.exists() {
            let _ = std::fs::copy(&src, &dst);
        }
        println!("cargo:rerun-if-changed={}", src.display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        ui_root.join("tauri.conf.json").display()
    );
    tauri_build::build();
}
