fn main() {
    println!("cargo:rerun-if-changed=deskpins.rc");
    println!("cargo:rerun-if-changed=deskpins.manifest");
    embed_resource::compile("deskpins.rc", embed_resource::NONE).manifest_required().unwrap();
}
