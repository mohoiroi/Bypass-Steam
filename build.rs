use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=icon.rc");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let version = concat!(
        "#define APP_VERSION ", env!("CARGO_PKG_VERSION_MAJOR"), ",",
        env!("CARGO_PKG_VERSION_MINOR"), ",", env!("CARGO_PKG_VERSION_PATCH"), ",0\n",
        "#define APP_VERSION_STRING \"", env!("CARGO_PKG_VERSION"), "\"\n",
    );
    fs::write(out_dir.join("steam_bypass_version.h"), version)
        .expect("write Windows version information");
    embed_resource::compile("icon.rc", embed_resource::ParamsIncludeDirs([out_dir]))
        .manifest_required()
        .expect("compile Windows application resources");
}
