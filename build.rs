fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=assets/windows.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
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
