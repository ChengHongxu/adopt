//! dnf (4 and 5) and brew adapters, adapter selection, and the cross-machine
//! package-name mapping flow.

mod common;

use adopt::model::Section;
use adopt::state::Local;
use common::Answer::*;
use common::*;

fn local(env: &Env) -> Local {
    Local::load(&env.paths).unwrap()
}

// ---- dnf ----

#[test]
fn dnf5_baselines_lists_and_claims() {
    let env = Env::new();
    env.with_dnf(&["bash", "coreutils"], &[], true);
    let out = env.ok(&["init", "--yes"], []);
    assert!(out.contains("dnf：记录了 2 个已装的包作为基线"), "{out}");
    env.ok(&[], []);

    env.dnf_add("htop");
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    let out = env.ok(&[], [Key('a')]);
    assert!(out.contains("已提交并推送：+htop(dnf)"), "{out}");
    assert!(env.packages_toml().contains("[system.htop]"));
    assert!(env.sys.ran("--queryformat"), "dnf5 flags");
}

#[test]
fn dnf4_uses_its_own_repoquery_flags() {
    let env = Env::new();
    env.with_dnf(&["bash"], &[], false);
    env.init("");
    env.dnf_add("htop");
    env.ok(&[], [Key('l')]);
    assert!(env.sys.ran("repoquery --userinstalled --qf"), "dnf4 flags");
    assert!(env.local_toml().contains("[local.system.htop]"));
}

#[test]
fn dnf_groups_install_once_and_are_removed_when_dropped() {
    let env = Env::new();
    env.with_dnf(&["bash"], &["htop", "@development-tools"], true);
    env.init("");
    env.publish(
        "version = 1\n[system.build-essential]\ndnf = \"@development-tools\"\n[system.htop]\n",
    );
    let out = env.batch(&["sync", "--yes"]);
    assert!(
        env.sys
            .ran("sudo -n dnf -y install @development-tools htop"),
        "{out}"
    );
    assert!(env.dnf_installed("htop"));

    // A group never shows up in --userinstalled; that must not look like "removed by user".
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("软件清单与本机一致"), "{out}");
    assert_eq!(env.count_runs("dnf -y install"), 1);
    assert_eq!(env.batch(&["status", "--prompt"]), "");

    env.publish("version = 1\n[system.htop]\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(
        out.contains("已应用：卸载 @development-tools(dnf)"),
        "{out}"
    );
}

// ---- brew ----

#[test]
fn brew_runs_without_sudo_and_honours_false() {
    let env = Env::new();
    env.with_brew(&["git"], &["htop", "openjdk"]);
    env.init("");
    // brew is not baselined, so what is already there is offered for claiming.
    env.ok(&[], [Key('i')]);

    env.publish("version = 1\n[system.htop]\n[system.openjdk]\nbrew = false\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：安装 htop(brew)"), "{out}");
    assert!(env.sys.ran("brew install --formula htop"));
    assert!(!env.sys.ran("sudo"), "Homebrew must never run as root");
    assert!(env.brew_installed("htop"));
    assert!(!env.brew_installed("openjdk"), "brew = false");
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
}

#[test]
fn linuxbrew_next_to_apt_is_not_the_system_manager() {
    let env = Env::new();
    env.with_apt(&["bash"], &[]);
    env.with_brew(&["git"], &[]);
    let out = env.ok(&["init", "--yes"], []);
    assert!(out.contains("apt：记录了 1 个"), "{out}");
    env.ok(&[], []); // brew's `git` is never asked about
    assert!(!env.sys.ran("brew leaves"));
}

// ---- package-name mapping across machines ----

/// Claimed on an Ubuntu machine as `build-essential`; this machine runs Fedora.
fn fedora_with_ubuntu_entry() -> Env {
    let env = Env::new();
    env.with_dnf(&["bash"], &["@development-tools"], true);
    env.init("");
    env.publish("version = 1\n\n# from the ubuntu box\n[system.build-essential]\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("dnf 安装 build-essential 失败"), "{out}");
    assert!(out.contains("1 个条目等你处理"), "{out}");
    let failed = local(&env).failed(Section::System);
    assert_eq!(failed["build-essential"].native, "build-essential");

    // Non-interactive runs skip it from now on instead of retrying.
    env.batch(&["sync", "--yes"]);
    assert_eq!(env.count_runs("install build-essential"), 1);
    env
}

#[test]
fn failed_name_is_asked_once_written_back_and_installed() {
    let env = fedora_with_ubuntu_entry();
    let script = Script::new([Text("@development-tools".into()), Yes(true)]);
    let (r, out) = env.run(&[], &script);
    r.unwrap();
    script.assert_done();
    assert!(out.contains("有 1 个软件安装失败"), "{out}");
    assert!(script.asked.borrow()[0].contains("dnf 上的正确包名"));
    assert!(
        out.contains("sudo dnf -y install @development-tools"),
        "{out}"
    );

    let pkgs = env.packages_toml();
    assert!(
        pkgs.contains("# from the ubuntu box"),
        "comments kept: {pkgs}"
    );
    assert!(
        pkgs.contains("[system.build-essential]\ndnf = \"@development-tools\""),
        "{pkgs}"
    );
    assert_eq!(
        env.remote_log()[0],
        "adopt(testhost): ~build-essential(dnf=@development-tools)"
    );
    assert!(local(&env).failed(Section::System).is_empty());

    // Settled on this machine...
    env.ok(&[], []);
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
    // ...and the Ubuntu name is untouched for apt machines.
    let shared = adopt::state::Shared::load(&env.repo).unwrap();
    let e = shared
        .get(Section::System, "build-essential")
        .unwrap()
        .unwrap();
    assert_eq!(
        e.native_for(adopt::model::ManagerId::Apt).as_deref(),
        Some("build-essential")
    );
}

#[test]
fn failed_name_left_blank_stays_pending() {
    let env = fedora_with_ubuntu_entry();
    env.ok(&[], [Text(String::new())]);
    assert!(
        local(&env)
            .failed(Section::System)
            .contains_key("build-essential")
    );
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
}

// ---- groups ----

#[test]
fn first_group_is_typed_when_none_exist_yet() {
    let env = Env::new();
    env.write_pixi(PIXI_TWO_ENVS);
    env.init("");
    env.ok(&[], [Key('g'), Text("homelab".into()), Key('s')]);
    assert!(env.packages_toml().contains("groups = [\"homelab\"]"));
    assert_eq!(
        env.remote_log()[0],
        "adopt(testhost): +ripgrep(pixi@homelab)"
    );
}

#[test]
fn group_entries_only_install_on_matching_machines() {
    let env = Env::new();
    env.with_apt(&["bash"], &["htop", "tmux"]);
    env.init("homelab");
    env.publish(
        "version = 1\n[system.htop]\ngroups = [\"work\"]\n[system.tmux]\ngroups = [\"homelab\", \"work\"]\n",
    );
    env.batch(&["sync", "--yes"]);
    assert!(!env.apt_installed("htop"));
    assert!(env.apt_installed("tmux"));
}
