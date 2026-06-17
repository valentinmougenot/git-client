//! A single-repo GUI git client focused on the daily commit loop.
//!
//! Launched from inside a git Repository. See `CONTEXT.md` for the domain
//! vocabulary and `docs/PRD.md` for the product spec.

mod app;
mod git;
mod ui;

fn main() -> iced::Result {
    // Repository discovery happens before any window opens. On failure we
    // print to stderr and exit 1 — no GUI is shown (PRD "Repository discovery").
    if let Err(error) = git2::Repository::discover(".") {
        eprintln!(
            "git-client: not inside a git repository (searched from the current directory)\n  {}",
            error.message()
        );
        std::process::exit(1);
    }

    app::run()
}
