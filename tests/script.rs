//! `[script.*]`: software installed by a command from the manifest, for tools with no
//! package manager (a `curl … | sh` installer).

mod common;

use std::fs;

use adopt::model::Section;
use adopt::state::Local;
use common::Answer::*;
use common::*;

const MAGPIE: &str = "magpie";
const INSTALL: &str = "curl -fsSL https://usemagpie.ai/install.sh | sh";
const CREATES: &str = "~/.local/bin/magpie";
const REMOVE: &str = "rm -f ~/.local/bin/magpie";

fn local(env: &Env) -> Local {
    Local::load(&env.paths).unwrap()
}

fn manifest(body: &str) -> String {
    format!("version = 1\n\n{body}")
}

fn exists(env: &Env, rel: &str) -> bool {
    env.paths.home.join(rel).exists()
}

/// A machine where the installer script works and writes `creates`.
fn with_magpie(env: &Env) {
    env.with_script_cmd(INSTALL, 0, Some(".local/bin/magpie"));
}

// ---- install ----

#[test]
fn installs_when_creates_is_missing_and_records_it() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));

    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：安装 magpie(script)"), "{out}");
    assert!(env.sys.ran(INSTALL), "{:?}", env.sys.log);
    assert!(exists(&env, ".local/bin/magpie"));

    let applied = local(&env).applied(Section::Script);
    assert!(applied[MAGPIE].by_adopt);
    assert_eq!(applied[MAGPIE].native, MAGPIE);
    assert!(
        applied[MAGPIE].fp.is_some(),
        "the install command is fingerprinted"
    );

    // Settled: nothing reruns.
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
    assert_eq!(env.count_runs("sh -c"), 1);
}

#[test]
fn creates_alone_decides_installed() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    fs::create_dir_all(env.paths.home.join(".local/bin")).unwrap();
    fs::write(env.paths.home.join(".local/bin/magpie"), "").unwrap();
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));

    let out = env.batch(&["sync", "--yes"]);
    assert!(!out.contains("安装 magpie"), "{out}");
    assert!(!env.sys.ran("sh -c"), "{:?}", env.sys.log);
    let applied = local(&env).applied(Section::Script);
    assert!(!applied[MAGPIE].by_adopt, "it was already there");
}

#[test]
fn a_failing_install_is_recorded_and_not_retried() {
    let env = Env::new();
    env.with_script_cmd(INSTALL, 3, Some(".local/bin/magpie"));
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));

    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("script 安装 magpie 失败"), "{out}");
    assert!(out.contains("退出码 3"), "{out}");
    assert!(!exists(&env, ".local/bin/magpie"));
    assert!(local(&env).failed(Section::Script).contains_key(MAGPIE));

    // No automatic retry with the same command.
    let out = env.batch(&["sync", "--yes"]);
    assert!(!out.contains("已应用"), "{out}");
    assert_eq!(env.count_runs("sh -c"), 1);
}

#[test]
fn success_without_the_file_is_a_failure() {
    let env = Env::new();
    // Exits 0 but never writes creates: a typo in the entry, not a working install.
    env.with_script_cmd(INSTALL, 0, None);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));

    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("script 安装 magpie 失败"), "{out}");
    assert!(out.contains("检查条目里的 creates 是否写错"), "{out}");
    assert!(local(&env).failed(Section::Script).contains_key(MAGPIE));
}

#[test]
fn only_a_changed_install_command_reruns_it() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    let v1 = manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    ));
    env.publish(&v1);
    env.batch(&["sync", "--yes"]);
    assert_eq!(env.count_runs("sh -c"), 1);

    // Editing anything but `install` is not a new install.
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\nremove = \"{REMOVE}\"\n"
    )));
    let out = env.batch(&["sync", "--yes"]);
    assert!(!out.contains("已应用"), "{out}");
    assert_eq!(env.count_runs("sh -c"), 1);

    // A new installer version does rerun it.
    let v2 = "curl -fsSL https://usemagpie.ai/install.sh?v=2 | sh";
    env.with_script_cmd(v2, 0, Some(".local/bin/magpie"));
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{v2}\"\n"
    )));
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：更新 magpie(script)"), "{out}");
    assert_eq!(env.count_runs("sh -c"), 2);
    assert!(env.sys.ran(v2));
}

// ---- removal ----

#[test]
fn deleted_by_hand_is_not_reinstalled() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));
    env.batch(&["sync", "--yes"]);
    fs::remove_file(env.paths.home.join(".local/bin/magpie")).unwrap();

    let out = env.batch(&["sync", "--yes"]);
    assert!(!out.contains("已应用"), "{out}");
    assert_eq!(env.count_runs("sh -c"), 1, "never reinstalled silently");
    assert!(out.contains("1 个条目等你处理"), "{out}");

    // Interactive: it is offered as missing, and `r` drops it from the shared list.
    let (out, prompts) = env.ok_prompts(&[], [Key('r')]);
    assert!(out.contains("缺少清单里的 1 个软件"), "{out}");
    assert!(prompts.contains("magpie"), "{prompts}");
    assert!(out.contains("已提交并推送：-magpie(script)"), "{out}");
    assert!(!env.packages_toml().contains("[script.magpie]"));
    assert!(local(&env).applied(Section::Script).is_empty());
}

#[test]
fn removed_from_the_list_runs_remove_only_if_adopt_installed_it() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\nremove = \"{REMOVE}\"\n"
    )));
    env.batch(&["sync", "--yes"]);
    assert!(exists(&env, ".local/bin/magpie"));
    env.with_script_cmd(REMOVE, 0, None);

    env.publish(&manifest(""));
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：卸载 magpie(script)"), "{out}");
    assert!(env.sys.ran(REMOVE), "{:?}", env.sys.log);
    assert!(local(&env).applied(Section::Script).is_empty());
}

#[test]
fn removed_without_a_remove_command_is_only_forgotten() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));
    env.batch(&["sync", "--yes"]);

    env.publish(&manifest(""));
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("不再跟踪 magpie"), "{out}");
    assert!(out.contains("需要手动清理"), "{out}");
    assert!(!env.sys.ran("rm -f"), "{:?}", env.sys.log);
    assert!(exists(&env, ".local/bin/magpie"), "left for the user");
    assert!(local(&env).applied(Section::Script).is_empty());
}

#[test]
fn what_adopt_did_not_install_is_never_removed() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    fs::create_dir_all(env.paths.home.join(".local/bin")).unwrap();
    fs::write(env.paths.home.join(".local/bin/magpie"), "").unwrap();
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\nremove = \"{REMOVE}\"\n"
    )));
    env.batch(&["sync", "--yes"]);

    env.publish(&manifest(""));
    env.batch(&["sync", "--yes"]);
    assert!(!env.sys.ran("rm -f"), "{:?}", env.sys.log);
    assert!(exists(&env, ".local/bin/magpie"));
}

// ---- prompting and dry runs ----

#[test]
fn interactive_runs_show_the_command_and_ask_first() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));

    let (out, prompts) = env.ok_prompts(&[], [Yes(false)]);
    assert!(out.contains("将要执行："), "{out}");
    assert!(out.contains(INSTALL), "{out}");
    assert!(prompts.contains("执行吗？"), "{prompts}");
    assert!(!exists(&env, ".local/bin/magpie"), "declined");
    assert!(!local(&env).failed(Section::Script).contains_key(MAGPIE));

    // Saying yes runs it.
    let (out, _) = env.ok_prompts(&[], [Yes(true)]);
    assert!(out.contains("已应用：安装 magpie(script)"), "{out}");
    assert!(exists(&env, ".local/bin/magpie"));
}

#[test]
fn dry_run_only_prints() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));
    let before = env.local_toml();
    // --dry-run does not pull; bring the checkout up to date first.
    env.git(&env.repo, &["pull", "--quiet", "--rebase"]);

    let out = env.batch(&["--dry-run", "sync", "--yes"]);
    assert!(out.contains("[dry-run] sh -c '"), "{out}");
    assert!(out.contains(INSTALL), "{out}");
    assert!(!exists(&env, ".local/bin/magpie"));
    assert_eq!(env.local_toml(), before);
}

// ---- status --prompt ----

#[test]
fn prompt_never_starts_a_process() {
    let env = Env::new();
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));
    env.git(&env.repo, &["pull", "--quiet", "--rebase"]);

    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    let runs = env.sys.log.borrow().len();
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    assert_eq!(env.sys.log.borrow().len(), runs, "cache hit ran something");

    // The installer writing the file is all it takes to notice.
    env.batch(&["sync", "--yes"]);
    assert_eq!(env.batch(&["status", "--prompt"]), "");
}

// ---- groups and ordering ----

#[test]
fn groups_keep_entries_off_other_machines() {
    let env = Env::new();
    with_magpie(&env);
    env.init("homelab");
    env.publish(&manifest(&format!(
        "[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\ngroups = [\"work\"]\n"
    )));
    let out = env.batch(&["sync", "--yes"]);
    assert!(!out.contains("安装 magpie"), "{out}");
    assert!(!env.sys.ran("sh -c"), "{:?}", env.sys.log);

    let out = env.ok(&["list"], []);
    assert!(out.contains("[script]"), "{out}");
    assert!(out.contains("不属于本机分组"), "{out}");
}

#[test]
fn scripts_run_after_npm() {
    let env = Env::new();
    env.with_npm(&[], &["pyright"]);
    with_magpie(&env);
    env.init("");
    env.publish(&manifest(&format!(
        "[npm.pyright]\n\n[script.{MAGPIE}]\ncreates = \"{CREATES}\"\ninstall = \"{INSTALL}\"\n"
    )));
    env.batch(&["sync", "--yes"]);

    let log = env.sys.log.borrow();
    let npm = log
        .iter()
        .position(|l| l.contains("npm install -g pyright"))
        .expect("npm ran");
    let script = log
        .iter()
        .position(|l| l.contains("sh -c"))
        .expect("script ran");
    assert!(npm < script, "{log:?}");
}

#[test]
fn bad_entries_fail_with_the_entry_name() {
    let env = Env::new();
    env.init("");
    for (body, needle) in [
        ("[script.x]\ninstall = \"true\"\n", "缺少 creates"),
        ("[script.y]\ncreates = \"~/.local/bin/y\"\n", "缺少 install"),
        (
            "[script.z]\ncreates = 1\ninstall = \"true\"\n",
            "必须是非空字符串",
        ),
        (
            "[script.w]\ncreates = \"a\"\ninstall = \"true\"\nextra = \"x\"\n",
            "不支持 extra",
        ),
    ] {
        env.publish(&manifest(body));
        let script = Script::new([]);
        let (r, _) = env.run(&["sync", "--yes"], &script);
        let err = format!("{:#}", r.unwrap_err());
        assert!(err.contains(needle), "{body} -> {err}");
    }
}
