fn main() {
    tauri_winres::WindowsResource::new()
        .set_icon("assets/Afterimage.ico")
        .compile()
        .expect("Windows application icon");
}
