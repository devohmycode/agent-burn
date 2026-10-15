use winresource::{VersionInfo, WindowsResource};

fn main() {
    println!("cargo:rerun-if-changed=src/app.manifest");
    println!("cargo:rerun-if-changed=assets/agent-burn.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut resource = WindowsResource::new();
    // A manifest file is embedded verbatim; `set_manifest` would indent the
    // XML declaration and break strict parsers.
    resource
        .set_manifest_file("src/app.manifest")
        // The first icon: Explorer, the notification area and the windows.
        .set_icon("assets/agent-burn.ico")
        .set("ProductName", "Agent Burn")
        .set("FileDescription", "Agent Burn for Windows")
        .set("FileVersion", version)
        .set("ProductVersion", version)
        .set_version_info(VersionInfo::FILEVERSION, pack_version(version))
        .set_version_info(VersionInfo::PRODUCTVERSION, pack_version(version));
    resource
        .compile()
        .expect("compile the Windows resources (manifest, icon, version)");
}

/// `major.minor.patch` as the four 16-bit fields of a PE version.
fn pack_version(version: &str) -> u64 {
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u64>().unwrap_or(0));
    let mut field = || parts.next().unwrap_or(0) & 0xFFFF;
    (field() << 48) | (field() << 32) | (field() << 16)
}
