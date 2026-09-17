//! `photos gui`：启动桌面版（photos-desktop，Tauri 2 壳）。

use anyhow::Result;
use photos_core::config::Config;

use crate::cli::GuiArgs;

/// 查找并启动 photos-desktop 可执行文件（与当前进程同目录）
pub fn run(_cfg: &Config, _args: &GuiArgs) -> Result<()> {
    let exe_name = if cfg!(windows) {
        "photos-desktop.exe"
    } else {
        "photos-desktop"
    };
    let exe = std::env::current_exe()?.with_file_name(exe_name);
    if !exe.exists() {
        anyhow::bail!(
            "未找到桌面版可执行文件 {}，请先构建：cargo build -p photos-desktop",
            exe.display()
        );
    }
    println!("正在启动桌面版：{}", exe.display());
    std::process::Command::new(&exe).spawn()?;
    Ok(())
}
