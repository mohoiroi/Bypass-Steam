use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=icon.rc");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let major = env!("CARGO_PKG_VERSION_MAJOR");
    let minor = env!("CARGO_PKG_VERSION_MINOR");
    let patch = env!("CARGO_PKG_VERSION_PATCH");
    let display = if patch == "0" && env!("CARGO_PKG_VERSION_PRE").is_empty() {
        format!("{major}.{minor}")
    } else {
        env!("CARGO_PKG_VERSION").to_owned()
    };
    println!("cargo:rustc-env=STEAM_BYPASS_DISPLAY_VERSION={display}");
    let version = format!(
        "#define APP_VERSION {major},{minor},{patch},0\n#define APP_VERSION_STRING \"{display}\"\n"
    );
    fs::write(out_dir.join("steam_bypass_version.h"), version)
        .expect("write Windows version information");
    embed_resource::compile("icon.rc", embed_resource::ParamsIncludeDirs([out_dir]))
        .manifest_required()
        .expect("compile Windows application resources");
}
