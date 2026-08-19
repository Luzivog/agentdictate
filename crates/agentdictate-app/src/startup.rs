use std::{fs, io, path::Path};

const AUTOSTART_ENTRY: &str = "local.agentdictate.AgentDictate.desktop";

/// Reconciles the user autostart entry with the persisted setting.
///
/// The daemon binary is used directly so startup never opens or focuses the
/// settings window.
pub fn sync_autostart(entry: &Path, enabled: bool, daemon: &Path) -> io::Result<()> {
    let (executable, arguments) = if let Some(app_image) = std::env::var_os("APPIMAGE") {
        (app_image.into(), vec!["--background".to_owned()])
    } else if let Some(executable) = std::env::var_os("AGENTDICTATE_AUTOSTART_EXEC") {
        let arguments = std::env::var("AGENTDICTATE_AUTOSTART_ARG")
            .ok()
            .filter(|argument| !argument.is_empty())
            .into_iter()
            .collect();
        (executable.into(), arguments)
    } else {
        (daemon.to_owned(), Vec::new())
    };
    sync_autostart_command(entry, enabled, &executable, &arguments)
}

pub fn sync_autostart_command(
    entry: &Path,
    enabled: bool,
    executable: &Path,
    arguments: &[String],
) -> io::Result<()> {
    let parent = entry.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "autostart entry has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let contents = if enabled {
        let mut command = quote_exec_path(executable);
        for argument in arguments {
            command.push(' ');
            command.push_str(&quote_exec_value(argument));
        }
        format!(
            "[Desktop Entry]\n\
Type=Application\n\
Name=AgentDictate background service\n\
Comment=Keep the AgentDictate global shortcut ready\n\
Exec={command}\n\
Icon=agentdictate\n\
Terminal=false\n\
NoDisplay=true\n\
X-GNOME-Autostart-enabled=true\n"
        )
    } else {
        // A Hidden user entry is the freedesktop override for package-level
        // entries installed under /etc/xdg/autostart.
        "[Desktop Entry]\nType=Application\nName=AgentDictate background service\nHidden=true\n"
            .to_owned()
    };
    let temporary = parent.join(format!(".{AUTOSTART_ENTRY}.tmp"));
    fs::write(&temporary, contents)?;
    fs::rename(temporary, entry)
}

fn quote_exec_path(path: &Path) -> String {
    quote_exec_value(&path.to_string_lossy())
}

fn quote_exec_value(value: &str) -> String {
    if value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
    }) {
        return value.to_owned();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$");
    format!("\"{escaped}\"")
}
