use std::fmt::Write as _;

use jarvis_plugin_protocol::manifest::Digest;
use jarvis_plugin_protocol::package::PackageFile;
use sha2::{Digest as _, Sha256};

const FILE_DOMAIN: &[u8] = b"jarvis-plugin-file-v1\0";
const MERKLE_DOMAIN: &[u8] = b"jarvis-plugin-merkle-v1\0";

pub(crate) fn sha256_digest(bytes: &[u8]) -> Digest {
    digest_from_bytes(Sha256::digest(bytes).into())
}

pub(crate) fn merkle_root(files: &[PackageFile]) -> Result<Digest, &'static str> {
    if files.is_empty() {
        return Err("package metadata requires plugin.json");
    }
    let mut level = Vec::with_capacity(files.len());
    for file in files {
        let canonical =
            serde_json_canonicalizer::to_vec(file).map_err(|_| "package file JCS failed")?;
        let mut hasher = Sha256::new();
        hasher.update(FILE_DOMAIN);
        hasher.update(canonical);
        level.push(<[u8; 32]>::from(hasher.finalize()));
    }

    while level.len() > 1 {
        let mut parents = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let left = pair[0];
            let right = pair.get(1).copied().unwrap_or(left);
            let mut hasher = Sha256::new();
            hasher.update(MERKLE_DOMAIN);
            hasher.update(left);
            hasher.update(right);
            parents.push(<[u8; 32]>::from(hasher.finalize()));
        }
        level = parents;
    }

    Ok(digest_from_bytes(level[0]))
}

fn digest_from_bytes(bytes: [u8; 32]) -> Digest {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("writing into a String cannot fail");
    }
    Digest::new(value).expect("lowercase SHA-256 bytes always form a valid digest")
}

#[cfg(test)]
mod tests {
    use jarvis_plugin_protocol::manifest::Digest;
    use jarvis_plugin_protocol::package::{
        PackageFile, PackageFileKind, PackageFileMode, PackagePath,
    };

    use super::{merkle_root, sha256_digest};

    fn file(path: &str, byte: u8) -> PackageFile {
        PackageFile {
            path: PackagePath::new(path).unwrap(),
            kind: PackageFileKind::Regular,
            mode: PackageFileMode::ReadOnly,
            size: 1,
            digest: sha256_digest(&[byte]),
        }
    }

    #[test]
    fn sha256_digest_is_lowercase_and_domain_free() {
        assert_eq!(
            sha256_digest(b""),
            Digest::new("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap()
        );
    }

    #[test]
    fn merkle_roots_for_one_two_three_and_five_leaves_are_stable() {
        let files = [
            file("plugin.json", b'a'),
            file("a", b'b'),
            file("b", b'c'),
            file("c", b'd'),
            file("d", b'e'),
        ];
        for (count, expected) in [
            (
                1,
                "sha256:a7d38c33b09206fab60a6cdb85428fd5f4b81562661afeee5a5fdad5027e200d",
            ),
            (
                2,
                "sha256:b33f828382627b269c1a9beff3d0251166726ea1dda9ea501257fd25f6e193c4",
            ),
            (
                3,
                "sha256:ed554f96bc6997104ddbc754f53f86a2737d1dab42b4af4d7e748141e11fdb0a",
            ),
            (
                5,
                "sha256:5269f48591b7f450f0d0966ff6f45e364d6fef86c17c2a5e41a0cbeb5ae4c1fe",
            ),
        ] {
            assert_eq!(
                merkle_root(&files[..count]).unwrap(),
                Digest::new(expected).unwrap(),
                "{count} leaves"
            );
        }
    }

    #[test]
    fn odd_levels_duplicate_the_final_node() {
        let three = vec![file("plugin.json", b'a'), file("a", b'b'), file("b", b'c')];
        let mut changed = three.clone();
        changed[2].mode = PackageFileMode::Executable;
        assert_ne!(merkle_root(&three).unwrap(), merkle_root(&changed).unwrap());
    }
}
