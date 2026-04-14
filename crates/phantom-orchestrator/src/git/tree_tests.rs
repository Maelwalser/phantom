use super::*;

use crate::test_support::init_repo;

#[test]
fn test_build_tree_with_blobs_root_files() {
    let (dir, _ops) = init_repo(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
    let repo = git2::Repository::open(dir.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let base_tree = head.tree().unwrap();

    let files = vec![
        (PathBuf::from("a.txt"), b"modified-a".to_vec()),
        (PathBuf::from("c.txt"), b"new-c".to_vec()),
    ];

    let new_tree_oid = build_tree_with_blobs(&repo, &base_tree, &files).unwrap();
    let new_tree = repo.find_tree(new_tree_oid).unwrap();

    let a_blob = repo.find_blob(new_tree.get_name("a.txt").unwrap().id()).unwrap();
    assert_eq!(a_blob.content(), b"modified-a");

    let b_blob = repo.find_blob(new_tree.get_name("b.txt").unwrap().id()).unwrap();
    assert_eq!(b_blob.content(), b"bbb");

    let c_blob = repo.find_blob(new_tree.get_name("c.txt").unwrap().id()).unwrap();
    assert_eq!(c_blob.content(), b"new-c");
}

#[test]
fn test_build_tree_with_blobs_nested_paths() {
    let (dir, _ops) = init_repo(&[("src/main.rs", b"fn main() {}"), ("src/lib.rs", b"pub mod lib;")]);
    let repo = git2::Repository::open(dir.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let base_tree = head.tree().unwrap();

    let files = vec![
        (PathBuf::from("src/main.rs"), b"fn main() { new }".to_vec()),
        (PathBuf::from("src/utils/helper.rs"), b"pub fn help() {}".to_vec()),
    ];

    let new_tree_oid = build_tree_with_blobs(&repo, &base_tree, &files).unwrap();
    let new_tree = repo.find_tree(new_tree_oid).unwrap();

    let src_tree = repo.find_tree(new_tree.get_name("src").unwrap().id()).unwrap();
    let main_blob = repo.find_blob(src_tree.get_name("main.rs").unwrap().id()).unwrap();
    assert_eq!(main_blob.content(), b"fn main() { new }");

    let lib_blob = repo.find_blob(src_tree.get_name("lib.rs").unwrap().id()).unwrap();
    assert_eq!(lib_blob.content(), b"pub mod lib;");

    let utils_tree = repo.find_tree(src_tree.get_name("utils").unwrap().id()).unwrap();
    let helper_blob = repo.find_blob(utils_tree.get_name("helper.rs").unwrap().id()).unwrap();
    assert_eq!(helper_blob.content(), b"pub fn help() {}");
}

#[test]
fn test_build_tree_from_oids_root_files() {
    let (dir, _ops) = init_repo(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
    let repo = git2::Repository::open(dir.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let base_tree = head.tree().unwrap();

    let file_oids = vec![
        (PathBuf::from("a.txt"), repo.blob(b"modified-a").unwrap()),
        (PathBuf::from("c.txt"), repo.blob(b"new-c").unwrap()),
    ];

    let new_tree_oid = build_tree_from_oids(&repo, &base_tree, &file_oids).unwrap();
    let new_tree = repo.find_tree(new_tree_oid).unwrap();

    let a_blob = repo.find_blob(new_tree.get_name("a.txt").unwrap().id()).unwrap();
    assert_eq!(a_blob.content(), b"modified-a");

    let b_blob = repo.find_blob(new_tree.get_name("b.txt").unwrap().id()).unwrap();
    assert_eq!(b_blob.content(), b"bbb");

    let c_blob = repo.find_blob(new_tree.get_name("c.txt").unwrap().id()).unwrap();
    assert_eq!(c_blob.content(), b"new-c");
}

#[test]
fn test_build_tree_from_oids_nested_paths() {
    let (dir, _ops) = init_repo(&[("src/main.rs", b"fn main() {}"), ("src/lib.rs", b"pub mod lib;")]);
    let repo = git2::Repository::open(dir.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let base_tree = head.tree().unwrap();

    let file_oids = vec![
        (PathBuf::from("src/main.rs"), repo.blob(b"fn main() { new }").unwrap()),
        (PathBuf::from("src/utils/helper.rs"), repo.blob(b"pub fn help() {}").unwrap()),
    ];

    let new_tree_oid = build_tree_from_oids(&repo, &base_tree, &file_oids).unwrap();
    let new_tree = repo.find_tree(new_tree_oid).unwrap();

    let src_tree = repo.find_tree(new_tree.get_name("src").unwrap().id()).unwrap();
    let main_blob = repo.find_blob(src_tree.get_name("main.rs").unwrap().id()).unwrap();
    assert_eq!(main_blob.content(), b"fn main() { new }");

    let lib_blob = repo.find_blob(src_tree.get_name("lib.rs").unwrap().id()).unwrap();
    assert_eq!(lib_blob.content(), b"pub mod lib;");

    let utils_tree = repo.find_tree(src_tree.get_name("utils").unwrap().id()).unwrap();
    let helper_blob = repo.find_blob(utils_tree.get_name("helper.rs").unwrap().id()).unwrap();
    assert_eq!(helper_blob.content(), b"pub fn help() {}");
}

#[test]
fn test_create_blobs_from_content() {
    let (dir, _ops) = init_repo(&[("x.txt", b"x")]);
    let repo = git2::Repository::open(dir.path()).unwrap();

    let files = vec![
        (PathBuf::from("a.txt"), b"aaa".to_vec()),
        (PathBuf::from("b.txt"), b"bbb".to_vec()),
    ];

    let oids = create_blobs_from_content(&repo, &files).unwrap();
    assert_eq!(oids.len(), 2);
    assert_eq!(oids[0].0, PathBuf::from("a.txt"));
    assert_eq!(oids[1].0, PathBuf::from("b.txt"));

    let a_blob = repo.find_blob(oids[0].1).unwrap();
    assert_eq!(a_blob.content(), b"aaa");
    let b_blob = repo.find_blob(oids[1].1).unwrap();
    assert_eq!(b_blob.content(), b"bbb");
}

#[test]
fn test_create_blobs_from_overlay() {
    let (dir, _ops) = init_repo(&[("x.txt", b"x")]);
    let repo = git2::Repository::open(dir.path()).unwrap();

    let upper = tempfile::TempDir::new().unwrap();
    std::fs::write(upper.path().join("hello.txt"), b"hello").unwrap();
    std::fs::create_dir(upper.path().join("sub")).unwrap();
    std::fs::write(upper.path().join("sub/nested.txt"), b"nested").unwrap();

    let oids = create_blobs_from_overlay(&repo, upper.path()).unwrap();
    assert_eq!(oids.len(), 2);

    for (path, oid) in &oids {
        let blob = repo.find_blob(*oid).unwrap();
        if path == &PathBuf::from("hello.txt") {
            assert_eq!(blob.content(), b"hello");
        } else if path == &PathBuf::from("sub/nested.txt") {
            assert_eq!(blob.content(), b"nested");
        } else {
            panic!("unexpected path: {path:?}");
        }
    }
}
