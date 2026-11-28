//! Opening a redb database that a process may end while it is being made.
//!
//! redb makes a database file in two steps: it sets the file's length and writes its header,
//! flushes that, and only then writes the magic number that says the file is a database. A
//! process ended between the two leaves a non-empty file without the magic, which redb refuses
//! from then on (`I/O error: invalid data`) because it takes any non-empty file without its magic
//! for another program's. An opened database must survive a stop at any instant, so a database is
//! made under a name nothing opens and given its own name once redb has finished with it: an end
//! anywhere in the making leaves either the previous database or none, never a half-made one
//! under the name the next open reads.
//!
//! A file already at the name without the magic holds nothing to lose: redb writes the magic
//! before it commits anything, so no transaction of a file without it ever landed. It is moved
//! aside (a process that ended inside the making before this wrote under a scratch name left
//! one) and the database is made again.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use redb::Database;

/// What redb writes into a file it has made a database of, and the first thing it checks before
/// it opens one.
const MAGIC: [u8; 9] = *b"redb\x1A\x0A\xA9\x0D\x0A";

/// Opens the database at `path`, making it if it is not there, with `configure` applied to redb's
/// builder. The database it answers is whole on disk before `path` names it.
pub(crate) fn open(path: &Path, configure: impl Fn(&mut redb::Builder)) -> io::Result<Database> {
    let make = |at: &Path| -> io::Result<Database> {
        let mut builder = redb::Builder::new();
        configure(&mut builder);
        builder.create(at).map_err(io::Error::other)
    };
    // What a process ended while making a database left under the scratch name was never this
    // store, and redb would refuse the file: it goes, and redb makes the file afresh.
    discard_scratch(path)?;
    if !a_database(path)? {
        return make_fresh(path, &make);
    }
    make(path)
}

/// Removes the name a database is made under before it has its own.
fn discard_scratch(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(with_suffix(path, ".new")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether `path` names a database redb opens. No file, or an empty one, is not a database: the
/// making replaces it, and no half-made file is ever created under the name.
fn a_database(path: &Path) -> io::Result<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    let mut magic = [0_u8; MAGIC.len()];
    let mut read = 0;
    while read < magic.len() {
        match file.read(magic.get_mut(read..).unwrap_or_default()) {
            Ok(0) => break,
            Ok(bytes) => read = read.saturating_add(bytes),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    if magic == MAGIC {
        return Ok(true);
    }
    // Nothing redb committed is in a file without its magic: it is kept beside the store, so
    // what it holds can still be looked at, and the store starts again.
    let aside = with_suffix(path, ".unopened");
    std::fs::rename(path, &aside)?;
    tracing::warn!(
        path = %path.display(),
        aside = %aside.display(),
        "a store file was ended while it was made and holds nothing redb committed; it is kept \
         aside and the store starts again"
    );
    Ok(false)
}

/// Makes the database `path` is to hold under a name nothing opens, and gives it its own name
/// once redb is done with it.
fn make_fresh(path: &Path, make: &impl Fn(&Path) -> io::Result<Database>) -> io::Result<Database> {
    let scratch = with_suffix(path, ".new");
    let database = make(&scratch)?;
    std::fs::rename(&scratch, path)?;
    // The entry that names the database is synced so a power loss keeps it too. redb synced the
    // file itself before this, so the name reaches only bytes that are.
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Err(error) = File::open(dir).and_then(|dir| dir.sync_all())
    {
        tracing::warn!(%error, dir = %dir.display(), "a store's directory was not synced");
    }
    Ok(database)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::{ReadableTableMetadata as _, TableDefinition};
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use tilo_test_env::SpawnUnderEnvLock as _;

    const ROWS: TableDefinition<u64, u64> = TableDefinition::new("rows");

    /// A row written to the database at `path`, made or opened through the helper.
    fn with_a_row(path: &Path) {
        let db = open(path, |_| {}).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut rows = txn.open_table(ROWS).unwrap();
            rows.insert(1, 1).unwrap();
        }
        txn.commit().unwrap();
    }

    fn rows(path: &Path) -> u64 {
        let db = open(path, |_| {}).unwrap();
        let txn = db.begin_write().unwrap();
        let len = txn.open_table(ROWS).unwrap().len().unwrap();
        txn.commit().unwrap();
        len
    }

    #[test]
    fn a_database_whose_making_was_cut_off_opens_again() {
        // redb's first write of a database sets the file's length and writes its header, and only
        // then its magic. A process ended between them leaves a non-empty file with no magic,
        // which redb refuses. Each shape it can leave is opened over.
        let dir = tempfile::tempdir().unwrap();
        let whole = dir.path().join("whole.redb");
        with_a_row(&whole);
        let bytes = std::fs::read(&whole).unwrap();
        let mut without_magic = bytes.clone();
        without_magic[..MAGIC.len()].fill(0);
        let zeroed = vec![0_u8; bytes.len()];
        for (index, cut) in [without_magic, zeroed].into_iter().enumerate() {
            let path = dir.path().join(format!("cut-{index}.redb"));
            std::fs::write(&path, &cut).unwrap();
            drop(open(&path, |_| {}).unwrap());
            assert_eq!(
                rows(&path),
                0,
                "a file with no magic held nothing, and the store starts empty"
            );
            with_a_row(&path);
            assert_eq!(rows(&path), 1, "the store is written and read again");
            assert!(
                with_suffix(&path, ".unopened").exists(),
                "the file redb cannot open is kept beside the store, not deleted"
            );
        }
        // An empty file is no database either, and nothing of it is lost by making one.
        let empty = dir.path().join("empty.redb");
        std::fs::write(&empty, b"").unwrap();
        drop(open(&empty, |_| {}).unwrap());
        assert_eq!(rows(&empty), 0);
        assert!(
            !with_suffix(&empty, ".unopened").exists(),
            "an empty file is replaced, not kept aside"
        );
    }

    #[test]
    fn a_database_ended_before_its_rename_leaves_the_previous_one_and_no_half_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("area.redb");
        with_a_row(&path);
        let whole = std::fs::read(&path).unwrap();
        let mut without_magic = whole.clone();
        without_magic[..MAGIC.len()].fill(0);
        // What a process ended inside the making leaves under the scratch name: nothing, the
        // header without its magic, or the database whole.
        for leftover in [None, Some(without_magic), Some(whole)] {
            let scratch = with_suffix(&path, ".new");
            match &leftover {
                Some(bytes) => std::fs::write(&scratch, bytes).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&scratch);
                }
            }
            assert_eq!(
                rows(&path),
                1,
                "the database the name held before the making opens, whatever the making left"
            );
            assert!(
                !scratch.exists(),
                "the name nothing opens is gone once the database has its own"
            );
            assert!(
                !with_suffix(&path, ".unopened").exists(),
                "a whole database is never taken for one redb cannot open"
            );
        }
    }

    /// The directory a child of
    /// [`a_process_ended_while_a_database_is_made_leaves_one_that_opens`] works in.
    const CHILD: &str = "TILO_FS_TEST_REDB_OPEN_CHILD";

    /// A process killed at an arbitrary instant of making and opening a database leaves one the
    /// next open reads: the child is killed at several points of the making, and the database it
    /// worked in opens after it is gone.
    #[test]
    #[cfg(unix)]
    fn a_process_ended_while_a_database_is_made_leaves_one_that_opens() {
        if let Some(dir) = std::env::var_os(CHILD) {
            let path = PathBuf::from(dir).join("area.redb");
            // Make it, write a row, close it, and go again until the parent kills this: the end
            // lands inside the making, the transaction or the close, whichever the moment reaches.
            loop {
                with_a_row(&path);
                drop(open(&path, |_| {}).unwrap());
            }
        }
        for delay in [0_u64, 100, 300, 700, 1_500, 3_000, 6_000, 12_000] {
            let dir = tempfile::tempdir().unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                // This test's own name, which the child's harness filters for: with [`CHILD`] in
                // its environment it is the making and opening worker killed below.
                .arg("a_process_ended_while_a_database_is_made_leaves_one_that_opens")
                .arg("--nocapture")
                .env(CHILD, dir.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn_locked()
                .unwrap();
            std::thread::sleep(Duration::from_micros(delay));
            child.kill().unwrap();
            let ended = child.wait().unwrap();
            assert!(
                ended.signal().is_some(),
                "the child was killed, not ended by its own failure: {ended:?}"
            );
            // Whatever it was doing, the database in its directory opens.
            let path = dir.path().join("area.redb");
            drop(open(&path, |_| {}).unwrap());
            assert!(!with_suffix(&path, ".new").exists());
        }
    }

    #[test]
    fn a_row_a_database_was_made_with_is_there_after_it_is_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("area.redb");
        with_a_row(&path);
        assert_eq!(rows(&path), 1);
        assert!(
            std::fs::read(&path).unwrap().starts_with(&MAGIC),
            "the database the name holds is redb's from its first page"
        );
        assert!(!with_suffix(&path, ".new").exists());
    }
}
