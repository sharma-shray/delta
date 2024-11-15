mod align;
mod ansi;
mod cli;
mod color;
mod colors;
mod config;
mod delta;
mod edits;
mod env;
mod features;
mod format;
mod git_config;
mod handlers;
mod minusplus;
mod options;
mod paint;
mod parse_style;
mod parse_styles;
mod style;
mod utils;
mod wrapping;

mod subcommands;

mod tests;

use std::ffi::OsString;
use std::io::{self, Cursor, ErrorKind, IsTerminal, Write};
use std::process;

use bytelines::ByteLinesReader;

use crate::cli::Call;
use crate::delta::delta;
use crate::utils::bat::assets::list_languages;
use crate::utils::bat::output::{OutputType, PagingMode};

pub fn fatal<T>(errmsg: T) -> !
where
    T: AsRef<str> + std::fmt::Display,
{
    #[cfg(not(test))]
    {
        eprintln!("{errmsg}");
        // As in Config::error_exit_code: use 2 for error
        // because diff uses 0 and 1 for non-error.
        process::exit(2);
    }
    #[cfg(test)]
    panic!("{}\n", errmsg);
}

pub mod errors {
    pub use anyhow::{anyhow, Context, Error, Result};
}

#[cfg(not(tarpaulin_include))]
fn main() -> std::io::Result<()> {
    // Do this first because both parsing all the input in `run_app()` and
    // listing all processes takes about 50ms on Linux.
    // It also improves the chance that the calling process is still around when
    // input is piped into delta (e.g. `git show  --word-diff=color | delta`).
    utils::process::start_determining_calling_process_in_thread();

    // Ignore ctrl-c (SIGINT) to avoid leaving an orphaned pager process.
    // See https://github.com/dandavison/delta/issues/681
    ctrlc::set_handler(|| {})
        .unwrap_or_else(|err| eprintln!("Failed to set ctrl-c handler: {err}"));
    let exit_code = run_app(std::env::args_os().collect::<Vec<_>>(), None)?;
    // when you call process::exit, no destructors are called, so we want to do it only once, here
    process::exit(exit_code);
}

#[cfg(not(tarpaulin_include))]
// An Ok result contains the desired process exit code. Note that 1 is used to
// report that two files differ when delta is called with two positional
// arguments and without standard input; 2 is used to report a real problem.
pub fn run_app(
    args: Vec<OsString>,
    capture_output: Option<&mut Cursor<Vec<u8>>>,
) -> std::io::Result<i32> {
    let env = env::DeltaEnv::init();
    let assets = utils::bat::assets::load_highlighting_assets();
    let opt = cli::Opt::from_args_and_git_config(args, &env, assets);

    let opt = match opt {
        Call::Version(msg) => {
            writeln!(std::io::stdout(), "{}", msg.trim_end())?;
            return Ok(0);
        }
        Call::Help(msg) => {
            OutputType::oneshot_write(msg)?;
            return Ok(0);
        }
        Call::Delta(opt) => opt,
    };

    let subcommand_result = if let Some(shell) = opt.generate_completion {
        Some(subcommands::generate_completion::generate_completion_file(
            shell,
        ))
    } else if opt.list_languages {
        Some(list_languages())
    } else if opt.list_syntax_themes {
        Some(subcommands::list_syntax_themes::list_syntax_themes())
    } else if opt.show_syntax_themes {
        Some(subcommands::show_syntax_themes::show_syntax_themes())
    } else if opt.show_themes {
        Some(subcommands::show_themes::show_themes(
            opt.dark,
            opt.light,
            opt.computed.color_mode,
        ))
    } else if opt.show_colors {
        Some(subcommands::show_colors::show_colors())
    } else if opt.parse_ansi {
        Some(subcommands::parse_ansi::parse_ansi())
    } else {
        None
    };
    if let Some(result) = subcommand_result {
        if let Err(error) = result {
            match error.kind() {
                ErrorKind::BrokenPipe => {}
                _ => fatal(format!("{error}")),
            }
        }
        return Ok(0);
    };

    let _show_config = opt.show_config;
    let config = config::Config::from(opt);

    if _show_config {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        subcommands::show_config::show_config(&config, &mut stdout)?;
        return Ok(0);
    }

    // The following block structure is because of `writer` and related lifetimes:
    let pager_cfg = (&config).into();
    let paging_mode = if capture_output.is_some() {
        PagingMode::Capture
    } else {
        config.paging_mode
    };
    let mut output_type =
        OutputType::from_mode(&env, paging_mode, config.pager.clone(), &pager_cfg).unwrap();
    let mut writer: &mut dyn Write = if paging_mode == PagingMode::Capture {
        &mut capture_output.unwrap()
    } else {
        output_type.handle().unwrap()
    };

    if let (Some(minus_file), Some(plus_file)) = (&config.minus_file, &config.plus_file) {
        let exit_code = subcommands::diff::diff(minus_file, plus_file, &config, &mut writer);
        return Ok(exit_code);
    }

    if io::stdin().is_terminal() {
        eprintln!(
            "\
    The main way to use delta is to configure it as the pager for git: \
    see https://github.com/dandavison/delta#get-started. \
    You can also use delta to diff two files: `delta file_A file_B`."
        );
        return Ok(config.error_exit_code);
    }

    if let Err(error) = delta(io::stdin().lock().byte_lines(), &mut writer, &config) {
        match error.kind() {
            ErrorKind::BrokenPipe => return Ok(0),
            _ => eprintln!("{error}"),
        }
    };
    Ok(0)
}
