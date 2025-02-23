use std::borrow::Cow;
use std::path::Path;

use anyhow::Context;
use tokio::fs;
use tokio::io::{self, AsyncWriteExt};

use gel_errors::ParameterTypeMismatchError;
use gel_tokio::raw::Description;

use crate::classify;
use crate::cli::env::Env;
use crate::commands::parser::Analyze;
use crate::connect::Connection;
use crate::interactive::QueryError;
use crate::platform::tmp_file_path;
use crate::repl::{self, LastAnalyze};
use crate::variables::input_variables;

mod contexts;
mod model;
mod table;
mod tree;

pub use model::Analysis;

pub async fn interactive(prompt: &mut repl::State, query: &str) -> anyhow::Result<()> {
    let cli = prompt.connection.as_mut().expect("connection established");
    let data = match cli.query_required_single::<String, _>(query, &()).await {
        Ok(data) => data,
        Err(e) if e.is::<ParameterTypeMismatchError>() => {
            let Some(data_description) = e.get::<Description>() else {
                return Err(e)?;
            };
            let indesc = data_description.input()?;
            let input = match cli
                .ping_while(input_variables(
                    &indesc,
                    &mut prompt.prompt,
                    prompt.input_language,
                ))
                .await
            {
                Ok(input) => input,
                Err(e) => {
                    eprintln!("{e:#}");
                    prompt.last_error = Some(e);
                    return Err(QueryError)?;
                }
            };
            cli.query_required_single::<String, _>(query, &input)
                .await?
        }
        Err(e) => return Err(e)?,
    };

    if Env::_analyze_debug_json()?.unwrap_or(false) {
        let json: serde_json::Value = serde_json::from_str(&data).unwrap();
        println!("JSON: {json}");
    }
    let jd = &mut serde_json::Deserializer::from_str(&data);
    let output = serde_path_to_error::deserialize(jd).context("parsing explain output")?;
    let output = contexts::preprocess(output);

    let analyze = prompt.last_analyze.insert(LastAnalyze {
        query: query.to_owned(),
        output,
    });
    render_explain(&analyze.output)?;
    Ok(())
}

pub async fn render_expanded_explain(data: &Analysis) -> anyhow::Result<()> {
    tree::print_expanded_tree(data);
    Ok(())
}

fn render_explain(explain: &Analysis) -> anyhow::Result<()> {
    contexts::print(explain);
    if Env::_analyze_debug_plan()?.unwrap_or(false) {
        tree::print_debug_plan(explain);
    }
    tree::print_shape(explain);
    Ok(())
}

#[fn_error_context::context("cannot lookup path {:?}", path)]
async fn is_special(path: &Path) -> anyhow::Result<bool> {
    match fs::metadata(path).await {
        Ok(meta) => Ok(!meta.is_file()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e)?,
    }
}

pub async fn command(cli: &mut Connection, options: &Analyze) -> anyhow::Result<()> {
    let data = if let Some(json_path) = &options.read_json {
        fs::read_to_string(&json_path)
            .await
            .with_context(|| format!("cannot read {json_path:?}"))?
    } else {
        let Some(inner_query) = &options.query else {
            anyhow::bail!("Query argument is required");
        };
        let query = if classify::is_analyze(inner_query) {
            // allow specifying options in the query itself
            Cow::Borrowed(inner_query)
        } else {
            // but also do not make user writing `analyze` twice
            Cow::Owned(format!("analyze {inner_query}"))
        };

        cli.query_required_single::<String, _>(&query, &()).await?
    };
    if let Some(out_path) = &options.debug_output_file {
        if out_path == Path::new("-") {
            let mut out = io::stdout();
            out.write_all(data.as_bytes()).await?;
            out.flush().await?;
        } else if is_special(out_path).await? {
            async {
                let mut out = fs::File::create(&out_path).await?;
                out.write_all(data.as_bytes()).await?;
                out.flush().await
            }
            .await
            .with_context(|| format!("error writing to {out_path:?}"))?;
        } else {
            let tmp = tmp_file_path(out_path);
            async {
                let mut out = fs::File::create(&tmp).await?;
                out.write_all(data.as_bytes()).await?;
                out.flush().await
            }
            .await
            .with_context(|| format!("error writing to {tmp:?}"))?;
            fs::rename(&tmp, &out_path)
                .await
                .with_context(|| format!("rename error {tmp:?} -> {out_path:?}"))?;
        }
    } else {
        let jd = &mut serde_json::Deserializer::from_str(&data);
        let output = serde_path_to_error::deserialize(jd)
            .with_context(|| "parsing explain output".to_string())?;
        let output = contexts::preprocess(output);

        render_explain(&output)?;
        if options.expand {
            println!();
            render_expanded_explain(&output).await?;
        }
    }
    Ok(())
}
