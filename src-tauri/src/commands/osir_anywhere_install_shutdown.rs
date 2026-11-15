//! Pure process ownership bookkeeping for installer shutdown. No signals, argv,
//! environment, user configuration, or process-name matching.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
};

pub(crate) struct Process {
    pub pid: i32,
    pub parent: i32,
    pub uid: u32,
    pub birth: (u64, u64),
    pub executable: Option<PathBuf>,
    pub alive: bool,
}

pub(crate) fn observe(
    rows: &[Process],
    bundles: &[PathBuf],
    roots: &[(i32, (u64, u64))],
    tracked: &mut HashSet<(i32, (u64, u64))>,
    uid: u32,
) -> Result<bool, String> {
    let active = rows
        .iter()
        .filter(|row| row.uid == uid && row.alive)
        .collect::<Vec<_>>();
    let mut children = HashMap::<i32, Vec<(i32, (u64, u64))>>::new();
    let mut current = HashMap::<i32, (u64, u64)>::new();
    for row in &active {
        current.insert(row.pid, row.birth);
        children
            .entry(row.parent)
            .or_default()
            .push((row.pid, row.birth));
    }
    let mut owned = HashSet::new();
    for row in &active {
        let key = (row.pid, row.birth);
        let bundled = row.executable.as_ref().is_some_and(|path| {
            path.is_absolute()
                && !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                && bundles
                    .iter()
                    .any(|bundle| path.starts_with(bundle.join("Contents")))
        });
        if roots.contains(&key) || tracked.contains(&key) || bundled {
            owned.insert(key);
        }
    }
    let mut queue = owned.iter().copied().collect::<VecDeque<_>>();
    while let Some((parent_pid, parent_birth)) = queue.pop_front() {
        // Only a currently observed, birth-bound parent can attribute a new
        // descendant. An old root PID reused by Claude is not a parent.
        if current.get(&parent_pid) != Some(&parent_birth) {
            continue;
        }
        if let Some(descendants) = children.get(&parent_pid) {
            for descendant in descendants {
                if owned.insert(*descendant) {
                    queue.push_back(*descendant);
                }
            }
        }
    }
    for row in &active {
        if !owned.contains(&(row.pid, row.birth)) {
            continue;
        }
        if row.executable.is_none() {
            return Err("无法确认 Anywhere 子进程身份，已停止安装".into());
        }
    }
    tracked.extend(owned.iter().copied());
    Ok(!owned.is_empty())
}

/// Capture ownership before polling the graceful quit, and keep sampling while
/// it runs. Completion requires a successful quit and two quiet snapshots.
/// The caller opens the installer only after this returns Ok.
pub(crate) async fn wait_for_exit<F, S>(
    quit: F,
    mut snapshot: S,
    bundles: &[PathBuf],
    roots: &[(i32, (u64, u64))],
    uid: u32,
    budget: std::time::Duration,
) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), String>>,
    S: FnMut() -> Result<Vec<Process>, String>,
{
    let mut tracked = HashSet::new();
    let deadline = tokio::time::Instant::now() + budget;
    tokio::pin!(quit);
    let mut quit_done = false;
    let mut quiet = 0;
    loop {
        let pending = observe(&snapshot()?, bundles, roots, &mut tracked, uid)?;
        if quit_done && !pending {
            quiet += 1;
            if quiet >= 2 {
                return Ok(());
            }
        } else {
            quiet = 0;
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                return Err("Anywhere 或其 Connector 尚未退出，已停止安装；请在 Anywhere 中结束任务并退出后重试".into());
            },
            result = &mut quit, if !quit_done => {
                result.map_err(|error| format!("Anywhere 未能正常退出，已停止安装：{error}"))?;
                quit_done = true;
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(150)) => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn process(pid: i32, parent: i32, birth: u64, executable: &str) -> Process {
        Process {
            pid,
            parent,
            uid: 501,
            birth: (birth, 0),
            executable: Some(executable.into()),
            alive: true,
        }
    }
    #[test]
    fn anywhere_install_shutdown_waits_for_connector_and_reparented_runtime_without_signals() {
        let bundle = PathBuf::from("/synthetic/OSIR Anywhere.app");
        let root = process(
            10,
            1,
            100,
            "/synthetic/OSIR Anywhere.app/Contents/MacOS/OSIR Anywhere",
        );
        let connector = process(11, 10, 101, "/synthetic/python");
        let runtime = process(12, 11, 102, "/synthetic/node");
        let mut tracked = HashSet::new();
        assert!(observe(
            &[root, connector, runtime],
            &[bundle.clone()],
            &[(10, (100, 0))],
            &mut tracked,
            501
        )
        .unwrap());
        assert_eq!(tracked.len(), 3);
        assert!(observe(
            &[process(12, 1, 102, "/synthetic/node")],
            &[bundle.clone()],
            &[],
            &mut tracked,
            501
        )
        .unwrap());
        assert!(!observe(&[], &[bundle], &[], &mut tracked, 501).unwrap());
    }
    #[test]
    fn anywhere_install_shutdown_ignores_other_apps_uid_and_pid_reuse() {
        let bundle = PathBuf::from("/synthetic/OSIR Anywhere.app");
        let mut other_user = process(
            30,
            1,
            3,
            "/synthetic/OSIR Anywhere.app/Contents/MacOS/OSIR Anywhere",
        );
        other_user.uid = 502;
        let rows = [
            process(10, 1, 999, "/Applications/Claude.app/Contents/MacOS/Claude"),
            process(11, 10, 1000, "/synthetic/codex"),
            process(
                20,
                1,
                2,
                "/Applications/Agents Anywhere.app/Contents/MacOS/Agents Anywhere",
            ),
            other_user,
        ];
        let mut tracked = HashSet::from([(10, (100, 0))]);
        assert!(!observe(&rows, &[bundle], &[(10, (100, 0))], &mut tracked, 501).unwrap());
        assert_eq!(tracked.len(), 1);
    }
    #[test]
    fn anywhere_install_shutdown_finds_bundle_orphans_and_blocks_unknown_children() {
        let bundle = PathBuf::from("/synthetic/OSIR Anywhere.app");
        let orphan = process(
            14,
            1,
            104,
            "/synthetic/OSIR Anywhere.app/Contents/Frameworks/Helper",
        );
        let mut tracked = HashSet::new();
        assert!(observe(&[orphan], &[bundle.clone()], &[], &mut tracked, 501).unwrap());
        let mut child = process(15, 14, 105, "/synthetic/python");
        child.executable = None;
        assert!(observe(
            &[
                process(
                    14,
                    1,
                    104,
                    "/synthetic/OSIR Anywhere.app/Contents/Frameworks/Helper"
                ),
                child
            ],
            &[bundle],
            &[],
            &mut tracked,
            501
        )
        .is_err());
    }

    #[tokio::test]
    async fn anywhere_install_shutdown_rejection_timeout_and_bad_snapshot_block_open() {
        use std::time::Duration;
        let bundle = PathBuf::from("/synthetic/OSIR Anywhere.app");
        let bundles = [bundle];
        for case in 0..4 {
            let result = wait_for_exit(
                async move {
                    match case {
                        0 => Err("应用未同意退出".into()),
                        3 => std::future::pending().await,
                        _ => Ok(()),
                    }
                },
                || {
                    if case == 2 {
                        Err("synthetic snapshot failure".into())
                    } else {
                        Ok(vec![process(
                            10,
                            1,
                            100,
                            "/synthetic/OSIR Anywhere.app/Contents/MacOS/OSIR Anywhere",
                        )])
                    }
                },
                &bundles,
                &[(10, (100, 0))],
                501,
                Duration::from_millis(30),
            )
            .await;
            let mut opened = false;
            let error = result
                .and_then(|()| {
                    opened = true;
                    Ok(())
                })
                .unwrap_err();
            assert!(!opened);
            if case == 0 {
                assert!(error.contains("未同意退出"));
            }
            if case == 1 || case == 3 {
                assert!(error.contains("尚未退出"));
            }
            if case == 2 {
                assert!(error.contains("snapshot failure"));
            }
        }
    }

    #[tokio::test]
    async fn anywhere_install_shutdown_opens_only_after_success_and_two_quiet_snapshots() {
        use std::{cell::Cell, time::Duration};
        let quit_done = Cell::new(false);
        let sample = Cell::new(0);
        let result = wait_for_exit(
            async {
                quit_done.set(true);
                Ok(())
            },
            || {
                let at = sample.get();
                sample.set(at + 1);
                Ok(match at {
                    0 => {
                        assert!(
                            !quit_done.get(),
                            "capture descendants before requesting quit"
                        );
                        vec![
                            process(
                                10,
                                1,
                                100,
                                "/synthetic/OSIR Anywhere.app/Contents/MacOS/OSIR Anywhere",
                            ),
                            process(11, 10, 101, "/synthetic/python"),
                        ]
                    }
                    1 => vec![process(11, 1, 101, "/synthetic/python")],
                    _ => vec![],
                })
            },
            &[PathBuf::from("/synthetic/OSIR Anywhere.app")],
            &[(10, (100, 0))],
            501,
            Duration::from_secs(2),
        )
        .await;
        assert!(result.is_ok());
        assert!(quit_done.get());
        assert_eq!(
            sample.get(),
            4,
            "one reparented runtime snapshot then two quiet snapshots"
        );
    }
}
