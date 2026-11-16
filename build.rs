fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint compilation failed");

    // Windows exe icon: compile the .ico into a resource object and link it in.
    // No extra crate — MinGW's windres is already required to link this project.
    #[cfg(target_os = "windows")]
    {
        use std::path::Path;
        use std::process::Command;

        let resource = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("focusboard-icon.res");
        let status = Command::new("windres")
            .args(["-O", "coff", "-i", "icon.rc", "-o"])
            .arg(&resource)
            .status()
            .expect("windres not found: add the MinGW bin directory to PATH");
        assert!(status.success(), "windres failed to compile icon.rc");
        println!("cargo:rustc-link-arg={}", resource.display());
        println!("cargo:rerun-if-changed=icon.rc");
        println!("cargo:rerun-if-changed=fb.ico");
    }
}
