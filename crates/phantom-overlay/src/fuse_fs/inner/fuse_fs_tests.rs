use super::*;

/// Helper: simulate paginated readdir over a snapshot, collecting all
/// returned entries.  Mimics the kernel calling readdir repeatedly with
/// the last cookie returned.
fn collect_readdir(snapshot: &DirSnapshot, page_size: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut offset: u64 = 0;
    loop {
        let start = offset as usize;
        let mut added = 0;
        for (idx, (_ino, _ft, name)) in snapshot.entries.iter().enumerate().skip(start) {
            let cookie = (idx as u64) + 1;
            result.push(name.clone());
            offset = cookie;
            added += 1;
            if added >= page_size {
                break;
            }
        }
        if added == 0 {
            break;
        }
    }
    result
}

#[test]
fn readdir_sequential_returns_all_entries() {
    let snapshot = DirSnapshot {
        entries: vec![
            (1, FileType::Directory, ".".into()),
            (1, FileType::Directory, "..".into()),
            (2, FileType::RegularFile, "a.txt".into()),
            (3, FileType::RegularFile, "b.txt".into()),
            (4, FileType::RegularFile, "c.txt".into()),
        ],
    };

    // Page size of 2 forces multiple readdir rounds.
    let names = collect_readdir(&snapshot, 2);
    assert_eq!(names, vec![".", "..", "a.txt", "b.txt", "c.txt"]);
}

#[test]
fn readdir_single_page_returns_all() {
    let snapshot = DirSnapshot {
        entries: vec![
            (1, FileType::Directory, ".".into()),
            (1, FileType::Directory, "..".into()),
            (2, FileType::RegularFile, "only.txt".into()),
        ],
    };

    let names = collect_readdir(&snapshot, 100);
    assert_eq!(names, vec![".", "..", "only.txt"]);
}

#[test]
fn readdir_empty_directory() {
    let snapshot = DirSnapshot {
        entries: vec![
            (1, FileType::Directory, ".".into()),
            (1, FileType::Directory, "..".into()),
        ],
    };

    let names = collect_readdir(&snapshot, 1);
    assert_eq!(names, vec![".", ".."]);
}

#[test]
fn readdir_page_size_one_returns_all() {
    let snapshot = DirSnapshot {
        entries: vec![
            (1, FileType::Directory, ".".into()),
            (1, FileType::Directory, "..".into()),
            (10, FileType::RegularFile, "x".into()),
            (11, FileType::RegularFile, "y".into()),
            (12, FileType::RegularFile, "z".into()),
        ],
    };

    // Page size 1 = worst-case pagination.
    let names = collect_readdir(&snapshot, 1);
    assert_eq!(names, vec![".", "..", "x", "y", "z"]);
}

#[test]
fn readdir_no_duplicate_entries() {
    let entries: Vec<(u64, FileType, String)> = (0..50)
        .map(|i| (i + 2, FileType::RegularFile, format!("file_{i:04}.txt")))
        .collect();
    let mut all: Vec<(u64, FileType, String)> = vec![
        (1, FileType::Directory, ".".into()),
        (1, FileType::Directory, "..".into()),
    ];
    all.extend(entries);
    let snapshot = DirSnapshot { entries: all };

    let names = collect_readdir(&snapshot, 7);
    // Verify no duplicates and correct count.
    assert_eq!(names.len(), 52);
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), 52, "readdir produced duplicate entries");
}
