//! `tilo-docs build --book EXPORT --out SITE [--fonts CATALOG]`: an exported
//! book (`tools/docs/developer_docs.py export`) as a static site.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tilo_docs::book::Book;
use tilo_docs::site::{self, Options};

#[derive(Parser)]
#[command(about = "Build a documentation book into a static site")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write the site for an exported book.
    Build {
        /// What `developer_docs.py export` wrote: manifest.json and pages/.
        #[arg(long)]
        book: PathBuf,
        /// Where the site goes; served at the book's base (`/docs/`).
        #[arg(long)]
        out: PathBuf,
        /// A font catalog holding Tilo Sans; without it the site uses the
        /// reader's system faces.
        #[arg(long)]
        fonts: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let Command::Build { book, out, fonts } = Cli::parse().command;
    let result = Book::load(&book).and_then(|book| site::build(&book, &out, &Options { fonts }));
    match result {
        Ok(report) => {
            println!(
                "docs site: {} pages, {} files in {}",
                report.pages,
                report.files,
                out.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("docs site: {error}");
            ExitCode::FAILURE
        }
    }
}
