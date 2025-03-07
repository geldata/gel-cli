use std::fmt;
use std::str::FromStr;

use edgedb_cli_derive::IntoArgs;
use gel_tokio::CloudName;

use crate::cloud::ops::CloudTier;
use crate::commands::ExitCode;
use crate::print::{self, err_marker, msg};
use crate::process::{self, IntoArg};

#[derive(Clone, Debug)]
pub enum InstanceName {
    Local(String),
    Cloud { org_slug: String, name: String },
}

impl From<gel_tokio::InstanceName> for InstanceName {
    fn from(x: gel_tokio::InstanceName) -> Self {
        match x {
            gel_tokio::InstanceName::Local(s) => InstanceName::Local(s),
            gel_tokio::InstanceName::Cloud(CloudName { org_slug, name }) => {
                InstanceName::Cloud { org_slug, name }
            }
        }
    }
}

impl Into<gel_tokio::InstanceName> for InstanceName {
    fn into(self) -> gel_tokio::InstanceName {
        match self {
            InstanceName::Local(s) => gel_tokio::InstanceName::Local(s),
            InstanceName::Cloud { org_slug, name } => {
                gel_tokio::InstanceName::Cloud(CloudName { org_slug, name })
            }
        }
    }
}

impl fmt::Display for InstanceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstanceName::Local(name) => name.fmt(f),
            InstanceName::Cloud { org_slug, name } => write!(f, "{org_slug}/{name}"),
        }
    }
}

impl FromStr for InstanceName {
    type Err = anyhow::Error;
    fn from_str(name: &str) -> anyhow::Result<InstanceName> {
        let name = gel_tokio::InstanceName::from_str(name)?;
        Ok(name.into())
    }
}

impl IntoArg for &InstanceName {
    fn add_arg(self, process: &mut process::Native) {
        process.arg(self.to_string());
    }
}

pub fn instance_arg(
    positional: &Option<InstanceName>,
    named: &Option<InstanceName>,
) -> anyhow::Result<InstanceName> {
    if let Some(name) = positional {
        print::error!(
            "Specifying instance name as positional argument has been removed. \
            Use `-I {name}` instead."
        );
        return Err(ExitCode::new(1).into());
    }
    if let Some(name) = named {
        return Ok(name.clone());
    }

    {
        // infer instance from current project
        let config = gel_tokio::Builder::new().build()?;

        let instance = config.instance_name().cloned();

        if let Some(instance) = instance {
            return Ok(instance.into());
        }
    };

    msg!(
        "{} Instance name argument is required, use '-I name'",
        err_marker()
    );
    Err(ExitCode::new(2).into())
}

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
