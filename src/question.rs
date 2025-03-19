use std::borrow::Cow;
use std::io::{BufRead, stdin};

use anyhow::Context;
use rustyline::{Config, DefaultEditor};
use tokio::task::spawn_blocking;

use crate::print;

pub struct Numeric<'a, T: Clone + 'a> {
    question: Cow<'a, str>,
    options: Vec<(Cow<'a, str>, T)>,
    suffix: &'a str,
}

pub struct String<'a> {
    question: Cow<'a, str>,
    default: Cow<'a, str>,
    initial: Option<std::string::String>,
}

pub struct Confirm<'a> {
    question: Cow<'a, str>,
    is_dangerous: bool,
    default: Option<bool>,
}

pub struct Variant<'a, T: 'a> {
    result: T,
    input: &'a [&'a str],
    help: Cow<'a, str>,
}

pub struct Choice<'a, T: 'a> {
    question: Cow<'a, str>,
    choices: Vec<Variant<'a, T>>,
}

pub fn read_choice() -> anyhow::Result<std::string::String> {
    let mut lines = stdin().lock().lines();

    let Some(line) = lines.next() else {
        anyhow::bail!("Unexpected end of input");
    };

    let line = line.context("reading user input")?;
    Ok(line.trim().to_lowercase())
}

impl<'a, T: Clone + 'a> Numeric<'a, T> {
    pub fn new<Q: Into<Cow<'a, str>>>(question: Q) -> Self {
        Numeric {
            question: question.into(),
            options: Vec::new(),
            suffix: "Type a number to select an option:",
        }
    }
    pub fn option<S: Into<Cow<'a, str>>>(&mut self, name: S, value: T) -> &mut Self {
        self.options.push((name.into(), value));
        self
    }
    pub fn ask(&self) -> anyhow::Result<T> {
        let mut editor = DefaultEditor::with_config(Config::builder().build())?;
        loop {
            print::prompt(&self.question);
            for (idx, (title, _)) in self.options.iter().enumerate() {
                print::prompt(format!("{}. {}", idx + 1, title));
            }
            print::prompt(self.suffix);
            let value = editor.readline("> ")?;
            let value = value.trim();
            let choice = match value.parse::<u32>() {
                Ok(choice) => choice,
                Err(e) => {
                    print::error!("Error reading choice: {e}");
                    print::prompt("Please enter a number");
                    continue;
                }
            };
            if choice == 0 || choice as usize > self.options.len() {
                print::error!("Please specify a choice from the list above");
                continue;
            }
            return Ok(self.options[(choice - 1) as usize].1.clone());
        }
    }
}

impl<T: Clone + Send> Numeric<'static, T> {
    pub async fn async_ask(self) -> anyhow::Result<T> {
        spawn_blocking(move || self.ask()).await?
    }
}

impl<'a> String<'a> {
    pub fn new(question: &'a str) -> String<'a> {
        String {
            question: question.into(),
            default: Cow::default(),
            initial: None,
        }
    }
    #[must_use]
    pub fn default<'b: 'a>(mut self, default: &'b str) -> Self {
        self.default = default.into();
        self
    }
    pub fn ask(&mut self) -> anyhow::Result<std::string::String> {
        if self.default.is_empty() {
            print::prompt(format!("{}: ", self.question));
        } else {
            print::prompt(format!("{} [default: {}]: ", self.question, self.default));
        }
        let mut editor = DefaultEditor::with_config(Config::builder().build())?;
        let initial = self
            .initial
            .as_ref()
            .map(|s| &s[..])
            .unwrap_or(self.default.as_ref());
        let val = editor.readline_with_initial("> ", (initial, ""))?;
        let mut val = val.trim();
        if val.is_empty() {
            val = self.default.as_ref();
        }
        self.initial = Some(val.into());
        Ok(val.into())
    }

    pub async fn async_ask(self) -> anyhow::Result<std::string::String> {
        let mut question: String<'static> = String {
            question: self.question.to_string().into(),
            default: self.default.to_string().into(),
            initial: self.initial.map(|s| s.to_string()),
        };
        spawn_blocking(move || question.ask()).await?
    }
}

impl<'a> Confirm<'a> {
    pub fn new<Q: Into<Cow<'a, str>>>(question: Q) -> Confirm<'a> {
        Confirm {
            question: question.into(),
            is_dangerous: false,
            default: None,
        }
    }
    pub fn new_dangerous<Q: Into<Cow<'a, str>>>(question: Q) -> Confirm<'a> {
        Confirm {
            question: question.into(),
            is_dangerous: true,
            default: None,
        }
    }
    pub fn default(&mut self, value: bool) -> &mut Self {
        self.default = Some(value);
        self
    }
    pub fn ask(&self) -> anyhow::Result<bool> {
        let mut editor = DefaultEditor::with_config(Config::builder().build())?;
        if self.is_dangerous {
            print::prompt(format!("{} (type `Yes`)", self.question));
        } else {
            print::prompt(format!(
                "{} [{}]",
                self.question,
                match self.default {
                    None => "y/n",
                    Some(true) => "Y/n",
                    Some(false) => "y/N",
                }
            ));
        };
        let mut initial = match self.default {
            None => "",
            Some(true) => "Y",
            Some(false) => "N",
        }
        .to_string();
        loop {
            let val = editor.readline_with_initial("> ", (&initial, ""))?;
            let val = val.trim();
            if self.is_dangerous {
                match val {
                    "Yes" => return Ok(true),
                    _ => return Ok(false),
                }
            } else {
                match val {
                    "y" | "Y" | "yes" | "Yes" | "YES" => return Ok(true),
                    "n" | "N" | "no" | "No" | "NO" => return Ok(false),
                    "" if self.default.is_some() => {
                        return Ok(self.default.unwrap());
                    }
                    _ => {
                        initial = val.into();
                        print::error!("Please answer Y or N");
                        continue;
                    }
                }
            }
        }
    }
}

impl Confirm<'static> {
    pub async fn async_ask(self) -> anyhow::Result<bool> {
        spawn_blocking(move || self.ask()).await?
    }
}

impl<'a, T: Clone + 'a> Choice<'a, T> {
    pub fn new<Q: Into<Cow<'a, str>>>(question: Q) -> Self {
        Choice {
            question: question.into(),
            choices: Vec::new(),
        }
    }
    pub fn option<H: Into<Cow<'a, str>>>(
        &mut self,
        result: T,
        input: &'a [&'a str],
        help: H,
    ) -> &mut Self {
        self.choices.push(Variant {
            result,
            input,
            help: help.into(),
        });
        self
    }
    pub fn ask(&self) -> anyhow::Result<T> {
        let mut editor = DefaultEditor::with_config(Config::builder().build())?;
        let options = self
            .choices
            .iter()
            .map(|c| c.input[0])
            .chain(Some("?"))
            .collect::<Vec<_>>()
            .join(",");
        loop {
            print::prompt(format!("{} [{}]", self.question, options));
            let val = editor.readline("> ")?;
            let val = val.trim();
            if matches!(val, "?" | "h" | "help") {
                const HELP: &str = "h or ?";
                let pad = self
                    .choices
                    .iter()
                    .map(|x| x.input.join(" or ").len())
                    .max()
                    .unwrap_or(0);
                let pad = std::cmp::max(pad, HELP.len());

                for choice in &self.choices {
                    println!(
                        "{:pad$} - {}",
                        choice.input.join(" or "),
                        choice.help,
                        pad = pad
                    )
                }
                println!("{HELP:pad$} - print help");
                continue;
            }
            for choice in &self.choices {
                for item in choice.input {
                    if item == &val {
                        return Ok(choice.result.clone());
                    }
                }
            }
            print::error!("Invalid option {val:?}, please use one of: [{options}]");
        }
    }
}

impl<T: Send + Clone> Choice<'static, T> {
    pub async fn async_ask(self) -> anyhow::Result<T> {
        spawn_blocking(move || self.ask()).await?
    }
}
