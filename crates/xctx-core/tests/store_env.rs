//! `XCTX_DATA_DIR` moves the store. This sets a process environment variable, so it is the only test in its binary.

mod common;

use common::{TestResult, temp_root};
use xctx_core::Store;
use xctx_core::store::{DATA_DIR_ENV, data_dir};

#[test]
fn xctx_data_dir_moves_the_default_store() -> TestResult {
    let root = temp_root()?;
    let dir = root.path.join("xctx-data");
    // SAFETY: this binary runs no other test, so nothing reads the environment concurrently.
    unsafe { std::env::set_var(DATA_DIR_ENV, &dir) };
    assert_eq!(data_dir()?, dir);
    let mut store = Store::open_default()?;
    assert_eq!(store.dir(), dir);
    store.add_root(root.path.join("code"))?;
    assert!(dir.join("spaces.json").is_file());
    Ok(())
}
