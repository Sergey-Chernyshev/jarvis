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
            Digest::new(
                "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )
            .unwrap()
        );
    }

    #[test]
    fn merkle_roots_for_one_two_three_and_five_leaves_are_stable() {
        let files = vec![
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
        let three = vec![
            file("plugin.json", b'a'),
            file("a", b'b'),
            file("b", b'c'),
        ];
        let mut changed = three.clone();
        changed[2].mode = PackageFileMode::Executable;
        assert_ne!(
            merkle_root(&three).unwrap(),
            merkle_root(&changed).unwrap()
        );
    }
}
