use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fn_error_context::context;
use gel_cli_derive::IntoArgs;
use gel_tokio::InstanceName;

use crate::branding::{BRANDING, BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::bug;
use crate::commands::ExitCode;
use crate::credentials;
use crate::hint::HintExt;
use crate::locking::LockManager;
use crate::options::{InstanceOptions, InstanceOptionsLegacy};
use crate::platform::current_exe;
use crate::portable::local::{InstanceInfo, lock_file, open_lock, runstate_dir};
use crate::portable::ver;
use crate::portable::{linux, macos, windows};
use crate::print;
use crate::process;

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Start {
    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Start server in the foreground.
    #[arg(long)]
    #[cfg_attr(
        target_os = "linux",
        arg(help = "Start the server in the foreground rather than using \
                  systemd to manage the process (note: you might need to \
                  stop the non-foreground instance first)")
    )]
    #[cfg_attr(
        target_os = "macos",
        arg(help = "Start the server in the foreground rather than using \
                  launchctl to manage the process (note: you might need to \
                  stop the non-foreground instance first)")
    )]
    pub foreground: bool,

    /// With `--foreground`, stops server running in the background; also restarts
    /// the service on exit.
    #[arg(long, conflicts_with = "managed_by")]
    pub auto_restart: bool,

    /// Indicate whether managed by edgedb-cli, systemd, launchctl, or None.
    #[arg(long, hide = true)]
    #[arg(value_parser=["systemd", "launchctl", "edgedb-cli"])]
    #[arg(conflicts_with = "auto_restart")]
    pub managed_by: Option<String>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Stop {
    /// Name of instance to stop.
    #[arg(hide = true)]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    pub name: Option<InstanceName>,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Restart {
    /// Name of instance to restart.
    #[arg(hide = true)]
    #[arg(value_hint=clap::ValueHint::Other)]
    pub name: Option<InstanceName>,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Logs {
    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Number of lines to show.
    #[arg(short = 'n', long)]
    pub tail: Option<usize>,

    /// Show log tail and continue watching for new entries.
    #[arg(short = 'f', long)]
    pub follow: bool,
}

fn supervisor_start(inst: &InstanceInfo) -> anyhow::Result<()> {
    if cfg!(windows) {
        windows::start_service(&inst.name)
    } else if cfg!(target_os = "macos") {
        macos::start_service(inst)
    } else if cfg!(target_os = "linux") {
        linux::start_service(&inst.name)
    } else {
        anyhow::bail!("unsupported platform");
    }
}

fn daemon_start(instance: &str) -> anyhow::Result<()> {
    if cfg!(windows) {
        windows::daemon_start(instance)
    } else {
        let lock = open_lock(instance)?;
        if lock.try_read().is_err() {
            // properly running
            log::info!("Instance {:?} is already running", instance);
            return Ok(());
        }
        process::Native::new("edgedb cli", "edgedb-cli", current_exe()?)
            .arg("instance")
            .arg("start")
            .arg("-I")
            .arg(instance)
            .arg("--managed-by=edgedb-cli")
            .daemonize_with_stdout()?;
        Ok(())
    }
}

pub fn do_start(inst: &InstanceInfo) -> anyhow::Result<()> {
    if !credentials::exists(&inst.instance_name)? {
        log::warn!(
            "No corresponding credentials file exists for {:#}. \
                    Use `{BRANDING_CLI_CMD} instance reset-password -I {}` to create one.",
            inst.instance_name,
            inst.instance_name
        );
    }
    if detect_supervisor(&inst.name) {
        supervisor_start(inst)
    } else {
        daemon_start(&inst.name)
    }
}

pub fn get_server_cmd(
    inst: &InstanceInfo,
    is_shutdown_supported: bool,
) -> anyhow::Result<process::Native> {
    if cfg!(windows) {
        windows::server_cmd(&inst.name, is_shutdown_supported)
    } else if cfg!(target_os = "macos") {
        macos::server_cmd(inst, is_shutdown_supported)
    } else if cfg!(target_os = "linux") {
        linux::server_cmd(inst, is_shutdown_supported)
    } else {
        anyhow::bail!("unsupported platform");
    }
}

pub fn ensure_runstate_dir(name: &str) -> anyhow::Result<PathBuf> {
    let runstate_dir = runstate_dir(name)?;
    match fs::create_dir_all(&runstate_dir) {
        Ok(()) => Ok(runstate_dir),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && cfg!(unix) => {
            Err(anyhow::Error::new(e)
                .context(format!("failed to create runstate dir {runstate_dir:?}"))
                .hint(
                    "This may mean that `XDG_RUNTIME_DIR` \
                            is inherited from another user's environment. \
                            Run `unset XDG_RUNTIME_DIR` or use a better login-as-user \
                            tool (use `sudo` instead of `su`).",
                )
                .into())
        }
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("failed to create runstate dir {runstate_dir:?}"))),
    }
}

#[context("cannot write lock metadata at {:?}", path)]
fn write_lock_info(
    path: &Path,
    lock: &mut fs::File,
    marker: &Option<String>,
) -> anyhow::Result<()> {
    use std::io::Write;

    lock.set_len(0)?;
    lock.write_all(marker.as_deref().unwrap_or("user").as_bytes())?;
    Ok(())
}

pub fn detect_supervisor(name: &str) -> bool {
    if cfg!(windows) {
        false
    } else if cfg!(target_os = "macos") {
        macos::detect_launchd()
    } else if cfg!(target_os = "linux") {
        linux::detect_systemd(name)
    } else {
        false
    }
}

fn pid_file_path(instance: &str) -> anyhow::Result<PathBuf> {
    Ok(runstate_dir(instance)?.join("edgedb.pid"))
}

#[cfg(unix)]
fn run_server_by_cli(meta: &InstanceInfo) -> anyhow::Result<()> {
    use crate::portable::local::log_file;
    use std::future::pending;
    use std::os::unix::io::AsRawFd;
    use tokio::net::UnixDatagram;

    unsafe { libc::setsid() };

    let pid_path = pid_file_path(&meta.name)?;
    let log_path = log_file(&meta.name)?;
    if let Some(dir) = log_path.parent() {
        fs_err::create_dir_all(dir)?;
    }
    let log_file = fs_err::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&log_path)?;
    let null = fs_err::OpenOptions::new().write(true).open("/dev/null")?;
    let notify_socket = runstate_dir(&meta.name)?.join(".s.daemon");
    if notify_socket.exists() {
        fs_err::remove_file(&notify_socket)?;
    }
    if let Some(dir) = notify_socket.parent() {
        fs_err::create_dir_all(dir)?;
    }
    get_server_cmd(meta, false)?
        .env("NOTIFY_SOCKET", &notify_socket)
        .pid_file(&pid_path)
        .log_file(&log_path)?
        .background_for(|| {
            // this is not async, but requires async context
            let sock = UnixDatagram::bind(&notify_socket).context("cannot create notify socket")?;
            Ok(async move {
                let mut buf = [0u8; 1024];
                while !matches!(sock.recv(&mut buf).await,
                               Ok(len) if &buf[..len] == b"READY=1")
                {}

                // Redirect stderr to log file, right before daemonizing.
                // So that all early errors are visible, but all later ones
                // (i.e. a message on term) do not clobber user's terminal.
                if unsafe { libc::dup2(log_file.as_raw_fd(), 2) } < 0 {
                    return Err(anyhow::Error::new(io::Error::last_os_error())
                        .context("cannot close stdout"));
                }
                drop(log_file);

                // Closing stdout to notify that daemon is successfully started.
                // Note: we can't just close the file descriptor as it will be
                // replaced with something unexpected on any next new file
                // descriptor creation. So we replace it with `/dev/null` (the
                // writing end of the original pipe is closed at this point).
                if unsafe { libc::dup2(null.as_raw_fd(), 1) } < 0 {
                    return Err(anyhow::Error::new(io::Error::last_os_error())
                        .context("cannot close stdout"));
                }
                drop(null);

                pending::<()>().await;
                Ok(())
            })
        })
}

#[cfg(windows)]
fn run_server_by_cli(_meta: &InstanceInfo) -> anyhow::Result<()> {
    anyhow::bail!("daemonizing is not yet supported for Windows");
}

#[cfg(unix)]
fn set_inheritable(file: &impl std::os::unix::io::AsRawFd) -> anyhow::Result<()> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    let flags = fcntl(file.as_raw_fd(), FcntlArg::F_GETFD).context("get FD flags")?;
    let flags = FdFlag::from_bits(flags).context("bad FD flags")?;
    fcntl(
        file.as_raw_fd(),
        FcntlArg::F_SETFD(flags & !FdFlag::FD_CLOEXEC),
    )
    .context("set FD flags")?;
    Ok(())
}

pub fn start(options: &Start) -> anyhow::Result<()> {
    let _lock = LockManager::lock_instance(&options.instance_opts.instance_allow_legacy()?)?;

    // Special case: instance name is allowed to be positional for start, because start
    // is used in systemd services and cannot be changed.
    // Maybe we should make "fixup" that updates those services?
    let name = match options.instance_opts.instance_allow_legacy()? {
        InstanceName::Local(name) => {
            if cfg!(windows) {
                return windows::start(options, &name);
            } else {
                name
            }
        }
        InstanceName::Cloud { .. } => {
            print::error!("Starting {BRANDING_CLOUD} instances is not yet supported.");
            return Err(ExitCode::new(1))?;
        }
    };
    let meta = InstanceInfo::read(&name)?;
    ensure_runstate_dir(&meta.name)?;
    if options.foreground || options.managed_by.is_some() {
        let lock_path = lock_file(&meta.name)?;
        let mut lock = open_lock(&meta.name)?;
        let mut needs_restart = false;
        let try_write = lock.try_write();
        let lock = if let Ok(mut lock) = try_write {
            write_lock_info(&lock_path, &mut lock, &options.managed_by)?;
            lock
        } else {
            drop(try_write);
            let locked_by = fs_err::read_to_string(&lock_path)
                .with_context(|| format!("cannot read lock file {lock_path:?}"))?;
            if options.managed_by.is_some() {
                log::warn!(
                    "Process is already running by {}. \
                            Waiting for that process to be stopped...",
                    locked_by.escape_default()
                );
            } else if options.auto_restart {
                log::warn!(
                    "Process is already running by {}. \
                            Stopping...",
                    locked_by.escape_default()
                );
                needs_restart = true;
                do_stop(&name).context("cannot stop service")?;
            } else {
                anyhow::bail!(
                    "Process is already running by {}. \
                    Please stop the service manually or run \
                    with `--auto-restart` option.",
                    locked_by.escape_default()
                );
            }
            let mut lock = lock.write()?;
            write_lock_info(&lock_path, &mut lock, &options.managed_by)?;
            lock
        };
        if matches!(options.managed_by.as_deref(), Some("edgedb-cli")) {
            debug_assert!(!needs_restart);
            run_server_by_cli(&meta)
        } else {
            #[cfg(unix)]
            if matches!(options.managed_by.as_deref(), Some("systemd" | "launchctl")) {
                use std::os::unix::io::AsRawFd;

                set_inheritable(&*lock).context("set inheritable for lock")?;
                get_server_cmd(&meta, true)?
                    .env_default("EDGEDB_SERVER_LOG_LEVEL", "info")
                    .env(
                        "EDGEDB_SERVER_EXTERNAL_LOCK_FD",
                        lock.as_raw_fd().to_string(),
                    )
                    .exec_replacing_self()?;
                drop(lock);
                unreachable!();
            }

            let pid_path = pid_file_path(&meta.name)?;
            #[allow(unused_mut)]
            let mut res = get_server_cmd(&meta, false)?
                .env_default("EDGEDB_SERVER_LOG_LEVEL", "info")
                .pid_file(&pid_path)
                .no_proxy()
                .run();

            drop(lock);

            // On macos we send SIGTERM to stop the service
            // And to convince launchctl not to start service we must return
            // exit code of zero
            #[cfg(target_os = "macos")]
            if let Err(err) = &res {
                if let Some(exit) = err.downcast_ref::<ExitCode>() {
                    if exit.code() == 128 + 15 {
                        // Sigterm exit code
                        res = Err(ExitCode::new(0).into());
                    }
                }
            }

            if needs_restart {
                log::warn!("Restarting service in background...");
                do_start(&meta)
                    .map_err(|e| {
                        log::warn!("Error starting service: {}", e);
                    })
                    .ok();
            }
            Ok(res?)
        }
    } else {
        do_start(&meta)
    }
}

fn supervisor_stop(name: &str) -> anyhow::Result<()> {
    if cfg!(windows) {
        windows::stop_service(name)
    } else if cfg!(target_os = "macos") {
        macos::stop_service(name)
    } else if cfg!(target_os = "linux") {
        linux::stop_service(name)
    } else {
        anyhow::bail!("unsupported platform");
    }
}

pub fn read_pid(instance: &str) -> anyhow::Result<Option<u32>> {
    let pid_path = pid_file_path(instance)?;
    match fs_err::read_to_string(&pid_path) {
        Ok(pid_str) => {
            let pid = pid_str
                .trim()
                .parse()
                .with_context(|| format!("cannot parse pid file {pid_path:?}"))?;
            Ok(Some(pid))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context(format!("cannot read pid file {pid_path:?}"))?,
    }
}

fn is_run_by_supervisor(lock: fd_lock::RwLock<fs::File>) -> bool {
    let mut buf = String::with_capacity(100);
    if lock.into_inner().read_to_string(&mut buf).is_err() {
        return false;
    }
    log::debug!("Service running by {:?}", buf);
    match &buf[..] {
        "systemd" if cfg!(target_os = "linux") => true,
        "launchctl" if cfg!(target_os = "macos") => true,
        _ => false,
    }
}

pub fn do_stop(name: &str) -> anyhow::Result<()> {
    let lock = open_lock(name)?;
    let supervisor = detect_supervisor(name);
    if lock.try_read().is_err() {
        // properly running
        if supervisor && is_run_by_supervisor(lock) {
            supervisor_stop(name)
        } else if let Some(pid) = read_pid(name)? {
            log::info!("Stopping {BRANDING} with pid {}", pid);
            process::term(pid)?;
            // wait for unlock
            let _ = open_lock(name)?
                .read()
                .context("cannot acquire read lock")?;
            Ok(())
        } else {
            Err(bug::error("cannot find pid"))
        }
    } else {
        // probably not running
        if supervisor {
            supervisor_stop(name)
        } else {
            if let Some(pid) = read_pid(name)? {
                log::info!("Stopping {BRANDING} with pid {}", pid);
                process::term(pid)?;
                // wait for unlock
                let _ = open_lock(name)?.read()?;
            } // nothing to do
            Ok(())
        }
    }
}

pub fn stop(options: &Stop) -> anyhow::Result<()> {
    let name = match options.instance_opts.instance()? {
        InstanceName::Local(name) => {
            if cfg!(windows) {
                return windows::stop(options, &name);
            } else {
                name
            }
        }
        InstanceName::Cloud { .. } => {
            print::error!("Stopping {BRANDING_CLOUD} instances is not yet supported.");
            return Err(ExitCode::new(1))?;
        }
    };
    let meta = InstanceInfo::read(&name)?;
    do_stop(&meta.name)
}

fn supervisor_stop_and_disable(instance: &str) -> anyhow::Result<bool> {
    if cfg!(target_os = "macos") {
        macos::stop_and_disable(instance)
    } else if cfg!(target_os = "linux") {
        linux::stop_and_disable(instance)
    } else if cfg!(windows) {
        windows::stop_and_disable(instance)
    } else {
        anyhow::bail!("service is not supported on the platform");
    }
}

pub fn stop_and_disable(instance: &str) -> anyhow::Result<bool> {
    let lock_path = lock_file(instance)?;
    let supervisor = detect_supervisor(instance);
    if lock_path.exists() {
        let lock = open_lock(instance)?;
        if lock.try_read().is_err() {
            // properly running
            if !supervisor || !is_run_by_supervisor(lock) {
                if let Some(pid) = read_pid(instance)? {
                    log::info!("Stopping {BRANDING} with pid {}", pid);
                    process::term(pid)?;
                    // wait for unlock
                    let _ = open_lock(instance)?.read()?;
                }
            }
        }
    }
    if supervisor {
        supervisor_stop_and_disable(instance)
    } else {
        let dir = runstate_dir(instance)?;
        Ok(dir.exists())
    }
}

fn supervisor_restart(inst: &InstanceInfo) -> anyhow::Result<()> {
    if cfg!(windows) {
        windows::restart_service(inst)
    } else if cfg!(target_os = "macos") {
        macos::restart_service(inst)
    } else if cfg!(target_os = "linux") {
        linux::restart_service(inst)
    } else {
        anyhow::bail!("unsupported platform");
    }
}

pub fn do_restart(inst: &InstanceInfo) -> anyhow::Result<()> {
    let lock = open_lock(&inst.name)?;
    let supervisor = detect_supervisor(&inst.name);
    if lock.try_read().is_err() {
        // properly running
        if supervisor && is_run_by_supervisor(lock) {
            supervisor_restart(inst)
        } else {
            if let Some(pid) = read_pid(&inst.name)? {
                log::info!("Stopping {BRANDING} with pid {}", pid);
                process::term(pid)?;
                // wait for unlock
                let _ = open_lock(&inst.name)?.read()?;
            } else {
                return Err(bug::error("cannot find pid"));
            }
            if supervisor {
                supervisor_start(inst)
            } else {
                daemon_start(&inst.name)
            }
        }
    } else {
        // probably not running
        if supervisor {
            supervisor_restart(inst)
        } else {
            if let Some(pid) = read_pid(&inst.name)? {
                log::info!("Stopping {BRANDING} with pid {}", pid);
                process::term(pid)?;
                // wait for unlock
                let _ = lock.read()?;
            } // nothing to do
            // todo(tailhook) optimize supervisor detection
            if supervisor {
                supervisor_start(inst)
            } else {
                daemon_start(&inst.name)
            }
        }
    }
}

pub fn restart(cmd: &Restart, options: &crate::Options) -> anyhow::Result<()> {
    match cmd.instance_opts.instance()? {
        InstanceName::Local(name) => {
            let meta = InstanceInfo::read(&name)?;
            do_restart(&meta)
        }
        InstanceName::Cloud(name) => {
            crate::cloud::ops::restart_cloud_instance(&name, &options.cloud_options)
        }
    }
}

pub fn logs(cmd: &Logs, options: &crate::Options) -> anyhow::Result<()> {
    if let InstanceName::Cloud(name) = cmd.instance_opts.instance()? {
        crate::cloud::ops::logs_cloud_instance(&name, cmd.tail, &options.cloud_options)
    } else if cfg!(windows) {
        windows::logs(cmd)
    } else if cfg!(target_os = "macos") {
        macos::logs(cmd)
    } else if cfg!(target_os = "linux") {
        linux::logs(cmd)
    } else {
        anyhow::bail!("unsupported platform");
    }
}

pub fn self_signed_arg(cmd: &mut process::Native, ver: &ver::Build) {
    if ver.specific() > "1.0-rc.2".parse().unwrap() {
        cmd.arg("--tls-cert-mode=generate_self_signed");
    } else {
        cmd.arg("--generate-self-signed-cert");
    }
    if ver.specific().major >= 2 {
        cmd.arg("--jose-key-mode=generate");
    }
}
