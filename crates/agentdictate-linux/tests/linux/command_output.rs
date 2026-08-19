use crate::support;

use std::time::{Duration, Instant};

use agentdictate_linux::command::{
    PlatformCapability, PlatformExecutable, PlatformTool, SystemCommandRunner,
};
use support::TestDirectory;

#[test]
fn command_output_drains_stdout_and_stderr_while_the_child_is_running() {
    let directory = TestDirectory::new();
    let executable = directory.executable(
        "large-output",
        concat!(
            "#!/bin/sh\n",
            "head -c 262144 /dev/zero\n",
            "head -c 262144 /dev/zero >&2\n",
        ),
    );
    let executable = PlatformExecutable::at(PlatformTool::Xprop, executable);

    let stdout = SystemCommandRunner
        .run_output(
            PlatformCapability::FocusObservation,
            &executable,
            &[],
            Instant::now() + Duration::from_secs(2),
        )
        .expect("full pipes are drained before the child deadline");

    assert_eq!(stdout.len(), 262_144);
    assert!(stdout.iter().all(|byte| *byte == 0));
}

#[test]
fn inherited_pipe_handles_remain_inside_the_command_deadline() {
    let directory = TestDirectory::new();
    let executable = directory.executable("inherited-pipe", "#!/bin/sh\nsleep 5 &\nexit 0\n");
    let executable = PlatformExecutable::at(PlatformTool::Xprop, executable);

    let error = SystemCommandRunner
        .run_output(
            PlatformCapability::FocusObservation,
            &executable,
            &[],
            Instant::now() + Duration::from_millis(500),
        )
        .expect_err("descendants cannot hold pipe readers beyond the deadline");

    assert!(
        matches!(
            error,
            agentdictate_linux::command::PlatformCommandError::Deadline {
                tool: PlatformTool::Xprop,
            }
        ),
        "unexpected command outcome: {error:?}"
    );
}
