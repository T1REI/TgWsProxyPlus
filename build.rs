fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-env-changed=TGWS_REVIEW_BUILD");
    println!(
        "cargo:rustc-env=TGWS_REVIEW_BUILD={}",
        std::env::var("TGWS_REVIEW_BUILD").unwrap_or_else(|_| "0".into())
    );
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=assets/windows.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
            embed_ui()?;
        }
        winresource::WindowsResource::new()
            .set_icon("icon.ico")
            .set_manifest_file("assets/windows.manifest")
            .set("ProductName", "TgWsProxy+ / TgWsProxyPlus")
            .set(
                "FileDescription",
                "TgWsProxy+ — Telegram MTProto WebSocket Proxy",
            )
            .set("CompanyName", "T1REI")
            .set(
                "LegalCopyright",
                "© 2026 T1REI. Based on Flowseal/tg-ws-proxy (MIT).",
            )
            .set("InternalName", "TgWsProxyPlus")
            .set("OriginalFilename", "TgWsProxyPlus.exe")
            .compile()?;
    }
    Ok(())
}

fn embed_ui() -> std::io::Result<()> {
    use std::{fs, path::Path};
    fn collect(dir: &Path, root: &Path, output: &mut String) -> std::io::Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, root, output)?;
            } else {
                let route = format!(
                    "/{}",
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                );
                let mime = match path.extension().and_then(|e| e.to_str()) {
                    Some("html") => "text/html; charset=utf-8",
                    Some("js" | "mjs") => "text/javascript; charset=utf-8",
                    Some("css") => "text/css; charset=utf-8",
                    Some("svg") => "image/svg+xml",
                    _ => "application/octet-stream",
                };
                output.push_str(&format!(
                    "({route:?}, {mime:?}, include_bytes!({:?})),\n",
                    fs::canonicalize(&path)?.to_string_lossy()
                ));
            }
        }
        Ok(())
    }
    println!("cargo:rerun-if-changed=ui/dist");
    let root = Path::new("ui/dist");
    if !root.join("index.html").is_file() {
        return Err(std::io::Error::other(
            "Build the interface first: npm ci --prefix ui && npm run build --prefix ui",
        ));
    }
    let mut generated = String::from("pub const ASSETS: &[(&str, &str, &[u8])] = &[\n");
    collect(root, root, &mut generated)?;
    generated.push_str("];\n");
    fs::write(
        Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("ui_assets.rs"),
        generated,
    )
}
