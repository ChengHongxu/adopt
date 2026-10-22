# adopt

先用原生命令装软件，再一次性"认领"到 dotfiles 仓库，同步到所有机器。

```
$ adopt
↓ 拉取 2 个提交
✔ 已应用：安装 setproxy(pixi)
本机新装了 3 个软件：
  pixi  ripgrep     [a]所有机器 [g]指定分组 [l]仅本机 [i]忽略 [s]稍后 > a
  apt   htop        > a
  apt   libssl-dev  > l
本机缺少清单里的 1 个软件：
  apt   tmux        [r]从所有机器移除 [k]本机例外 [u]重新装上 [s]稍后 > k
配置文件改动（来自 chezmoi 源仓库的 git diff）：
  ~/.zshrc +2 行   [a]同步 [r]撤销 [d]看 diff [s]稍后 > a
✔ 已提交并推送：+ripgrep(pixi) +htop(apt) ~zshrc
```

平时照常用 `apt install`、`dnf install`、`brew install`、`pixi global install` 装软件，不需要任何包装命令，也不用事先决定要不要同步。adopt 对比"本机实际装了什么"和"清单里有什么"，把差异列出来让你一次分好类，然后自动提交并推送。其他机器用 `adopt sync --yes` 拉取并应用。

支持的包管理器：apt、dnf（dnf4 和 dnf5）、Homebrew（formula，暂不支持 cask）、pixi global。每台机器只用一个系统包管理器，按 apt、dnf、brew 的顺序选。

## 安装

pixi（推荐）：

```sh
pixi global install -c https://chenghongxu.github.io/packages/conda -c conda-forge adopt
```

或者从 [Releases](https://github.com/ChengHongxu/adopt/releases) 下载静态二进制（`adopt-linux-amd64`、`adopt-linux-arm64`、`adopt-darwin-arm64`，附 `SHA256SUMS`）：

```sh
curl -fsSL -o ~/.local/bin/adopt \
  https://github.com/ChengHongxu/adopt/releases/latest/download/adopt-linux-amd64
chmod +x ~/.local/bin/adopt
```

或者从源码：`cargo install --locked --git https://github.com/ChengHongxu/adopt`。

## 快速开始

第一台机器：

```sh
adopt init     # 选 dotfiles 仓库（默认取 chezmoi 源目录）、设本机分组、记录基线
adopt          # 以后每次装完软件跑一下
```

`init` 会把系统包管理器当前列出的几百个包记为基线，之后只关心基线以外新装的。pixi 和 brew 不记基线，已有的会在第一次运行 `adopt` 时逐个问你。

其他机器：

```sh
adopt init --clone https://github.com/<you>/dotfiles.git --groups homelab
adopt sync --yes
```

## 命令

| 命令 | 作用 |
| --- | --- |
| `adopt` | 交互模式：pull 并应用，再逐项认领差异，最后自动生成提交信息并 commit + push |
| `adopt sync [--yes]` | 只 pull 和应用，从不认领。带 `--yes` 不提问；不带时先展示完整命令（含 sudo）确认一次，没有终端时只打印计划 |
| `adopt status [--prompt]` | 待处理的条目数。`--prompt` 只输出数字，为 0 或出错时什么都不输出 |
| `adopt init` | 首次使用。`--repo`、`--clone <URL>`、`--groups a,b`、`--rebaseline`、`--apt-hook`、`--yes`（不提问，适合 cloud-init） |
| `adopt list` | 列出共享清单和本机清单，标出分组、包名映射、本机例外 |

所有命令都支持 `--dry-run`：只打印将要做的事，不改文件、不装卸软件、不动 git。

交互模式里各选项的含义：

- 新装的软件：`a` 写入共享清单，所有机器都装；`g` 只装到选中的分组；`l` 只记在本机；`i` 以后不再问；`s` 下次再说
- 本机缺少的软件（你自己卸载了清单里的包）：`r` 从共享清单删除；`k` 本机例外，其他机器照装；`u` 重新装上
- 在本机安装失败的软件：输入这个包管理器下的正确包名，写回清单（例如 `dnf = "@development-tools"`）；输入 `-` 表示这个包管理器不装
- 本机改过的 pixi 环境：`a` 把本机版本推到共享清单；`r` 恢复为清单版本
- 配置文件：`a` 一起提交；`r` 撤销（`git checkout`，会再确认一次）；`d` 看 diff。只有当 dotfiles 仓库就是 chezmoi 的源仓库时才有这一步

需要 sudo 的命令执行前都会先展示完整命令并确认（`sync --yes` 除外）。

## sync 的规则

- 共享清单里新增、本机还没应用过：安装
- 以前应用过、现在本机没有了（你自己卸载的）：不重装，等交互模式里问你
- 从共享清单删掉的：只有当初是 adopt 装的才卸载；adopt 之前就装着的只是不再跟踪
- 安装失败：记下来，同一个名字不再自动重试；改了包名映射后才会再试
- pixi 环境：清单更新、本机没改过就更新；本机改过就保留本机版本，等你决定
- 拿不到 sudo：跳过系统包并给出提示，pixi 照常应用，不报错退出
- pull 失败：给出警告，用本地的清单继续
- push 被拒：先 `pull --rebase` 再推；还失败就停下来，提交留在本地，绝不强推

## 文件

`<dotfiles 仓库>/packages.toml`：共享清单，三份状态里唯一进 git 的。可以手动编辑，注释和格式会保留。

```toml
version = 1

[system.htop]                       # 逻辑包名，默认在 apt/dnf/brew 里同名

[system.build-essential]
dnf = "@development-tools"          # 某个包管理器下名字不同
brew = false                        # 这个包管理器不装
groups = ["work"]                   # 只装到这些分组的机器；不写就是所有机器

[pixi.setproxy]                     # 原样保存 pixi-global.toml 里这个 env 的完整表
channels = ["https://chenghongxu.github.io/packages/conda", "conda-forge"]
dependencies = { setproxy = "*" }
exposed = { setproxy = "setproxy" }
```

如果 dotfiles 仓库就是 chezmoi 的源目录，`init` 会把 `packages.toml` 加进 `.chezmoiignore`，免得它被当成 `~/packages.toml`。

`~/.config/adopt/local.toml`：本机状态，不进 git，也可以手动编辑。记录仓库路径、本机分组、仅本机的软件、本机例外、忽略列表、基线、adopt 应用过的包（以及哪些是 adopt 装的）、安装失败的记录。

`~/.pixi/manifests/pixi-global.toml`：adopt 在这里增删 `[envs.*]` 表，然后执行 `pixi global sync`。没认领的 env 和注释不会动；`pixi global sync` 失败时清单会回退。

## 提示符（starship）

`adopt status --prompt` 用缓存：只比较两份清单的内容和几个系统文件（`/var/lib/dpkg/status`、rpm 数据库、brew 的 Cellar、pixi 清单）的修改时间和大小，有变化才调用包管理器重新计算。

```toml
# ~/.config/starship.toml
[custom.adopt]
command = "adopt status --prompt"
when = true
shell = ["sh"]
format = "([📦 $output]($style) )"
style = "bold yellow"
```

没有待处理的条目时整个片段都不显示。

可选的 apt 钩子：`adopt init --apt-hook` 会以 root 写入 `/etc/apt/apt.conf.d/99adopt`，每次 dpkg 运行后更新一个时间戳，作为缓存失效的额外信号。交互运行 `init` 时会展示文件内容并询问，默认不装。

## 定时同步和 cloud-init

systemd 用户定时器：

```ini
# ~/.config/systemd/user/adopt-sync.service
[Unit]
Description=adopt sync

[Service]
Type=oneshot
ExecStart=%h/.pixi/bin/adopt sync --yes
```

```ini
# ~/.config/systemd/user/adopt-sync.timer
[Timer]
OnBootSec=5min
OnUnitActiveSec=1h

[Install]
WantedBy=timers.target
```

```sh
systemctl --user enable --now adopt-sync.timer
```

非交互时只会用免密的 `sudo -n`。没有免密 sudo 的机器上，定时器只应用 pixi 环境，系统包留给下次交互运行。

cloud-init：

```yaml
runcmd:
  - su - <user> -c 'adopt init --yes --clone https://github.com/<you>/dotfiles.git --groups homelab && adopt sync --yes'
```

## 开发

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

测试不会碰本机真实的 `~/.pixi`、系统软件或 chezmoi 目录：所有外部命令都经过 `CommandRunner` trait，测试里换成模拟 apt/dnf/brew/pixi/chezmoi 的假实现，HOME 和 dotfiles 仓库都在临时目录里。git 是真实执行的，但用的是临时目录里的仓库和隔离的配置。

`tests/e2e/run.sh` 在 Docker 容器里用真实的 apt 或 dnf 跑一遍 init、认领检测和 sync 的三条规则。CI 在 debian:stable、fedora:latest（dnf5）、rockylinux:9（dnf4）里跑它。它会真实装卸软件，只能在一次性容器里运行：

```sh
cargo build --release --target x86_64-unknown-linux-musl
docker run --rm -v "$PWD:/src:ro" fedora:latest \
  sh /src/tests/e2e/run.sh /src/target/x86_64-unknown-linux-musl/release/adopt
```

依赖保持最少：clap、toml_edit（读写都用它，保留注释和格式）、anyhow、dialoguer（加上它本身依赖的 console）。没选 inquire，因为 dialoguer 的依赖树小得多（不拉 crossterm），需要的确认、输入、多选它都有。

## 发布

1. 在 `Cargo.toml` 和 `recipe/recipe.yaml` 里改版本号，提交
2. `git tag vX.Y.Z && git push origin vX.Y.Z`
3. GitHub Actions 检查三处版本号一致，构建三个静态二进制和 `SHA256SUMS` 发到 Releases，再用 rattler-build 打 conda 包，最后通知 [ChengHongxu/packages](https://github.com/ChengHongxu/packages) 更新 channel。最后一步需要仓库 secret `CHANNEL_DISPATCH_TOKEN`

## License

MIT
