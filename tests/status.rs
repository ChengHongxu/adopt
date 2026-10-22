//! `adopt status`, `adopt list`, and the optional apt hook from `adopt init`.

mod common;

use std::fs;

use adopt::model::Section;
use adopt::state::{Entry, Local};
use common::Answer::*;
use common::*;

#[test]
fn prompt_counts_and_only_recomputes_when_something_changed() {
    let env = Env::new();
    env.with_apt(&["bash"], &[]);
    env.init("");
    assert_eq!(
        env.batch(&["status", "--prompt"]),
        "",
        "zero prints nothing"
    );

    env.apt_add("htop");
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    let runs = env.count_runs("apt-mark showmanual");
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    assert_eq!(
        env.count_runs("apt-mark showmanual"),
        runs,
        "a cache hit must not run apt-mark"
    );

    env.apt_add("tmux");
    assert_eq!(env.batch(&["status", "--prompt"]), "2\n");
    env.write_pixi(PIXI_TWO_ENVS);
    assert_eq!(env.batch(&["status", "--prompt"]), "4\n");

    // Deciding everything rewrites local.toml and refreshes the cache.
    env.ok(&[], [Key('i'), Key('i'), Key('i'), Key('i')]);
    let runs = env.count_runs("apt-mark showmanual");
    assert_eq!(env.batch(&["status", "--prompt"]), "");
    assert_eq!(
        env.count_runs("apt-mark showmanual"),
        runs,
        "adopt refreshed the cache"
    );
}

#[test]
fn unapplied_shared_changes_count_too() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop"]);
    env.init("");
    fs::write(
        env.repo.join("packages.toml"),
        "version = 1\n[system.htop]\n",
    )
    .unwrap();
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    let out = env.batch(&["status"]);
    assert!(out.contains("1 个条目待处理"), "{out}");
}

#[test]
fn prompt_is_silent_before_init() {
    let env = Env::new();
    assert_eq!(env.batch(&["status", "--prompt"]), "");
    let (r, _) = env.run_as(&["status"], &Script::default(), false);
    assert!(format!("{:#}", r.unwrap_err()).contains("adopt init"));
}

#[test]
fn list_shows_groups_mappings_and_local_entries() {
    let env = Env::new();
    env.with_apt(&["bash"], &[]);
    env.init("work");
    fs::write(
        env.repo.join("packages.toml"),
        "version = 1\n[system.fd]\napt = \"fd-find\"\n[system.tmux]\ngroups = [\"homelab\"]\n[system.htop]\n",
    )
    .unwrap();
    let mut l = Local::load(&env.paths).unwrap();
    l.add_exception(Section::System, "htop");
    l.upsert_entry(&Entry::system("libssl-dev"));
    l.file.save().unwrap();

    let out = env.ok(&["list"], []);
    let line = |name: &str| {
        out.lines()
            .find(|l| l.trim_start().starts_with(name))
            .unwrap_or_else(|| panic!("no line for {name}: {out}"))
            .to_string()
    };
    assert!(out.contains("本机分组：work"), "{out}");
    assert!(line("fd").contains("apt: fd-find"), "{out}");
    assert!(line("tmux").contains("@homelab") && line("tmux").contains("不属于本机分组"));
    assert!(line("htop").contains("本机例外"), "{out}");
    assert!(line("libssl-dev").contains("仅本机"), "{out}");
}

#[test]
fn apt_hook_is_only_installed_with_consent() {
    let env = Env::new();
    env.with_apt(&["bash"], &[]);
    let hook = env.dir.path().join("root/etc/apt/apt.conf.d/99adopt");
    env.ok(&["init", "--yes"], []);
    assert!(!hook.exists(), "never without consent");
    assert!(!env.sys.ran("99adopt"));

    let out = env.ok(&["init", "--yes", "--apt-hook"], []);
    assert!(out.contains("已安装 /etc/apt/apt.conf.d/99adopt"), "{out}");
    assert!(env.read(&hook).contains("DPkg::Post-Invoke"));
    assert!(env.sys.ran("sudo sh -c"), "written as root");
}
