// 外部命令子进程的跨平台辅助。

/// 在 Windows 下为子进程设置 `CREATE_NO_WINDOW`，避免备份/恢复、客户端检测、
/// SQL 文件执行等操作在 Windows 上弹出黑色控制台窗口；非 Windows 平台原样返回。
#[cfg(target_os = "windows")]
pub fn no_console(mut command: std::process::Command) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// 非 Windows 平台：无需隐藏控制台，原样返回。
#[cfg(not(target_os = "windows"))]
pub fn no_console(command: std::process::Command) -> std::process::Command {
    command
}

/// spawn 前调用，让目标命令可按整棵进程树取消（Unix 侧配置进程组）。
///
/// 原生客户端（dump/psql/mysql 等）取消时必须连同其派生的子进程一起终止，否则
/// worker 会继续占用连接或写入半成品。Unix 让子进程成为新进程组组长（进程组 id
/// 等于子进程 pid），取消时对整组发 SIGKILL；Windows 当前均为单进程调用，不需要。
// ponytail: Windows 若启用并行 dump（`--jobs`）或出现会派生子进程的工具，需引入
// Job Object（`CreateJobObjectW` + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）再整树终止；
// std 的 `create_job_object` 是 nightly-only，届时用 windows crate 实现。
pub fn prepare_tree_kill_command(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd;
}

/// 终止整个进程树并回收直接子进程；失败记录日志，不 panic。
pub fn kill_child_tree(child: &mut std::process::Child) {
    #[cfg(target_os = "windows")]
    {
        let _ = child.kill();
    }
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        // 进程组 id 等于组长（子进程）pid；负 pid 表示对整组发信号。
        let group = Pid::from_raw(-(child.id() as i32));
        if let Err(error) = kill(group, Signal::SIGKILL) {
            tracing::warn!(%error, "终止子进程进程组失败");
        }
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod kill_child_tree_tests {
    /// 取消时必须连孙进程一起终止，否则会残留孤儿进程（方案 §12.5/12.11）。
    #[test]
    fn kills_the_whole_process_tree() {
        // 一个随父进程退出的孙进程：写 PID 到文件，供断言其已被终止。
        let pid_file = std::env::temp_dir().join(format!("fluxdb-tree-{}", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let script = format!(
            "sh -c 'sleep 30 & echo $! > {pid}; wait'",
            pid = pid_file.display()
        );
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(&script)
            .stdout(std::process::Stdio::null());
        super::prepare_tree_kill_command(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        // 等待孙进程 PID 落盘，再 kill 整棵进程树。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let grandchild_pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "孙进程 PID 未及时落盘"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        super::kill_child_tree(&mut child);

        // 孙进程应已被终止。刚被终止的进程可能仍是未被父进程回收的僵尸（kill 仍返回 Ok），
        // 孤儿进程由 init 异步回收；因此轮询等待其彻底消失（ESRCH）而不是单次断言。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let gone = loop {
            let probe = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(grandchild_pid),
                nix::sys::signal::Signal::SIGKILL,
            );
            if probe.is_err() {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let _ = std::fs::remove_file(&pid_file);
        assert!(gone, "取消后孙进程仍然存活");
    }
}
