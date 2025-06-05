use std::collections::hash_map::{Entry, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use edgeql_parser::hash::{self, Hasher};
use fn_error_context::context;
use indexmap::IndexMap;
use regex::Regex;
use tokio::fs;
use tokio::io;

use crate::migrations::NULL_MIGRATION;
use crate::migrations::context::Context;
use crate::migrations::grammar::parse_migration;
use crate::print;

#[derive(Debug)]
pub struct Migration {
    pub message: Option<String>,
    pub id: String,
    pub id_range: (usize, usize),
    pub parent_id: String,
    pub parent_id_range: (usize, usize),
    pub text_range: (usize, usize),
}

#[derive(Debug)]
pub struct MigrationFile {
    pub path: PathBuf,
    pub fixup_target: Option<String>,
    pub data: Migration,
}

#[derive(PartialOrd, PartialEq, Eq, Ord)]
pub enum SortKey<'a> {
    Numeric(u64),
    Text(&'a OsStr),
}

pub fn hashing_error(_source: &str, e: hash::Error) -> anyhow::Error {
    match e {
        hash::Error::Tokenizer(msg, pos) => {
            anyhow::anyhow!("Tokenizer error at {}: {}", pos, msg)
        }
    }
}

impl Migration {
    pub fn expected_id(&self, text: &str) -> anyhow::Result<String> {
        let mut hasher = Hasher::start_migration(&self.parent_id);
        let txt = &text[self.text_range.0..self.text_range.1];
        hasher.add_source(txt).map_err(|e| hashing_error(txt, e))?;
        let id = hasher.make_migration_id();
        Ok(id)
    }
    pub fn replace_id(&self, text: &str, new_id: &str) -> String {
        let mut s = String::with_capacity(text.len());
        s.push_str(&text[..self.id_range.0]);
        s.push_str(new_id);
        s.push_str(&text[self.id_range.1..]);
        s
    }

    pub fn replace_parent_id(&self, text: &str, new_id: &str) -> String {
        let mut s = String::with_capacity(text.len());
        s.push_str(&text[..self.parent_id_range.0]);
        s.push_str(new_id);
        s.push_str(&text[self.parent_id_range.1..]);
        s
    }
}

fn validate_text(text: &str, migration: &Migration) -> anyhow::Result<()> {
    if migration.id.starts_with("m1") {
        let id = migration.expected_id(text)?;
        if id != migration.id {
            anyhow::bail!(
                "\n Migration name should be: \n  {computed}, but \
                \n  {file} found instead. \
                \n\nMigration names are computed from the hash \
                of the migration contents. To proceed you must fix the \
                statement to read as:\n  \
                CREATE MIGRATION {computed} ONTO ...\n\
                Alternatively, revert the changes to the file.",
                computed = id,
                file = migration.id
            );
        }
        Ok(())
    } else {
        anyhow::bail!("cannot parse migration name {:?}", migration.id);
    }
}

#[context("could not read migration file {}", path.display())]
pub async fn read_file(path: &Path, validate_hashes: bool) -> anyhow::Result<Migration> {
    let text = fs::read_to_string(&path).await?;
    let data = parse_migration(&text)?;
    if validate_hashes {
        validate_text(&text, &data)?;
    }
    return Ok(data);
}

pub async fn read_names(ctx: &Context) -> anyhow::Result<Vec<PathBuf>> {
    let dir = ctx.schema_dir.join("migrations");
    _read_names(dir.as_ref())
        .await
        .with_context(|| format!("could not list migrations in {}", dir.display()))
}

async fn _read_names(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut dir = match fs::read_dir(dir).await {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => Err(e)?,
    };
    let mut result = Vec::new();
    let old_name_re = Regex::new(r"^\d{5}\.edgeql$")?;

    let mut has_old_filename = false;

    while let Some(item) = dir.next_entry().await? {
        let fname = item.file_name();
        let lossy_name = fname.to_string_lossy();
        if lossy_name.starts_with('.')
            || !lossy_name.ends_with(".edgeql")
            || !item.file_type().await?.is_file()
        {
            continue;
        }

        if !has_old_filename && old_name_re.is_match(&lossy_name) {
            has_old_filename = true;
        }

        result.push(item.path());
    }

    if has_old_filename {
        print::warn!(
            "Legacy migration file names detected, consider running 'gel migration upgrade-format'"
        )
    }

    Ok(result)
}

pub fn file_num(path: &Path) -> Option<u64> {
    path.file_stem().and_then(|x| x.to_str()).and_then(|x| {
        if x.contains('-') {
            let spl: Vec<&str> = x.split('-').collect();
            return spl[0].parse().ok();
        }

        x.parse().ok()
    })
}

#[context("could not read migrations in {}", dir.display())]
async fn _read_all(
    dir: &Path,
    validate_hashes: bool,
) -> anyhow::Result<IndexMap<String, MigrationFile>> {
    let mut all = HashMap::new();
    for path in _read_names(dir).await? {
        let data = read_file(&path, validate_hashes).await?;
        match all.entry(data.parent_id.clone()) {
            Entry::Vacant(v) => {
                v.insert(MigrationFile {
                    path: path.to_path_buf(),
                    fixup_target: None,
                    data,
                });
            }
            Entry::Occupied(o) => {
                anyhow::bail!(
                    "Two files {:?} and {:?} have the same \
                    parent revision {:?}. Multiple branches in revision \
                    history are not supported yet, please rebase one of the \
                    branches on top of the other.",
                    path,
                    o.get().path,
                    data.parent_id
                );
            }
        }
    }
    sort_revisions(all)
}

fn sort_revisions(
    mut all: HashMap<String, MigrationFile>,
) -> anyhow::Result<IndexMap<String, MigrationFile>> {
    let mut res = IndexMap::new();
    let mut counter = 1;
    let mut parent_id = String::from(NULL_MIGRATION);
    while !all.is_empty() {
        if let Some(item) = all.remove(&parent_id) {
            if file_num(&item.path).map(|n| n != counter).unwrap_or(true) {
                anyhow::bail!(
                    "File `{}` should be named `{:05}.edgeql`",
                    item.path.display(),
                    counter
                );
            }
            counter += 1;
            parent_id.clone_from(&item.data.id);
            res.insert(item.data.id.clone(), item);
        } else {
            let next = all
                .values()
                .min_by_key(|item| match file_num(&item.path) {
                    Some(n) => SortKey::Numeric(n),
                    None => SortKey::Text(item.path.file_stem().unwrap()),
                })
                .unwrap();
            let valid_number = file_num(&next.path).map(|n| n == counter).unwrap_or(false);
            if valid_number {
                anyhow::bail!(
                    "File `{}` should have {:?} as the parent migration \
                    (`CREATE MIGRATION {} ONTO {} {{...`)",
                    next.path.display(),
                    parent_id,
                    next.data.id,
                    parent_id
                );
            } else {
                anyhow::bail!(
                    "could not find file `{:05}.edgeql` with \
                    parent migration {:?} (perhaps {} should be fixed?)",
                    counter,
                    parent_id,
                    next.path.display()
                );
            }
        }
    }
    Ok(res)
}

pub async fn read_all(
    ctx: &Context,
    validate_hashes: bool,
) -> anyhow::Result<IndexMap<String, MigrationFile>> {
    _read_all(ctx.schema_dir.join("migrations").as_ref(), validate_hashes).await
}

#[context("could not read migrations in {}", dir.display())]
async fn _read_fixups(dir: &Path, validate_hashes: bool) -> anyhow::Result<Vec<MigrationFile>> {
    let mut result = Vec::new();
    for path in _read_names(dir).await? {
        let data = read_file(&path, validate_hashes).await?;
        let fixup_target = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('-').nth(1));
        let Some(fixup_target) = fixup_target else {
            log::warn!("Invalid fixup file name {:?}", path);
            continue;
        };
        result.push(MigrationFile {
            path: path.to_path_buf(),
            fixup_target: Some(fixup_target.into()),
            data,
        });
    }
    Ok(result)
}

pub async fn read_fixups(
    ctx: &Context,
    validate_hashes: bool,
) -> anyhow::Result<Vec<MigrationFile>> {
    _read_fixups(ctx.schema_dir.join("fixups").as_ref(), validate_hashes).await
}

#[cfg(test)]
mod test {
    use super::{Migration, MigrationFile};
    use super::{parse_migration, sort_revisions, validate_text};
    use crate::migrations::NULL_MIGRATION;
    use std::collections::HashMap;

    #[test]
    #[should_panic(
        expected = "Migration name should be: \n  m1tjyzfl33vvzwjd5izo5nyp4zdsekyvxpdm7zhtt5ufmqjzczopdq, but \n  m124 found instead."
    )]
    fn test_bad_hash() {
        let text = r###"
            CREATE MIGRATION m124 ONTO initial {
            };
        "###;
        let migr = Migration {
            id: "m124".into(),
            id_range: (30, 34),
            parent_id: "initial".into(),
            parent_id_range: (40, 47),
            message: None,
            text_range: (62, 62),
        };
        let result = parse_migration(text).unwrap();
        assert_eq!(result.id, migr.id);
        assert_eq!(result.parent_id, migr.parent_id);
        assert_eq!(result.message, migr.message);
        assert_eq!(result.text_range, migr.text_range);
        validate_text(text, &migr).unwrap();
    }

    #[test]
    fn test_hash_zero() {
        let text = r###"
            CREATE MIGRATION
                m1tjyzfl33vvzwjd5izo5nyp4zdsekyvxpdm7zhtt5ufmqjzczopdq
                ONTO initial
            {
            };
        "###;
        let migr = Migration {
            id: "m1tjyzfl33vvzwjd5izo5nyp4zdsekyvxpdm7zhtt5ufmqjzczopdq".into(),
            id_range: (0, 0),
            parent_id: "initial".into(),
            parent_id_range: (0, 0),
            message: None,
            text_range: (156, 156),
        };
        let result = parse_migration(text).unwrap();
        assert_eq!(result.id, migr.id);
        assert_eq!(result.parent_id, migr.parent_id);
        assert_eq!(result.message, migr.message);
        assert_eq!(result.text_range, migr.text_range);
        validate_text(text, &migr).unwrap();
    }

    #[test]
    fn test_hash_1() {
        let text = r###"
            CREATE MIGRATION
                m1fvpcra5cxntkss3k2to2yfu7pit3t3owesvdw2nysqvvpihdiszq
                ONTO m1g3qzqdr57pp3w2mdwdkq4g7dq4oefawqdavzgeiov7fiwntpb3lq
            {
                CREATE TYPE Type1;
            };
        "###;
        let migr = Migration {
            id: "m1fvpcra5cxntkss3k2to2yfu7pit3t3owesvdw2nysqvvpihdiszq".into(),
            id_range: (0, 0),
            parent_id: "m1g3qzqdr57pp3w2mdwdkq4g7dq4oefawqdavzgeiov7fiwntpb3lq".into(),
            parent_id_range: (0, 0),
            message: None,
            text_range: (207, 238),
        };
        let result = parse_migration(text).unwrap();
        assert_eq!(result.id, migr.id);
        assert_eq!(result.parent_id, migr.parent_id);
        assert_eq!(result.message, migr.message);
        assert_eq!(result.text_range, migr.text_range);
        validate_text(text, &migr).unwrap();
    }

    #[test]
    #[should_panic(
        expected = "Migration name should be: \n  m1q3jjfe7zjl74v3n2vxjwzneousdas6vvd4qwrfd6j6xmhmktyada, but \n  m154kc2cbzmzz2tzcjz5rpsspdew3azydwhwpkhcgkznpp6ibwhevq found instead."
    )]
    fn test_hash_depends_on_parent() {
        let text = r###"
            CREATE MIGRATION
                m154kc2cbzmzz2tzcjz5rpsspdew3azydwhwpkhcgkznpp6ibwhevq
                ONTO initial
            {
                CREATE TYPE Type1;
            };
        "###;
        let migr = Migration {
            id: "m154kc2cbzmzz2tzcjz5rpsspdew3azydwhwpkhcgkznpp6ibwhevq".into(),
            id_range: (0, 0),
            parent_id: "initial".into(),
            parent_id_range: (0, 0),
            message: None,
            text_range: (160, 191),
        };
        let result = parse_migration(text).unwrap();
        assert_eq!(result.id, migr.id);
        assert_eq!(result.parent_id, migr.parent_id);
        assert_eq!(result.message, migr.message);
        assert_eq!(result.text_range, migr.text_range);
        validate_text(text, &migr).unwrap();
    }

    #[test]
    fn sort_empty() {
        assert!(sort_revisions(HashMap::new()).unwrap().is_empty());
    }

    fn mk_seq(input: &[(&str, &str, &str)]) -> HashMap<String, MigrationFile> {
        input
            .iter()
            .cloned()
            .map(|(id, parent, filename)| {
                (
                    parent.into(),
                    MigrationFile {
                        path: filename.into(),
                        fixup_target: None,
                        data: Migration {
                            id: id.into(),
                            id_range: (0, 0),
                            parent_id: parent.into(),
                            parent_id_range: (0, 0),
                            message: None,
                            text_range: (0, 0),
                        },
                    },
                )
            })
            .collect()
    }

    #[test]
    fn sort_single() {
        assert_eq!(
            sort_revisions(mk_seq(&[("m10001", NULL_MIGRATION, "0001.edgeql"),]))
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            &["m10001"]
        );
    }

    #[test]
    #[should_panic(expected = "File `0001.edgeql` should have \"initial\" \
        as the parent migration")]
    fn first_wrong_parent() {
        sort_revisions(mk_seq(&[("m10001", "m10002", "0001.edgeql")])).unwrap();
    }

    #[test]
    #[should_panic(expected = "File `0002.edgeql` should be named `00001.edgeql`")]
    fn first_wrong_filename() {
        sort_revisions(mk_seq(&[("m10001", NULL_MIGRATION, "0002.edgeql")])).unwrap();
    }

    #[test]
    fn sort_two() {
        assert_eq!(
            sort_revisions(mk_seq(&[
                ("m10001", NULL_MIGRATION, "0001.edgeql"),
                ("m10002", "m10001", "0002.edgeql"),
            ]))
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
            &["m10001", "m10002"]
        );
    }

    #[test]
    #[should_panic(expected = "File `some.edgeql` should be \
                             named `00001.edgeql`")]
    fn two_filename_bad1() {
        sort_revisions(mk_seq(&[
            ("m10001", NULL_MIGRATION, "some.edgeql"),
            ("m10002", "m10001", "0002.edgeql"),
        ]))
        .unwrap();
    }

    #[test]
    #[should_panic(expected = "File `0003.edgeql` should be \
                             named `00001.edgeql`")]
    fn two_filename_non_seq1() {
        sort_revisions(mk_seq(&[
            ("m10001", NULL_MIGRATION, "0003.edgeql"),
            ("m10002", "m10001", "0002.edgeql"),
        ]))
        .unwrap();
    }

    #[test]
    #[should_panic(expected = "File `some.edgeql` should be \
                             named `00002.edgeql`")]
    fn two_filename_bad2() {
        sort_revisions(mk_seq(&[
            ("m10001", NULL_MIGRATION, "0001.edgeql"),
            ("m10002", "m10001", "some.edgeql"),
        ]))
        .unwrap();
    }

    #[test]
    #[should_panic(expected = "File `0003.edgeql` should be \
                             named `00002.edgeql`")]
    fn two_filename_non_seq2() {
        sort_revisions(mk_seq(&[
            ("m10001", NULL_MIGRATION, "0001.edgeql"),
            ("m10002", "m10001", "0003.edgeql"),
        ]))
        .unwrap();
    }

    #[test]
    #[should_panic(expected = "could not find file `00002.edgeql` with parent \
        migration \"m10001\" (perhaps 0003.edgeql should be fixed?)")]
    fn two_missing_second() {
        sort_revisions(mk_seq(&[
            ("m10001", NULL_MIGRATION, "0001.edgeql"),
            ("m10003", "m10002", "0003.edgeql"),
            ("m10004", "m10003", "0004.edgeql"),
        ]))
        .unwrap();
    }

    #[test]
    #[should_panic(expected = "File `0002.edgeql` should have \"m10001\" \
        as the parent migration")]
    fn two_bad_next_parent() {
        sort_revisions(mk_seq(&[
            ("m10001", NULL_MIGRATION, "0001.edgeql"),
            ("m10003", "m10002", "0002.edgeql"),
            ("m10004", "m10003", "0003.edgeql"),
        ]))
        .unwrap();
    }
}
