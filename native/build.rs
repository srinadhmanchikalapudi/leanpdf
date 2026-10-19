fn main() {
    // Embed the app icon and version info so Explorer, the taskbar and the installer show them.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "LeanPDF");
        res.set("FileDescription", "LeanPDF - a small, fast PDF reader");
        res.set("LegalCopyright", "Copyright (c) 2026 srinadhmanchikalapudi");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
