fn main() {
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=icon.rc");
    let _ = embed_resource::compile("icon.rc", embed_resource::NONE);
}
