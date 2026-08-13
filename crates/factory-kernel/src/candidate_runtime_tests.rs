use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use factory_protocol::{
    ApprovedToolV1, CommandProfileV1, DurationMillis, ExecutableV1, RepositoryRelativePath,
};

use super::*;
use crate::command_supervision::{CommandExpectation, CommandStdin, ComparisonRevision};
use crate::git::{DefaultBranchName, WorktreeKind};

static NEXT_GIT_FIXTURE: AtomicU64 = AtomicU64::new(1);

fn command(name: &str, argv: &[&str]) -> DeterministicCommand {
    DeterministicCommand::new(
        CommandProfileV1 {
            name: name.to_owned(),
            executable: ExecutableV1::ApprovedTool(ApprovedToolV1::Cargo),
            argv: argv.iter().map(|argument| (*argument).to_owned()).collect(),
            working_directory: RepositoryRelativePath::parse(".").unwrap(),
            environment: Vec::new(),
            timeout: DurationMillis::new(1_000),
            stdout_byte_limit: 1_024,
            stderr_byte_limit: 1_024,
            expected_exit_status: 0,
        },
        CommandStdin::Empty,
        CommandExpectation::new(ComparisonRevision::parse("test-v1").unwrap(), None, None),
    )
    .unwrap()
}

#[test]
fn long_actor_command_ids_have_bounded_deterministic_kernel_evidence_ids() {
    let base = "a".repeat(160);
    let first = derived_command_id(&base, "hard-command-set").unwrap();
    let nested = derived_command_id(&first, "command-0-stdout").unwrap();
    assert_eq!(
        first,
        derived_command_id(&base, "hard-command-set").unwrap()
    );
    assert_ne!(first, derived_command_id(&base, "candidate-patch").unwrap());
    assert!(first.starts_with("candidate-runtime-"));
    assert!(first.len() <= 160);
    assert!(nested.len() <= 160);
    assert!(derived_command_id("invalid id", "log").is_err());
}

#[test]
fn full_validation_profile_is_an_exact_ordered_command_set() {
    let first = command("reproducer", &["test", "--exact", "ticket"]);
    let full = command("full", &["test", "--workspace"]);
    let declared = vec![first.profile().clone(), full.profile().clone()];
    assert!(exact_command_profiles(
        &declared,
        &[first.clone(), full.clone()]
    ));
    assert!(!exact_command_profiles(&declared, &[full, first]));
}

#[test]
fn forbidden_paths_are_exact_not_prefix_or_substring_rules() {
    let forbidden = vec![RepositoryRelativePath::parse(".factory/private").unwrap()];
    assert_eq!(
        forbidden_changed_path(
            &forbidden,
            &[
                ".factory/private-not-this".to_owned(),
                "src/.factory/private-copy".to_owned(),
                ".factory/private".to_owned(),
            ],
        ),
        Some(".factory/private")
    );
}

#[test]
fn repository_object_conversion_keeps_git_ids_distinct_from_content_digests() {
    assert!(repository_object(&"a".repeat(40)).is_ok());
    assert!(repository_object(&"b".repeat(64)).is_ok());
    assert!(repository_object(&"A".repeat(40)).is_err());
    assert!(repository_object("not-a-git-object").is_err());
}

#[test]
fn hard_candidate_whitespace_check_rejects_trailing_space_and_accepts_clean_tree() {
    let fixture = WhitespaceFixture::new();
    let repository = fixture.qualify();
    assert_eq!(repository.default_branch().as_str(), "main");
    let actor = fixture.actor(&repository, "whitespace-fail");
    fs::write(actor.path().join("trailing.txt"), b"trailing space \n")
        .expect("write synthetic whitespace defect");
    let captured = fixture.custody.capture_tree(&actor).expect("capture tree");
    let failed = fixture
        .custody
        .check_candidate_whitespace(&repository, captured.tree())
        .expect("run exact whitespace check");
    assert!(!failed.is_clean());
    assert!(
        String::from_utf8_lossy(failed.stdout()).contains("trailing whitespace")
            || String::from_utf8_lossy(failed.stderr()).contains("trailing whitespace")
    );
    fixture
        .custody
        .cleanup_worktree(actor)
        .expect("cleanup actor");

    let clean_actor = fixture.actor(&repository, "whitespace-pass");
    fs::write(clean_actor.path().join("clean.txt"), b"clean line\n")
        .expect("write synthetic clean candidate");
    let clean_capture = fixture
        .custody
        .capture_tree(&clean_actor)
        .expect("capture clean tree");
    assert!(
        fixture
            .custody
            .check_candidate_whitespace(&repository, clean_capture.tree())
            .expect("run clean whitespace check")
            .is_clean()
    );
    fixture
        .custody
        .cleanup_worktree(clean_actor)
        .expect("cleanup clean actor");
}

struct WhitespaceFixture {
    root: PathBuf,
    repository: PathBuf,
    custody: GitCustody,
}

impl WhitespaceFixture {
    fn new() -> Self {
        let number = NEXT_GIT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "factory-candidate-runtime-whitespace-{}-{number}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create fixture root");
        let git = system_git();
        run_git(&root, &git, &["init", "--initial-branch=main", "product"]);
        let repository = root.join("product");
        run_git(
            &repository,
            &git,
            &["config", "user.name", "Synthetic Tester"],
        );
        run_git(
            &repository,
            &git,
            &["config", "user.email", "synthetic@example.test"],
        );
        fs::write(repository.join("README.md"), b"base\n").expect("write base");
        run_git(&repository, &git, &["add", "README.md"]);
        run_git(&repository, &git, &["commit", "-m", "base"]);
        let custody = GitCustody::new(&git, root.join("runtime")).expect("create custody");
        Self {
            root,
            repository,
            custody,
        }
    }

    fn qualify(&self) -> QualifiedRepository {
        self.custody
            .qualify_repository(&self.repository, DefaultBranchName::parse("main").unwrap())
            .expect("qualify synthetic repository")
    }

    fn actor(&self, repository: &QualifiedRepository, name: &str) -> OwnedWorktree {
        self.custody
            .create_detached_worktree(
                repository,
                WorktreeKind::Actor,
                WorktreeName::parse(name).unwrap(),
            )
            .expect("create detached actor worktree")
    }
}

impl Drop for WhitespaceFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn system_git() -> PathBuf {
    for candidate in ["/usr/bin/git", "/usr/local/bin/git"] {
        if let Ok(path) = fs::canonicalize(candidate) {
            return path;
        }
    }
    panic!("synthetic Git test requires system Git")
}

fn run_git(root: &Path, git: &Path, argv: &[&str]) {
    let status = Command::new(git)
        .args(argv)
        .current_dir(root)
        .status()
        .expect("run synthetic Git");
    assert!(status.success(), "synthetic Git failed: {argv:?}");
}
