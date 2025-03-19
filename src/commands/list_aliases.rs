use prettytable::{Cell, Row, Table};

use gel_derive::Queryable;
use is_terminal::IsTerminal;

use crate::commands::Options;
use crate::commands::filter;
use crate::connect::Connection;
use crate::table;

#[derive(Queryable)]
struct Alias {
    name: String,
    expr: String,
    klass: String,
}

pub async fn list_aliases(
    cli: &mut Connection,
    options: &Options,
    pattern: &Option<String>,
    system: bool,
    case_sensitive: bool,
    verbose: bool,
) -> Result<(), anyhow::Error> {
    let filter = match (pattern, system) {
        (None, true) => "FILTER .is_from_alias",
        (None, false) => {
            r#"FILTER .is_from_alias AND
                NOT re_test("^(?:std|schema|math|sys|cfg|cal|stdgraphql)::",
                    .name)"#
        }
        (Some(_), true) => "FILTER .is_from_alias AND re_test(<str>$0, .name)",
        (Some(_), false) => {
            r#"FILTER .is_from_alias
                AND re_test(<str>$0, .name) AND
                NOT re_test("^(?:std|schema|math|sys|cfg|cal|stdgraphql)::",
                .name)"#
        }
    };
    let query = &format!(
        r###"
        WITH MODULE schema
        SELECT Type {{
            name,
            expr,
            klass := (
                'object' IF Type IS ObjectType ELSE
                'scalar' IF Type IS ScalarType ELSE
                'tuple' IF Type IS Tuple ELSE
                'array' IF Type IS Array ELSE
                'unknown'
            ),
        }}
        {filter}
        ORDER BY .name;
    "###
    );
    let items = filter::query::<Alias>(cli, query, pattern, case_sensitive).await?;
    if !options.command_line || std::io::stdout().is_terminal() {
        let mut table = Table::new();
        table.set_format(*table::FORMAT);
        if verbose {
            table.set_titles(Row::new(
                ["Name", "Class", "Expression"]
                    .iter()
                    .map(|x| table::header_cell(x))
                    .collect(),
            ));
            for item in items {
                table.add_row(Row::new(vec![
                    Cell::new(&item.name),
                    Cell::new(&item.klass),
                    Cell::new(&item.expr),
                ]));
            }
        } else {
            table.set_titles(Row::new(
                ["Name", "Class"]
                    .iter()
                    .map(|x| table::header_cell(x))
                    .collect(),
            ));
            for item in items {
                table.add_row(Row::new(vec![
                    Cell::new(&item.name),
                    Cell::new(&item.klass),
                ]));
            }
        }
        if table.is_empty() {
            if let Some(pattern) = pattern {
                eprintln!("No aliases found matching {pattern:?}");
            } else if !system {
                eprintln!("No user-defined expression aliases found.");
            } else {
                eprintln!("No aliases found.");
            }
        } else {
            table.printstd();
        }
    } else if verbose {
        for item in items {
            println!("{}\t{}\t{}", item.name, item.klass, item.expr);
        }
    } else {
        for item in items {
            println!("{}\t{}", item.name, item.klass);
        }
    }
    Ok(())
}
