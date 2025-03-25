use prettytable::{Cell, Row, Table};

use gel_derive::Queryable;
use std::io::IsTerminal;
use terminal_size::{Width, terminal_size};

use crate::commands::Options;
use crate::commands::filter;
use crate::connect::Connection;
use crate::table;

#[derive(Queryable)]
struct ScalarType {
    name: String,
    extending: String,
    kind: String,
}

pub async fn list_scalar_types<'x>(
    cli: &mut Connection,
    options: &Options,
    pattern: &Option<String>,
    system: bool,
    case_sensitive: bool,
) -> Result<(), anyhow::Error> {
    let filter = match (pattern, system) {
        (None, true) => "FILTER NOT .is_from_alias",
        (None, false) => {
            r#"FILTER NOT
                re_test("^(?:std|schema|math|sys|cfg|cal|stdgraphql)::",
                .name)"#
        }
        (Some(_), true) => "FILTER NOT .is_from_alias AND re_test(<str>$0, .name)",
        (Some(_), false) => {
            r#"FILTER NOT .is_from_alias
                AND re_test(<str>$0, .name) AND
                NOT re_test("^(?:std|schema|math|sys|cfg|cal|stdgraphql)::",
                .name)"#
        }
    };

    let query = &format!(
        r###"
        WITH MODULE schema
        SELECT ScalarType {{
            name,
            `extending` := to_str(array_agg(.bases.name), ', '),
            kind := (
                'enum' IF 'std::anyenum' IN .ancestors.name ELSE
                'sequence' IF 'std::sequence' IN .ancestors.name ELSE
                'normal'
            ),
        }}
        {filter}
        ORDER BY .name;
    "###
    );

    let items = filter::query::<ScalarType>(cli, query, pattern, case_sensitive).await?;
    if !options.command_line || std::io::stdout().is_terminal() {
        let term_width = terminal_size().map(|(Width(w), _h)| w).unwrap_or(80);
        let extending_width: usize = ((term_width - 10) / 2).into();
        let mut table = Table::new();
        table.set_format(*table::FORMAT);
        table.set_titles(Row::new(
            ["Name", "Extending", "Kind"]
                .iter()
                .map(|x| table::header_cell(x))
                .collect(),
        ));
        for item in items {
            table.add_row(Row::new(vec![
                Cell::new(&item.name),
                Cell::new(&textwrap::fill(&item.extending, extending_width)),
                Cell::new(&item.kind),
            ]));
        }
        if table.is_empty() {
            if let Some(pattern) = pattern {
                eprintln!("No scalar types found matching {pattern:?}");
            } else if !system {
                eprintln!(
                    "No user-defined scalar types found. {}",
                    if options.command_line {
                        "Try --system"
                    } else {
                        r"Try \ls -s"
                    }
                );
            }
        } else {
            table.printstd();
        }
    } else {
        for item in items {
            println!("{}\t{}\t{}", item.name, item.extending, item.kind);
        }
    }
    Ok(())
}
