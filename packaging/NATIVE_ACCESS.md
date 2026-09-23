# Native input access

AgentDictate needs Linux device access with significant authority:

- read access to keyboard event devices, which can expose every key press;
- write access to `/dev/uinput`, which can synthesize arbitrary input.

AgentDictate uses this access only for its global shortcut and for paste
delivery from its own in-process uinput virtual keyboard.
`70-agentdictate-input.rules` grants it to the active local logind session with
`uaccess`. Device nodes remain mode `0660`. Do not replace the rule with
world-readable or world-writable permissions. Do not add a desktop user
permanently to the broad `input` group.

## Grant access

The window's **Set up AgentDictate** screen has a **Grant access** button that
does this for you. Any installation can also run:

```bash
agentdictate setup-access
```

After an administrator password prompt from `pkexec`, it installs the rule in
`/etc/udev/rules.d`, unless the Debian package already ships it in
`/usr/lib/udev/rules.d`, then reloads the udev rules and applies them to existing
keyboards and `/dev/uinput`. If AgentDictate still cannot read the keyboard, log
out and back in so logind applies the new access.

From a repository checkout, `./install.sh --setup-native-access` does the same
with `sudo`. It prints the one command it will run and what that command does,
and asks before running it. It only offers this while access is missing.

## Check access

From a repository checkout, run:

```bash
./install.sh --check-native-access
```

The last line and the exit status tell you where you are. `./install.sh` itself
ends with the same status after installing.

| Exit status | Last line | Meaning |
| --- | --- | --- |
| 0 | `Native input readiness: ready` | AgentDictate can read the keyboard and paste. |
| 2 | `Native input readiness: needs setup (run ./install.sh --setup-native-access)` | The session cannot read the keyboard or write `/dev/uinput`. Grant access as above. |
| 3 | `Native input readiness: working, but insecure` | AgentDictate works, but the listed devices are world-accessible, so any local user or program can read your keystrokes or type into your apps. |

Status 3 comes from another app's udev rule or a manual permission change,
never from AgentDictate's rule, and the message names the rule when it finds
one. Remove or narrow that rule to close the hole. The app that installed it
may stop working.

## Debian package

The Debian package installs the vendor rule in `/usr/lib/udev/rules.d`. The
package manager reloads and retriggers the relevant udev devices, but an
existing desktop session may still need a logout and login before logind
applies the new access. `agentdictate setup-access` reapplies the rule.

## Repository user install by hand

`./install.sh` copies the rule to a user data directory, but it does not use
sudo, change host udev policy, or start a service. To install the rule
yourself:

```bash
agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
sudo install -Dm0644 \
  "$agentdictate_data_home/agentdictate/native-access/70-agentdictate-input.rules" \
  /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
```

Log out and back in if the device access stays unchanged. Then return to the
cloned `agentdictate` directory and run `./install.sh --check-native-access`.

## AppImage by hand

An AppImage cannot change host device policy by itself, so
`./AgentDictate-*.AppImage setup-access` is the simplest route. The AppImage
also includes the rule and this guide under
`usr/share/doc/agentdictate/native-access`. To install the rule yourself,
extract it with the AppImage runtime:

```bash
./AgentDictate-*.AppImage --appimage-extract \
  'usr/share/doc/agentdictate/native-access/*'
sudo install -Dm0644 \
  squashfs-root/usr/share/doc/agentdictate/native-access/70-agentdictate-input.rules \
  /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
```
