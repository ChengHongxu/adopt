//! Questions to the user. [`Prompter`] is a trait so tests can script the answers.

use std::fmt;

use anyhow::Result;
use console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect};

/// Returned (wrapped in anyhow) when the user hits Ctrl-C or Esc at a prompt.
#[derive(Debug)]
pub struct Aborted;

impl fmt::Display for Aborted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("已取消")
    }
}

impl std::error::Error for Aborted {}

pub trait Prompter {
    /// Print `prompt` and read a single key out of `keys`.
    fn key(&self, prompt: &str, keys: &[char]) -> Result<char>;
    fn confirm(&self, question: &str, default: bool) -> Result<bool>;
    /// Free text; an empty answer returns `default`.
    fn input(&self, question: &str, default: &str) -> Result<String>;
    /// Indices of the checked `items`; `defaults` says which start checked.
    fn multi_select(
        &self,
        question: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>>;
}

pub struct TermPrompter {
    term: Term,
}

impl TermPrompter {
    pub fn new() -> TermPrompter {
        TermPrompter {
            term: Term::stdout(),
        }
    }
}

impl Default for TermPrompter {
    fn default() -> Self {
        Self::new()
    }
}

fn map_err(e: dialoguer::Error) -> anyhow::Error {
    let dialoguer::Error::IO(io) = e;
    if io.kind() == std::io::ErrorKind::Interrupted {
        Aborted.into()
    } else {
        io.into()
    }
}

impl Prompter for TermPrompter {
    fn key(&self, prompt: &str, keys: &[char]) -> Result<char> {
        self.term.write_str(prompt)?;
        loop {
            let c = match self.term.read_char() {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    self.term.write_line("")?;
                    return Err(Aborted.into());
                }
                Err(e) => return Err(e.into()),
            };
            let c = c.to_ascii_lowercase();
            if c == '\x1b' {
                self.term.write_line("")?;
                return Err(Aborted.into());
            }
            if keys.contains(&c) {
                self.term.write_line(&c.to_string())?;
                return Ok(c);
            }
        }
    }

    fn confirm(&self, question: &str, default: bool) -> Result<bool> {
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(question)
            .default(default)
            .interact_on(&self.term)
            .map_err(map_err)
    }

    fn multi_select(
        &self,
        question: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>> {
        let theme = ColorfulTheme::default();
        MultiSelect::with_theme(&theme)
            .with_prompt(question)
            .items(items)
            .defaults(defaults)
            .interact_on_opt(&self.term)
            .map_err(map_err)?
            .ok_or_else(|| Aborted.into())
    }

    fn input(&self, question: &str, default: &str) -> Result<String> {
        let theme = ColorfulTheme::default();
        let mut input = Input::<String>::with_theme(&theme)
            .with_prompt(question)
            .allow_empty(true);
        if !default.is_empty() {
            input = input.default(default.to_string());
        }
        let answer = input.interact_text_on(&self.term).map_err(map_err)?;
        Ok(if answer.trim().is_empty() {
            default.to_string()
        } else {
            answer.trim().to_string()
        })
    }
}
