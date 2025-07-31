use std::fs;
use std::path::Path;
use std::str;

use codespan_reporting::diagnostic::{Diagnostic, Label, LabelStyle};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term::emit;
use gel_protocol::annotations::Warning;
use termcolor::{ColorChoice, StandardStream};

use edgeql_parser::tokenizer::Tokenizer;
use gel_errors::{Error, InternalServerError};

use crate::migrations::create::SourceName;
use crate::migrations::source_map::SourceMap;
use crate::print;

fn end_of_last_token(data: &str) -> Option<u64> {
    let mut tokenizer = Tokenizer::new(data);
    let mut off = 0;
    for tok in &mut tokenizer {
        off = tok.ok()?.span.end;
    }
    Some(off)
}

fn get_span_info(
    start: usize,
    end: usize,
    source_map: &'_ SourceMap<SourceName>,
) -> Option<(&'_ Path, String, usize, usize, bool)> {
    let (src, offset) = source_map.translate_range(start, end).ok()?;
    let res = match src {
        SourceName::File(path) => {
            let data = fs::read_to_string(path).ok()?;
            (path.as_ref(), data, start - offset, end - offset, false)
        }
        SourceName::Semicolon(path) => {
            let data = fs::read_to_string(path).ok()?;
            let tok_offset = end_of_last_token(&data)? as usize;
            (path.as_ref(), data, tok_offset, tok_offset, true)
        }
        _ => return None,
    };
    Some(res)
}

pub fn print_migration_error(
    err: &Error,
    source_map: &SourceMap<SourceName>,
) -> Result<(), anyhow::Error> {
    let info = Option::zip(err.position_start(), err.position_end())
        .and_then(|(s, e)| get_span_info(s, e, source_map));
    let (file_name, data, pstart, pend, eof) = match info {
        Some(pair) => pair,
        None => {
            print::gel_error(err, false);
            return Ok(());
        }
    };

    let message = if eof {
        "Unexpected end of file"
    } else {
        err.initial_message().unwrap_or(err.kind_name())
    };
    let hint = err.hint().unwrap_or("error");
    let detail = err.details().map(|s| s.into());
    let file_name_display = file_name.display();
    let files = SimpleFile::new(&file_name_display, data);
    let diag = Diagnostic::error()
        .with_message(message)
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

    if err.is::<InternalServerError>() {
        if let Some(traceback) = err.server_traceback() {
            eprintln!("  Server traceback:");
            for line in traceback.lines() {
                eprintln!("      {line}");
            }
        }
    }
    Ok(())
}

pub fn print_warnings(
    warnings: Vec<Warning>,
    source_map: Option<&SourceMap<SourceName>>,
) -> Result<(), Error> {
    for mut w in warnings {
        let info = source_map
            .zip(Option::zip(w.start, w.end))
            .and_then(|(m, (s, e))| get_span_info(s, e, m));

        if let Some((path, source, start, end, _is_eof)) = info {
            w.start = Some(start);
            w.end = Some(end);

            print::query_warning(&w, &source, path.to_str())?;
        } else {
            // we don't know which file this warning originated from
            // print a "plain" warning (single line)
            w.start = None;
            w.end = None;
            print::query_warning(&w, "", None)?;
        }
    }
    Ok(())
}
