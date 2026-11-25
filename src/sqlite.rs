//! An app's own SQLite database: opened with the house pragmas and migrated by
//! `PRAGMA user_version`.
//!
//! ```no_run
//! /// The schema, one entry per version: `MIGRATIONS[i]` takes the database from version `i`
//! /// to `i + 1`. Append only; never edit a shipped entry.
//! const MIGRATIONS: &[&str] = &[
//!     "CREATE TABLE tasks (id INTEGER PRIMARY KEY, user_id TEXT NOT NULL, title TEXT NOT NULL);",
//!     "ALTER TABLE tasks ADD COLUMN done_at TEXT;",
//! ];
//!
//! let conn = aba_kit::sqlite::open(std::path::Path::new("data/state.db"), MIGRATIONS)?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! Every row of an app is keyed by the user id of [`crate::AbaUser`]. One connection writes;
//! an app keeps it behind a mutex and runs its queries on blocking threads.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

/// How many `<file>.pre-v<N>` copies (taken before a schema upgrade) are kept.
pub const KEEP_UPGRADE_BACKUPS: usize = 3;

/// Opens the database at `path`, creating it if needed, and brings it to the newest schema.
///
/// - Pragmas: a busy timeout of 10 seconds, `journal_mode = WAL`, `synchronous = NORMAL`,
///   `foreign_keys = ON`.
/// - `migrations[i]` takes the database from schema version `i` to `i + 1`. Each one runs in
///   its own transaction together with its `PRAGMA user_version`, so a failed one leaves the
///   database at the version before it.
/// - A database newer than `migrations` is refused: an old build must not write to it.
/// - Before an existing database is upgraded, a consistent copy is written next to it as
///   `<file>.pre-v<new version>`, so the previous build can still start after a rollback. The
///   newest [`KEEP_UPGRADE_BACKUPS`] copies are kept. A failed copy stops the upgrade.
pub fn open(path: &Path, migrations: &[&str]) -> Result<Connection> {
    let conn =
        Connection::open(path).with_context(|| format!("opening database {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
    migrate(&conn, path, migrations).with_context(|| format!("migrating {}", path.display()))?;
    Ok(conn)
}

/// The file name of `db`, for messages and for the names of its copies.
fn file_name(db: &Path) -> String {
    db.file_name().map_or_else(
        || "database".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `YYYYMMDDTHHMMSSZ` for `time` (UTC), as the copies' names carry it.
fn utc_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm, for dates from 1970).
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The copy taken before upgrading `db` to schema `version`: `state.db.pre-v3`, or with a
/// timestamp when that name is taken (an earlier attempt, a restore and a second upgrade).
fn upgrade_backup_path(db: &Path, version: i64) -> PathBuf {
    let name = file_name(db);
    let plain = db.with_file_name(format!("{name}.pre-v{version}"));
    if !plain.exists() {
        return plain;
    }
    let stamp = utc_stamp(SystemTime::now());
    let stamped = db.with_file_name(format!("{name}.pre-v{version}-{stamp}"));
    let mut candidate = stamped.clone();
    let mut n = 1;
    while candidate.exists() {
        candidate = db.with_file_name(format!("{name}.pre-v{version}-{stamp}-{n}"));
        n += 1;
    }
    candidate
}

/// SQLite's side files of a database (`-wal`, `-shm`, `-journal`): not copies of their own.
fn is_side_file(name: &str) -> bool {
    name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with("-journal")
}

/// Takes a consistent copy of the database before a schema upgrade (`VACUUM INTO`) and keeps
/// the newest [`KEEP_UPGRADE_BACKUPS`] of them.
fn backup_before_upgrade(conn: &Connection, db: &Path, from: i64, to: i64) -> Result<()> {
    let target = upgrade_backup_path(db, to);
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
        .with_context(|| {
            format!(
                "backing up {} (schema {from}) to {} before upgrading to {to}; \
                 free some disk space or take the copy by hand",
                file_name(db),
                target.display()
            )
        })?;
    // One self-contained file: restoring it is a copy, opening it leaves no -wal behind.
    Connection::open(&target)
        .and_then(|c| c.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(())))
        .with_context(|| format!("finishing the backup {}", target.display()))?;
    tracing::info!(from, to, backup = %target.display(), "database backed up before the upgrade");
    prune_upgrade_backups(db);
    Ok(())
}

/// Deletes all but the newest [`KEEP_UPGRADE_BACKUPS`] `<file>.pre-v*` copies next to `db`.
fn prune_upgrade_backups(db: &Path) {
    let Some(dir) = db.parent() else { return };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{}.pre-v", file_name(db));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut copies: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix) && !is_side_file(&name)
        })
        .filter_map(|e| {
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, e.path()))
        })
        .collect();
    copies.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, old) in copies.into_iter().skip(KEEP_UPGRADE_BACKUPS) {
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::info!(path = %old.display(), "old upgrade backup removed"),
            Err(error) => tracing::warn!(path = %old.display(), %error, "cannot remove old backup"),
        }
        for side in ["-wal", "-shm", "-journal"] {
            let mut p = old.clone().into_os_string();
            p.push(side);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
}

fn migrate(conn: &Connection, path: &Path, migrations: &[&str]) -> Result<()> {
    let newest = migrations.len() as i64;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > newest {
        bail!(
            "{} has schema version {version}, newer than this build ({newest}); \
             run a newer build or restore a backup",
            file_name(path)
        );
    }
    if version < 0 {
        bail!("{} has the schema version {version}", file_name(path));
    }
    // A fresh file (version 0) has nothing to lose; an existing one is copied first.
    if version > 0 && version < newest {
        backup_before_upgrade(conn, path, version, newest)?;
    }
    for (i, sql) in migrations.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("migration {}", i + 1))?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        tracing::info!(version = i + 1, db = %file_name(path), "database migrated");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATIONS: &[&str] = &[
        "CREATE TABLE settings (user_id TEXT PRIMARY KEY, data TEXT NOT NULL);
         CREATE TABLE plans (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL);
         CREATE TABLE plan_checks (
           plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
           path    TEXT NOT NULL,
           PRIMARY KEY (plan_id, path)
         );",
        "ALTER TABLE plan_checks ADD COLUMN mark TEXT NOT NULL DEFAULT 'bought';",
        "ALTER TABLE plans ADD COLUMN version INTEGER NOT NULL DEFAULT 0;",
    ];

    fn version_of(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn pragma<T: rusqlite::types::FromSql>(conn: &Connection, name: &str) -> T {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn migrates_once_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = open(&path, MIGRATIONS).unwrap();
        assert_eq!(pragma::<i64>(&conn, "user_version"), 3);
        conn.execute("INSERT INTO settings VALUES ('aba', '{}')", [])
            .unwrap();
        drop(conn);
        let conn = open(&path, MIGRATIONS).unwrap();
        let user: String = conn
            .query_row("SELECT user_id FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user, "aba");
        // The newest column is there.
        conn.execute("INSERT INTO plans (user_id, version) VALUES ('aba', 7)", [])
            .unwrap();
    }

    #[test]
    fn the_house_pragmas_are_set() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("state.db"), MIGRATIONS).unwrap();
        assert_eq!(pragma::<String>(&conn, "journal_mode"), "wal");
        assert_eq!(pragma::<i64>(&conn, "synchronous"), 1, "NORMAL");
        assert_eq!(pragma::<i64>(&conn, "foreign_keys"), 1);
        assert_eq!(pragma::<i64>(&conn, "busy_timeout"), 10_000);
        // foreign_keys is on for real: a check of a plan that does not exist is refused.
        let orphan = conn.execute(
            "INSERT INTO plan_checks (plan_id, path) VALUES (99, 'x')",
            [],
        );
        assert!(orphan.is_err());
    }

    #[test]
    fn refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 999")
            .unwrap();
        let error = open(&path, MIGRATIONS).unwrap_err();
        assert!(format!("{error:#}").contains("newer than this build"));
        assert_eq!(version_of(&path), 999, "untouched");
    }

    #[test]
    fn a_failed_migration_leaves_the_version_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let broken = [
            MIGRATIONS[0],
            "CREATE TABLE half (id INTEGER); THIS IS NOT SQL;",
        ];
        let error = open(&path, &broken).unwrap_err();
        assert!(format!("{error:#}").contains("migration 2"), "{error:#}");
        assert_eq!(version_of(&path), 1);
        // Nothing of the failed step stays behind, so the fixed build starts cleanly.
        let conn = open(&path, MIGRATIONS).unwrap();
        assert_eq!(pragma::<i64>(&conn, "user_version"), 3);
        let half: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'half'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(half, 0);
    }

    /// A state.db at `version`, as an older build left it.
    fn create_at(path: &Path, version: usize) {
        let conn = Connection::open(path).unwrap();
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .unwrap();
        for sql in &MIGRATIONS[..version] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))
            .unwrap();
        conn.execute(
            "INSERT INTO settings (user_id, data) VALUES ('default', '{\"people\": 5}')",
            [],
        )
        .unwrap();
    }

    fn copies(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                let name = e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                name.starts_with("state.db.pre-v") && !is_side_file(&name)
            })
            .count()
    }

    /// An upgrade in place first takes a copy the previous build can start from.
    #[test]
    fn an_upgrade_backs_up_first_and_keeps_three() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        create_at(&path, 2);
        let conn = open(&path, MIGRATIONS).unwrap();
        assert_eq!(version_of(&path), 3);
        let data: String = conn
            .query_row("SELECT data FROM settings", [], |r| r.get(0))
            .unwrap();
        assert!(data.contains('5'), "the data came along");
        drop(conn);
        let backup = dir.path().join("state.db.pre-v3");
        assert!(backup.is_file());
        assert_eq!(version_of(&backup), 2, "the copy is the old schema");
        let old = Connection::open(&backup).unwrap();
        let data: String = old
            .query_row("SELECT data FROM settings", [], |r| r.get(0))
            .unwrap();
        assert!(data.contains('5'));
        drop(old);
        assert!(
            !dir.path().join("state.db.pre-v3-wal").exists(),
            "the copy is one file"
        );
        // Opening the current schema again takes no copy.
        open(&path, MIGRATIONS).unwrap();
        assert_eq!(copies(dir.path()), 1);
        // A fresh file needs none.
        let fresh = tempfile::tempdir().unwrap();
        open(&fresh.path().join("state.db"), MIGRATIONS).unwrap();
        assert_eq!(copies(fresh.path()), 0);
        // Repeated upgrades (restores): the newest three are kept.
        for _ in 0..4 {
            drop(Connection::open(&path).unwrap());
            std::fs::remove_file(&path).unwrap();
            let _ = std::fs::remove_file(dir.path().join("state.db-wal"));
            let _ = std::fs::remove_file(dir.path().join("state.db-shm"));
            create_at(&path, 1);
            std::thread::sleep(Duration::from_millis(30));
            open(&path, MIGRATIONS).unwrap();
        }
        assert_eq!(copies(dir.path()), KEEP_UPGRADE_BACKUPS);
    }

    #[test]
    fn a_second_copy_for_the_same_version_gets_a_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        assert_eq!(
            upgrade_backup_path(&db, 3),
            dir.path().join("state.db.pre-v3")
        );
        std::fs::write(dir.path().join("state.db.pre-v3"), "taken").unwrap();
        let second = file_name(&upgrade_backup_path(&db, 3));
        let stamp = second.strip_prefix("state.db.pre-v3-").unwrap();
        assert_eq!(stamp.len(), "20261124T230700Z".len(), "{second}");
        assert!(stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T');
    }

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        let at = |secs: u64| utc_stamp(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "19700101T000000Z");
        assert_eq!(at(951_782_400), "20000229T000000Z", "a leap day");
        assert_eq!(at(1_709_251_199), "20240229T235959Z");
        assert_eq!(at(1_709_251_200), "20240301T000000Z");
        assert_eq!(at(1_795_561_620), "20261124T230700Z");
        assert_eq!(at(4_102_444_799), "20991231T235959Z");
    }
}
