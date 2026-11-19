fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/kadr.manifest");
        println!("cargo:rerun-if-changed=assets/kadr.ico");
        embed_resource::compile("assets/kadr.rc", embed_resource::NONE)
            .manifest_required()
            .expect("embed the Windows manifest and application icon");
    }
}
