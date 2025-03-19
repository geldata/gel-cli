mod buffer;
mod color;
mod formatter;
mod json;
mod native;
mod stream;
pub mod style;
#[cfg(test)]
mod tests;

pub use crate::error_display::print_query_warning as warning;
pub use crate::error_display::print_query_warnings as warnings;
pub use crate::msg;
pub use color::Highlight;
pub use color::TERMINAL_LUMA;

use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::io;
use std::sync::OnceLock;

use const_format::concatcp;
use is_terminal::IsTerminal;
use snafu::{AsErrorSource, ResultExt, Snafu};
use terminal_size::{Width, terminal_size};
use tokio_stream::{Stream, StreamExt};

use gel_errors::display::display_error;

use crate::branding::BRANDING_CLI_CMD;
use crate::repl::VectorLimit;

use buffer::{Delim, Exception, UnwrapExc, UnwrapInfallible, WrapErr};
use formatter::Formatter;
use native::FormatExt;
use stream::Output;

use crate::table::{self, Cell, Row, Table};
use gel_protocol::value::Value;

#[derive(Snafu, Debug)]
#[snafu(context(suffix(false)))]
pub enum PrintError<S: AsErrorSource + Error, P: AsErrorSource + Error> {
    #[snafu(display("error fetching element"))]
    StreamErr { source: S },
    #[snafu(display("error printing element"))]
    PrintErr { source: P },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub colors: Option<bool>,
    pub indent: usize,
    pub expand_strings: bool,
    pub max_width: Option<usize>,
    pub implicit_properties: bool,
    pub max_items: Option<usize>,
    pub max_vector_length: VectorLimit,
    pub styler: style::Styler,
}

pub(in crate::print) struct Printer<T> {
    // config
    colors: bool,
    indent: usize,
    expand_strings: bool,
    max_width: usize,
    implicit_properties: bool,
    max_items: Option<usize>,
    max_vector_length: VectorLimit,
    trailing_comma: bool,

    // state
    buffer: String,
    stream: T,
    delim: Delim,
    flow: bool,
    committed: usize,
    committed_indent: usize,
    committed_column: usize,
    column: usize,
    cur_indent: usize,

    styler: style::Styler,
}

struct Stdout {}

impl Config {
    pub fn new() -> Config {
        Config {
            colors: None,
            indent: 2,
            expand_strings: true,
            max_width: None,
            implicit_properties: false,
            max_items: None,
            max_vector_length: VectorLimit::Unlimited,
            styler: style::Styler::new(),
        }
    }
    #[allow(dead_code)]
    pub fn max_width(&mut self, value: usize) -> &mut Config {
        self.max_width = Some(value);
        self
    }
    pub fn max_items(&mut self, value: Option<usize>) -> &mut Config {
        self.max_items = value;
        self
    }
    pub fn max_vector_length(&mut self, value: VectorLimit) -> &mut Config {
        self.max_vector_length = value;
        self
    }
    pub fn colors(&mut self, value: bool) -> &mut Config {
        self.colors = Some(value);
        self
    }
    pub fn expand_strings(&mut self, value: bool) -> &mut Config {
        self.expand_strings = value;
        self
    }
    pub fn implicit_properties(&mut self, value: bool) -> &mut Config {
        self.implicit_properties = value;
        self
    }
}

pub fn completion<B: AsRef<[u8]>>(res: B) {
    msg!("OK: {}", String::from_utf8_lossy(res.as_ref()).emphasized());
}

async fn format_rows_buf<S, I, E, O>(
    prn: &mut Printer<O>,
    rows: &mut S,
    row_buf: &mut Vec<I>,
    end_of_stream: &mut bool,
) -> Result<(), Exception<PrintError<E, O::Error>>>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt,
    E: fmt::Debug + Error + 'static,
    O: Output,
    O::Error: fmt::Debug + Error + 'static,
{
    let branch = prn
        .open_block(prn.styler.apply(style::Style::Set, "{"))
        .wrap_err(PrintErr)?;

    debug_assert!(branch);
    while let Some(v) = rows.next().await.transpose().wrap_err(StreamErr)? {
        row_buf.push(v);
        if let Some(limit) = prn.max_items {
            if row_buf.len() > limit {
                prn.ellipsis().wrap_err(PrintErr)?;
                // consume extra items if any
                while rows.next().await.transpose().wrap_err(StreamErr)?.is_some() {}
                break;
            }
        }
        let v = row_buf.last().unwrap();
        v.format(prn).wrap_err(PrintErr)?;
        prn.comma().wrap_err(PrintErr)?;
        // Buffer rows up to one visual line.
        // After line is reached we get Exception::DisableFlow
    }
    *end_of_stream = true;
    prn.close_block(&prn.styler.apply(style::Style::Set, "}"), true)
        .wrap_err(PrintErr)?;
    Ok(())
}

async fn format_rows<S, I, E, O>(
    prn: &mut Printer<O>,
    buffered_rows: Vec<I>,
    rows: &mut S,
) -> Result<(), Exception<PrintError<E, O::Error>>>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt,
    E: fmt::Debug + Error + 'static,
    O: Output,
    O::Error: fmt::Debug + Error + 'static,
{
    prn.reopen_block().wrap_err(PrintErr)?;
    let mut counter: usize = 0;
    for v in buffered_rows {
        counter += 1;
        if let Some(limit) = prn.max_items {
            if counter > limit {
                prn.ellipsis().wrap_err(PrintErr)?;
                break;
            }
        }
        v.format(prn).wrap_err(PrintErr)?;
        prn.comma().wrap_err(PrintErr)?;
    }
    while let Some(v) = rows.next().await.transpose().wrap_err(StreamErr)? {
        counter += 1;
        if let Some(limit) = prn.max_items {
            if counter > limit {
                prn.ellipsis().wrap_err(PrintErr)?;
                // consume extra items if any
                while rows.next().await.transpose().wrap_err(StreamErr)?.is_some() {}
                break;
            }
        }
        v.format(prn).wrap_err(PrintErr)?;
        prn.comma().wrap_err(PrintErr)?;
    }
    prn.close_block(&prn.styler.apply(style::Style::Set, "}"), true)
        .wrap_err(PrintErr)?;
    Ok(())
}

pub async fn native_to_stdout<S, I, E>(
    rows: S,
    config: &Config,
) -> Result<(), PrintError<E, io::Error>>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt,
    E: fmt::Debug + Error + 'static,
{
    let w = config
        .max_width
        .unwrap_or_else(|| terminal_size().map(|(Width(w), _h)| w.into()).unwrap_or(80));
    let colors = config.colors.unwrap_or_else(|| io::stdout().is_terminal());
    _native_format(rows, config, w, colors, Stdout {}).await
}

fn get_printer_string(prn: &mut Printer<&mut String>) -> String {
    prn.commit().unwrap_exc().unwrap_infallible();
    prn.flush_buf().unwrap_exc().unwrap_infallible();
    let mut s = String::new();
    std::mem::swap(prn.stream, &mut s);
    s
}

fn is_numeric(v: &Value) -> bool {
    matches!(
        v,
        Value::Int16(_)
            | Value::Int32(_)
            | Value::Int64(_)
            | Value::Float32(_)
            | Value::Float64(_)
            | Value::BigInt(_)
            | Value::Decimal(_)
    )
}

fn to_cell(prn: &mut Printer<&mut String>, v: &Option<Value>) -> table::Cell {
    match v {
        Some(Value::Str(s)) => {
            let s = native::format_string(s, prn.expand_strings());
            prn.const_string(&s[1..s.len() - 1])
                .unwrap_exc()
                .unwrap_infallible()
        }
        Some(vi) => vi.format(prn).unwrap_exc().unwrap_infallible(),
        None => {}
    };
    let mut cell = Cell::new(&get_printer_string(prn));
    // Right justify numbers.
    match v {
        Some(vi) if is_numeric(vi) => {
            cell = cell.style_spec("r");
        }
        _ => {}
    }
    cell
}

async fn format_table_rows<S, I, E>(
    // We use a Printer to do the formatting, and it needs to be a string
    prn: &mut Printer<&mut String>,
    rows: &mut S,
) -> Result<table::Table, E>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt + Into<Value>,
    E: fmt::Debug + Error + 'static,
{
    let mut counter: usize = 0;

    let mut table = Table::new();
    table.set_format(*table::FORMAT);

    let mut title_row = Vec::new();
    let mut titles_set = false;
    while let Some(v) = rows.next().await.transpose()? {
        counter += 1;
        if let Some(limit) = prn.max_items {
            if counter > limit {
                table.add_row(Row::new(vec![Cell::new("...")]));
                // consume extra items if any
                while rows.next().await.transpose()?.is_some() {}
                break;
            }
        }

        let mut table_row = Vec::new();
        let v: Value = v.into();
        match &v {
            Value::SQLRow { shape, fields } => {
                for (s, vi) in shape.elements.iter().zip(fields) {
                    if !titles_set {
                        title_row.push(table::header_cell(&s.name));
                    }

                    table_row.push(to_cell(prn, vi));
                }
            }
            Value::Object { shape, fields } => {
                for (s, vi) in shape.elements.iter().zip(fields) {
                    if !titles_set {
                        title_row.push(table::header_cell(&s.name));
                    }

                    table_row.push(to_cell(prn, vi));
                }
            }
            // Q: Should we do NamedTuple and Tuple also?
            _ => {
                table_row.push(to_cell(prn, &Some(v)));
            }
        }

        if !titles_set && !title_row.is_empty() {
            table.set_titles(Row::new(title_row.clone()));
            titles_set = true;
        }

        table.add_row(Row::new(table_row));
    }

    Ok(table)
}

async fn _table_format<S, I, E>(
    mut rows: S,
    config: &Config,
    _max_width: usize,
    _colors: bool,
) -> Result<table::Table, E>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt + Into<Value>,
    E: fmt::Debug + Error + 'static,
{
    // This is kind of hacky, but for ~performance~ and to avoid
    // needing to pass around enough config info to recreate new ones,
    // we repeatedly invoke a single Printer and then pull the strings
    // out and put them in a table we are building.
    let mut buf = String::new();
    let mut prn = Printer {
        // We don't use colors yet because the table library gets
        // confused.
        colors: false,
        indent: config.indent,
        expand_strings: config.expand_strings,
        max_width: usize::MAX,
        implicit_properties: config.implicit_properties,
        max_items: config.max_items,
        max_vector_length: config.max_vector_length,
        trailing_comma: false,

        buffer: String::with_capacity(128),
        stream: &mut buf,
        delim: Delim::None,
        flow: false,
        committed: 0,
        committed_indent: 0,
        committed_column: 0,
        column: 0,
        cur_indent: 0,

        styler: config.styler.clone(),
    };

    let table = format_table_rows(&mut prn, &mut rows).await?;

    Ok(table)
}

pub async fn table_to_stdout<S, I, E>(
    rows: S,
    config: &Config,
) -> Result<(), PrintError<E, io::Error>>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt + Into<Value>,
    E: fmt::Debug + Error + 'static,
{
    let w = config
        .max_width
        .unwrap_or_else(|| terminal_size().map(|(Width(w), _h)| w.into()).unwrap_or(80));
    let colors = config.colors.unwrap_or_else(|| io::stdout().is_terminal());
    let table = _table_format(rows, config, w, colors)
        .await
        .map_err(|e| PrintError::StreamErr { source: e })?;

    // TODO: We allegedly (per our type signature, and by analogy with
    // native_to_stdout), should return a PrintErr if this write
    // fails. But prettytable makes that kind of annoying (we'd need
    // to pull in another dependency to do it!), so we don't.
    // Also, who cares.
    table.printstd();
    Ok(())
}

async fn _native_format<S, I, E, O>(
    mut rows: S,
    config: &Config,
    max_width: usize,
    colors: bool,
    output: O,
) -> Result<(), PrintError<E, O::Error>>
where
    S: Stream<Item = Result<I, E>> + Send + Unpin,
    I: FormatExt,
    E: fmt::Debug + Error + 'static,
    O: Output,
    O::Error: Error + 'static,
{
    let mut prn = Printer {
        colors,
        indent: config.indent,
        expand_strings: config.expand_strings,
        max_width,
        implicit_properties: config.implicit_properties,
        max_items: config.max_items,
        max_vector_length: config.max_vector_length,
        trailing_comma: true,

        buffer: String::with_capacity(8192),
        stream: output,
        delim: Delim::None,
        flow: false,
        committed: 0,
        committed_indent: 0,
        committed_column: 0,
        column: 0,
        cur_indent: 0,

        styler: config.styler.clone(),
    };
    let mut row_buf = Vec::new();
    let mut eos = false;
    match format_rows_buf(&mut prn, &mut rows, &mut row_buf, &mut eos).await {
        Ok(()) => {}
        Err(Exception::DisableFlow) => {
            if !eos {
                format_rows(&mut prn, row_buf, &mut rows)
                    .await
                    .unwrap_exc()?;
            }
        }
        Err(Exception::Error(e)) => return Err(e),
    };
    prn.end().unwrap_exc().context(PrintErr)?;
    Ok(())
}

fn format_rows_str<I: FormatExt>(
    prn: &mut Printer<&mut String>,
    items: &[I],
    open: &str,
    close: &str,
    reopen: bool,
) -> buffer::Result<Infallible> {
    if reopen {
        prn.reopen_block()?;
    } else {
        let cp = prn.open_block(open.unstyled())?;
        debug_assert!(cp);
    }
    for v in items {
        v.format(prn)?;
        prn.comma()?;
    }
    prn.close_block(&close.unstyled(), true)?;
    Ok(())
}

pub fn json_to_string<I: FormatExt>(items: &[I], config: &Config) -> Result<String, Infallible> {
    let mut out = String::new();
    let mut prn = Printer {
        colors: config.colors.unwrap_or(false),
        indent: config.indent,
        expand_strings: config.expand_strings,
        max_width: config.max_width.unwrap_or(80),
        implicit_properties: config.implicit_properties,
        max_items: config.max_items,
        max_vector_length: config.max_vector_length,
        trailing_comma: false,

        buffer: String::with_capacity(8192),
        stream: &mut out,
        delim: Delim::None,
        flow: false,
        committed: 0,
        committed_indent: 0,
        committed_column: 0,
        column: 0,
        cur_indent: 0,

        styler: config.styler.clone(),
    };
    match format_rows_str(&mut prn, items, "[", "]", false) {
        Ok(()) => {}
        Err(Exception::DisableFlow) => {
            format_rows_str(&mut prn, items, "[", "]", true).unwrap_exc()?;
        }
        Err(Exception::Error(e)) => return Err(e),
    };
    prn.end().unwrap_exc()?;
    Ok(out)
}

pub fn json_item_to_string<I: FormatExt>(item: &I, config: &Config) -> Result<String, Infallible> {
    let mut out = String::new();
    let mut prn = Printer {
        colors: config.colors.unwrap_or(false),
        indent: config.indent,
        expand_strings: config.expand_strings,
        max_width: config.max_width.unwrap_or(80),
        implicit_properties: config.implicit_properties,
        max_items: config.max_items,
        max_vector_length: config.max_vector_length,
        trailing_comma: false,

        buffer: String::with_capacity(8192),
        stream: &mut out,
        delim: Delim::None,
        flow: false,
        committed: 0,
        committed_indent: 0,
        committed_column: 0,
        column: 0,
        cur_indent: 0,

        styler: config.styler.clone(),
    };
    match item.format(&mut prn) {
        Ok(()) => {}
        Err(Exception::DisableFlow) => unreachable!(),
        Err(Exception::Error(e)) => return Err(e),
    }
    prn.end().unwrap_exc()?;
    Ok(out)
}

/// Does this terminal support UTF-8?
pub fn use_utf8() -> bool {
    static UTF8_ENV: OnceLock<bool> = OnceLock::new();

    let utf8_env = UTF8_ENV.get_or_init(|| {
        let env_vars = ["LANG", "LC_CTYPE", "LC_ALL"];
        env_vars.iter().any(|var| {
            std::env::var_os(var)
                .map(|v| {
                    let v = v.to_str().unwrap_or_default();
                    v.contains("UTF8") || v.contains("UTF-8")
                })
                .unwrap_or(false)
        })
    });

    cfg!(windows) || *utf8_env
}

/// Does this terminal support ANSI colors?
pub fn use_color() -> bool {
    concolor::get(concolor::Stream::Stdout).ansi_color()
}

pub fn prompt(line: impl fmt::Display) {
    println!("{}", line.to_string().emphasized().warning());
}

pub fn err_marker() -> impl fmt::Display {
    concatcp!(BRANDING_CLI_CMD, " error:").danger().emphasized()
}

#[doc(hidden)]
pub fn write_error(line: impl fmt::Display) {
    let text = format!("{line:#}");
    if text.len() > 60 {
        msg!("{} {}", err_marker(), text);
    } else {
        // Emphasise only short lines. Long lines with bold look ugly.
        msg!("{} {}", err_marker(), text.emphasized());
    }
}

pub fn edgedb_error(err: &gel_errors::Error, verbose: bool) {
    // Note: not using `error()` as display_error has markup inside
    msg!("{} {}", err_marker(), display_error(err, verbose));
}

#[doc(hidden)]
pub fn write_success(line: impl fmt::Display) {
    msg!("{}", line.to_string().success().emphasized());
}

pub fn success_msg(title: impl fmt::Display, msg: impl fmt::Display) {
    msg!(
        "{}: {}",
        title.to_string().emphasized().success(),
        msg.to_string().emphasized()
    );
}

#[doc(hidden)]
pub fn write_warn(line: impl fmt::Display) {
    msg!("{}", line.to_string().emphasized().warning());
}

pub trait AsRelativeToCurrentDir {
    fn as_relative(&self) -> &Self;
}

impl AsRelativeToCurrentDir for std::path::Path {
    fn as_relative(&self) -> &Self {
        let curr_dir = std::env::current_dir().ok();

        if let Some(stripped) = curr_dir.and_then(|wd| self.strip_prefix(&wd).ok()) {
            if stripped.components().count() == 0 {
                self
            } else {
                stripped
            }
        } else {
            self
        }
    }
}

#[macro_export]
macro_rules! warn {
    ($($args:tt)*) => {
        $crate::print::write_warn(format_args!($($args)*))
    }
}

#[macro_export]
macro_rules! error {
    ($($args:tt)*) => {
        $crate::print::write_error(format_args!($($args)*))
    }
}

#[macro_export]
macro_rules! success {
    ($($args:tt)*) => {
        $crate::print::write_success(format_args!($($args)*))
    }
}

pub fn init_colors() {
    once_cell::sync::Lazy::force(&TERMINAL_LUMA);
}

pub use crate::{error, success, warn};
