use crate::instance::{Instance, InstanceOpError, backup::InstanceBackup};

pub struct LocalInstanceHandle {
    pub name: String,
}

impl Instance for LocalInstanceHandle {
    fn backup(&self) -> Result<Box<dyn InstanceBackup>, InstanceOpError> {
        Err(InstanceOpError::Unsupported("local".to_string()))
    }
}
