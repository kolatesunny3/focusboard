//! FocusBoard — lightweight Windows task planner.

pub mod app;
pub mod model;
pub mod storage;
pub mod table;
pub mod values;

slint::include_modules!();

/// Entry point for the GUI binary.
pub fn run() -> Result<(), slint::PlatformError> {
    app::run()
}
