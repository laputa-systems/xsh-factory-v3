use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use factory_kernel::cas::{CasError, CasStore};
use factory_protocol::ContentDigest;

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let suffix = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "factory-kernel-cas-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("unique test root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn adopts_exact_digest_and_length_at_the_planned_layout() {
    let root = TestRoot::new();
    let staging = root.path().join("staging");
    fs::create_dir(&staging).expect("staging");
    fs::write(staging.join("packet.bin"), b"exact bytes").expect("source");
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 1024, 1).expect("CAS");

    let adopted = cas.adopt(&staging, "packet.bin").expect("adopt");
    assert_eq!(adopted.digest(), ContentDigest::of_bytes(b"exact bytes"));
    assert_eq!(adopted.byte_length(), 11);
    let digest_hex = adopted.digest().to_hex();
    assert_eq!(
        cas.object_path(adopted.digest()),
        fs::canonicalize(root.path().join("runtime"))
            .expect("canonical runtime")
            .join("objects/blake3")
            .join(&digest_hex[..2])
            .join(&digest_hex[2..])
    );
    assert_eq!(
        cas.read_verified(adopted.digest()).expect("read"),
        b"exact bytes"
    );
    assert_eq!(cas.verify(adopted.digest()).expect("verify"), adopted);
}

#[test]
fn duplicate_adoption_reuses_the_object_and_does_not_rewrite_it() {
    let root = TestRoot::new();
    let staging = root.path().join("staging");
    fs::create_dir(&staging).expect("staging");
    fs::write(staging.join("packet.bin"), b"same bytes").expect("source");
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 1024, 2).expect("CAS");

    let first = cas.adopt(&staging, "packet.bin").expect("first adopt");
    let object = cas.object_path(first.digest());
    let first_metadata = fs::metadata(&object).expect("object metadata");
    let second = cas.adopt(&staging, "packet.bin").expect("duplicate adopt");
    let second_metadata = fs::metadata(&object).expect("object metadata");
    assert_eq!(first, second);
    assert_eq!(first_metadata.len(), second_metadata.len());
    assert_eq!(
        cas.read_verified(first.digest()).expect("read"),
        b"same bytes"
    );
}

#[test]
fn append_only_retains_prior_objects_and_rejects_corruption() {
    let root = TestRoot::new();
    let staging = root.path().join("staging");
    fs::create_dir(&staging).expect("staging");
    let source = staging.join("packet.bin");
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 1024, 3).expect("CAS");

    fs::write(&source, b"first").expect("source");
    let first = cas.adopt(&staging, "packet.bin").expect("first adopt");
    fs::write(&source, b"second").expect("source");
    let second = cas.adopt(&staging, "packet.bin").expect("second adopt");
    assert_ne!(first.digest(), second.digest());
    assert_eq!(
        cas.read_verified(first.digest()).expect("old object"),
        b"first"
    );
    assert_eq!(
        cas.read_verified(second.digest()).expect("new object"),
        b"second"
    );

    fs::write(cas.object_path(first.digest()), b"xxxxx").expect("corrupt object");
    assert!(matches!(
        cas.read_verified(first.digest()),
        Err(CasError::CorruptObject { .. })
    ));
    fs::write(&source, b"first").expect("source");
    assert!(matches!(
        cas.adopt(&staging, "packet.bin"),
        Err(CasError::CorruptObject { .. })
    ));
}

#[test]
fn size_limit_rejects_before_sealing_and_leaves_no_object() {
    let root = TestRoot::new();
    let staging = root.path().join("staging");
    fs::create_dir(&staging).expect("staging");
    fs::write(staging.join("too-large"), b"123456").expect("source");
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 5, 4).expect("CAS");

    assert!(matches!(
        cas.adopt(&staging, "too-large"),
        Err(CasError::SizeLimitExceeded { .. })
    ));
    let objects = root.path().join("runtime/objects/blake3");
    let files = fs::read_dir(&objects)
        .expect("objects root")
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    assert!(files.is_empty(), "oversized adoption left an object");
}

#[test]
fn rejects_escape_and_symlinked_staging_paths() {
    let root = TestRoot::new();
    let staging = root.path().join("staging");
    let outside = root.path().join("outside");
    fs::create_dir(&staging).expect("staging");
    fs::create_dir(&outside).expect("outside");
    fs::write(outside.join("secret"), b"secret").expect("outside source");
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 1024, 5).expect("CAS");

    assert!(matches!(
        cas.adopt(&staging, "../outside/secret"),
        Err(CasError::UnsafeSourcePath { .. })
    ));

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret"), staging.join("link")).expect("symlink");
        assert!(matches!(
            cas.adopt(&staging, "link"),
            Err(CasError::SourceSymlink { .. })
        ));
    }
}

#[test]
fn zero_limit_and_missing_reads_are_rejected() {
    let root = TestRoot::new();
    assert!(matches!(
        CasStore::new(root.path().join("runtime"), 0, 6),
        Err(CasError::InvalidLimit)
    ));
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 1024, 6).expect("CAS");
    let missing = ContentDigest::of_bytes(b"missing");
    assert!(matches!(
        cas.read_verified(missing),
        Err(CasError::MissingObject { .. })
    ));
}

#[cfg(unix)]
#[test]
fn rejects_digest_directory_symlink_escape() {
    let root = TestRoot::new();
    let staging = root.path().join("staging");
    let outside = root.path().join("outside");
    fs::create_dir(&staging).expect("staging");
    fs::create_dir(&outside).expect("outside");
    fs::write(staging.join("packet"), b"directory escape").expect("source");
    let cas = CasStore::new_with_seed(root.path().join("runtime"), 1024, 10).expect("CAS");
    let digest = ContentDigest::of_bytes(b"directory escape");
    let object_path = cas.object_path(digest);
    let parent = object_path.parent().expect("object parent");
    std::os::unix::fs::symlink(&outside, parent).expect("directory symlink");

    assert!(matches!(
        cas.adopt(&staging, "packet"),
        Err(CasError::SourcePathEscape { .. })
    ));
    assert!(fs::read_dir(outside).expect("outside").next().is_none());
}
