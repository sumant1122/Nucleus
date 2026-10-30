//! Thin binary entry point for the `nucleus-core` crate.
//!
//! Delegates to [`nucleus_core::cli::run`], which is shared with `nucleus-cli`.

fn main() {
    if let Err(e) = nucleus_core::cli::run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
