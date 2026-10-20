fn main() {
    // 更新公钥在发布构建时注入；本地未配置时更新入口会明确显示不可用。
    println!("cargo:rerun-if-env-changed=FLUXDB_UPDATE_PUBLIC_KEY");
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("windows")) {
        println!("cargo:rerun-if-changed=assets/app-icon.ico");
        println!("cargo:rerun-if-changed=assets/windows.rc");
        embed_resource::compile("assets/windows.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed Windows application icon");
    }
}
