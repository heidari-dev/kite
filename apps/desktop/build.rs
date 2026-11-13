fn main() {
    // One style on every platform; fluent follows the system light and dark setting.
    // The brand SVGs are embedded, so the binary needs no files next to it.
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config).expect("compile the Slint UI");
}
