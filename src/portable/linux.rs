use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use fn_error_context::context;

use crate::branding::BRANDING_CLOUD;
use crate::commands::ExitCode;
use crate::platform::{current_exe, detect_ipv6, home_dir};
use crate::portable::instance::control;
use crate::portable::instance::destroy::InstanceNotFound;
use crate::portable::instance::status;
use crate::portable::local::{InstanceInfo, log_file, runstate_dir};
use crate::portable::options::InstanceName;
use crate::print;
use crate::process;

pub fn unit_dir() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?.join(".config/systemd/user"))
}

fn unit_name(name: &str) -> String {
    format!("edgedb-server@{name}.service")
}

fn socket_name(name: &str) -> String {
    format!("edgedb-server@{name}.socket")
}

pub fn service_files(name: &str) -> anyhow::Result<Vec<PathBuf>> {
    let dir = unit_dir()?;
    Ok(vec![dir.join(unit_name(name)), dir.join(socket_name(name))])
}

pub fn create_service(info: &InstanceInfo) -> anyhow::Result<()> {
    let name = &info.name;
    let unit_dir = unit_dir()?;
    fs::create_dir_all(&unit_dir)
        .with_context(|| format!("cannot create directory {unit_dir:?}"))?;
    let unit_name = unit_name(name);
    let socket_name = socket_name(name);
    let unit_path = unit_dir.join(unit_name);
    let socket_unit_path = unit_dir.join(socket_name);
    fs::write(&unit_path, systemd_unit(name, info)?)
        .with_context(|| format!("cannot write {unit_path:?}"))?;
    if info.get_version()?.specific().major >= 2 {
        fs::write(&socket_unit_path, systemd_socket(name, info)?)
            .with_context(|| format!("cannot write {socket_unit_path:?}"))?;
    }
    if preliminary_detect().is_some() {
        process::Native::new("systemctl", "systemctl", "systemctl")
            .arg("--user")
            .arg("daemon-reload")
            .run()
            .map_err(|e| log::warn!("failed to reload systemd daemon: {}", e))
            .ok();
        start_service(name)?;
    } else {
        anyhow::bail!("either systemctl not found or environment configured incorrectly");
    }
    Ok(())
}

#[context("cannot compose service file")]
pub fn systemd_unit(name: &str, _info: &InstanceInfo) -> anyhow::Result<String> {
    Ok(format!(
        r###"
[Unit]
Description=EdgeDB Database Service, instance {instance_name:?}
Documentation=https://edgedb.com/
After=syslog.target
After=network.target

[Service]
Type=notify

RuntimeDirectory=edgedb-{instance_name}
ExecStart={executable} instance start --instance {instance_name} --managed-by=systemd
ExecReload=/bin/kill -HUP ${{MAINPID}}
KillMode=mixed
TimeoutSec=0

[Install]
WantedBy=default.target
    "###,
        instance_name = name,
        executable = current_exe()?.display(),
    ))
}

#[context("cannot compose service file")]
pub fn systemd_socket(name: &str, info: &InstanceInfo) -> anyhow::Result<String> {
    Ok(format!(
        r###"
[Unit]
Description=EdgeDB Database Service socket, instance {instance_name:?}
Documentation=https://edgedb.com/

[Socket]
FileDescriptorName=edgedb-server
ListenStream=127.0.0.1:{port}
{ipv6_listen}

[Install]
WantedBy=default.target
    "###,
        instance_name = name,
        port = info.port,
        ipv6_listen = if detect_ipv6() {
            format!("ListenStream=[::1]:{port}", port = info.port)
        } else {
            String::new()
        },
    ))
}

fn systemd_is_not_found_error(e: &str) -> bool {
    e.contains("Failed to get D-Bus connection")
        || e.contains("Failed to connect to bus")
        || e.contains("No such file or directory")
        || e.contains(".service not loaded")
        || e.contains(".service does not exist")
}

pub fn stop_and_disable(name: &str) -> anyhow::Result<bool> {
    let mut found = false;
    let svc_name = unit_name(name);
    let socket_name = socket_name(name);
    log::info!("Stopping service {}", svc_name);
    let mut not_found_error = None;
    let mut cmd = process::Native::new("stop service", "systemctl", "systemctl");
    cmd.arg("--user");
    cmd.arg("stop");
    cmd.arg(&svc_name);
    match cmd.run_or_stderr()? {
        Ok(()) => found = true,
        Err((_, e)) if systemd_is_not_found_error(&e) => {
            not_found_error = Some(e);
        }
        Err((s, e)) => {
            log::warn!(
                "Error running systemctl (command-line: {:?}): {}: {}",
                cmd.command_line(),
                s,
                e
            );
        }
    }

    let mut cmd = process::Native::new("stop socket", "systemctl", "systemctl");
    cmd.arg("--user");
    cmd.arg("stop");
    cmd.arg(&socket_name);
    match cmd.run_or_stderr()? {
        Ok(()) => found = true,
        Err((s, e)) => {
            log::warn!(
                "Error running systemctl (command-line: {:?}): {}: {}",
                cmd.command_line(),
                s,
                e
            );
        }
    }

    let mut cmd = process::Native::new("disable service", "systemctl", "systemctl");
    cmd.arg("--user");
    cmd.arg("disable");
    cmd.arg(&svc_name);
    match cmd.run_or_stderr()? {
        Ok(()) => found = true,
        Err((_, e)) if systemd_is_not_found_error(&e) => {
            not_found_error = Some(e);
        }
        Err((s, e)) => {
            log::warn!(
                "Error running systemctl (command-line: {:?}): {}: {}",
                cmd.command_line(),
                s,
                e
            );
        }
    }

    let mut cmd = process::Native::new("disable socket", "systemctl", "systemctl");
    cmd.arg("--user");
    cmd.arg("disable");
    cmd.arg(&socket_name);
    match cmd.run_or_stderr()? {
        Ok(()) => found = true,
        Err((s, e)) => {
            log::warn!(
                "Error running systemctl (command-line: {:?}): {}: {}",
                cmd.command_line(),
                s,
                e
            );
        }
    }

    if let Some(e) = not_found_error {
        return Err(InstanceNotFound(anyhow::anyhow!(
            "no instance {:?} found: {}",
            name,
            e.trim()
        ))
        .into());
    }
    Ok(found)
}

pub fn server_cmd(
    inst: &InstanceInfo,
    is_shutdown_supported: bool,
) -> anyhow::Result<process::Native> {
    let data_dir = inst.data_dir()?;
    let server_path = inst.server_path()?;
    let mut pro = process::Native::new("edgedb", "edgedb", server_path);
    pro.env_default("EDGEDB_SERVER_LOG_LEVEL", "warn");
    pro.env_default("EDGEDB_SERVER_HTTP_ENDPOINT_SECURITY", "optional");
    pro.env_default("EDGEDB_SERVER_INSTANCE_NAME", &inst.name);
    pro.env_default(
        "EDGEDB_SERVER_CONFIG_cfg::auto_rebuild_query_cache",
        "false",
    );
    pro.arg("--data-dir").arg(data_dir);
    pro.arg("--runstate-dir").arg(runstate_dir(&inst.name)?);
    pro.arg("--port").arg(inst.port.to_string());
    if inst.get_version()?.specific().major >= 2 {
        pro.arg("--compiler-pool-mode=on_demand");
        pro.arg("--admin-ui=enabled");
        if is_shutdown_supported {
            pro.arg("--auto-shutdown-after=600");
        }
    }
    Ok(pro)
}

pub fn detect_systemd(instance: &str) -> bool {
    _detect_systemd(instance).is_some()
}

fn preliminary_detect() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR").or_else(|| env::var_os("DBUS_SESSION_BUS_ADDRESS"))?;
    if let Ok(path) = which::which("systemctl") {
        Some(path)
    } else {
        None
    }
}

fn _detect_systemd(instance: &str) -> Option<PathBuf> {
    let path = preliminary_detect()?;
    let unit_name = unit_name(instance);
    let out = process::Native::new("detect systemd", "systemctl", &path)
        .arg("--user")
        .arg("is-active")
        .arg(&unit_name)
        .get_output()
        .ok()?;
    if out.status.success() {
        return Some(path);
    }
    if !out.stderr.is_empty() {
        log::info!(
            "cannot access systemd daemon: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    log::debug!(
        "service is-enabled returned: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    Some(path)
}

pub fn start_service(instance: &str) -> anyhow::Result<()> {
    let socket_name = socket_name(instance);
    let socket_file = unit_dir()?.join(&socket_name);
    if socket_file.exists() {
        process::Native::new("service start", "systemctl", "systemctl")
            .arg("--user")
            .arg("enable")
            .arg(&socket_name)
            .run()?;
        process::Native::new("service start", "systemctl", "systemctl")
            .arg("--user")
            .arg("start")
            .arg(&socket_name)
            .run()?;
    }
    process::Native::new("service start", "systemctl", "systemctl")
        .arg("--user")
        .arg("enable")
        .arg(unit_name(instance))
        .run()?;
    process::Native::new("service start", "systemctl", "systemctl")
        .arg("--user")
        .arg("start")
        .arg(unit_name(instance))
        .run()?;
    Ok(())
}

pub fn stop_service(name: &str) -> anyhow::Result<()> {
    let socket_name = socket_name(name);
    let socket_file = unit_dir()?.join(&socket_name);
    if socket_file.exists() {
        process::Native::new("stop service", "systemctl", "systemctl")
            .arg("--user")
            .arg("stop")
            .arg(&socket_name)
            .run()?;
        process::Native::new("stop service", "systemctl", "systemctl")
            .arg("--user")
            .arg("disable")
            .arg(&socket_name)
            .run()?;
    }
    process::Native::new("stop service", "systemctl", "systemctl")
        .arg("--user")
        .arg("stop")
        .arg(unit_name(name))
        .run()?;
    process::Native::new("stop service", "systemctl", "systemctl")
        .arg("--user")
        .arg("disable")
        .arg(unit_name(name))
        .run()?;
    Ok(())
}

pub fn restart_service(inst: &InstanceInfo) -> anyhow::Result<()> {
    process::Native::new("restart service", "systemctl", "systemctl")
        .arg("--user")
        .arg("stop")
        .arg(unit_name(&inst.name))
        .run()?;
    process::Native::new("systemctl", "systemctl", "systemctl")
        .arg("--user")
        .arg("restart")
        .arg(socket_name(&inst.name))
        .run()?;
    process::Native::new("restart service", "systemctl", "systemctl")
        .arg("--user")
        .arg("start")
        .arg(unit_name(&inst.name))
        .run()?;
    Ok(())
}

fn is_ready(name: &str) -> bool {
    let mut cmd = process::Native::new("service status", "systemctl", "systemctl");
    cmd.arg("--user");
    cmd.arg("show");
    cmd.arg(socket_name(name));
    let txt = match cmd.get_stdout_text() {
        Ok(txt) => txt,
        Err(_) => return false,
    };
    for line in txt.lines() {
        if let Some(state) = line.strip_prefix("ActiveState=") {
            return state.trim() == "active";
        }
    }
    false
}

pub fn service_status(name: &str) -> status::Service {
    use status::Service::*;

    let mut cmd = process::Native::new("service status", "systemctl", "systemctl");
    cmd.arg("--user");
    cmd.arg("show");
    cmd.arg(unit_name(name));
    let txt = match cmd.get_stdout_text() {
        Ok(txt) => txt,
        Err(e) => {
            return status::Service::Inactive {
                error: format!("cannot determine service status: {e:#}"),
            };
        }
    };
    let mut pid = None;
    let mut exit = None;
    let mut load_error = None;
    for line in txt.lines() {
        if let Some(pid_str) = line.strip_prefix("MainPID=") {
            pid = pid_str.trim().parse().ok();
        }
        if let Some(status_str) = line.strip_prefix("ExecMainStatus=") {
            exit = status_str.trim().parse().ok();
        }
        if let Some(err) = line.strip_prefix("LoadError=") {
            load_error = Some(err.trim().to_string());
        }
    }
    match pid {
        None | Some(0) => {
            if let Some(error) = load_error {
                Inactive { error }
            } else if exit == Some(0) && is_ready(name) {
                Ready
            } else {
                Failed { exit_code: exit }
            }
        }
        Some(pid) => Running { pid },
    }
}

pub fn external_status(inst: &InstanceInfo) -> anyhow::Result<()> {
    process::Native::new("service status", "systemctl", "systemctl")
        .arg("--user")
        .arg("status")
        .arg(unit_name(&inst.name))
        .no_proxy()
        .run_and_exit()?;
    Ok(())
}

pub fn logs(options: &control::Logs) -> anyhow::Result<()> {
    let name = match options.instance_opts.instance()? {
        InstanceName::Local(name) => name,
        InstanceName::Cloud { .. } => {
            print::error!("This operation is not yet supported on {BRANDING_CLOUD} instances.");
            return Err(ExitCode::new(1))?;
        }
    };
    if detect_systemd(&name) {
        let mut cmd = process::Native::new("logs", "journalctl", "journalctl");
        cmd.arg("--user-unit").arg(unit_name(&name));
        if let Some(n) = options.tail {
            cmd.arg(format!("--lines={n}"));
        }
        if options.follow {
            cmd.arg("--follow");
        }
        cmd.no_proxy().run()
    } else {
        let mut cmd = process::Native::new("log", "tail", "tail");
        if let Some(n) = options.tail {
            cmd.arg("-n").arg(n.to_string());
        }
        if options.follow {
            cmd.arg("-F");
        }
        cmd.arg(log_file(&name)?);
        cmd.no_proxy().run()
    }
}
