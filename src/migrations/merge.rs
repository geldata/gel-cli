use fs_err as fs;
use indexmap::IndexMap;

use crate::connect::Connection;
use crate::migrations::create::{MigrationKey, MigrationToText, write_migration};
use crate::migrations::db_migration::{DBMigration, read_all};
use crate::migrations::migration::MigrationFile;
use crate::migrations::{Context, apply, migration};

pub struct MergeMigrations {
    pub base_migrations: IndexMap<String, MergeMigration>,
    pub target_migrations: IndexMap<String, MergeMigration>,
}

impl MergeMigrations {
    fn flatten(&self) -> IndexMap<&String, &MergeMigration> {
        let mut result = IndexMap::new();

        for migration in self.base_migrations.iter() {
            result.insert(migration.0, migration.1);
        }

        for migration in self.target_migrations.iter() {
            result.insert(migration.0, migration.1);
        }

        result
    }
}

pub struct MergeMigration {
    key: MigrationKey,
    migration: DBMigration,
}

impl<'a> MigrationToText<'a, std::iter::Once<&'a String>> for MergeMigration {
    fn key(&self) -> &MigrationKey {
        &self.key
    }

    fn parent(&self) -> anyhow::Result<&str> {
        Ok(self
            .migration
            .parent_names
            .first()
            .map(String::as_str)
            .unwrap_or("initial"))
    }

    fn id(&self) -> anyhow::Result<&str> {
        Ok(self.migration.name.as_str())
    }

    fn statements(&'a self) -> std::iter::Once<&'a String> {
        std::iter::once(&self.migration.script)
    }
}

pub async fn get_merge_migrations(
    base: &mut Connection,
    target: &mut Connection,
) -> anyhow::Result<MergeMigrations> {
    let base_migrations = read_all(base, true, false).await?;
    let mut target_migrations = read_all(target, true, false).await?;

    // check if the base branch is up-to-date with target:
    // we do this by verifying that all target migrations
    // exist within the base migrations
    if base_migrations.len() >= target_migrations.len()
        && target_migrations.iter().enumerate().all(|(i, (k, _))| {
            base_migrations
                .get_index(i)
                .map(|v| v.0 == k)
                .unwrap_or(false)
        })
    {
        anyhow::bail!("Already up to date.")
    }

    let mut diverging_migrations = Vec::new();

    for (index, (base_migration_id, _)) in base_migrations.iter().enumerate() {
        if let Some((target_migration_id, _)) = target_migrations.get_index(index) {
            if target_migration_id != base_migration_id {
                diverging_migrations.push((index, base_migration_id, target_migration_id))
            }
        }
    }

    if !diverging_migrations.is_empty() {
        eprintln!(
            "\nThe migration history of {} diverges from {}:",
            target.database(),
            base.database()
        );

        for (index, expecting, actual) in &diverging_migrations {
            eprintln!(
                "{}. Expecting {} but has {}",
                index,
                &expecting[..7],
                &actual[..7]
            )
        }

        eprintln!();

        anyhow::bail!(
            "Cannot complete fast-forward merge, the histories of {0} and {1} are incompatible. Try rebasing {1} onto {0}",
            base.database(),
            target.database()
        )
    }

    let mut target_merge_migrations: IndexMap<String, MergeMigration> = IndexMap::new();
    let mut base_merge_migrations: IndexMap<String, MergeMigration> = IndexMap::new();

    for (index, (id, migration)) in target_migrations
        .split_off(base_migrations.len())
        .into_iter()
        .enumerate()
    {
        target_merge_migrations.insert(
            id,
            MergeMigration {
                migration,
                key: MigrationKey::Index((base_migrations.len() + index + 1) as u64),
            },
        );
    }

    for (index, (id, migration)) in target_migrations.into_iter().enumerate() {
        base_merge_migrations.insert(
            id,
            MergeMigration {
                migration,
                key: MigrationKey::Index((index + 1) as u64),
            },
        );
    }

    Ok(MergeMigrations {
        target_migrations: target_merge_migrations,
        base_migrations: base_merge_migrations,
    })
}

pub async fn write_merge_migrations(
    context: &Context,
    migrations: &mut MergeMigrations,
) -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let temp_ctx = Context {
        schema_dir: temp_dir.path().to_path_buf(),
        quiet: false,
        project: None,
    };

    for (_, migration) in migrations.flatten() {
        write_migration(&temp_ctx, migration, false).await?;
    }

    for from in migration::read_names(&temp_ctx).await? {
        let to = context
            .schema_dir
            .join("migrations")
            .join(from.file_name().unwrap());

        fs::copy(from, to)?;
    }

    Ok(())
}

pub async fn apply_merge_migration_files(
    merge_migrations: &MergeMigrations,
    context: &Context,
    connection: &mut Connection,
) -> anyhow::Result<()> {
    // apply the new migrations
    let migrations: IndexMap<String, MigrationFile> = migration::read_all(context, true)
        .await?
        .into_iter()
        .filter(|(id, _)| merge_migrations.target_migrations.contains_key(id))
        .collect();

    apply::apply_migrations(connection, &migrations, context, true).await
}
