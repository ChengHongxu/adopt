//! Uncommitted dotfile changes in chezmoi's source repo, offered next to packages.

mod common;

use std::fs;

use common::Answer::*;
use common::*;

fn setup() -> Env {
    let env = Env::new();
    env.with_chezmoi();
    env.init("");
    env
}

#[test]
fn init_finds_the_repo_through_chezmoi_and_ignores_packages_toml() {
    let env = Env::new();
    env.with_chezmoi();
    env.init("");
    assert!(env.local_toml().contains(&env.repo.display().to_string()));
    assert_eq!(
        env.read(&env.repo.join(".chezmoiignore")),
        "packages.toml\n"
    );
}

#[test]
fn modified_dotfile_is_committed_with_packages() {
    let env = setup();
    env.write_pixi(PIXI_TWO_ENVS);
    fs::write(
        env.repo.join("dot_zshrc"),
        "export EDITOR=vim\nalias ll='ls -l'\nalias g=git\n",
    )
    .unwrap();

    let (out, prompts) = env.ok_prompts(&[], [Key('a'), Key('s'), Key('a')]);
    assert!(
        out.contains("配置文件改动（来自 chezmoi 源仓库的 git diff）："),
        "{out}"
    );
    let row = prompts.lines().last().unwrap();
    assert!(
        row.contains("~/.zshrc") && row.contains("+2 行"),
        "{prompts}"
    );
    assert!(row.contains("[a]同步 [r]撤销 [d]看 diff"), "{prompts}");
    assert!(out.contains("已提交并推送：+ripgrep(pixi) ~zshrc"), "{out}");
    assert_eq!(
        env.remote_log()[0],
        "adopt(testhost): +ripgrep(pixi) ~zshrc"
    );
    let files = env.git(&env.remote, &["show", "--name-only", "--format=", "main"]);
    assert_eq!(
        files.lines().collect::<Vec<_>>(),
        ["dot_zshrc", "packages.toml"]
    );
    assert!(env.git(&env.repo, &["status", "--porcelain"]).is_empty());
}

#[test]
fn diff_then_revert_after_confirming() {
    let env = setup();
    fs::write(env.repo.join("dot_zshrc"), "export EDITOR=nano\n").unwrap();
    let out = env.ok(&[], [Key('d'), Key('r'), Yes(false), Key('r'), Yes(true)]);
    assert!(
        out.contains("-export EDITOR=vim") && out.contains("+export EDITOR=nano"),
        "{out}"
    );
    assert!(out.contains("已撤销 ~/.zshrc"), "{out}");
    assert_eq!(env.read(&env.repo.join("dot_zshrc")), "export EDITOR=vim\n");
    assert_eq!(env.remote_log().len(), 2, "nothing committed");
}

#[test]
fn new_files_cannot_be_reverted_and_later_leaves_them() {
    let env = setup();
    fs::create_dir_all(env.repo.join("dot_config/nvim")).unwrap();
    fs::write(
        env.repo.join("dot_config/nvim/init.lua"),
        "vim.o.number = true\n",
    )
    .unwrap();
    let (_, prompts) = env.ok_prompts(&[], [Key('s')]);
    assert!(prompts.contains("~/.config/nvim/init.lua"), "{prompts}");
    assert!(prompts.contains("新文件 +1 行"), "{prompts}");
    assert!(
        !prompts.contains("[r]"),
        "new files cannot be reverted: {prompts}"
    );
    assert!(env.repo.join("dot_config/nvim/init.lua").exists());
}

#[test]
fn deleted_dotfile_can_be_synced() {
    let env = setup();
    fs::remove_file(env.repo.join("dot_zshrc")).unwrap();
    let (_, prompts) = env.ok_prompts(&[], [Key('a')]);
    assert!(
        prompts.contains("~/.zshrc") && prompts.contains("已删除"),
        "{prompts}"
    );
    assert_eq!(env.remote_log()[0], "adopt(testhost): ~zshrc");
    assert!(
        !env.git(&env.remote, &["ls-tree", "--name-only", "main"])
            .contains("dot_zshrc")
    );
}

#[test]
fn chezmoi_apply_runs_after_pull_only() {
    let env = setup();
    env.ok(&[], []);
    assert!(!env.sys.ran("chezmoi apply"), "nothing pulled");
    env.publish("version = 1\n");
    env.ok(&[], []);
    assert!(env.sys.ran("chezmoi apply"));
    env.publish("version = 1\n# again\n");
    env.batch(&["sync", "--yes"]);
    assert!(env.sys.ran("chezmoi apply --no-tty"));
}

#[test]
fn without_chezmoi_dotfile_changes_are_not_offered() {
    let env = Env::new();
    env.init("");
    fs::write(env.repo.join("dot_zshrc"), "changed\n").unwrap();
    let out = env.ok(&[], []);
    assert!(!out.contains("配置文件改动"), "{out}");
}
