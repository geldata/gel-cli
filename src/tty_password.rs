use anyhow::Context;
use std::io::{self, BufRead};

use crate::interrupt::MemorizeTerm;

pub fn read(prompt: impl AsRef<str>) -> anyhow::Result<String> {
    let _term = MemorizeTerm::new()?;
    let passwd = rpassword::prompt_password(prompt.as_ref()).context("error reading password")?;
    Ok(passwd)
}

pub fn read_stdin() -> anyhow::Result<String> {
    let passwd = io::stdin()
        .lock()
        .lines()
        .next()
        .context("password is expected")?
        .context("error reading password from stdin")?;
    Ok(passwd)
}

pub async fn read_async(prompt: impl AsRef<str>) -> anyhow::Result<String> {
    let prompt = prompt.as_ref().to_string();
    tokio::task::spawn_blocking(|| read(prompt)).await?
}

pub async fn read_stdin_async() -> anyhow::Result<String> {
    tokio::task::spawn_blocking(read_stdin).await?
}
