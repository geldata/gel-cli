use prettytable::{Cell, Row, Table};

use gel_derive::Queryable;
use std::io::IsTerminal;

use crate::commands::Options;
use crate::commands::filter;
use crate::connect::Connection;
use crate::table;

#[derive(Queryable)]
struct Index {
    expr: String,
    is_implicit: bool,
    subject_name: String,
}

pub async fn list_indexes(
    cli: &mut Connection,
    options: &Options,
    pattern: &Option<String>,
    system: bool,
    case_sensitive: bool,
    verbose: bool,
) -> Result<(), anyhow::Error> {
    let mut filters = Vec::with_capacity(3);
    if !system {
        filters.push(
            r#"NOT re_test("^(?:std|schema|math|sys|cfg|cal|stdgraphql)::",
               .subject_name)"#,
        );
    }
    if !verbose {
        filters.push("NOT .is_implicit");
    }
    if pattern.is_some() {
        filters.push("re_test(<str>$0, .subject_name)");
    }
    let filter = if filters.is_empty() {
        String::from("")
    } else {
        format!("FILTER {}", filters.join(" AND "))
    };
    let query = &format!(
        r###"
        WITH
            MODULE schema,
            I := {{
                Index,
                (
                    SELECT Constraint
                    FILTER .name = 'std::exclusive' AND NOT .is_abstract
                )
            }}
        SELECT I {{
            expr,
            subject_name := I[IS Index].<indexes[IS Source].name,
            cons_on := '.' ++ I[IS Constraint].subject.name,
            cons_of := I[Is Constraint].subject[IS Pointer]
                .<pointers[IS Source].name,
            cons_of_of := I[Is Constraint].subject[IS Property]
                .<pointers[IS Link].<pointers[IS Source].name,
        }} {{
            expr := .cons_on ?? .expr,
            is_implicit := EXISTS .cons_on,
            subject_name :=
                (.cons_of_of ++ '.' ++ .cons_of) ??
                (.cons_of) ??
                (.subject_name)
        }}
        {filter}
        ORDER BY .subject_name;
    "###
    );
    let items = filter::query::<Index>(cli, query, pattern, case_sensitive).await?;
    if !options.command_line || std::io::stdout().is_terminal() {
        let mut table = Table::new();
        table.set_format(*table::FORMAT);
        if verbose {
            table.set_titles(Row::new(
                ["Index On", "Implicit", "Subject"]
                    .iter()
                    .map(|x| table::header_cell(x))
                    .collect(),
            ));
            for item in items {
                table.add_row(Row::new(vec![
                    Cell::new(&item.expr),
                    Cell::new(&item.is_implicit.to_string()),
                    Cell::new(&item.subject_name),
                ]));
            }
        } else {
            table.set_titles(Row::new(
                ["Index On", "Subject"]
                    .iter()
                    .map(|x| table::header_cell(x))
                    .collect(),
            ));
            for item in items {
                table.add_row(Row::new(vec![
                    Cell::new(&item.expr),
                    Cell::new(&item.subject_name),
                ]));
            }
        }
        if table.is_empty() {
            if let Some(pattern) = pattern {
                eprintln!("No indexes found matching {pattern:?}");
            } else if !verbose {
                if options.command_line {
                    eprintln!("No explicit indexes found. Try --verbose");
                } else {
                    eprintln!("No explicit indexes found. Try \\li -v");
                }
            } else {
                eprintln!("No indexes found.");
            }
        } else {
            table.printstd();
        }
    } else if verbose {
        for item in items {
            println!("{}\t{}\t{}", item.expr, item.is_implicit, item.subject_name);
        }
    } else {
        for item in items {
            println!("{}\t{}", item.expr, item.subject_name);
        }
    }
    Ok(())
}
