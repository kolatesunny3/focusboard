#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), slint::PlatformError> {
    // One instance only: two copies would fight over the same JSON files.
    let instance = match single_instance::SingleInstance::new("FocusBoard") {
        Ok(instance) => instance,
        Err(err) => {
            eprintln!("FocusBoard: could not check for a running instance: {err}");
            return Ok(());
        }
    };
    if !instance.is_single() {
        return Ok(());
    }

    focusboard::run()
}
