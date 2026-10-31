//! Embeds the Windows icon, version information and manifest into PlumSpirit.exe.
//! Uses `rc.exe` (MSVC) or `windres` (GNU / cross-compilation) via embed-resource.

fn main() {
    println!("cargo:rerun-if-changed=windows/plumspirit.rc");
    println!("cargo:rerun-if-changed=windows/plumspirit.manifest");
    println!("cargo:rerun-if-changed=../../assets/icon/plumspirit.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("windows/plumspirit.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed Windows resources (icon, version info, manifest)");
    }
}
