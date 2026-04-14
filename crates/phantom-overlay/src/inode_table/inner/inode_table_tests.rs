use super::*;

#[test]
fn rename_self_is_noop() {
    let table = InodeTable::new();
    let path = PathBuf::from("some/file.txt");
    let ino = table.get_or_create_inode(&path);

    // Rename to self must not corrupt the inode table.
    table.rename(&path, &path);

    // The inode must still be reachable via path lookup.
    assert_eq!(
        table.get_or_create_inode(&path),
        ino,
        "inode changed after self-rename"
    );
    assert_eq!(
        table.get_path(ino),
        Some(path),
        "path lookup broken after self-rename"
    );
}

#[test]
fn rename_directory_rekeys_children() {
    let table = InodeTable::new();
    let dir = PathBuf::from("a");
    let child1 = PathBuf::from("a/b");
    let child2 = PathBuf::from("a/c");
    let grandchild = PathBuf::from("a/c/d");

    let dir_ino = table.get_or_create_inode(&dir);
    let c1_ino = table.get_or_create_inode(&child1);
    let c2_ino = table.get_or_create_inode(&child2);
    let gc_ino = table.get_or_create_inode(&grandchild);

    let new_dir = PathBuf::from("x");
    table.rename(&dir, &new_dir);

    // Directory itself is rekeyed.
    assert_eq!(table.get_path(dir_ino), Some(PathBuf::from("x")));
    // Children are rekeyed.
    assert_eq!(table.get_path(c1_ino), Some(PathBuf::from("x/b")));
    assert_eq!(table.get_path(c2_ino), Some(PathBuf::from("x/c")));
    // Grandchildren are rekeyed.
    assert_eq!(table.get_path(gc_ino), Some(PathBuf::from("x/c/d")));

    // Old paths should no longer resolve.
    assert_ne!(
        table.get_or_create_inode(&child1),
        c1_ino,
        "old child path should not resolve to original inode"
    );
}

#[test]
fn rename_directory_does_not_affect_siblings() {
    let table = InodeTable::new();
    // "ab" is a sibling of "a", not a child — the range query must not touch it.
    let dir = PathBuf::from("a");
    let child = PathBuf::from("a/b");
    let sibling = PathBuf::from("ab");

    table.get_or_create_inode(&dir);
    table.get_or_create_inode(&child);
    let sib_ino = table.get_or_create_inode(&sibling);

    let new_dir = PathBuf::from("x");
    table.rename(&dir, &new_dir);

    // Sibling must be untouched.
    assert_eq!(
        table.get_path(sib_ino),
        Some(PathBuf::from("ab")),
        "sibling 'ab' must not be affected by renaming 'a'"
    );
}
