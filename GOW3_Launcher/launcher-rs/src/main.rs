#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod builds;
mod catalog;
#[cfg(test)]
mod discord_stub;
mod emulator_config;
mod fluid;
mod game;
mod i18n;
#[cfg(test)]
mod launch_probe;
mod presence;
mod process;
mod save_watch;
mod session;
mod storage;
mod trophies;
mod updates;

slint::include_modules!();

fn main() {
    if let Err(error) = app::run() {
        let message = format!("{error:#}");
        eprintln!("gow3-launcher: {message}");
        let _ = rfd::MessageDialog::new()
            .set_title("GoW III Launcher")
            .set_description(&message)
            .set_level(rfd::MessageLevel::Error)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
        std::process::exit(1);
    }
}
