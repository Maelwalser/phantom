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
