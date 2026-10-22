use std::io::IsTerminal;
use std::process::ExitCode;

use adopt::App;
use adopt::cli::Cli;
use adopt::paths::Paths;
use adopt::prompt::{Aborted, TermPrompter};
use adopt::runner::SystemRunner;
use adopt::ui::Ui;
use clap::Parser;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ui = Ui::stdout();
    let result = Paths::from_env().and_then(|paths| {
        let prompter = TermPrompter::new();
        let app = App {
            paths,
            runner: &SystemRunner,
            prompter: &prompter,
            ui: &ui,
            dry_run: cli.dry_run,
            interactive: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            hostname: None,
            privilege: Default::default(),
        };
        app.run(&cli)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is::<Aborted>() => {
            ui.warn("已取消，没有写入任何改动");
            ExitCode::from(130)
        }
        Err(e) => {
            ui.fail(format!("{e:#}"));
            ExitCode::FAILURE
        }
    }
}
