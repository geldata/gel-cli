use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fn_error_context::context;
use fs_err as fs;

use crate::branding::{BRANDING, BRANDING_CLI_CMD};
use crate::cli::install::{get_rc_files, no_dir_in_path};
use crate::commands::ExitCode;
use crate::credentials;
use crate::platform::binary_path;
use crate::platform::{config_dir, home_dir, symlink_dir, tmp_file_path};
use crate::portable::project;
use crate::print;
use crate::print_markdown;
use crate::question;

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    /// Dry run: do not actually move anything
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Dry run: do not actually move anything (with increased verbosity)
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Clone, Debug)]
enum ConfirmOverwrite {
    Yes,
    Skip,
    Quit,
}

pub fn run(cmd: &Command) -> anyhow::Result<()> {
    let base = home_dir()?.join(".edgedb");
    if base.exists() {
        migrate(&base, cmd.dry_run)
    } else {
        log::warn!(
            "Directory {:?} does not exist. No actions will be taken.",
            base
        );
        Ok(())
    }
}

fn file_is_non_empty(path: &Path) -> anyhow::Result<bool> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.len() > 0),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn dir_is_non_empty(path: &Path) -> anyhow::Result<bool> {
    match fs::read_dir(path) {
        Ok(mut dir) => Ok(dir.next().is_some()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn move_file(src: &Path, dest: &Path, dry_run: bool) -> anyhow::Result<()> {
    use ConfirmOverwrite::*;

    if file_is_non_empty(dest)? {
        if dry_run {
            log::warn!(
                "File {:?} exists in both locations, \
                        will prompt for overwrite",
                dest
            );
            return Ok(());
        }
        let mut q = question::Choice::new(format!(
            "Attempting to move {src:?} -> {dest:?}, but \
            destination file exists. Do you want to overwrite?"
        ));
        q.option(Yes, &["y"], "overwrite the destination file");
        q.option(Skip, &["s"], "skip, keep destination file, remove source");
        q.option(Quit, &["q"], "quit now without overwriting");
        match q.ask()? {
            Yes => {}
            Skip => return Ok(()),
            Quit => anyhow::bail!("Canceled by user"),
        }
    } else if dry_run {
        log::info!("Would move {:?} -> {:?}", src, dest);
        return Ok(());
    }
    let tmp = tmp_file_path(dest);
    fs::copy(src, &tmp)?;
    fs::rename(tmp, dest)?;
    fs::remove_file(src)?;
    Ok(())
}

fn move_dir(src: &Path, dest: &Path, dry_run: bool) -> anyhow::Result<()> {
    use ConfirmOverwrite::*;

    if dir_is_non_empty(dest)? {
        if dry_run {
            log::warn!(
                "Directory {:?} exists in both locations, \
                        will prompt for overwrite",
                dest
            );
            return Ok(());
        }
        let mut q = question::Choice::new(format!(
            "Attempting to move {src:?} -> {dest:?}, but \
            destination directory exists. Do you want to overwrite?"
        ));
        q.option(Yes, &["y"], "overwrite the destination dir");
        q.option(Skip, &["s"], "skip, keep destination dir, remove source");
        q.option(Quit, &["q"], "quit now without overwriting");
        match q.ask()? {
            Yes => {}
            Skip => return Ok(()),
            Quit => anyhow::bail!("Canceled by user"),
        }
    } else if dry_run {
        log::info!("Would move {:?} -> {:?}", src, dest);
        return Ok(());
    }
    fs::create_dir_all(dest)?;
    for item in fs::read_dir(src)? {
        let item = item?;
        let dest_path = &dest.join(item.file_name());
        match item.file_type()? {
            typ if typ.is_file() => {
                let tmp = tmp_file_path(dest_path);
                fs::copy(item.path(), &tmp)?;
                fs::rename(&tmp, dest_path)?;
            }
            #[cfg(unix)]
            typ if typ.is_symlink() => {
                let path = fs::read_link(item.path())?;
                symlink_dir(path, dest_path)
                    .map_err(|e| {
                        log::info!("Error symlinking project at {:?}: {}", dest_path, e);
                    })
                    .ok();
            }
            _ => {
                log::warn!("Skipping {:?} of unexpected type", item.path());
            }
        }
    }
    fs::remove_dir_all(src)?;
    Ok(())
}

fn try_move_bin(exe_path: &Path, bin_path: &Path) -> anyhow::Result<()> {
    let bin_dir = bin_path.parent().unwrap();
    if !bin_dir.exists() {
        fs::create_dir_all(bin_dir)?;
    }
    fs::rename(exe_path, bin_path)?;
    Ok(())
}

#[context("error updating {:?}", path)]
fn replace_line(path: &PathBuf, old_line: &str, new_line: &str) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path).context("cannot read file")?;
    if let Some(idx) = text.find(old_line) {
        log::info!("File {:?} contains old path, replacing", path);
        let mut file = fs::File::create(path)?;
        file.write_all(text[..idx].as_bytes())?;
        file.write_all(new_line.as_bytes())?;
        file.write_all(text[idx + old_line.len()..].as_bytes())?;
        Ok(true)
    } else {
        log::info!("File {:?} has no old path, skipping", path);
        return Ok(false);
    }
}

fn update_path(base: &Path, new_bin_path: &Path) -> anyhow::Result<()> {
    log::info!("Updating PATH");
    let old_bin_dir = base.join("bin");
    let new_bin_dir = new_bin_path.parent().unwrap();
    #[cfg(windows)]
    {
        use std::env::join_paths;

        let mut modified = false;
        crate::cli::install::windows_augment_path(|orig_path| {
            if orig_path.iter().any(|p| p == new_bin_dir) {
                return None;
            }
            Some(
                join_paths(orig_path.iter().map(|x| {
                    if x == &old_bin_dir {
                        modified = true;
                        new_bin_dir
                    } else {
                        x.as_ref()
                    }
                }))
                .expect("paths can be joined"),
            )
        })?;
        if modified && no_dir_in_path(&new_bin_dir) {
            print::success!("The `{BRANDING_CLI_CMD}` executable has moved!");
            print_markdown!(
                "\
                \n\
                `${dir}` has been added to your `PATH`.\n\
                You may need to reopen the terminal for this change to\n\
                take effect, and for the `${cmd}` command to become\n\
                available.\
                ",
                cmd = BRANDING_CLI_CMD,
                dir = new_bin_dir.display(),
            );
        }
    }
    if cfg!(unix) {
        let rc_files = get_rc_files()?;
        let old_line = format!("\nexport PATH=\"{}:$PATH\"\n", old_bin_dir.display(),);
        let new_line = format!("\nexport PATH=\"{}:$PATH\"\n", new_bin_dir.display(),);
        let mut modified = false;
        for path in &rc_files {
            if replace_line(path, &old_line, &new_line)? {
                modified = true;
            }
        }

        let cfg_dir = config_dir()?;
        let env_file = cfg_dir.join("env");

        fs::create_dir_all(&cfg_dir).with_context(|| format!("failed to create {cfg_dir:?}"))?;
        fs::write(&env_file, new_line + "\n")
            .with_context(|| format!("failed to write env file {env_file:?}"))?;

        if modified && no_dir_in_path(new_bin_dir) {
            print::success!("The `{BRANDING_CLI_CMD}` executable has moved!");
            print_markdown!(
                "\
                \n\
                Your shell profile has been updated to have ${dir} in your\n\
                `PATH`. The next time you open the terminal\n\
                it will be configured automatically.\n\
                \n\
                For this session please run:\n\
                ```\n\
                    source \"${env_path}\"\n\
                ```\n\
                Depending on your shell type you might also need \
                to run `rehash`.\
                ",
                dir = new_bin_dir.display(),
                env_path = env_file.display(),
            );
        }
    }
    Ok(())
}

pub fn migrate(base: &Path, dry_run: bool) -> anyhow::Result<()> {
    if let Ok(exe_path) = env::current_exe() {
        if exe_path.starts_with(base) {
            let new_bin_path = binary_path()?;
            try_move_bin(&exe_path, &new_bin_path).inspect_err(|_| {
                print::error!("Cannot move executable to new location.");
                eprintln!("  Try `{BRANDING_CLI_CMD} cli upgrade` instead.");
            })?;
            update_path(base, &new_bin_path)?;
        }
    }

    let source = base.join("credentials");
    let target = credentials::base_dir()?;
    if source.exists() {
        if !dry_run {
            fs::create_dir_all(&target)?;
        }
        for item in fs::read_dir(&source)? {
            let item = item?;
            move_file(&item.path(), &target.join(item.file_name()), dry_run)?;
        }
        if !dry_run {
            fs::remove_dir(&source)
                .map_err(|e| log::warn!("Cannot remove {:?}: {}", source, e))
                .ok();
        }
    }

    let source = base.join("projects");
    let target = project::stash_base()?;
    if source.exists() {
        if !dry_run {
            fs::create_dir_all(&target)?;
        }
        for item in fs::read_dir(&source)? {
            let item = item?;
            if item.metadata()?.is_dir() {
                move_dir(&item.path(), &target.join(item.file_name()), dry_run)?;
            }
        }
        if !dry_run {
            fs::remove_dir(&source)
                .map_err(|e| log::warn!("Cannot remove {:?}: {}", source, e))
                .ok();
        }
    }

    let source = base.join("config");
    let target = config_dir()?;
    if source.exists() {
        if !dry_run {
            fs::create_dir_all(&target)?;
        }
        for item in fs::read_dir(&source)? {
            let item = item?;
            move_file(&item.path(), &target.join(item.file_name()), dry_run)?;
        }
        if !dry_run {
            fs::remove_dir(&source)
                .map_err(|e| log::warn!("Cannot remove {:?}: {}", source, e))
                .ok();
        }
    }

    remove_file(&base.join("env"), dry_run)?;
    remove_dir_all(&base.join("bin"), dry_run)?;
    remove_dir_all(&base.join("run"), dry_run)?;
    remove_dir_all(&base.join("logs"), dry_run)?;
    remove_dir_all(&base.join("cache"), dry_run)?;

    if !dry_run && dir_is_non_empty(base)? {
        eprintln!(
            "\
            Directory {base:?} is no longer used by {BRANDING} tools and must be \
            removed to finish migration, but some files or directories \
            remain after all known files have moved. \
            The files may have been left by a third party tool. \
        "
        );
        let q = question::Confirm::new(format!(
            "Do you want to remove all files and directories within {base:?}?",
        ));
        if !q.ask()? {
            print::error!("Canceled by user.");
            print_markdown!(
                "\
                Once all files are backed up, run one of:\n\
                ```\n\
                rm -rf ~/.edgedb\n\
                ${cmd} cli migrate\n\
                ```\
            ",
                cmd = BRANDING_CLI_CMD
            );
            return Err(ExitCode::new(2).into());
        }
    }
    remove_dir_all(base, dry_run)?;
    print::success!("Directory layout migration successful!");

    Ok(())
}

fn remove_file(path: &Path, dry_run: bool) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if dry_run {
        log::info!("Would remove {:?}", path);
        return Ok(());
    }
    log::info!("Removing {:?}", path);
    fs::remove_file(path)?;
    Ok(())
}

fn remove_dir_all(path: &Path, dry_run: bool) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if dry_run {
        log::info!("Would remove dir {:?} recursively", path);
        return Ok(());
    }
    log::info!("Removing dir {:?}", path);
    fs::remove_dir_all(path)?;
    Ok(())
}
