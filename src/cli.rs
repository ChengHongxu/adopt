use clap::{Args, Parser, Subcommand};

/// 先用原生命令装软件，再用 adopt 认领到 dotfiles 仓库，同步到所有机器。
#[derive(Debug, Parser)]
#[command(name = "adopt", version, about)]
pub struct Cli {
    /// 只打印将要做的事，不改动任何文件、软件或 git 仓库
    #[arg(long, global = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 首次使用：设置 dotfiles 仓库和本机分组，并记录基线
    Init(InitArgs),
    /// 非交互：拉取并应用新改动，不认领，不提问
    Sync(SyncArgs),
    /// 列出待处理的条目数（给 starship 等提示符用）
    Status(StatusArgs),
    /// 列出共享清单和本机清单
    List,
}

#[derive(Debug, Args, Default)]
pub struct SyncArgs {
    /// 不确认，直接应用（适合 systemd timer、cloud-init）
    #[arg(long, short)]
    pub yes: bool,
}

#[derive(Debug, Args, Default)]
pub struct StatusArgs {
    /// 只输出数字，给 starship 等状态栏用
    #[arg(long, short)]
    pub prompt: bool,
}

#[derive(Debug, Args, Default)]
pub struct InitArgs {
    /// dotfiles 仓库路径（默认取 chezmoi 源目录）
    #[arg(long)]
    pub repo: Option<String>,
    /// 仓库不存在时从这个地址 clone
    #[arg(long, value_name = "URL")]
    pub clone: Option<String>,
    /// 本机分组标签，逗号分隔，例如 work,homelab
    #[arg(long)]
    pub groups: Option<String>,
    /// 重新记录基线（会把当前已装、但清单里没有的包都当作基线）
    #[arg(long)]
    pub rebaseline: bool,
    /// 安装 apt 的 DPkg::Post-Invoke 钩子（需要 sudo），让 `adopt status` 更快发现变化
    #[arg(long)]
    pub apt_hook: bool,
    /// 不提问，全部用参数或默认值（适合 cloud-init）
    #[arg(long, short)]
    pub yes: bool,
}
