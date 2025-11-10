use std::borrow::Cow;
use std::io::IsTerminal;

use anyhow::Context as _;
use edgeql_parser::expr;
use rustyline::completion::Completer;
use rustyline::config::EditMode;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::FileHistory;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Cmd, KeyEvent, Modifiers, error::ReadlineError};
use rustyline::{Config, Editor, Helper};

use crate::highlight;
use crate::print::Highlight;
use crate::print::style::Styler;
use crate::prompt::{load_history, save_history};

pub struct ExpressionHelper {
    styler: Styler,
}
impl Helper for ExpressionHelper {}
impl Hinter for ExpressionHelper {
    type Hint = String;
}
impl Highlighter for ExpressionHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        prompt.into()
    }
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        let mut buf = String::with_capacity(line.len() + 8);
        highlight::edgeql(&mut buf, line, &self.styler);
        buf.into()
    }
    fn highlight_char(&self, _line: &str, _pos: usize, _forced: bool) -> bool {
        // TODO(tailhook) optimize: only need to return true on insert
        true
    }
    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        hint.muted().to_string().into()
    }
}

impl Validator for ExpressionHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> Result<ValidationResult, ReadlineError> {
        match expr::check(ctx.input()) {
            Ok(()) => Ok(ValidationResult::Valid(None)),
            Err(expr::Error::MissingBracket { .. }) | Err(expr::Error::Empty) => {
                Ok(ValidationResult::Incomplete)
            }
            Err(e) => Ok(ValidationResult::Invalid(Some(e.to_string()))),
        }
    }
}

impl Completer for ExpressionHelper {
    type Candidate = String;
}

pub fn expression(
    prompt: &str,
    history_name: &str,
    default: &str,
) -> Result<String, anyhow::Error> {
    use std::io::Write;

    if !std::io::stdin().is_terminal() {
        print!("{prompt}");
        _ = std::io::stdout().flush();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        if buf.is_empty() {
            println!("{default}");
            return Ok(default.to_string());
        }
        println!();
        return Ok(buf);
    }

    let history_name = format!("migr_{}", &history_name);
    let config = Config::builder();
    let config = config.edit_mode(EditMode::Emacs);
    let mut editor = Editor::<ExpressionHelper, FileHistory>::with_config(config.clone().build())?;
    editor.bind_sequence(
        KeyEvent::new('\r', Modifiers::NONE),
        Cmd::AcceptOrInsertLine {
            accept_in_the_middle: false,
        },
    );
    editor.bind_sequence(KeyEvent::new('\r', Modifiers::ALT), Cmd::AcceptLine);
    load_history(&mut editor, &history_name)
        .map_err(|e| {
            eprintln!("Can't load history: {e:#}");
        })
        .ok();
    editor.set_helper(Some(ExpressionHelper {
        styler: Styler::new(),
    }));
    let text = editor
        .readline_with_initial(prompt, (default, ""))
        .context("readline error")?;
    editor.add_history_entry(&text)?;
    save_history(&mut editor, &history_name);
    Ok(text)
}
