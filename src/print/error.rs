use std::str;

use codespan_reporting::diagnostic::{Diagnostic, Label, LabelStyle};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term::emit;
use const_format::concatcp;
use gel_protocol::annotations::Warning;
use termcolor::{ColorChoice, StandardStream};

use gel_errors::{Error, InternalServerError};

use crate::branding::BRANDING_CLI_CMD;
use crate::print::{self, Highlight, msg};

pub fn print_query_error(
    err: &Error,
    query: &str,
    verbose: bool,
    source_name: &str,
) -> Result<(), anyhow::Error> {
    let pstart = err.position_start();
    let pend = err.position_end();
    let (pstart, pend) = match (pstart, pend) {
        (Some(s), Some(e)) => (s, e),
        _ => {
            print::gel_error(err, verbose);
            return Ok(());
        }
    };
    let hint = err.hint().unwrap_or("error");
    let detail = err.details().map(|s| s.into());
    let files = SimpleFile::new(source_name, query);
    let context_error = err.contexts().rev().collect::<Vec<_>>();
    if !context_error.is_empty() {
        print::error!("{}", context_error.join(": "));
    }
    let diag = Diagnostic::error()
        .with_message(format!(
            "{}{}",
            err.kind_name(),
            err.initial_message()
                .map(|s| format!(": {s}"))
                .unwrap_or("".into())
        ))
        .with_labels(vec![Label {
            file_id: (),
            style: LabelStyle::Primary,
            range: pstart..pend,
            message: hint.into(),
        }])
        .with_notes(detail.into_iter().collect());

    emit(
        &mut StandardStream::stderr(ColorChoice::Auto),
        &Default::default(),
        &files,
        &diag,
    )?;

    if err.is::<InternalServerError>() || verbose {
        if let Some(traceback) = err.server_traceback() {
            eprintln!("  Server traceback:");
            for line in traceback.lines() {
                eprintln!("      {line}");
            }
        }
    }
    Ok(())
}

pub fn print_query_warnings(warnings: &[Warning], source: &str) -> Result<(), anyhow::Error> {
    for w in warnings {
        print_query_warning(w, source, None)?;
    }
    Ok(())
}

pub fn print_query_warning(
    warning: &Warning,
    source: &str,
    source_file: Option<&str>,
) -> Result<(), anyhow::Error> {
    let Some((start, end)) = warning.start.zip(warning.end) else {
        print_query_warning_plain(warning);
        return Ok(());
    };
    let filename = warning
        .filename
        .as_deref()
        .or(source_file)
        .unwrap_or("<query>");
    let files = SimpleFile::new(filename, source);
    let diag = Diagnostic::warning()
        .with_message(&warning.r#type)
        .with_labels(vec![Label {
            file_id: (),
            style: LabelStyle::Primary,
            range: start..end,
            message: warning.message.clone(),
        }]);

    emit(
        &mut StandardStream::stderr(ColorChoice::Auto),
        &Default::default(),
        &files,
        &diag,
    )?;

    Ok(())
}

fn print_query_warning_plain(warning: &Warning) {
    let marker = concatcp!(BRANDING_CLI_CMD, " warning:")
        .emphasized()
        .warning();

    msg!("{marker} {warning}");
}
