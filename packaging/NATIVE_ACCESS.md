# Native input access

AgentDictate needs Linux device access with significant authority:

- read access to keyboard event devices, which can expose every key press;
- write access to `/dev/uinput`, which can synthesize arbitrary input.

AgentDictate uses this access only for its global shortcut and Wayland paste.
`70-agentdictate-input.rules` grants it to the active local logind session with
`uaccess`. Device nodes remain mode `0660`. Do not replace the rule with
world-readable or world-writable permissions. Do not add a desktop user
permanently to the broad `input` group.

## Debian package

The Debian package installs the vendor rule in `/usr/lib/udev/rules.d` and the
user unit in `/usr/lib/systemd/user`. The package manager reloads and retriggers
the relevant udev devices, but an existing desktop session may still need a
logout/login before logind applies the new ACL. Enable the per-user daemon once:

```bash
systemctl --user daemon-reload
systemctl --user enable --now agentdictate-ydotoold.service
```

## Repository user install

`./install.sh` copies the rule and unit to user-visible data locations, but it
does not use sudo, change host udev policy, or start a service. Install the rule
as an administrator, then reload it:

```bash
agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
sudo install -Dm0644 \
  "$agentdictate_data_home/agentdictate/native-access/70-agentdictate-input.rules" \
  /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
systemctl --user daemon-reload
systemctl --user enable --now agentdictate-ydotoold.service
```

Log out and back in if device ACLs remain unchanged. After signing back in,
return to the cloned `agentdictate` directory. Then run:

```bash
./install.sh --check-native-access
```

## AppImage

An AppImage cannot change host device policy. It includes the rule, unit, and
this guide under `usr/share/doc/agentdictate/native-access`. Extract them with
the AppImage runtime, install the extracted rule, and copy the unit into the
user systemd data directory:

```bash
./AgentDictate-*.AppImage --appimage-extract \
  'usr/share/doc/agentdictate/native-access/*'
agentdictate_systemd_user_dir="${XDG_DATA_HOME:-$HOME/.local/share}/systemd/user"
mkdir -p "$agentdictate_systemd_user_dir"
install -m0644 \
  squashfs-root/usr/share/doc/agentdictate/native-access/agentdictate-ydotoold.service \
  "$agentdictate_systemd_user_dir/agentdictate-ydotoold.service"
sudo install -Dm0644 \
  squashfs-root/usr/share/doc/agentdictate/native-access/70-agentdictate-input.rules \
  /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
systemctl --user daemon-reload
systemctl --user enable --now agentdictate-ydotoold.service
```

Install the host packages that provide both `ydotool` and `ydotoold`. Debian 13
provides both binaries in its `ydotool` backport. Ubuntu 24.04 packages them
separately. AgentDictate never starts `ydotoold` with elevated privileges.
