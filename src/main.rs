#![windows_subsystem = "windows"]
mod audio;
mod desktop;
mod library;
mod media;
fn main() {
    if let Err(error) = desktop::run() {
        rfd::MessageDialog::new()
            .set_title("Afterimage")
            .set_description(error.to_string())
            .show();
    }
}
