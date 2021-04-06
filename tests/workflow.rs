use assert_fs::prelude::*;
use assert_fs::TempDir;
use git2::build::CheckoutBuilder;
use git2::{CherrypickOptions, Index, Oid, Repository, RepositoryInitOptions};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::fs::remove_file;
use std::path::{Path, PathBuf};
#[allow(unused)]
use std::process::Command;

mod logger {
    use log::LevelFilter;

    #[derive(Debug)]
    struct Logger;

    use log::{Level, Metadata, Record};

    struct SimpleLogger;

    impl log::Log for SimpleLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            metadata.level() <= Level::Debug
        }

        fn log(&self, record: &Record) {
            if self.enabled(record.metadata()) {
                println!(
                    "[{:5}][{}] {}",
                    record.level(),
                    record.target(),
                    record.args()
                );
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: &SimpleLogger = &SimpleLogger;

    pub fn init() {
        log::set_logger(LOGGER)
            .map(|()| log::set_max_level(LevelFilter::Debug))
            .unwrap();
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitFileStatus {
    pub index: GitStatus,
    pub workdir: GitStatus,
}

impl Default for GitFileStatus {
    fn default() -> Self {
        Self {
            index: GitStatus::Default,
            workdir: GitStatus::Default,
        }
    }
}

impl GitFileStatus {
    pub fn new(status: git2::Status) -> Self {
        Self {
            index: match status {
                s if s.contains(git2::Status::INDEX_NEW) => GitStatus::NewInIndex,
                s if s.contains(git2::Status::INDEX_DELETED) => GitStatus::Deleted,
                s if s.contains(git2::Status::INDEX_MODIFIED) => GitStatus::Modified,
                s if s.contains(git2::Status::INDEX_RENAMED) => GitStatus::Renamed,
                s if s.contains(git2::Status::INDEX_TYPECHANGE) => GitStatus::Typechange,
                _ => GitStatus::Unmodified,
            },

            workdir: match status {
                s if s.contains(git2::Status::WT_NEW) => GitStatus::NewInWorkdir,
                s if s.contains(git2::Status::WT_DELETED) => GitStatus::Deleted,
                s if s.contains(git2::Status::WT_MODIFIED) => GitStatus::Modified,
                s if s.contains(git2::Status::WT_RENAMED) => GitStatus::Renamed,
                s if s.contains(git2::Status::IGNORED) => GitStatus::Ignored,
                s if s.contains(git2::Status::WT_TYPECHANGE) => GitStatus::Typechange,
                s if s.contains(git2::Status::CONFLICTED) => GitStatus::Conflicted,
                _ => GitStatus::Unmodified,
            },
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GitStatus {
    /// No status info
    Default,
    /// No changes (got from git status)
    Unmodified,
    /// Entry is ignored item in workdir
    Ignored,
    /// Entry does not exist in old version (now in stage)
    NewInIndex,
    /// Entry does not exist in old version (not in stage)
    NewInWorkdir,
    /// Type of entry changed between old and new
    Typechange,
    /// Entry does not exist in new version
    Deleted,
    /// Entry was renamed between old and new
    Renamed,
    /// Entry content changed between old and new
    Modified,
    /// Entry in the index is conflicted
    Conflicted,
}

impl Default for GitStatus {
    fn default() -> Self {
        Self::Default
    }
}

pub struct GitCache {
    statuses: Vec<(PathBuf, git2::Status)>,
    _cached_dir: Option<PathBuf>,
}

fn splitpath(path: &Path) {
    debug!("Split path : {:?}", path);
    for component in path.components() {
        debug!("\t{:?}", component);
    }
}

impl GitCache {
    pub fn new(path: &Path) -> GitCache {
        let cachedir = std::fs::canonicalize(&path).unwrap();
        info!("Trying to retrieve Git statuses for {:?}", cachedir);

        let repo = match git2::Repository::discover(&path) {
            Ok(r) => r,
            Err(_e) => {
                warn!("Git discovery error: {:?}", _e);
                return Self::empty();
            }
        };

        if let Some(workdir) = repo.workdir().and_then(|x| std::fs::canonicalize(x).ok()) {
            let mut statuses = Vec::new();
            info!("Retrieving Git statuses for workdir {:?}", workdir);
            match repo.statuses(None) {
                Ok(status_list) => {
                    for status_entry in status_list.iter() {
                        let str_path = status_entry.path().unwrap();
                        let path: PathBuf =
                            str_path.split("/").collect::<Vec<_>>().iter().collect();
                        let path = workdir.join(path);
                        splitpath(&path);
                        let elem = (path, status_entry.status());
                        debug!("{:?}", elem);
                        statuses.push(elem);
                    }
                }
                Err(_e) => warn!("Git retrieve statuses error: {:?}", _e),
            }
            info!("GitCache path: {:?}", cachedir);

            GitCache {
                statuses,
                _cached_dir: Some(cachedir),
            }
        } else {
            debug!("No workdir");
            Self::empty()
        }
    }

    pub fn empty() -> Self {
        GitCache {
            statuses: Vec::new(),
            _cached_dir: None,
        }
    }

    pub fn get(&self, filepath: &PathBuf, is_directory: bool) -> Option<GitFileStatus> {
        debug!("Before canonicalize");
        splitpath(&filepath);
        match std::fs::canonicalize(filepath) {
            Ok(filename) => {
                splitpath(&filename);
                Some(self.inner_get(&filename, is_directory))
            }
            Err(_err) => {
                log::debug!("error {}", _err);
                None
            }
        }
    }

    fn inner_get(&self, filepath: &PathBuf, is_directory: bool) -> GitFileStatus {
        debug!("Look for [recurse={}] {:?}", is_directory, filepath);
        debug!(
            "Cache content=\n{1:#<20}\n{:?}\n{1:#<20}",
            self.statuses, ""
        );

        assert_eq!(
            filepath.to_string_lossy(),
            std::fs::canonicalize(&filepath).unwrap().to_string_lossy()
        );

        if is_directory {
            self.statuses
                .iter()
                .filter(|&x| x.0.starts_with(filepath))
                .inspect(|&x| debug!("\t{:?}", x.0))
                .map(|x| GitFileStatus::new(x.1))
                .fold(GitFileStatus::default(), |acc, x| GitFileStatus {
                    index: std::cmp::max(acc.index, x.index),
                    workdir: std::cmp::max(acc.workdir, x.workdir),
                })
        } else {
            self.statuses
                .iter()
                .find(|&x| filepath == &x.0)
                .map(|e| GitFileStatus::new(e.1))
                .unwrap_or_default()
        }
    }
}

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(e) => e,
            Err(e) => panic!("{} failed with {}", stringify!($e), e),
        }
    };
}

fn repo_init() -> (TempDir, Repository) {
    let td = t!(TempDir::new());
    let mut opts = RepositoryInitOptions::new();
    opts.initial_head("master");
    let repo = Repository::init_opts(td.path(), &opts).unwrap();
    {
        let mut config = t!(repo.config());
        t!(config.set_str("user.name", "name"));
        t!(config.set_str("user.email", "email"));
        let mut index = t!(repo.index());
        let id = t!(index.write_tree());
        let tree = t!(repo.find_tree(id));
        let sig = t!(repo.signature());
        t!(repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[]));
    }
    (td, repo)
}

fn commit(repo: &Repository, index: &mut Index, msg: &str) -> (Oid, Oid) {
    let tree_id = t!(index.write_tree());
    let tree = t!(repo.find_tree(tree_id));
    let sig = t!(repo.signature());
    let head_id = t!(repo.refname_to_id("HEAD"));
    let parent = t!(repo.find_commit(head_id));
    let commit = t!(repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent]));
    (commit, tree_id)
}

fn check_cache(root: &Path, statuses: &HashMap<&PathBuf, GitFileStatus>, msg: &str) {
    debug!("{:-<50}", "");
    let cache = GitCache::new(root);
    for (&path, status) in statuses.iter() {
        match std::fs::canonicalize(&root.join(path)) {
            Ok(filename) => {
                let is_directory = filename.is_dir();
                assert_eq!(
                    &cache.inner_get(&filename, is_directory),
                    status,
                    "Invalid status for file {} at stage {}",
                    filename.to_string_lossy(),
                    msg
                );
            }
            Err(_) => {}
        }
    }
}

#[test]
fn test_git_workflow() {
    logger::init();
    // rename as test_git_workflow
    let (root, repo) = repo_init();
    let mut index = repo.index().unwrap();
    let mut expected_statuses = HashMap::new();

    // Check now
    check_cache(root.path(), &expected_statuses, "initialization");

    let f0 = PathBuf::from(".gitignore");
    root.child(&f0).write_str("*.bak").unwrap();
    expected_statuses.insert(
        &f0,
        GitFileStatus {
            index: GitStatus::Unmodified,
            workdir: GitStatus::NewInWorkdir,
        },
    );

    let _success = Command::new("git")
        .current_dir(root.path())
        .arg("status")
        .status()
        .expect("Git status failed")
        .success();

    // Check now
    check_cache(root.path(), &expected_statuses, "new .gitignore");

    index.add_path(f0.as_path()).unwrap();

    // Check now
    check_cache(root.path(), &expected_statuses, "unstaged .gitignore");

    index.write().unwrap();
    *expected_statuses.get_mut(&f0).unwrap() = GitFileStatus {
        index: GitStatus::NewInIndex,
        workdir: GitStatus::Unmodified,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "staged .gitignore");

    commit(&repo, &mut index, "Add gitignore");
    *expected_statuses.get_mut(&f0).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Committed .gitignore");

    let d1 = PathBuf::from("d1");
    let f1 = d1.join("f1");
    root.child(&f1).touch().unwrap();
    let f2 = d1.join("f2.bak");
    root.child(&f2).touch().unwrap();
    expected_statuses.insert(
        &d1,
        GitFileStatus {
            index: GitStatus::Unmodified,
            workdir: GitStatus::NewInWorkdir,
        },
    );
    expected_statuses.insert(
        &f1,
        GitFileStatus {
            index: GitStatus::Unmodified,
            workdir: GitStatus::NewInWorkdir,
        },
    );
    expected_statuses.insert(
        &f2,
        GitFileStatus {
            index: GitStatus::Unmodified,
            workdir: GitStatus::Ignored,
        },
    );

    // Check now
    check_cache(root.path(), &expected_statuses, "New files");

    index.add_path(f1.as_path()).unwrap();
    index.write().unwrap();
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::NewInIndex,
        workdir: GitStatus::Ignored,
    };
    *expected_statuses.get_mut(&f1).unwrap() = GitFileStatus {
        index: GitStatus::NewInIndex,
        workdir: GitStatus::Unmodified,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Unstaged new files");

    index.add_path(f2.as_path()).unwrap();
    index.write().unwrap();
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::NewInIndex,
        workdir: GitStatus::Unmodified,
    };
    *expected_statuses.get_mut(&f2).unwrap() = GitFileStatus {
        index: GitStatus::NewInIndex,
        workdir: GitStatus::Unmodified,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Staged new files");

    let (commit1_oid, _) = commit(&repo, &mut index, "Add new files");
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };
    *expected_statuses.get_mut(&f1).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };
    *expected_statuses.get_mut(&f2).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Committed new files");

    remove_file(&root.child(&f2).path()).unwrap();
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Deleted,
    };
    *expected_statuses.get_mut(&f2).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Deleted,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Remove file");

    root.child(&f1).write_str("New content").unwrap();
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Modified,
    }; // more important to see modified vs deleted ?
    *expected_statuses.get_mut(&f1).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Modified,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Change file");

    index.remove_path(&f2).unwrap();
    index.write().unwrap();
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Deleted,
        workdir: GitStatus::Modified,
    };
    *expected_statuses.get_mut(&f2).unwrap() = GitFileStatus {
        index: GitStatus::Deleted,
        workdir: GitStatus::Unmodified,
    };

    // Check now
    check_cache(root.path(), &expected_statuses, "Staged changes");

    commit(&repo, &mut index, "Remove backup file");
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Modified,
    };
    *expected_statuses.get_mut(&f2).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };

    // Check now
    check_cache(
        root.path(),
        &expected_statuses,
        "Committed changes (first part)",
    );

    index.add_path(&f1).unwrap();
    index.write().unwrap();
    commit(&repo, &mut index, "Save modified file");
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };
    *expected_statuses.get_mut(&f1).unwrap() = GitFileStatus {
        index: GitStatus::Default,
        workdir: GitStatus::Default,
    };

    // Check now
    check_cache(
        root.path(),
        &expected_statuses,
        "Committed changes (second part)",
    );

    let branch_commit = repo.find_commit(commit1_oid).unwrap();
    let branch = repo
        .branch("conflict-branch", &branch_commit, true)
        .unwrap();
    repo.set_head(format!("refs/heads/{}", branch.name().unwrap().unwrap()).as_str())
        .unwrap();
    let mut checkout_opts = CheckoutBuilder::new();
    checkout_opts.force();
    repo.checkout_head(Some(&mut checkout_opts)).unwrap();

    root.child(&f1)
        .write_str("New conflicting content")
        .unwrap();
    root.child(&f2)
        .write_str("New conflicting content")
        .unwrap();
    index.add_path(&f1).unwrap();
    index.add_path(&f2).unwrap();
    index.write().unwrap();
    let (commit2_oid, _) = commit(&repo, &mut index, "Save conflicting changes");

    // Check now
    check_cache(
        root.path(),
        &expected_statuses,
        "Committed changes in branch",
    );

    repo.set_head("refs/heads/master").unwrap();
    repo.checkout_head(Some(&mut checkout_opts)).unwrap();
    let mut cherrypick_opts = CherrypickOptions::new();
    let branch_commit = repo.find_commit(commit2_oid).unwrap();
    repo.cherrypick(&branch_commit, Some(&mut cherrypick_opts))
        .unwrap();
    *expected_statuses.get_mut(&d1).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Conflicted,
    };
    *expected_statuses.get_mut(&f1).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Conflicted,
    };
    *expected_statuses.get_mut(&f2).unwrap() = GitFileStatus {
        index: GitStatus::Unmodified,
        workdir: GitStatus::Conflicted,
    };

    // let _success = Command::new("git")
    //     .current_dir(root.path())
    //     .arg("status")
    //     .status()
    //     .expect("Git status failed")
    //     .success();

    // Check now
    check_cache(
        root.path(),
        &expected_statuses,
        "Conflict between master and branch",
    );
}
