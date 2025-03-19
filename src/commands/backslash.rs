use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use clap::{CommandFactory, FromArgMatches};
use const_format::concatcp;
use gel_dsn::gel::DatabaseBranch;
use once_cell::sync::Lazy;
use prettytable::{Cell, Row, Table};
use regex::Regex;

use gel_errors::Error;
use gel_errors::display::display_error_verbose;
use gel_protocol::model::Duration;

use crate::analyze;
use crate::branding::BRANDING;
use crate::commands::Options;
use crate::commands::execute;
use crate::commands::parser::{Backslash, BackslashCmd, Setting, StateParam};
use crate::print;
use crate::print::style::Styler;
use crate::prompt;
use crate::repl;
use crate::table;

pub static CMD_CACHE: Lazy<CommandCache> = Lazy::new(CommandCache::new);

pub enum ExecuteResult {
    Skip,
    Quit,
    Input(String),
}

const HELP: &str = concatcp!(
    r###"
                            "###,
    BRANDING,
    r###" REPL Commands

Introspection
  Options:
    -v    verbose
    -s    show system objects
    -c    case-sensitive match

  \d [-v] NAME              Describe schema object
  \ds                       Describe whole schema   (alias: \describe schema)
  \l                        List databases/branches (alias: \list branches)
  \ls [-sc]  [PATTERN]      List scalar types       (alias: \list scalars)
  \lt [-sc]  [PATTERN]      List object types       (alias: \list types)
  \lr [-c]   [PATTERN]      List roles              (alias: \list roles)
  \lm [-c]   [PATTERN]      List modules            (alias: \list modules)
  \la [-vsc] [PATTERN]      List expression aliases (alias: \list aliases)
  \lc [-c]   [PATTERN]      List casts              (alias: \list casts)
  \li [-vsc] [PATTERN]      List indexes            (alias: \list indexes)

Operations
  \dump FILENAME            Create dump of current database as a file
  \restore FILENAME         Restore database from file into current database
  \expand                   Print expanded output of last `analyze` operation
  \E, \last-error           More information on most recent error

Editing
  \s, \history              Show history
  \e, \edit [N]             Spawn $EDITOR to edit the last used query, using
                            the editor output as input in the REPL.
                            Defaults to vi (Notepad in Windows).

Connection
  \b, \branch ...           Branch commands
  \c, \connect [DBNAME]     Connect to database/branch DBNAME

Settings
  \set [OPTION [VALUE]]     Show/change settings. Type \set to list
                            all available options

Help
  \?, \h, \help             Show help on backslash commands
  \set                      Describe current settings
  \q, \quit, \exit, Ctrl+D  Quit REPL
"###
);

#[derive(Debug)]
pub struct ParseError {
    pub help: bool,
    #[allow(dead_code)]
    pub span: Option<(usize, usize)>,
    pub message: String,
}

#[derive(Debug, PartialEq)]
pub struct Token<'a> {
    pub item: Item<'a>,
    pub span: (usize, usize),
}

#[derive(Debug, PartialEq)]
pub enum Item<'a> {
    Command(&'a str),
    Argument(&'a str),
    Error { message: &'a str },
    Incomplete { message: &'a str },
    Semicolon,
    Newline,
}

pub struct Parser<'a> {
    data: &'a str,
    first_item: bool,
    offset: usize,
}

#[derive(Debug)]
pub struct Argument {
    pub name: String,
}

pub enum Command {
    Settings,
    Normal(CommandInfo),
    Subcommands(BTreeMap<String, CommandInfo>),
}

#[derive(Debug)]
pub struct CommandInfo {
    pub options: String,
    pub arguments: Vec<Argument>,
    pub description: Option<String>,
    pub name_description: String,
}

#[derive(Debug)]
pub struct SettingInfo {
    pub name: &'static str,
    pub description: String,
    pub name_description: String,
    pub setting: &'static Setting,
    pub value_name: String,
    pub values: Option<Vec<String>>,
}

pub struct CommandCache {
    pub settings: BTreeMap<&'static str, SettingInfo>,
    pub commands: BTreeMap<String, Command>,
    pub aliases: BTreeMap<&'static str, &'static [&'static str]>,
    pub top_commands: BTreeSet<String>,
}

impl<'a> Parser<'a> {
    pub fn new(s: &'a str) -> Parser<'a> {
        Parser {
            data: s,
            first_item: true,
            offset: 0,
        }
    }
    fn token(&self) -> Option<Token<'a>> {
        let whitespace: &[_] = &[' ', '\t'];
        let tail = self.data[self.offset..].trim_start_matches(whitespace);
        if tail.is_empty() {
            return None;
        }
        let offset = self.data.len() - tail.len();
        let mut iter = tail.char_indices();
        let end = loop {
            let (idx, c) = match iter.next() {
                Some(pair) => pair,
                None => break tail.len(),
            };
            match c {
                '\'' | '"' | '`' => {
                    let quote = c;
                    loop {
                        match iter.next() {
                            Some((_, c)) if c == quote => break,
                            Some((end, '\n')) | Some((end, '\r')) => {
                                return Some(Token {
                                    item: Item::Error {
                                        message: match quote {
                                            '\'' => {
                                                "expected end of single \
                                                quote `'` , got end of line"
                                            }
                                            '"' => {
                                                "expected end of double \
                                                quote `\"` , got end of line"
                                            }
                                            '`' => {
                                                "expected end of backtick \
                                                quote '`' , got end of line"
                                            }
                                            _ => unreachable!(),
                                        },
                                    },
                                    span: (offset + idx, offset + end),
                                });
                            }
                            Some((_, _)) => {}
                            None => {
                                return Some(Token {
                                    item: Item::Incomplete {
                                        message: match quote {
                                            '\'' => {
                                                "incomplete 'single-quoted' \
                                                argument"
                                            }
                                            '"' => {
                                                "incomplete \"double-quoted\" \
                                                argument"
                                            }
                                            '`' => {
                                                "incomplete `backtick-quoted` \
                                                argument"
                                            }
                                            _ => unreachable!(),
                                        },
                                    },
                                    span: (offset, self.data.len()),
                                });
                            }
                        }
                    }
                }
                ';' if idx == 0 => {
                    return Some(Token {
                        item: Item::Semicolon,
                        span: (offset, offset + 1),
                    });
                }
                '\n' if idx == 0 => {
                    return Some(Token {
                        item: Item::Newline,
                        span: (offset, offset + 1),
                    });
                }
                '\r' if idx == 0 => {
                    let ln = if let Some((_, '\n')) = iter.next() {
                        2
                    } else {
                        1
                    };
                    return Some(Token {
                        item: Item::Newline,
                        span: (offset, offset + ln),
                    });
                }
                ' ' | '\t' | '\r' | '\n' | ';' => break idx,
                _ => {}
            }
        };
        let value = &tail[..end];
        let item = if self.first_item {
            if !value.starts_with('\\') {
                let char_len = value.chars().next().unwrap().len_utf8();
                return Some(Token {
                    item: Item::Error {
                        message: "command must start with backslash `\\`",
                    },
                    span: (offset, offset + char_len),
                });
            }
            if value.starts_with("\\-") {
                return Some(Token {
                    item: Item::Error {
                        message: "unexpected `-`, try \\help",
                    },
                    span: (offset + 1, offset + 2),
                });
            }
            Item::Command(value)
        } else {
            Item::Argument(value)
        };
        Some(Token {
            item,
            span: (offset, offset + end),
        })
    }
}

impl<'a> Iterator for Parser<'a> {
    type Item = Token<'a>;
    fn next(&mut self) -> Option<Token<'a>> {
        let result = self.token();
        if let Some(ref tok) = result {
            if self.first_item {
                self.first_item = false;
            }
            self.offset = tok.span.1;
        }
        result
    }
}

impl CommandInfo {
    fn from(cmd: &clap::Command) -> CommandInfo {
        CommandInfo {
            options: cmd.get_arguments().filter_map(|a| a.get_short()).collect(),
            arguments: cmd
                .get_arguments()
                .filter(|a| a.get_short().is_none())
                .filter(|a| a.get_long().is_none())
                .map(|a| Argument {
                    name: a.get_id().to_string().to_owned(),
                })
                .collect(),
            description: cmd
                .get_about()
                .map(|x| format!("{}", x.ansi()).trim().to_owned()),
            name_description: if let Some(desc) = cmd.get_about() {
                format!(
                    "{} -- {}",
                    cmd.get_name(),
                    format!("{}", desc.ansi()).trim()
                )
            } else {
                cmd.get_name().to_string()
            },
        }
    }
}

impl CommandCache {
    fn new() -> CommandCache {
        let mut clap = Backslash::command();
        let mut aliases = BTreeMap::new();
        aliases.insert("d", &["describe", "object"][..]);
        aliases.insert("ds", &["describe", "schema"]);
        aliases.insert("l", &["list", "branches"]);
        aliases.insert("ls", &["list", "scalars"]);
        aliases.insert("lt", &["list", "types"]);
        aliases.insert("lr", &["list", "roles"]);
        aliases.insert("lm", &["list", "modules"]);
        aliases.insert("la", &["list", "aliases"]);
        aliases.insert("lc", &["list", "casts"]);
        aliases.insert("li", &["list", "indexes"]);
        aliases.insert("s", &["history"]);
        aliases.insert("e", &["edit"]);
        aliases.insert("c", &["connect"]);
        aliases.insert("E", &["last-error"]);
        aliases.insert("q", &["exit"]);
        aliases.insert("quit", &["exit"]);
        aliases.insert("?", &["help"]);
        aliases.insert("h", &["help"]);
        aliases.insert("branch", &["branch"]);
        aliases.insert("b", &["branch"]);
        let mut setting_cmd = None;
        let commands: BTreeMap<_, _> = clap
            .get_subcommands_mut()
            .map(|cmd| {
                let name = cmd.get_name().to_owned();
                let cmd_info = if name == "set" {
                    setting_cmd = Some(&*cmd);
                    Command::Settings
                } else if cmd.has_subcommands() {
                    Command::Subcommands(
                        cmd.get_subcommands()
                            .map(|cmd| (cmd.get_name().into(), CommandInfo::from(cmd)))
                            .collect(),
                    )
                } else {
                    Command::Normal(CommandInfo::from(cmd))
                };
                (name, cmd_info)
            })
            .collect();
        let setting_cmd = setting_cmd.expect("set command exists");
        let mut setting_cmd: BTreeMap<_, _> = setting_cmd
            .get_subcommands()
            .map(|cmd| (cmd.get_name(), cmd))
            .collect();
        let settings = Setting::all_items()
            .iter()
            .map(|setting| {
                let cmd = setting_cmd
                    .remove(&setting.name())
                    .expect("all settings have cmd");
                let arg = cmd
                    .get_arguments()
                    .find(|a| a.get_id() != "help" && a.get_id() != "version")
                    .expect("setting has argument");
                let values = arg
                    .get_value_parser()
                    .possible_values()
                    .map(|v| v.map(|x| x.get_name().to_owned()).collect());
                let description = match cmd.get_about() {
                    Some(x) => format!("{}", x.ansi()),
                    None => String::from(""),
                }
                .trim()
                .to_owned();
                let info = SettingInfo {
                    name: setting.name(),
                    name_description: format!("{} -- {}", setting.name(), description),
                    description,
                    setting,
                    value_name: arg.get_id().to_string().to_owned(),
                    values,
                };
                (info.name, info)
            })
            .collect();
        CommandCache {
            settings,
            top_commands: commands
                .keys()
                .map(|x| &x[..])
                .chain(aliases.keys().copied())
                .map(|n| String::from("\\") + n)
                .collect(),
            commands,
            aliases,
        }
    }
}

pub fn full_statement(s: &str) -> usize {
    for token in Parser::new(s) {
        match token.item {
            Item::Semicolon | Item::Newline => return token.span.1,
            _ => {}
        }
    }
    s.len()
}

pub fn backslashify_help(text: &str) -> Cow<'_, str> {
    pub static USAGE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(USAGE:\s*)(\w)").unwrap());
    USAGE.replace(text, "$1\\$2")
}

pub fn parse(s: &str) -> Result<Backslash, ParseError> {
    use Item::*;

    let mut arguments = Vec::new();
    for token in Parser::new(s) {
        match token.item {
            Command(x) => {
                if x == "\\?" || x == "\\h" || x == "\\help" {
                    return Ok(Backslash {
                        command: BackslashCmd::Help,
                    });
                }
                if let Some(cmd) = CMD_CACHE.aliases.get(&x[1..]) {
                    arguments.extend(cmd.iter().map(|s| s.to_string()))
                } else {
                    arguments.push(x[1..].to_owned())
                }
            }
            Argument(x) => arguments.push(unquote_argument(x)),
            Newline | Semicolon => break,
            Incomplete { message } => {
                return Err(ParseError {
                    help: false,
                    message: message.to_string(),
                    span: Some(token.span),
                });
            }
            Error { message } => {
                return Err(ParseError {
                    help: false,
                    message: message.to_string(),
                    span: Some(token.span),
                });
            }
        }
    }
    Backslash::command()
        .try_get_matches_from(arguments)
        .and_then(|m| Backslash::from_arg_matches(&m))
        .map_err(|e| ParseError {
            help: e.kind() == clap::error::ErrorKind::DisplayHelp,
            message: backslashify_help(&e.to_string()).into(),
            span: None,
        })
}

fn unquote_argument(s: &str) -> String {
    let mut buf = String::with_capacity(s.len());
    let mut iter = s.chars();
    while let Some(c) = iter.next() {
        match c {
            '\'' => {
                for c in &mut iter {
                    if c == '\'' {
                        break;
                    }
                    buf.push(c);
                }
            }
            '"' => {
                for c in &mut iter {
                    if c == '"' {
                        break;
                    }
                    buf.push(c);
                }
            }
            '`' => {
                for c in &mut iter {
                    if c == '`' {
                        break;
                    }
                    buf.push(c);
                }
            }
            _ => buf.push(c),
        }
    }
    buf
}

pub fn bool_str(val: bool) -> &'static str {
    match val {
        true => "on",
        false => "off",
    }
}

pub fn get_setting(s: &Setting, prompt: &repl::State) -> Cow<'static, str> {
    use Setting::*;

    match s {
        InputMode(_) => prompt.input_mode.as_str().into(),
        ImplicitProperties(_) => bool_str(prompt.print.implicit_properties).into(),
        VerboseErrors(_) => bool_str(prompt.verbose_errors).into(),
        Limit(_) => {
            if let Some(limit) = prompt.implicit_limit {
                limit.to_string().into()
            } else {
                "0  # no limit".into()
            }
        }
        VectorDisplayLength(_) => prompt.print.max_vector_length.to_string().into(),
        IdleTransactionTimeout(_) => {
            if prompt.idle_transaction_timeout.to_micros() > 0 {
                prompt.idle_transaction_timeout.to_string().into()
            } else {
                "0  # no timeout".into()
            }
        }
        Language(_) => prompt.input_language.as_str().into(),
        HistorySize(_) => prompt.history_limit.to_string().into(),
        OutputFormat(_) => prompt.output_format.as_str().into(),
        SqlOutputFormat(_) => prompt.sql_output_format.as_str().into(),
        DisplayTypenames(_) => bool_str(prompt.display_typenames).into(),
        ExpandStrings(_) => bool_str(prompt.print.expand_strings).into(),
        PrintStats(_) => prompt.print_stats.as_str().into(),
    }
}

fn list_settings(prompt: &mut repl::State) {
    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.set_titles(Row::new(
        ["Setting", "Current", "Description"]
            .iter()
            .map(|x| table::header_cell(x))
            .collect(),
    ));
    for setting in CMD_CACHE.settings.values() {
        table.add_row(Row::new(vec![
            Cell::new(setting.name),
            Cell::new(&get_setting(setting.setting, prompt)),
            Cell::new(&textwrap::fill(&setting.description, 40)),
        ]));
    }
    table.printstd();
}

pub async fn execute(
    cmd: &BackslashCmd,
    prompt: &mut repl::State,
) -> Result<ExecuteResult, anyhow::Error> {
    use crate::commands::parser::BackslashCmd::*;
    use crate::commands::parser::SetCommand;
    use ExecuteResult::*;
    use Setting::*;

    let mut options = Options {
        command_line: false,
        styler: Some(Styler::new()),
        conn_params: prompt.conn_params.clone(),
        instance_name: None,
    };
    options.infer_instance_name()?;
    match cmd {
        Help => {
            print!("{HELP}");
            Ok(Skip)
        }
        Common(cmd) => {
            prompt.soft_reconnect().await?;
            let conn = prompt.connection.as_mut().expect("connection established");
            let result = execute::common(Some(conn), cmd, &options).await?;

            if let Some(branch) = result.new_branch {
                prompt.try_connect(DatabaseBranch::Branch(branch)).await?;
            }

            Ok(Skip)
        }
        Set(SetCommand { setting: None }) => {
            list_settings(prompt);
            Ok(Skip)
        }
        Set(SetCommand { setting: Some(cmd) }) if cmd.is_show() => {
            println!("{}: {}", cmd.name(), get_setting(cmd, prompt));
            Ok(Skip)
        }
        Set(SetCommand { setting: Some(cmd) }) => {
            match cmd {
                InputMode(m) => {
                    prompt
                        .input_mode(m.value.expect("only writes here"))
                        .await?;
                }
                ImplicitProperties(b) => {
                    prompt.print.implicit_properties = b.unwrap_value();
                }
                VerboseErrors(b) => {
                    prompt.verbose_errors = b.unwrap_value();
                }
                Limit(c) => {
                    let limit = c.value.expect("only set here");
                    if limit == 0 {
                        prompt.implicit_limit = None;
                        prompt.print.max_items = None;
                    } else {
                        prompt.implicit_limit = Some(limit);
                        prompt.print.max_items = Some(limit);
                    }
                }
                VectorDisplayLength(c) => {
                    prompt.print.max_vector_length = c.value.expect("only set here");
                }
                IdleTransactionTimeout(t) => {
                    prompt.idle_transaction_timeout =
                        Duration::from_str(t.value.as_deref().expect("only set here"))?;
                    prompt.set_idle_transaction_timeout().await?;
                }
                HistorySize(c) => {
                    let limit = c.value.expect("only set here");
                    prompt.set_history_limit(limit).await?;
                }
                Language(l) => {
                    prompt.input_language = l.value.expect("only writes here");
                }
                OutputFormat(c) => {
                    prompt.output_format = c.value.expect("only writes here");
                }
                SqlOutputFormat(c) => {
                    prompt.sql_output_format = c.value.expect("only writes here");
                }
                DisplayTypenames(b) => {
                    prompt.display_typenames = b.unwrap_value();
                }
                ExpandStrings(b) => {
                    prompt.print.expand_strings = b.unwrap_value();
                }
                PrintStats(v) => {
                    prompt.print_stats = v.value.expect("only writes here");
                }
            }
            Ok(Skip)
        }
        Connect(c) => {
            if prompt.in_transaction() {
                print::warn!("WARNING: Transaction canceled.");
            }
            prompt
                .try_connect(DatabaseBranch::Ambiguous(c.database_name.clone()))
                .await
                .map_err(|e| {
                    print::error!("Cannot connect: {e:#}");
                })
                .ok();
            Ok(Skip)
        }
        LastError => {
            if let Some(ref err) = prompt.last_error {
                match err.downcast_ref::<Error>() {
                    Some(e) => println!("{}", display_error_verbose(e)),
                    None => println!("{err:#}"),
                }
            } else {
                eprintln!("== no previous error ==");
            }
            Ok(Skip)
        }
        Expand => {
            if let Some(ref last) = prompt.last_analyze {
                analyze::render_expanded_explain(&last.output).await?;
            } else {
                eprintln!("== no previous analyze statement ==");
            }
            Ok(Skip)
        }
        DebugState(StateParam { base }) => {
            let (desc_id, value) = if *base {
                prompt.get_state_as_value()?
            } else {
                prompt
                    .connection
                    .as_ref()
                    .map(|c| c.get_state_as_value())
                    .unwrap_or_else(|| prompt.get_state_as_value())?
            };
            println!("Descriptor id: {desc_id}");
            print::native_to_stdout(tokio_stream::iter([Ok::<_, Error>(value)]), &prompt.print)
                .await?;
            println!();
            Ok(Skip)
        }
        DebugStateDesc(StateParam { base }) => {
            let desc = if *base {
                prompt.edgeql_state_desc.clone()
            } else {
                prompt
                    .connection
                    .as_ref()
                    .map(|c| c.get_state_desc())
                    .unwrap_or(prompt.edgeql_state_desc.clone())
            };
            let typedesc = desc.decode()?;
            eprintln!("Descriptor id: {}", desc.id);
            eprintln!("Descriptor: {:#?}", typedesc.descriptors());
            eprintln!("Codec: {:#?}", typedesc.build_codec()?);
            Ok(Skip)
        }
        History => {
            prompt.show_history().await?;
            Ok(Skip)
        }
        Edit(c) => match prompt.spawn_editor(c.entry).await? {
            prompt::Input::Text(text) => Ok(Input(text)),
            prompt::Input::Interrupt | prompt::Input::Eof => Ok(Skip),
        },
        Exit => Ok(Quit),
    }
}

#[cfg(test)]
mod test {
    use super::Item::{self, *};
    use super::Parser;

    fn tok_values(s: &str) -> Vec<Item<'_>> {
        Parser::new(s).map(|tok| tok.item).collect::<Vec<_>>()
    }

    #[test]
    fn test_parser() {
        assert_eq!(tok_values("\\x"), [Command("\\x")]);
        assert_eq!(
            tok_values("\\x a b"),
            [Command("\\x"), Argument("a"), Argument("b")]
        );
        assert_eq!(tok_values("\\x 'a b'"), [Command("\\x"), Argument("'a b'")]);
        assert_eq!(
            tok_values("\\describe schema::`Object`"),
            [Command("\\describe"), Argument("schema::`Object`")]
        );
    }
}
