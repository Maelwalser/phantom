//! Lossless conversions between [`phantom_core::GitOid`] and [`git2::Oid`].

use phantom_core::id::GitOid;

/// Convert a `git2::Oid` into a `GitOid`.
#[must_use]
pub fn oid_to_git_oid(oid: git2::Oid) -> GitOid {
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(oid.as_bytes());
    GitOid(bytes)
}

/// Convert a `GitOid` into a `git2::Oid`.
pub fn git_oid_to_oid(oid: &GitOid) -> Result<git2::Oid, git2::Error> {
    git2::Oid::from_bytes(&oid.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_oid_roundtrip() {
        let hex = "aabbccddee00112233445566778899aabbccddee";
        let original = git2::Oid::from_str(hex).unwrap();

        let phantom_oid = oid_to_git_oid(original);
        let recovered = git_oid_to_oid(&phantom_oid).unwrap();

        assert_eq!(original, recovered);
    }
}
