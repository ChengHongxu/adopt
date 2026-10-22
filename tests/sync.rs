//! `adopt sync` and the interactive handling of what sync leaves pending.

mod common;

use std::fs;

use adopt::model::Section;
use adopt::state::Local;
use common::Answer::*;
use common::*;

const RG: &str =
    "channels = [\"conda-forge\"]\ndependencies = { ripgrep = \"*\" }\nexposed = { rg = \"rg\" }\n";

fn local(env: &Env) -> Local {
    Local::load(&env.paths).unwrap()
}

// ---- rule 1: new shared entries are installed ----

#[test]
fn installs_new_shared_packages_for_this_machine_only() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop", "tmux", "ncdu"]);
    env.init("work");
    env.publish(
        "version = 1\n\n[system.htop]\n\n[system.tmux]\ngroups = [\"homelab\"]\n\n[system.ncdu]\napt = false\n",
    );
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("↓ 拉取 1 个提交"), "{out}");
    assert!(out.contains("已应用：安装 htop(apt)"), "{out}");
    assert!(env.apt_installed("htop"));
    assert!(!env.apt_installed("tmux"), "other group");
    assert!(!env.apt_installed("ncdu"), "apt = false");
    assert!(local(&env).applied(Section::System)["htop"].by_adopt);

    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("软件清单与本机一致"), "{out}");
    assert_eq!(env.count_runs("install htop"), 1);
}

// ---- rule 2: something the user uninstalled is never reinstalled silently ----

fn installed_then_removed_by_user() -> Env {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop"]);
    env.init("");
    env.publish("version = 1\n[system.htop]\n");
    env.batch(&["sync", "--yes"]);
    env.apt_remove("htop");
    let out = env.batch(&["sync", "--yes"]);
    assert!(!env.apt_installed("htop"), "must not reinstall: {out}");
    assert!(out.contains("1 个条目等你处理"), "{out}");
    assert_eq!(env.count_runs("install htop"), 1);
    env
}

#[test]
fn removed_by_user_then_kept_off_this_machine() {
    let env = installed_then_removed_by_user();
    let out = env.ok(&[], [Key('k')]);
    assert!(out.contains("本机缺少清单里的 1 个软件"), "{out}");
    assert!(local(&env).exceptions(Section::System).contains("htop"));
    assert!(
        env.packages_toml().contains("[system.htop]"),
        "still shared"
    );
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("软件清单与本机一致"), "{out}");
}

#[test]
fn removed_by_user_then_removed_everywhere() {
    let env = installed_then_removed_by_user();
    let out = env.ok(&[], [Key('r')]);
    assert!(out.contains("已提交并推送：-htop(apt)"), "{out}");
    assert!(!env.packages_toml().contains("htop"));
    assert_eq!(env.remote_log()[0], "adopt(testhost): -htop(apt)");
    assert!(local(&env).applied(Section::System).is_empty());
}

#[test]
fn removed_by_user_then_reinstalled_after_confirming_sudo() {
    let env = installed_then_removed_by_user();
    let out = env.ok(&[], [Key('u'), Yes(true)]);
    assert!(out.contains("将要执行："), "{out}");
    assert!(
        out.contains("sudo env DEBIAN_FRONTEND=noninteractive apt-get -y -q install htop"),
        "{out}"
    );
    assert!(env.apt_installed("htop"));
    env.ok(&[], []);
}

// ---- rule 3: dropped entries are uninstalled only if adopt installed them ----

#[test]
fn dropped_entries_remove_only_what_adopt_installed() {
    let env = Env::new();
    env.with_apt(&["bash", "tmux"], &["htop"]);
    env.init("");
    env.publish("version = 1\n[system.htop]\n[system.tmux]\n");
    env.batch(&["sync", "--yes"]);
    let applied = local(&env).applied(Section::System);
    assert!(applied["htop"].by_adopt);
    assert!(!applied["tmux"].by_adopt, "tmux was installed before adopt");

    env.publish("version = 1\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：卸载 htop(apt)"), "{out}");
    assert!(!env.apt_installed("htop"));
    assert!(
        env.apt_installed("tmux"),
        "never remove what adopt did not install"
    );
    assert!(local(&env).applied(Section::System).is_empty());
}

// ---- consent and privilege ----

fn htop_in_local_repo(env: &Env) {
    fs::write(
        env.repo.join("packages.toml"),
        "version = 1\n[system.htop]\n",
    )
    .unwrap();
    env.git(&env.repo, &["commit", "--quiet", "-am", "htop"]);
}

#[test]
fn without_yes_and_without_terminal_nothing_changes() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop"]);
    env.init("");
    htop_in_local_repo(&env);
    let before = env.local_toml();
    let out = env.batch(&["sync"]);
    assert!(out.contains("将要应用：安装 htop(apt)"), "{out}");
    assert!(out.contains("加 --yes"), "{out}");
    assert!(!env.apt_installed("htop"));
    assert_eq!(env.local_toml(), before);
}

#[test]
fn without_yes_on_a_terminal_confirms_once_with_full_commands() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop"]);
    env.init("");
    htop_in_local_repo(&env);
    let out = env.ok(&["sync"], [Yes(true)]);
    assert!(
        out.contains("sudo env DEBIAN_FRONTEND=noninteractive apt-get -y -q install htop"),
        "{out}"
    );
    assert!(env.apt_installed("htop"));
}

#[test]
fn missing_sudo_skips_system_packages_but_applies_pixi() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop"]);
    *env.sys.sudo_ok.borrow_mut() = false;
    env.init("");
    env.publish(&format!("version = 1\n[system.htop]\n[pixi.ripgrep]\n{RG}"));
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("跳过 1 个 apt 改动：sudo 需要密码"), "{out}");
    assert!(out.contains("已应用：安装 ripgrep(pixi)"), "{out}");
    assert!(!env.apt_installed("htop"));
    assert!(
        env.read(&env.paths.pixi_manifest)
            .contains("[envs.ripgrep]")
    );
    assert!(
        local(&env).failed(Section::System).is_empty(),
        "skipped is not failed"
    );
}

#[test]
fn dry_run_changes_nothing() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop"]);
    env.init("");
    htop_in_local_repo(&env);
    let before = env.local_toml();
    let out = env.batch(&["--dry-run", "sync", "--yes"]);
    assert!(out.contains("[dry-run] git pull"), "{out}");
    assert!(
        out.contains("[dry-run] sudo -n env DEBIAN_FRONTEND"),
        "{out}"
    );
    assert!(out.contains("将应用：安装 htop(apt)"), "{out}");
    assert!(!env.apt_installed("htop"));
    assert_eq!(env.local_toml(), before);
}

// ---- failures and name mappings ----

fn failed_htop() -> Env {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop-ng"]);
    env.init("");
    env.publish("version = 1\n[system.htop]\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("apt 安装 htop 失败"), "{out}");
    assert_eq!(local(&env).failed(Section::System)["htop"].native, "htop");
    env.batch(&["sync", "--yes"]);
    assert_eq!(
        env.count_runs("install htop"),
        1,
        "a failed name is not retried"
    );
    env
}

#[test]
fn failed_install_asks_for_the_right_name_and_writes_it_back() {
    let env = failed_htop();
    let out = env.ok(&[], [Text("htop-ng".into()), Yes(true)]);
    assert!(env.apt_installed("htop-ng"), "{out}");
    let pkgs = env.packages_toml();
    assert!(pkgs.contains("[system.htop]\napt = \"htop-ng\""), "{pkgs}");
    assert_eq!(env.remote_log()[0], "adopt(testhost): ~htop(apt=htop-ng)");
    assert!(local(&env).failed(Section::System).is_empty());
    env.ok(&[], []);
}

#[test]
fn failed_install_can_be_marked_not_for_this_manager() {
    let env = failed_htop();
    env.ok(&[], [Text("-".into())]);
    assert!(env.packages_toml().contains("[system.htop]\napt = false"));
    env.ok(&[], []);
}

// ---- pixi ----

#[test]
fn pixi_env_is_written_without_groups_and_unclaimed_envs_survive() {
    let env = Env::new();
    env.write_pixi(
        "version = 1\n\n# my envs\n[envs.bat]\nchannels = [\"conda-forge\"]\ndependencies = { bat = \"*\" }\nexposed = { bat = \"bat\" }\n",
    );
    env.init("work");
    env.publish(&format!(
        "version = 1\n\n[pixi.ripgrep]\n{RG}groups = [\"work\"]\n"
    ));
    let out = env.batch(&["sync", "--yes"]);
    let m = env.read(&env.paths.pixi_manifest);
    assert!(m.starts_with("version = 1\n\n# my envs\n[envs.bat]"), "{m}");
    assert!(
        m.contains("[envs.ripgrep]\nchannels = [\"conda-forge\"]"),
        "{m}"
    );
    assert!(!m.contains("groups"), "{m}");
    assert!(env.sys.ran("pixi global sync"));
    assert!(
        out.contains("1 个条目等你处理"),
        "bat is still unclaimed: {out}"
    );
}

#[test]
fn pixi_shared_changes_apply_unless_edited_locally() {
    let env = Env::new();
    env.init("");
    env.publish(&format!("version = 1\n[pixi.ripgrep]\n{RG}"));
    env.batch(&["sync", "--yes"]);

    // Untouched here: the new shared version is taken.
    let v2 = RG.replace("\"*\"", "\">=14\"");
    env.publish(&format!("version = 1\n[pixi.ripgrep]\n{v2}"));
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：更新 ripgrep(pixi)"), "{out}");
    assert!(env.read(&env.paths.pixi_manifest).contains(">=14"));

    // Edited here, then changed upstream again: the local edit is kept.
    let m = env
        .read(&env.paths.pixi_manifest)
        .replace("[\"conda-forge\"]", "[\"conda-forge\", \"bioconda\"]");
    env.write_pixi(&m);
    let v3 = RG.replace("\"*\"", "\">=15\"");
    env.publish(&format!("version = 1\n[pixi.ripgrep]\n{v3}"));
    let out = env.batch(&["sync", "--yes"]);
    let m = env.read(&env.paths.pixi_manifest);
    assert!(
        m.contains("bioconda") && m.contains(">=14"),
        "local edit kept: {m}"
    );
    assert!(out.contains("1 个条目等你处理"), "{out}");

    let out = env.ok(&[], [Key('a')]);
    assert!(out.contains("本机改动了 1 个环境"), "{out}");
    assert!(
        env.packages_toml().contains("bioconda"),
        "{}",
        env.packages_toml()
    );
    assert_eq!(env.remote_log()[0], "adopt(testhost): ~ripgrep(pixi)");
    env.ok(&[], []);
}

#[test]
fn pixi_sync_failure_rolls_the_manifest_back() {
    let env = Env::new();
    env.write_pixi("version = 1\n");
    env.init("");
    env.publish(&format!("version = 1\n[pixi.ripgrep]\n{RG}"));
    *env.sys.pixi_sync_code.borrow_mut() = 1;
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("pixi 安装 ripgrep 失败"), "{out}");
    assert_eq!(env.read(&env.paths.pixi_manifest), "version = 1\n");
    assert!(local(&env).failed(Section::Pixi).contains_key("ripgrep"));
}
