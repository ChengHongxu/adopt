mod common;

use adopt::adapters::{Adapter, PkgSpec, apt::Apt};
use common::Answer::*;
use common::*;

fn spec(n: &str) -> PkgSpec {
    PkgSpec {
        logical: n.into(),
        native: n.into(),
        body: None,
    }
}

#[test]
fn init_baselines_apt_and_only_new_packages_are_asked() {
    let env = Env::new();
    env.with_apt(&["bash", "coreutils", "openssh-server"], &[]);
    let out = env.ok(&["init", "--yes", "--groups", "work"], []);
    assert!(out.contains("apt：记录了 3 个已装的包作为基线"), "{out}");
    let local = env.local_toml();
    assert!(local.contains("[baseline]"), "{local}");
    assert!(local.contains("\"openssh-server\""), "{local}");

    // Nothing new yet: no prompts.
    env.ok(&[], []);

    env.sys.apt_manual.borrow_mut().insert("htop".into());
    env.sys.apt_manual.borrow_mut().insert("libssl-dev".into());
    let out = env.ok(&[], [Key('a'), Key('l')]);
    assert!(out.contains("本机新装了 2 个软件"), "{out}");
    assert!(out.contains("已提交并推送：+htop(apt)"), "{out}");

    let pkgs = env.packages_toml();
    assert!(pkgs.contains("[system.htop]"), "{pkgs}");
    assert!(!pkgs.contains("libssl-dev"), "{pkgs}");
    assert!(env.local_toml().contains("[local.system.libssl-dev]"));

    // Both are accounted for now.
    env.ok(&[], []);
    assert_eq!(env.remote_log()[0], "adopt(testhost): +htop(apt)");
}

#[test]
fn rebaseline_keeps_or_retakes() {
    let env = Env::new();
    env.with_apt(&["bash"], &[]);
    env.init("");
    env.sys.apt_manual.borrow_mut().insert("htop".into());

    let out = env.ok(&["init", "--yes"], []);
    assert!(out.contains("已有基线，保留"), "{out}");
    env.ok(&[], [Key('s')]); // htop still new

    env.ok(&["init", "--yes", "--rebaseline"], []);
    env.ok(&[], []); // htop is baseline now
}

#[test]
fn install_uses_sudo_and_retries_each_on_batch_failure() {
    let env = Env::new();
    env.with_apt(&[], &["htop", "tmux"]);
    let script = Script::default();
    let (res, out) = env.with_app(&script, true, |app| {
        Apt.install(&app.ctx(), &[spec("htop"), spec("nope"), spec("tmux")])
            .unwrap()
    });
    let results: Vec<_> = res
        .iter()
        .map(|o| (o.logical.as_str(), o.result.is_ok()))
        .collect();
    assert_eq!(results, [("htop", true), ("nope", false), ("tmux", true)]);
    assert!(out.contains("逐个重试"), "{out}");
    assert!(env.apt_installed("htop") && env.apt_installed("tmux"));
    assert!(
        env.sys
            .ran("sudo env DEBIAN_FRONTEND=noninteractive apt-get -y -q install htop nope tmux")
    );
    let err = res[1].result.as_ref().unwrap_err();
    assert!(err.contains("install nope") && err.contains("100"), "{err}");
}

#[test]
fn root_runs_without_sudo() {
    let env = Env::new();
    env.with_apt(&[], &["htop"]);
    *env.sys.uid.borrow_mut() = 0;
    let script = Script::default();
    let (res, _) = env.with_app(&script, false, |app| {
        Apt.install(&app.ctx(), &[spec("htop")]).unwrap()
    });
    assert!(res[0].result.is_ok());
    assert!(!env.sys.ran("sudo"));
}

#[test]
fn non_interactive_without_passwordless_sudo_skips() {
    let env = Env::new();
    env.with_apt(&[], &["htop"]);
    *env.sys.sudo_ok.borrow_mut() = false;
    let script = Script::default();
    let (res, _) = env.with_app(&script, false, |app| {
        Apt.install(&app.ctx(), &[spec("htop")]).unwrap()
    });
    let err = res[0].result.as_ref().unwrap_err();
    assert!(err.contains("sudo 需要密码"), "{err}");
    assert!(!env.apt_installed("htop"));
    assert!(!env.sys.ran("apt-get"));
}

#[test]
fn no_sudo_at_all_skips() {
    let env = Env::new();
    env.with_apt(&[], &["htop"]);
    env.sys.programs.borrow_mut().remove("sudo");
    let script = Script::default();
    let (res, _) = env.with_app(&script, true, |app| {
        Apt.remove(&app.ctx(), &[spec("htop")]).unwrap()
    });
    assert!(res[0].result.as_ref().unwrap_err().contains("没有 sudo"));
}

#[test]
fn dry_run_installs_nothing() {
    let env = Env::new();
    env.with_apt(&[], &["htop"]);
    let script = Script::default();
    let (ui, buf) = adopt::ui::Ui::capture();
    let app = adopt::App {
        paths: env.paths.clone(),
        runner: &env.sys,
        prompter: &script,
        ui: &ui,
        dry_run: true,
        interactive: true,
        hostname: Some(HOST.into()),
        privilege: Default::default(),
    };
    let res = Apt.install(&app.ctx(), &[spec("htop")]).unwrap();
    assert!(res[0].result.is_ok());
    assert!(!env.apt_installed("htop"));
    assert!(
        buf.contents()
            .contains("[dry-run] sudo env DEBIAN_FRONTEND")
    );
}

#[test]
fn shared_entry_with_other_apt_name_is_not_new() {
    let env = Env::new();
    env.with_apt(&["bash"], &[]);
    env.init("");
    std::fs::write(
        env.repo.join("packages.toml"),
        "version = 1\n\n[system.fd]\napt = \"fd-find\"\n\n[system.java]\napt = false\n",
    )
    .unwrap();
    env.git(&env.repo, &["commit", "--quiet", "-am", "fd"]);
    env.sys.apt_manual.borrow_mut().insert("fd-find".into());
    env.sys.apt_manual.borrow_mut().insert("java".into());
    env.ok(&[], []);
}
