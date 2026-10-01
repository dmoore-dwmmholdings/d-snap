//! Checks that `dsnap-test-support` works as a dev-dependency of `dsnap-core`.

use dsnap_core::{Home, RelPath};
use dsnap_test_support::{FixtureProject, TestHome, tree_bytes};

#[test]
fn fixture_paths_are_core_rel_paths() {
    let fx = FixtureProject::new()
        .file("src/a.rs", "fn main() {}")
        .build();
    let tree = tree_bytes(fx.root());
    let key: &RelPath = tree.keys().next().unwrap();
    assert_eq!(key.as_str(), "src/a.rs");
}

#[test]
fn test_home_resolves_as_explicit_home() {
    let home = TestHome::new();
    let resolved = Home::resolve(Some(home.path().to_path_buf())).unwrap();
    resolved.ensure().unwrap();
    assert_eq!(resolved.db_path(), home.path().join("dsnap.db"));
    assert!(resolved.objects_dir().is_dir());
}
