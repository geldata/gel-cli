use std::fs;
use std::process;

use crate::cli::env::Env;
use anyhow::Context;

pub fn get_cli() -> anyhow::Result<&'static str> {
    if cfg!(target_arch = "x86_64") {
        if cfg!(target_os = "macos") {
            Ok("x86_64-apple-darwin")
        } else if cfg!(target_os = "linux") {
            return Ok("x86_64-unknown-linux-musl");
        } else if cfg!(windows) {
            return Ok("x86_64-pc-windows-msvc");
        } else {
            anyhow::bail!("unsupported OS on x86_64");
        }
    } else if cfg!(target_arch = "aarch64") {
        if cfg!(target_os = "macos") {
            return Ok("aarch64-apple-darwin");
        } else if cfg!(target_os = "linux") {
            return Ok("aarch64-unknown-linux-musl");
        } else if cfg!(windows) {
            return Ok("aarch64-pc-windows-msvc");
        } else {
            anyhow::bail!("unsupported OS on aarch64")
        }
    } else {
        anyhow::bail!("unsupported architecture");
    }
}

pub fn get_server() -> anyhow::Result<&'static str> {
    if cfg!(target_arch = "x86_64") {
        if cfg!(target_os = "macos") {
            Ok("x86_64-apple-darwin")
        } else if cfg!(target_os = "linux") {
            if is_musl()? {
                return Ok("x86_64-unknown-linux-musl");
            } else {
                return Ok("x86_64-unknown-linux-gnu");
            }
        } else if cfg!(windows) {
            // on windows use server version from linux
            // as we run server in WSL
            return Ok("x86_64-unknown-linux-gnu");
        } else {
            anyhow::bail!("unsupported OS on x86_64");
        }
    } else if cfg!(target_arch = "aarch64") {
        if cfg!(target_os = "macos") {
            return Ok("aarch64-apple-darwin");
        } else if cfg!(target_os = "linux") {
            if is_musl()? {
                return Ok("aarch64-unknown-linux-musl");
            } else {
                return Ok("aarch64-unknown-linux-gnu");
            }
        } else {
            anyhow::bail!("unsupported OS on aarch64")
        }
    } else {
        anyhow::bail!("unsupported architecture");
    }
}

pub fn is_musl() -> anyhow::Result<bool> {
    // Check `ldd --version` output for the string 'musl'.
    // This is what the rustup install script does so it is probably
    // good enough.
    let output = process::Command::new("ldd").arg("--version").output()?;
    // Combine stdout and stderr and look in both.
    // (musl puts it in stderr, but it bothers me to not look in stdout,
    // and also I think musl is arguably wrong to do that.)
    let mut full_output = output.stdout;
    full_output.extend(output.stderr);

    let output_string = String::from_utf8(full_output)?;

    Ok(output_string.contains("musl"))
}

fn docker_check() -> anyhow::Result<bool> {
    let cgroups =
        fs::read_to_string("/proc/self/cgroup").context("cannot read /proc/self/cgroup")?;
    for line in cgroups.lines() {
        let mut fields = line.split(':');
        if fields
            .nth(2)
            .map(|f| f.starts_with("/docker/"))
            .unwrap_or(false)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn optional_docker_check() -> anyhow::Result<bool> {
    use crate::cli::env::InstallInDocker;
    if cfg!(target_os = "linux") {
        match Env::install_in_docker()?.unwrap_or(InstallInDocker::Default) {
            InstallInDocker::Forbid | InstallInDocker::Default => {
                let result = docker_check()
                    .map_err(|e| {
                        log::warn!(
                            "Failed to check if running within \
                                   a container: {e:#}"
                        )
                    })
                    .unwrap_or(false);
                return Ok(result);
            }
            InstallInDocker::Allow => return Ok(false),
        };
    }
    Ok(false)
}

#[cfg(not(target_os = "macos"))]
pub fn is_arm64_hardware() -> bool {
    false
}

#[cfg(target_os = "macos")]
pub fn is_arm64_hardware() -> bool {
    let mut utsname = libc::utsname {
        sysname: [0; 256],
        nodename: [0; 256],
        release: [0; 256],
        version: [0; 256],
        machine: [0; 256],
    };
    if unsafe { libc::uname(&mut utsname) } == 1 {
        log::warn!("Cannot get uname: {}", std::io::Error::last_os_error());
        return false;
    }
    let machine: &[u8] = unsafe { std::mem::transmute(&utsname.machine[..]) };
    let mend: usize = machine.iter().position(|&b| b == 0).unwrap_or(256);
    match std::str::from_utf8(&machine[..mend]) {
        Ok(machine) => {
            log::debug!("Architecture {:?}", machine);

            // uname returns the emulated architecture
            if machine != "x86_64" {
                return false;
            }
        }
        Err(e) => {
            log::warn!("Cannot decode machine from uname: {}", e);
            return false;
        }
    }

    let mut result: libc::c_int = 0;
    let mut size: libc::size_t = std::mem::size_of_val(&result);
    let sname = std::ffi::CString::new("sysctl.proc_translated").expect("cstring can be created");
    let sysctl_result = unsafe {
        libc::sysctlbyname(
            sname.as_ptr(),
            &mut result as *mut libc::c_int as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sysctl_result == -1 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return false;
        }
        log::warn!("Cannot get sysctl.proc_translated: {:#}", err);
        return false;
    }
    log::debug!("Got sysctl.proc_translated: {:?}", result);
    return result != 0;
}
