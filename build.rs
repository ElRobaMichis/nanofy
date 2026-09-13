fn main() {
    println!("cargo:rerun-if-changed=assets/nanofy.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/nanofy.ico")
            .compile()
            .expect("No se pudo integrar el icono de Nanofy en el ejecutable");
    }
}
