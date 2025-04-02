use std::str::FromStr;

use crate::connect::Connector;
use crate::portable;
use crate::print::style::Styler;

pub struct Options {
    pub command_line: bool,
    pub styler: Option<Styler>,
    pub conn_params: Connector,
    pub instance_name: Option<portable::options::InstanceName>,
    pub branch: Option<String>,
}

impl Options {
    pub fn infer_instance_name(&mut self) -> anyhow::Result<()> {
        let config = &self.conn_params.get()?;
        self.instance_name = config
            .instance_name()
            .map(|x| portable::options::InstanceName::from_str(&x.to_string()))
            .transpose()?;
        self.branch = config.branch().map(|x| x.to_string());
        Ok(())
    }
}
