//! npm global packages: `[npm.*]` entries, claiming, sync rules, prefix checks,
//! `status --prompt` watching, and the first sync on a fresh machine.

mod common;

use std::fs;

use adopt::model::Section;
use adopt::state::Local;
use common::Answer::*;
use common::*;

fn local(env: &Env) -> Local {
    Local::load(&env.paths).unwrap()
}

fn npmrc(env: &Env, prefix: &str) {
    fs::write(env.paths.home.join(".npmrc"), format!("prefix={prefix}\n")).unwrap();
}

// ---- claiming ----

#[test]
fn claims_hand_installed_packages_including_scoped_ones() {
    let env = Env::new();
    env.with_npm(&[], &[]);
    env.init("work");
    env.npm_add("@anthropic-ai/claude-code@2.0.0");
    env.npm_add("pyright@1.1.400");

    let (out, prompts) = env.ok_prompts(&[], [Key('a'), Key('l')]);
    assert!(out.contains("本机新装了 2 个软件"), "{out}");
    assert!(
        prompts.contains("npm   @anthropic-ai/claude-code"),
        "{prompts}"
    );
    // npm and corepack ship with node and are never offered.
    assert!(
        !prompts.contains("corepack") && !prompts.contains("npm   npm "),
        "{prompts}"
    );
    assert!(
        out.contains("已提交并推送：+@anthropic-ai/claude-code(npm)"),
        "{out}"
    );

    let pkgs = env.packages_toml();
    assert!(
        pkgs.contains("[npm.\"@anthropic-ai/claude-code\"]\n"),
        "{pkgs}"
    );
    assert!(
        pkgs.ends_with("[npm.\"@anthropic-ai/claude-code\"]\n"),
        "claimed unpinned: {pkgs}"
    );
    assert!(env.local_toml().contains("[local.npm.pyright]"));
    assert_eq!(
        env.remote_log()[0],
        "adopt(testhost): +@anthropic-ai/claude-code(npm)"
    );

    // Settled, and nothing gets reinstalled.
    env.ok(&[], []);
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
    assert!(!env.sys.ran("npm install"));

    let out = env.ok(&["list"], []);
    assert!(out.contains("[npm]"), "{out}");
    assert!(out.contains("  @anthropic-ai/claude-code"), "{out}");
}

// ---- sync ----

#[test]
fn installs_for_this_machine_respecting_groups_and_exceptions() {
    let env = Env::new();
    env.with_npm(&[], &["@openai/codex", "pyright", "tsx"]);
    env.init("homelab");
    let mut l = local(&env);
    l.add_exception(Section::Npm, "tsx");
    l.file.save().unwrap();
    env.publish(
        "version = 1\n\n[npm.\"@openai/codex\"]\n\n[npm.pyright]\nversion = \"^1.1\"\ngroups = [\"work\"]\n\n[npm.tsx]\n",
    );
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：安装 @openai/codex(npm)"), "{out}");
    assert!(env.sys.ran("npm install -g @openai/codex"));
    assert!(env.npm_installed("@openai/codex"));
    assert!(!env.npm_installed("pyright"), "other group");
    assert!(!env.npm_installed("tsx"), "local exception");
    assert!(!env.sys.ran("sudo"), "npm never runs as root");

    let out = env.ok(&["list"], []);
    let line = |n: &str| {
        out.lines()
            .find(|l| l.trim_start().starts_with(n))
            .unwrap()
            .to_string()
    };
    assert!(
        line("pyright").contains("version ^1.1") && line("pyright").contains("不属于本机分组"),
        "{out}"
    );
    assert!(line("tsx").contains("本机例外"), "{out}");
}

#[test]
fn dropped_entries_remove_only_what_adopt_installed() {
    let env = Env::new();
    env.with_npm(&["pyright"], &["@openai/codex"]);
    env.init("");
    env.publish("version = 1\n[npm.\"@openai/codex\"]\n[npm.pyright]\n");
    env.batch(&["sync", "--yes"]);
    let applied = local(&env).applied(Section::Npm);
    assert!(applied["@openai/codex"].by_adopt);
    assert!(
        !applied["pyright"].by_adopt,
        "pyright was there before adopt"
    );

    env.publish("version = 1\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：卸载 @openai/codex(npm)"), "{out}");
    assert!(env.sys.ran("npm uninstall -g @openai/codex"));
    assert!(!env.npm_installed("@openai/codex"));
    assert!(
        env.npm_installed("pyright"),
        "never remove what adopt did not install"
    );
    assert!(local(&env).applied(Section::Npm).is_empty());
}

#[test]
fn version_change_reinstalls_but_self_updates_are_left_alone() {
    let env = Env::new();
    env.with_npm(&[], &["pyright"]);
    env.init("");
    env.publish("version = 1\n[npm.pyright]\nversion = \"^1.1\"\n");
    env.batch(&["sync", "--yes"]);
    assert!(env.sys.ran("npm install -g 'pyright@^1.1'"));
    assert_eq!(env.count_runs("npm install"), 1);

    // The tool updates itself (or the user runs `npm update -g`): not a change.
    env.npm_add("pyright@1.1.999");
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
    assert_eq!(env.count_runs("npm install"), 1);
    assert_eq!(env.npm_version("pyright"), "1.1.999");

    // Only the manifest's version moving triggers one reinstall.
    env.publish("version = 1\n[npm.pyright]\nversion = \"^2\"\n");
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("已应用：更新 pyright(npm)"), "{out}");
    assert!(env.sys.ran("npm install -g 'pyright@^2'"));
    assert!(
        local(&env).applied(Section::Npm)["pyright"].by_adopt,
        "still adopt's"
    );
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
    assert_eq!(env.count_runs("npm install"), 2);

    // Dropping the pin is a change too; installing unpinned is one more reinstall.
    env.publish("version = 1\n[npm.pyright]\n");
    env.batch(&["sync", "--yes"]);
    assert_eq!(env.count_runs("npm install"), 3);
    assert!(env.sys.ran("npm install -g pyright"));
}

#[test]
fn removed_by_user_is_not_reinstalled() {
    let env = Env::new();
    env.with_npm(&[], &["@openai/codex"]);
    env.init("");
    env.publish("version = 1\n[npm.\"@openai/codex\"]\n");
    env.batch(&["sync", "--yes"]);
    env.sys.npm_put("placeholder"); // keep node_modules non-empty
    fs::remove_dir_all(env.paths.home.join(".local/lib/node_modules/@openai")).unwrap();
    let out = env.batch(&["sync", "--yes"]);
    assert!(!env.npm_installed("@openai/codex"), "{out}");
    assert_eq!(env.count_runs("npm install"), 1);
    env.ok(&[], [Key('i'), Key('k')]); // ignore the placeholder; keep codex off here
    assert!(
        local(&env)
            .exceptions(Section::Npm)
            .contains("@openai/codex")
    );
}

#[test]
fn dry_run_prints_the_npm_commands() {
    let env = Env::new();
    env.with_npm(&["tsx"], &["@openai/codex", "pyright"]);
    env.init("");
    env.publish("version = 1\n[npm.\"@openai/codex\"]\n[npm.pyright]\nversion = \"^1.1\"\n");
    env.batch(&["sync", "--yes"]);
    env.publish("version = 1\n[npm.pyright]\nversion = \"^2\"\n[npm.tsx]\n");
    // --dry-run does not pull; bring the checkout up to date first.
    env.git(&env.repo, &["pull", "--quiet", "--rebase"]);
    let before = env.local_toml();
    let out = env.batch(&["--dry-run", "sync", "--yes"]);
    assert!(
        out.contains("[dry-run] npm install -g 'pyright@^2'"),
        "{out}"
    );
    assert!(
        out.contains("[dry-run] npm uninstall -g @openai/codex"),
        "{out}"
    );
    assert!(out.contains("将应用："), "{out}");
    assert!(env.npm_installed("@openai/codex"));
    assert_eq!(env.local_toml(), before);
}

// ---- prefix checks ----

#[test]
fn prefix_inside_a_pixi_env_fails_with_a_hint_and_spares_the_rest() {
    let env = Env::new();
    env.with_npm(&[], &["@openai/codex"]);
    let pixi_prefix = env.paths.home.join(".pixi/envs/nodejs");
    fs::create_dir_all(pixi_prefix.join("lib/node_modules")).unwrap();
    *env.sys.npm_prefix.borrow_mut() = pixi_prefix;
    env.init("");
    env.publish(&format!(
        "version = 1\n[npm.\"@openai/codex\"]\n{}",
        common::PIXI_NODEJS
    ));
    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("npm 安装 @openai/codex 失败"), "{out}");
    assert!(
        out.contains("pixi 环境里") && out.contains("prefix=${HOME}/.local"),
        "{out}"
    );
    assert!(
        out.contains("已应用：安装 nodejs(pixi)"),
        "pixi still applied: {out}"
    );
    assert!(!env.sys.ran("npm install"));
    assert!(
        local(&env)
            .failed(Section::Npm)
            .contains_key("@openai/codex")
    );
}

#[test]
fn unwritable_prefix_fails_without_sudo() {
    let env = Env::new();
    env.with_npm(&[], &["pyright"]);
    let ro = env.paths.home.join("ro-prefix");
    fs::create_dir_all(ro.join("lib/node_modules")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    *env.sys.npm_prefix.borrow_mut() = ro.clone();
    env.init("");
    env.publish("version = 1\n[npm.pyright]\n");
    let out = env.batch(&["sync", "--yes"]);
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        out.contains("不可写") && out.contains("不会用 sudo"),
        "{out}"
    );
    assert!(!env.sys.ran("sudo"));
    assert!(!env.sys.ran("npm install"));
}

// ---- status --prompt ----

#[test]
fn prompt_watches_the_prefix_without_starting_npm() {
    let env = Env::new();
    env.with_npm(&["@openai/foo"], &[]);
    npmrc(&env, "${HOME}/.local");
    env.init("");
    env.ok(&[], [Key('i')]);

    assert_eq!(env.batch(&["status", "--prompt"]), "");
    let runs = env.sys.log.borrow().len();
    assert_eq!(env.batch(&["status", "--prompt"]), "");
    assert_eq!(
        env.sys.log.borrow().len(),
        runs,
        "a cache hit must not start any process: {:?}",
        &env.sys.log.borrow()[runs..]
    );

    // A second package in an existing scope only touches node_modules/@openai.
    let modules = env.paths.home.join(".local/lib/node_modules");
    let before = fs::metadata(&modules).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    env.npm_add("@openai/codex");
    assert_eq!(fs::metadata(&modules).unwrap().modified().unwrap(), before);
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    let runs = env.sys.log.borrow().len();
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
    assert_eq!(env.sys.log.borrow().len(), runs);
}

#[test]
fn prompt_uses_the_cached_prefix_when_npmrc_is_silent() {
    let env = Env::new();
    env.with_npm(&[], &[]);
    env.init("");
    // The first full count learns the prefix from `npm ls` output.
    env.batch(&["status", "--prompt"]);
    let cached = env.read(&env.paths.cache_dir.join("npm-prefix"));
    assert_eq!(
        cached.trim(),
        env.paths.home.join(".local").display().to_string()
    );
    env.npm_add("tsx");
    assert_eq!(env.batch(&["status", "--prompt"]), "1\n");
}

// ---- a fresh machine ----

#[test]
fn first_sync_installs_npm_entries_once_pixi_provides_node() {
    let env = Env::new();
    env.sys
        .npm_registry
        .borrow_mut()
        .extend(["@anthropic-ai/claude-code", "pyright"].map(String::from));
    fs::create_dir_all(env.paths.home.join(".local/lib/node_modules")).unwrap();
    env.init("work");
    env.publish(&format!(
        "version = 1\n\n{}\n[npm.\"@anthropic-ai/claude-code\"]\n\n[npm.pyright]\nversion = \"^1.1\"\n",
        common::PIXI_NODEJS
    ));
    assert!(!env.sys.which_npm(), "no node yet");

    let out = env.batch(&["sync", "--yes"]);
    assert!(out.contains("刚装好了 npm"), "{out}");
    assert!(env.npm_installed("@anthropic-ai/claude-code"), "{out}");
    assert!(
        env.sys
            .ran("npm install -g @anthropic-ai/claude-code 'pyright@^1.1'")
    );
    assert!(
        out.contains(
            "已应用：安装 nodejs(pixi)、安装 @anthropic-ai/claude-code(npm)、安装 pyright(npm)"
        ),
        "{out}"
    );
    assert!(!out.contains("等你处理"), "{out}");
    assert!(env.batch(&["sync", "--yes"]).contains("软件清单与本机一致"));
}
