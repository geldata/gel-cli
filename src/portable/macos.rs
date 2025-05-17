use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time;

use fn_error_context::context;
use gel_tokio::InstanceName;

use crate::branding::BRANDING;
use crate::commands::ExitCode;
use crate::platform::{current_exe, detect_ipv6};
use crate::platform::{data_dir, get_current_uid, home_dir};
use crate::portable::instance::control;
use crate::portable::instance::status::Service;
use crate::portable::local::{InstanceInfo, log_file, runstate_dir};
use crate::print::{self, Highlight, msg};
use crate::process;

enum Status {
    Ready,
    Running { pid: u32 },
    Failed { exit_code: Option<u16> },
    Inactive { error: String },
    NotLoaded,
}

pub fn plist_dir() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?.join("Library/LaunchAgents"))
}

fn plist_name(name: &str) -> String {
    format!("com.edgedb.edgedb-server-{name}.plist")
}

fn plist_path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(plist_dir()?.join(plist_name(name)))
}

fn get_domain_target() -> String {
    format!("gui/{}", get_current_uid())
}

fn launchd_name(name: &str) -> String {
    format!("{}/edgedb-server-{}", get_domain_target(), name)
}

pub fn service_files(name: &str) -> anyhow::Result<Vec<PathBuf>> {
    Ok(vec![plist_path(name)?])
}

pub fn create_service(info: &InstanceInfo) -> anyhow::Result<()> {
    // bootout on upgrade
    if is_service_loaded(&info.name) {
        bootout(&info.name)?;
    }

    _create_service(info)
}

#[context("cannot compose plist file")]
fn plist_data(name: &str, info: &InstanceInfo) -> anyhow::Result<String> {
    let sockets = if info.get_version()?.specific().major >= 2 {
        format!(
            r###"
            <key>Sockets</key>
            <dict>
              <key>edgedb-server</key>
              <array>
                <dict>
                  <key>SockNodeName</key><string>127.0.0.1</string>
                  <key>SockServiceName</key><string>{port}</string>
                  <key>SockType</key><string>stream</string>
                  <key>SockFamily</key><string>IPv4</string>
                </dict>
                {ipv6_listen}
              </array>
            </dict>
            "###,
            port = info.port,
            ipv6_listen = if detect_ipv6() {
                format!(
                    "<dict>
                      <key>SockNodeName</key><string>::1</string>
                      <key>SockServiceName</key><string>{port}</string>
                      <key>SockType</key><string>stream</string>
                      <key>SockFamily</key><string>IPv6</string>
                    </dict>
                ",
                    port = info.port
                )
            } else {
                String::new()
            },
        )
    } else {
        "".into()
    };
    Ok(format!(
        r###"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN"
        "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>edgedb-server-{instance_name}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{executable}</string>
        <string>instance</string>
        <string>start</string>
        <string>--instance={instance_name}</string>
        <string>--managed-by=launchctl</string>
    </array>

    <key>StandardOutPath</key>
    <string>{log_path}</string>
    <key>StandardErrorPath</key>
    <string>{log_path}</string>

    <key>KeepAlive</key>
    <dict>
         <key>SuccessfulExit</key>
         <false/>
    </dict>

    <key>LSBackgroundOnly</key>
    <true/>

    {sockets}

</dict>
</plist>
"###,
        instance_name = name,
        executable = current_exe()?.display(),
        log_path = log_file(name)?.display(),
    ))
}

fn _create_service(info: &InstanceInfo) -> anyhow::Result<()> {
    let name = &info.name;

    let plist_dir_path = plist_dir()?;
    fs::create_dir_all(&plist_dir_path)?;
    let plist_path = plist_dir_path.join(plist_name(name));
    let unit_name = launchd_name(name);
    fs::write(&plist_path, plist_data(name, info)?)?;
    if let Some(dir) = runstate_dir(name)?.parent() {
        fs::create_dir_all(dir)?;
    }

    // Clear the disabled status of the unit name, in case the user disabled
    // a service with the same name some time ago and it's likely forgotten
    // because the user is now creating a new service with the same name.
    // This doesn't make the service auto-starting, because we're "hiding" the
    // plist file from launchd if the service is configured as manual start.
    // Actually it is necessary to clear the disabled status even for manually-
    // starting services, because manual start won't work on disabled services.
    process::Native::new("create service", "launchctl", "launchctl")
        .arg("enable")
        .arg(&unit_name)
        .run()?;
    process::Native::new("create service", "launchctl", "launchctl")
        .arg("bootstrap")
        .arg(get_domain_target())
        .arg(plist_path)
        .run()?;

    Ok(())
}

fn bootout(name: &str) -> anyhow::Result<()> {
    let unit_name = launchd_name(name);
    let status = process::Native::new("remove service", "launchctl", "launchctl")
        .arg("bootout")
        .arg(&unit_name)
        .status_only()?;
    if !status.success() && status.code() != Some(36) {
        // MacOS Catalina has a bug of returning:
        //   Boot-out failed: 36: Operation now in progress
        // when process has successfully booted out
        anyhow::bail!("launchctl bootout failed: {}", status)
    }
    let deadline = time::Instant::now() + time::Duration::from_secs(30);
    while is_service_loaded(name) {
        if time::Instant::now() > deadline {
            anyhow::bail!(
                "launchctl bootout timed out in 30 seconds: \
                 service is still loaded"
            )
        }
        thread::sleep(time::Duration::from_secs_f32(0.3));
    }
    Ok(())
}

pub fn is_service_loaded(name: &str) -> bool {
    !matches!(_service_status(name), Status::NotLoaded)
}

pub fn service_status(name: &str) -> Service {
    match _service_status(name) {
        Status::Ready => Service::Ready,
        Status::Running { pid } => Service::Running { pid },
        Status::Failed { exit_code } => Service::Failed { exit_code },
        Status::Inactive { error } => Service::Inactive { error },
        Status::NotLoaded => Service::Inactive {
            error: "Service is not loaded".into(),
        },
    }
}

fn _service_status(name: &str) -> Status {
    use Status::*;

    let list = process::Native::new("service info", "launchctl", "launchctl")
        .arg("print")
        .arg(launchd_name(name))
        .get_output();
    let output = match list {
        Ok(output) => output,
        Err(e) => {
            return Inactive {
                error: format!("cannot determine service status: {e:#}"),
            };
        }
    };
    if !output.status.success() {
        log::debug!(
            "`launchctl print {}` errored out with {:?}. \
                      Assuming service is not loaded.",
            launchd_name(name),
            output.stderr
        );
        return NotLoaded;
    }
    let mut pid: Option<u32> = None;
    let mut exit_code: Option<u16> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut iter = line.splitn(2, '=');
        let pair = iter.next().zip(iter.next());
        match pair.map(|(k, v)| (k.trim(), v.trim())) {
            Some(("pid", value)) => match value.parse() {
                Ok(value) => pid = Some(value),
                Err(_) => {
                    log::warn!("launchctl returned invalid pid: {}", value);
                }
            },
            Some(("state", "waiting")) => {
                return Status::Ready;
            }
            Some(("last exit code", value)) => {
                if let Ok(value) = value.parse() {
                    exit_code = Some(value)
                } else {
                    // assuming "(never exited)"
                }
            }
            _ => {}
        }
    }
    if let Some(pid) = pid {
        return Running { pid };
    }
    if exit_code.is_some() && exit_code != Some(0) {
        return Failed { exit_code };
    }
    Inactive {
        error: "no pid found".into(),
    }
}

#[context("cannot stop and disable service")]
pub fn stop_and_disable(name: &str) -> anyhow::Result<bool> {
    if is_service_loaded(name) {
        // bootout will fail if the service is not loaded (e.g. manually-
        // starting services that never started after reboot), also it's
        // unnecessary to unload the service if it wasn't loaded.
        log::info!("Unloading service");
        bootout(name)?;
    }

    let mut found = false;
    let unit_path = plist_path(name)?;
    if unit_path.exists() {
        found = true;
        log::info!("Removing unit file {}", unit_path.display());
        fs::remove_file(unit_path)?;
    }

    // Clear the runstate dir of socket files and symlinks - macOS wouldn't
    // delete UNIX domain socket files after server shutdown, which may lead to
    // issues in upgrades
    #[cfg(unix)]
    for entry in fs::read_dir(runstate_dir(name)?)? {
        use std::os::unix::fs::FileTypeExt;
        let entry = entry?;
        let path = entry.path();

        if let Ok(metadata) = path.metadata() {
            if metadata.file_type().is_socket() || metadata.file_type().is_symlink() {
                fs::remove_file(&path)?;
            }
        }
    }

    Ok(found)
}

pub fn server_cmd(
    inst: &InstanceInfo,
    is_shutdown_supported: bool,
) -> anyhow::Result<process::Native> {
    let data_dir = data_dir()?.join(&inst.name);
    let runstate_dir = runstate_dir(&inst.name)?;
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
    pro.arg("--runstate-dir").arg(runstate_dir);
    pro.arg("--port").arg(inst.port.to_string());
    if inst.get_version()?.specific().major >= 2 {
        pro.arg("--compiler-pool-mode=on_demand");
        pro.arg("--admin-ui=enabled");
        if is_shutdown_supported {
            pro.arg("--auto-shutdown-after=600");
        }
    }
    pro.no_proxy();
    Ok(pro)
}

pub fn detect_launchd() -> bool {
    let path = if let Ok(path) = which::which("launchctl") {
        path
    } else {
        return false;
    };
    let out = process::Native::new("detect launchd", "launchctl", path)
        .arg("print-disabled") // Faster than bare print
        .arg(get_domain_target())
        .get_output();
    match out {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            log::info!(
                "detecting launchd session: {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
            false
        }
        Err(e) => {
            log::info!("detecting launchd session: {:#}", e);
            false
        }
    }
}

pub fn start_service(inst: &InstanceInfo) -> anyhow::Result<()> {
    if is_service_loaded(&inst.name) {
        // For auto-starting services, we assume they are already loaded.
        // If the server is already running, kickstart won't do anything;
        // or else it will try to (re-)start the server.
        let lname = launchd_name(&inst.name);
        process::Native::new("launchctl", "launchctl", "launchctl")
            .arg("kickstart")
            .arg(&lname)
            .run()?;
        wait_started(&inst.name)?;
    } else {
        _create_service(inst)?;
    }
    Ok(())
}

fn wait_started(name: &str) -> anyhow::Result<()> {
    use Service::*;

    let cut_off = time::SystemTime::now() + time::Duration::from_secs(30);
    loop {
        let service = service_status(name);
        match service {
            Inactive { .. } | Ready => {
                thread::sleep(time::Duration::from_millis(30));
                if time::SystemTime::now() > cut_off {
                    print::error!("{BRANDING} failed to start for 30 seconds");
                    break;
                }
                continue;
            }
            Running { .. } => {
                return Ok(());
            }
            Failed {
                exit_code: Some(code),
            } => {
                msg!(
                    "{} {} with exit code {}",
                    print::err_marker(),
                    "{BRANDING} failed".emphasized(),
                    code
                );
            }
            Failed { exit_code: None } => {
                msg!(
                    "{} {} {}",
                    print::err_marker(),
                    BRANDING,
                    "failed".emphasized()
                );
            }
        }
    }
    println!("--- Last 10 log lines ---");
    let mut cmd = process::Native::new("log", "tail", "tail");
    cmd.arg("-n").arg("10");
    cmd.arg(log_file(name)?);
    cmd.no_proxy()
        .run()
        .map_err(|e| log::warn!("Cannot show log: {}", e))
        .ok();
    println!("--- End of log ---");
    anyhow::bail!("Failed to start {BRANDING}");
}

pub fn stop_service(name: &str) -> anyhow::Result<()> {
    stop_and_disable(name)?;
    Ok(())
}

pub fn restart_service(inst: &InstanceInfo) -> anyhow::Result<()> {
    if is_service_loaded(&inst.name) {
        // Only use kickstart -k to restart the service if it's loaded
        // already, or it will fail with an error. We assume the service is
        // loaded for auto-starting services.
        process::Native::new("launchctl", "launchctl", "launchctl")
            .arg("kickstart")
            .arg("-k")
            .arg(launchd_name(&inst.name))
            .run()?;
        wait_started(&inst.name)?;
    } else {
        _create_service(inst)?;
    }
    Ok(())
}

pub fn external_status(inst: &InstanceInfo) -> anyhow::Result<()> {
    if is_service_loaded(&inst.name) {
        process::Native::new("service status", "launchctl", "launchctl")
            .arg("print")
            .arg(launchd_name(&inst.name))
            .no_proxy()
            .run_and_exit()?;
    } else {
        // launchctl print will fail if the service is not loaded, let's
        // just give a more understandable error here.
        log::error!("Service is not loaded");
        return Err(ExitCode::new(1).into());
    }
    Ok(())
}

pub fn logs(options: &control::Logs) -> anyhow::Result<()> {
    let name = match options.instance_opts.instance()? {
        InstanceName::Local(name) => name,
        InstanceName::Cloud { .. } => todo!(),
    };
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
