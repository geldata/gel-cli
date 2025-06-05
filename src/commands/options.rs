use crate::connect::Connector;
use crate::print::style::Styler;

#[derive(Debug)]
pub struct Options {
    pub command_line: bool,
    pub styler: Option<Styler>,
    pub conn_params: Connector,
    pub instance_name: Option<gel_tokio::InstanceName>,
    pub skip_hooks: bool,
}

impl Options {
    pub fn infer_instance_name(&mut self) -> anyhow::Result<()> {
        self.instance_name = self.conn_params.instance_name()?;
        Ok(())
    }
}
