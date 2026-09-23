//! Read-only checks of what the desktop provides for dictation: the paste
//! keyboard, input device permissions, and the programs AgentDictate runs.

use std::{
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use agentdictate_core::{DesktopReadiness, ExposedInput, MissingTool};

const UINPUT: &str = "/dev/uinput";
const INPUT_DEVICES: &str = "/dev/input";
/// Where udev reads rules from, in the order it applies them.
const RULE_DIRECTORIES: [&str; 3] = [
    "/etc/udev/rules.d",
    "/run/udev/rules.d",
    "/usr/lib/udev/rules.d",
];

/// Checks this desktop. Takes a few milliseconds; nothing is changed.
#[must_use]
pub fn check_desktop() -> DesktopReadiness {
    let rule_directories = RULE_DIRECTORIES.map(PathBuf::from);
    DesktopReadiness {
        paste_access: is_writable(Path::new(UINPUT)),
        exposed_input: exposed_input(
            Path::new(UINPUT),
            Path::new(INPUT_DEVICES),
            &rule_directories,
        ),
        missing_tools: MissingTool::ALL
            .into_iter()
            .filter(|tool| !is_on_path(tool.program()))
            .collect(),
    }
}

fn is_writable(path: &Path) -> bool {
    CString::new(path.as_os_str().as_bytes()).is_ok_and(|path| {
        // SAFETY: `path` is a NUL-terminated string that outlives the call;
        // `access` only reads it.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    })
}

/// World-accessible input devices: a paste keyboard (`uinput`) any user
/// can write, or an event device any user can read. Returns the rule that
/// grants it, when one of `rule_directories` has one.
fn exposed_input(
    uinput: &Path,
    input_devices: &Path,
    rule_directories: &[PathBuf],
) -> Option<ExposedInput> {
    let world_bits = |path: &Path| {
        fs::metadata(path).map_or(0, |metadata| metadata.permissions().mode() & 0o007)
    };
    let uinput_exposed = world_bits(uinput) & 0o002 != 0;
    let events_exposed = fs::read_dir(input_devices).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_name().as_bytes().starts_with(b"event")
                && world_bits(&entry.path()) & 0o004 != 0
        })
    });
    (uinput_exposed || events_exposed).then(|| ExposedInput {
        rule: world_access_rule(rule_directories),
    })
}

/// The first udev rule file that gives input devices a world-accessible
/// mode, such as `MODE="0666"`.
fn world_access_rule(rule_directories: &[PathBuf]) -> Option<PathBuf> {
    rule_directories.iter().find_map(|directory| {
        let mut rules = fs::read_dir(directory)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "rules")
            })
            .collect::<Vec<_>>();
        rules.sort();
        rules.into_iter().find(|rule| {
            fs::read_to_string(rule).is_ok_and(|text| text.lines().any(grants_world_input_access))
        })
    })
}

/// Whether one rule line sets a world-accessible mode on input devices.
fn grants_world_input_access(line: &str) -> bool {
    let line = line.trim();
    if line.starts_with('#') || !line.contains("input") && !line.contains("event") {
        return false;
    }
    line.match_indices("MODE").any(|(start, _)| {
        let value = line[start + "MODE".len()..]
            .trim_start()
            .trim_start_matches([':', '=', '+'])
            .trim_start();
        value
            .strip_prefix('"')
            .and_then(|value| value.split('"').next())
            .and_then(|mode| u32::from_str_radix(mode, 8).ok())
            .is_some_and(|mode| mode & 0o006 != 0)
    })
}

fn is_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|search_path| {
        std::env::split_paths(&search_path).any(|directory| {
            fs::metadata(directory.join(program)).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_world_accessible_device_is_reported_with_the_rule_that_opens_it() {
        let directory = tempfile::tempdir().unwrap();
        let uinput = directory.path().join("uinput");
        let input = directory.path().join("input");
        let rules = directory.path().join("rules.d");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&rules).unwrap();
        fs::write(&uinput, "").unwrap();
        fs::set_permissions(&uinput, fs::Permissions::from_mode(0o660)).unwrap();
        let keyboard = input.join("event4");
        fs::write(&keyboard, "").unwrap();
        fs::set_permissions(&keyboard, fs::Permissions::from_mode(0o660)).unwrap();
        fs::write(
            rules.join("70-agentdictate-input.rules"),
            "KERNEL==\"uinput\", MODE=\"0660\", TAG+=\"uaccess\"\n",
        )
        .unwrap();
        let other_app = rules.join("99-vibetyper-uinput.rules");
        fs::write(
            &other_app,
            "# MODE=\"0666\" in a comment grants nothing\nKERNEL==\"event*\", SUBSYSTEM==\"input\", MODE=\"0666\"\n",
        )
        .unwrap();
        let exposed = || exposed_input(&uinput, &input, std::slice::from_ref(&rules));

        assert_eq!(exposed(), None, "AgentDictate's own 0660 rule is private");

        fs::set_permissions(&keyboard, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            exposed(),
            Some(ExposedInput {
                rule: Some(other_app)
            })
        );
    }

    #[test]
    fn only_a_mode_with_world_bits_on_input_devices_counts() {
        for (line, grants) in [
            (r#"KERNEL=="uinput", MODE="0666""#, true),
            (
                r#"KERNEL=="event*", SUBSYSTEM=="input", MODE:="0664""#,
                true,
            ),
            (r#"KERNEL=="uinput", MODE="0660", TAG+="uaccess""#, false),
            (r#"SUBSYSTEM=="usb", MODE="0666""#, false),
            (r#"# KERNEL=="uinput", MODE="0666""#, false),
        ] {
            assert_eq!(grants_world_input_access(line), grants, "{line}");
        }
    }
}
