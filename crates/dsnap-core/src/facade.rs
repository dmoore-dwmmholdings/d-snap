//! The [`Dsnap`] handle. Owner: Chain A (DSNA-4).

use std::path::PathBuf;

use crate::db::Db;
use crate::error::Result;
use crate::paths::Home;
use crate::store::Store;

/// Handle to a D-Snap data directory: database plus object store.
///
/// `Send + Sync`; share one instance (e.g. in an `Arc`) across threads. Methods live in the
/// module that owns each feature (`projects`, `snapshot`, `diff`, `restore`, ...).
pub struct Dsnap {
    pub(crate) home: Home,
    #[allow(dead_code)] // read once Chain D implements the db methods
    pub(crate) db: Db,
    pub(crate) store: Store,
}

impl Dsnap {
    /// Open (creating if needed) the data directory.
    ///
    /// `home` overrides the `DSNAP_HOME` / platform default lookup; tests always pass one.
    pub fn open(home: Option<PathBuf>) -> Result<Self> {
        let home = Home::resolve(home)?;
        home.ensure()?;
        let store = Store::open(home.objects_dir())?;
        let db = Db::open(&home.db_path())?;
        Ok(Self { home, db, store })
    }

    /// The resolved data directory.
    pub fn home(&self) -> &Home {
        &self.home
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsnap_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Dsnap>();
    }
}
