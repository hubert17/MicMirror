fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/bridge/ui_bridge.m");
        println!("cargo:rerun-if-changed=src/bridge/ui_bridge.h");

        cc::Build::new()
            .file("src/bridge/ui_bridge.m")
            .flag("-fobjc-arc")
            .compile("micmirror_ui");

        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
    }
}
