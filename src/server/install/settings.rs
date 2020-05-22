use std::collections::HashMap;

use linked_hash_map::LinkedHashMap;
use prettytable::{Table, Row, Cell};

use crate::server::options::Install;
use crate::server::os_trait::{CurrentOs, Method};
use crate::server::detect::{VersionQuery, InstallationMethods};
use crate::server::version::Version;
use crate::server::install::InstallMethod;
use crate::table;


#[derive(Debug)]
pub struct SettingsBuilder<'a> {
    pub method: InstallMethod,
    pub possible_methods: InstallationMethods,
    pub version_query: VersionQuery,
    pub package_name: Option<String>,
    pub major_version: Option<Version<String>>,
    pub version: Option<Version<String>>,
    pub extra: LinkedHashMap<String, String>,
    pub os: &'a dyn CurrentOs,
    pub methods: HashMap<InstallMethod, Box<dyn Method + 'a>>,
}

#[derive(Debug)]
pub struct Settings {
    pub method: InstallMethod,
    pub package_name: String,
    pub major_version: Version<String>,
    pub version: Version<String>,
    pub nightly: bool,
    pub extra: LinkedHashMap<String, String>,
}

impl<'os> SettingsBuilder<'os> {
    pub fn new(os: &'os dyn CurrentOs, options: &Install,
        methods: InstallationMethods)
        -> Result<SettingsBuilder<'os>, anyhow::Error>
    {
        let version_query = if options.nightly {
                VersionQuery::Nightly
            } else {
                VersionQuery::Stable(options.version.clone())
            };
        Ok(SettingsBuilder {
            os,
            method: options.method.clone()
                .or_else(|| methods.pick_first())
                .unwrap_or(InstallMethod::Package),
            possible_methods: methods,
            version_query,
            package_name: None,
            major_version: None,
            version: None,
            extra: LinkedHashMap::new(),
            methods: HashMap::new(),
        })
    }
    pub fn build(mut self)
        -> anyhow::Result<(Settings, Box<dyn Method + 'os>)>
    {
        self.ensure_method()?;
        let method = self.methods.remove(&self.method)
            .expect("method created by ensure_method");
        let settings = Settings {
            method: self.method,
            package_name: self.package_name.unwrap(),
            major_version: self.major_version.unwrap(),
            version: self.version.unwrap(),
            nightly: self.version_query.is_nightly(),
            extra: self.extra,
        };
        Ok((settings, method))
    }
    fn ensure_method(&mut self) -> anyhow::Result<()> {
        if self.methods.get(&self.method).is_none() {
            self.methods.insert(self.method.clone(),
                self.os.make_method(&self.method, &self.possible_methods)?);
        }
        Ok(())
    }
    pub fn auto_version(&mut self) -> anyhow::Result<()> {
        self.ensure_method()?;
        let res = self.methods.get(&self.method).expect("method exists")
            .get_version(&self.version_query)
            .map_err(|e| {
                log::warn!("Unable to determine version: {:#}", e);
            })
            .ok();
        if let Some(res) = res {
            self.version = Some(res.version);
            self.package_name = Some(res.package_name);
            self.major_version = Some(res.major_version);
        }
        Ok(())
    }
}

impl Settings {
    pub fn print(&self) {
        let mut table = Table::new();
        let version_opt = format!("--version={}", self.major_version);
        table.add_row(Row::new(vec![
            Cell::new("Installation method"),
            Cell::new(self.method.title()),
            Cell::new(self.method.option()),
        ]));
        table.add_row(Row::new(vec![
            Cell::new("Major version"),
            Cell::new(self.major_version.num()),
            Cell::new(if self.nightly {
                "--nightly"
            } else {
                &version_opt
            }),
        ]));
        table.add_row(Row::new(vec![
            Cell::new("Exact version"),
            Cell::new(self.version.num()),
        ]));
        for (k, v) in &self.extra {
            table.add_row(Row::new(vec![
                Cell::new(k),
                Cell::new(v),
            ]));
        }
        table.set_format(*table::FORMAT);
        table.printstd();
    }
}


