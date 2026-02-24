fn main() {
    slint_build::compile("ui/main-window.slint").unwrap();

    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("ui/tendril.ico");
        res.compile().unwrap();
    }
}
