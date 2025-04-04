use gel_cli_derive::IntoArgs;
use gel_cli_instance::cloud::CloudTier;

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct CloudInstanceParams {
    /// The region in which to create the instance (for cloud instances).
    #[arg(long)]
    pub region: Option<String>,

    #[command(flatten)]
    pub billables: CloudInstanceBillables,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct CloudInstanceBillables {
    /// Cloud instance subscription tier.
    #[arg(long, value_name = "tier")]
    #[arg(value_enum)]
    pub tier: Option<CloudTier>,

    /// The size of compute to be allocated for the Cloud instance in
    /// Compute Units.
    #[arg(long, value_name="number", value_parser=billable_unit)]
    pub compute_size: Option<String>,

    /// The size of storage to be allocated for the Cloud instance in
    /// Gigabytes.
    #[arg(long, value_name="GiB", value_parser=billable_unit)]
    pub storage_size: Option<String>,
}

fn billable_unit(s: &str) -> Result<String, String> {
    let (numerator, denominator) = match s.split_once('/') {
        Some(v) => v,
        None => (s, "1"),
    };

    let n: u64 = numerator
        .parse()
        .map_err(|_| format!("`{s}` is not a positive number or valid fraction"))?;

    let d: u64 = denominator
        .parse()
        .map_err(|_| format!("`{s}` is not a positive number or valid fraction"))?;

    if n == 0 || d == 0 {
        Err(String::from(
            "`{s}` is not a positive number or valid fraction",
        ))
    } else {
        Ok(s.to_string())
    }
}
