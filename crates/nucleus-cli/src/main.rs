//! Nucleus CLI binary entry point.
//!
//! Delegates to `nucleus_core::cli::run`. The `nucleus-cli` crate exists so
//! the command-line interface can be versioned and released independently of
//! the core engine library.

fn main() {
    if let Err(e) = nucleus_core::cli::run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
