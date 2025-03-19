use crate::commands::Options;
use crate::commands::helpers::quote_namespaced;
use crate::connect::Connection;
use crate::highlight;

pub async fn describe(
    cli: &mut Connection,
    options: &Options,
    name: &str,
    verbose: bool,
) -> Result<(), anyhow::Error> {
    let items = cli
        .query::<String, _>(
            &format!(
                "DESCRIBE OBJECT {name} AS TEXT {flag}",
                name = quote_namespaced(name),
                flag = if verbose { "VERBOSE" } else { "" }
            ),
            &(),
        )
        .await?;
    for text in items {
        if let Some(ref styler) = options.styler {
            let mut out = String::with_capacity(text.len());
            highlight::edgeql(&mut out, &text, styler);
            println!("{out}");
        } else {
            println!("{text}");
        }
    }
    Ok(())
}
