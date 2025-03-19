use std::collections::HashMap;

use fs_err as fs;

use crate::connect::Connection;
use crate::migrations::create::{MigrationKey, MigrationToText};
use crate::migrations::db_migration::{DBMigration, read_all};
use crate::migrations::migration::MigrationFile;
use crate::migrations::{Context, apply, create, migration};
use crate::print::{self, Highlight};
use anyhow::Context as _;
use indexmap::IndexMap;

#[derive(PartialEq)]
enum RebaseMigrationKind {
    Base,
    Source,
    Target,
}

struct RebaseMigration<'a> {
    key: MigrationKey,
    migration: &'a DBMigration,
    parent_override: Option<&'a str>,
    kind: RebaseMigrationKind,
}

impl<'a> MigrationToText<'a, std::iter::Once<&'a String>> for RebaseMigration<'_> {
    fn key(&self) -> &MigrationKey {
        &self.key
    }

    fn parent(&self) -> anyhow::Result<&str> {
        if let Some(parent) = self.parent_override {
            return Ok(parent);
        }

        if self.migration.parent_names.is_empty() {
            return Ok("initial");
        }

        Ok(&self.migration.parent_names[0])
    }

    fn id(&self) -> anyhow::Result<&str> {
        Ok(&self.migration.name)
    }

    fn statements(&'a self) -> std::iter::Once<&'a String> {
        std::iter::once(&self.migration.script)
    }
}

#[derive(Clone)]
pub struct RebaseMigrations {
    /// initial..base : the commonly shared migrations between both 'source' and 'target'
    base_migrations: IndexMap<String, DBMigration>,
    /// base..source : the migrations that are unique to 'source'
    source_migrations: IndexMap<String, DBMigration>,
    /// base..target : the migrations that are unique to 'target'
    target_migrations: IndexMap<String, DBMigration>,
}

impl RebaseMigrations {
    pub fn print_status(&self) {
        let last_common = self
            .base_migrations
            .last()
            .map(|v| v.0.as_str())
            .unwrap_or("initial")
            .success();

        let format_migration_on_length = |c: usize| {
            if c > 1 { "migrations" } else { "migration" }
        };

        eprintln!("Last common migration is {last_common}");
        eprintln!(
            "Since then, there are:\n- {} new {} on the target branch,\n- {} {} to rebase",
            self.target_migrations.len().to_string().success(),
            format_migration_on_length(self.target_migrations.len()),
            self.source_migrations.len().to_string().success(),
            format_migration_on_length(self.source_migrations.len())
        );
    }

    fn flatten(&self) -> anyhow::Result<Vec<RebaseMigration>> {
        let mut result = Vec::new();
        let mut key_index = 0;

        let mut next_key = || -> MigrationKey {
            key_index += 1;
            MigrationKey::Index(key_index)
        };

        for (_, migration) in &self.base_migrations {
            result.push(RebaseMigration {
                key: next_key(),
                parent_override: None,
                migration,
                kind: RebaseMigrationKind::Base,
            });
        }

        if !self.target_migrations.is_empty() {
            let (_, first_migration) = self.target_migrations.first().unwrap();
            let source_last = result.last();

            result.push(RebaseMigration {
                key: next_key(),
                migration: first_migration,
                parent_override: source_last.map(|v| v.migration.name.as_str()),
                kind: RebaseMigrationKind::Target,
            });

            for (_, migration) in &self.target_migrations[1..] {
                result.push(RebaseMigration {
                    key: next_key(),
                    migration,
                    parent_override: None,
                    kind: RebaseMigrationKind::Target,
                });
            }
        }

        if !self.source_migrations.is_empty() {
            let (_, first_migration) = self.source_migrations.first().unwrap();
            let base_last = result.last();

            result.push(RebaseMigration {
                key: next_key(),
                migration: first_migration,
                parent_override: base_last.map(|v| v.migration.name.as_str()),
                kind: RebaseMigrationKind::Source,
            });

            for (_, migration) in &self.source_migrations[1..] {
                result.push(RebaseMigration {
                    key: next_key(),
                    migration,
                    parent_override: None,
                    kind: RebaseMigrationKind::Source,
                });
            }
        }

        Ok(result)
    }
}

pub async fn get_diverging_migrations(
    source: &mut Connection,
    target: &mut Connection,
) -> anyhow::Result<RebaseMigrations> {
    let mut source_migrations = read_all(source, true, false).await?;
    let mut target_migrations = read_all(target, true, false).await?;

    if source_migrations.is_empty() {
        return Ok(RebaseMigrations {
            base_migrations: IndexMap::new(),
            source_migrations: IndexMap::new(),
            target_migrations,
        });
    }

    for (index, (id, _)) in target_migrations.iter().enumerate().rev() {
        if source_migrations.contains_key(id) {
            if index == target_migrations.len() - 1 {
                // target is up to date with source
                anyhow::bail!("Branch {} is already up-to-date", target.database())
            }

            let source_index = source_migrations
                .get_index_of(id)
                .context("Expected source_migrations to contain ID")?;

            let new_source_migrations = source_migrations.split_off(source_index + 1);

            // add 1 to index since we want the migrations not apart of source
            return Ok(RebaseMigrations {
                base_migrations: source_migrations,
                source_migrations: new_source_migrations,
                target_migrations: target_migrations.split_off(index + 1),
            });
        }
    }

    Ok(RebaseMigrations {
        base_migrations: IndexMap::new(),
        source_migrations,
        target_migrations,
    })
}

async fn rebase_migration_ids(
    context: &Context,
    rebase_migrations: &mut RebaseMigrations,
) -> anyhow::Result<()> {
    fn update_id(old: &str, new: &str, col: &mut IndexMap<String, DBMigration>) {
        if let Some((old_index, _, value)) = col.shift_remove_full(old) {
            let (new_index, _) = col.insert_full(new.to_string(), value);
            col.move_index(new_index, old_index);
        }
    }

    fix_migration_ids(context, |old, new| {
        update_id(old, new, &mut rebase_migrations.base_migrations);
        update_id(old, new, &mut rebase_migrations.target_migrations);
        update_id(old, new, &mut rebase_migrations.source_migrations);
    })
    .await
}

pub async fn fix_migration_ids<T>(context: &Context, mut on_update: T) -> anyhow::Result<()>
where
    T: FnMut(&String, &String),
{
    let migrations = migration::read_all(context, false).await?;
    let mut changed_ids: HashMap<String, String> = HashMap::new();

    for (id, mut migration_file) in migrations {
        let mut migration_text = fs::read_to_string(&migration_file.path)?;

        // its important to change parent before the main ID, since parent effects the hash id of the migration
        if changed_ids.contains_key(&migration_file.data.parent_id) {
            let new_parent_id = changed_ids.get(&migration_file.data.parent_id).unwrap();
            migration_text = migration_file
                .data
                .replace_parent_id(&migration_text, new_parent_id);
            migration_file.data.parent_id.clone_from(new_parent_id); // change for hash computation below
        }

        let expected_id = migration_file.data.expected_id(&migration_text)?;

        if id != expected_id {
            migration_text = migration_file
                .data
                .replace_id(&migration_text, &expected_id);
            migration_file.data.id.clone_from(&expected_id);
            changed_ids.insert(id, expected_id);
        }

        fs::write(&migration_file.path, migration_text)?;
    }

    for (old_id, new_id) in changed_ids {
        on_update(&old_id, &new_id);
    }

    Ok(())
}

pub async fn do_rebase(
    rebase_migrations: &mut RebaseMigrations,
    context: &Context,
) -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let temp_ctx = Context {
        schema_dir: temp_dir.path().to_path_buf(),
        quiet: false,
        project: None,
    };

    // write all the migrations to disk.
    let to_flatten = rebase_migrations.clone();
    let flattened_migrations = to_flatten.flatten()?;
    for migration in &flattened_migrations {
        create::write_migration(&temp_ctx, migration, false).await?;
    }

    // update the IDs of the migrations for the rebase, since we're changing the history the IDs can be invalid so we
    // need to update them.
    rebase_migration_ids(&temp_ctx, rebase_migrations).await?;

    // remove the old migrations
    for old in migration::read_names(context).await? {
        fs::remove_file(old)?;
    }

    // move the new migrations from temp to schema dir
    let mut last: Option<&RebaseMigrationKind> = None;
    let mut new_migration_files = migration::read_names(&temp_ctx).await?;
    new_migration_files.sort();
    for (index, from) in new_migration_files.iter().enumerate() {
        let rebase_migration = flattened_migrations.get(index);
        let to = context
            .schema_dir
            .join("migrations")
            .join(from.file_name().unwrap());

        if let Some(migration) = rebase_migration {
            if last != Some(&migration.kind) {
                match migration.kind {
                    RebaseMigrationKind::Target => {
                        eprintln!("\nNew migrations on target branch:")
                    }
                    RebaseMigrationKind::Source => {
                        eprintln!("\nMigrations to rebase:")
                    }
                    RebaseMigrationKind::Base => {}
                }
            }

            match migration.kind {
                RebaseMigrationKind::Target | RebaseMigrationKind::Source => {
                    print::success_msg("Writing", to.display());
                }
                RebaseMigrationKind::Base => {}
            }
        }

        fs::copy(from, to)?;

        last = rebase_migration.map(|v| &v.kind);
    }

    Ok(())
}

pub async fn write_rebased_migration_files(
    rebase_migrations: &RebaseMigrations,
    context: &Context,
    connection: &mut Connection,
) -> anyhow::Result<()> {
    // apply the new migrations
    let migrations: IndexMap<String, MigrationFile> = migration::read_all(context, true)
        .await?
        .into_iter()
        .filter(|(id, _)| rebase_migrations.source_migrations.contains_key(id))
        .collect();

    apply::apply_migrations(connection, &migrations, context, true).await
}
