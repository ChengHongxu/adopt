mod common;

use std::fs;

use common::{Answer::*, Env, PIXI_TWO_ENVS, Script};

#[test]
fn init_creates_manifest_ignores_it_for_chezmoi_and_pushes() {
    let env = Env::new();
    env.init("work, homelab");

    let local = env.local_toml();
    assert!(
        local.contains(&format!("repo = \"{}\"", env.repo.display())),
        "{local}"
    );
    assert!(local.contains(r#"groups = ["homelab", "work"]"#), "{local}");
    assert!(env.packages_toml().contains("version = 1"));
    assert_eq!(
        env.read(&env.repo.join(".chezmoiignore")),
        "packages.toml\n"
    );
    assert_eq!(env.remote_log()[0], "adopt(testhost): init");

    // Running it again changes nothing and makes no new commit.
    env.init("work,homelab");
    assert_eq!(env.remote_log().len(), 2);
}

#[test]
fn claim_all_local_and_ignore() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("work");

    // Sorted by name: ripgrep, setproxy.
    let out = env.ok(&[], [Key('a'), Key('l')]);
    assert!(out.contains("本机新装了 2 个软件"), "{out}");
    assert!(out.contains("已提交并推送：+ripgrep(pixi)"), "{out}");

    let shared = env.packages_toml();
    assert!(
        shared.contains("[pixi.ripgrep]\nchannels = [\"conda-forge\"]"),
        "{shared}"
    );
    assert!(shared.contains("exposed = { rg = \"rg\" }"), "{shared}");
    assert!(
        !shared.contains("setproxy"),
        "local-only stays out of git: {shared}"
    );

    let local = env.local_toml();
    assert!(local.contains("[local.pixi.setproxy]"), "{local}");
    assert!(local.contains("[applied.pixi]"), "{local}");
    assert_eq!(env.remote_log()[0], "adopt(testhost): +ripgrep(pixi)");

    // Nothing left to ask about.
    let out = env.ok(&[], []);
    assert!(out.contains("没有需要提交的改动"), "{out}");
}

#[test]
fn claim_for_groups_and_ignore() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("work");

    env.ok(&[], [Key('g'), Text("homelab, work".into()), Key('i')]);
    let shared = env.packages_toml();
    assert!(
        shared.contains("groups = [\"homelab\", \"work\"]"),
        "{shared}"
    );
    assert!(
        env.local_toml().contains("[ignore]\npixi = [\"setproxy\"]"),
        "{}",
        env.local_toml()
    );
    assert_eq!(
        env.remote_log()[0],
        "adopt(testhost): +ripgrep(pixi@homelab,work)"
    );
    env.ok(&[], []);
}

#[test]
fn later_leaves_item_pending() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("");
    let out = env.ok(&[], [Key('s'), Key('s')]);
    assert!(out.contains("没有需要提交的改动"), "{out}");
    env.ok(&[], [Key('s'), Key('s')]);
}

#[test]
fn abort_writes_nothing() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("");
    let local_before = env.local_toml();
    let shared_before = env.packages_toml();

    let script = Script::new([Key('a'), Abort]);
    let (r, _) = env.run(&[], &script);
    assert!(r.unwrap_err().is::<adopt::prompt::Aborted>());
    assert_eq!(env.local_toml(), local_before);
    assert_eq!(env.packages_toml(), shared_before);
    assert_eq!(env.remote_log().len(), 2);
}

#[test]
fn dry_run_writes_nothing() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("");
    let local_before = env.local_toml();
    let out = env.ok(&["--dry-run"], [Key('a'), Key('a')]);
    assert!(out.contains("[dry-run] git push"), "{out}");
    assert_eq!(env.local_toml(), local_before);
    assert!(!env.packages_toml().contains("ripgrep"));
    assert_eq!(env.remote_log().len(), 2);
}

#[test]
fn pull_reports_incoming_commits() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("");
    let other = env.other_clone("laptop");
    fs::write(other.join("dot_vimrc"), "set nu\n").unwrap();
    env.git(&other, &["add", "."]);
    env.git(&other, &["commit", "--quiet", "-m", "laptop: vimrc"]);
    env.git(&other, &["push", "--quiet"]);

    let out = env.ok(&[], [Key('a'), Key('s')]);
    assert!(out.contains("↓ 拉取 1 个提交"), "{out}");
    assert_eq!(
        env.remote_log()[..2],
        ["adopt(testhost): +ripgrep(pixi)", "laptop: vimrc"]
    );
}

#[test]
fn rejected_push_rebases_then_pushes() {
    let env = Env::new();
    env.init("");
    let other = env.other_clone("laptop");
    fs::write(other.join("dot_vimrc"), "set nu\n").unwrap();
    env.git(&other, &["add", "."]);
    env.git(&other, &["commit", "--quiet", "-m", "laptop: vimrc"]);
    env.git(&other, &["push", "--quiet"]);

    let git = adopt::git::Git::new(&env.sys, &env.repo);
    fs::write(
        env.repo.join("packages.toml"),
        "version = 1\n[system.tmux]\n",
    )
    .unwrap();
    assert!(git.commit(&["packages.toml"], "local: tmux").unwrap());
    assert_eq!(git.push().unwrap(), adopt::git::Push::PushedAfterRebase);
    assert_eq!(env.remote_log()[..2], ["local: tmux", "laptop: vimrc"]);
}

#[test]
fn push_conflict_stops_without_force() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("");
    // Commit locally but make the remote diverge on the same file.
    let other = env.other_clone("laptop");
    fs::write(other.join("packages.toml"), "version = 1\n[system.htop]\n").unwrap();
    env.git(&other, &["commit", "--quiet", "-am", "laptop: htop"]);
    env.git(&other, &["push", "--quiet"]);

    let git = adopt::git::Git::new(&env.sys, &env.repo);
    fs::write(
        env.repo.join("packages.toml"),
        "version = 1\n[system.tmux]\n",
    )
    .unwrap();
    assert!(git.commit(&["packages.toml"], "local: tmux").unwrap());
    let err = git.push().unwrap_err();
    assert!(format!("{err:#}").contains("没有强推"), "{err:#}");
    assert_eq!(env.remote_log()[0], "laptop: htop", "remote untouched");
    // Repo is left usable, not mid-rebase.
    assert!(git.is_repo());
    assert!(
        env.git(&env.repo, &["status", "--porcelain"])
            .trim()
            .is_empty()
    );
}

#[test]
fn not_initialised_is_an_error() {
    let env = Env::new();
    let (r, _) = env.run(&[], &Script::default());
    assert!(format!("{:#}", r.unwrap_err()).contains("adopt init"));
}
