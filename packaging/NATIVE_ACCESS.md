# Native input access

AgentDictate needs two narrowly scoped Linux device capabilities:

- read access to active keyboard event devices for the global shortcut;
- write access to `/dev/uinput` so `ydotoold` can paste on Wayland.

`70-agentdictate-input.rules` grants those capabilities to the active local
logind session with `uaccess`. Device nodes remain mode `0660`; do not replace
the rule with world-readable or world-writable permissions, and do not add a
desktop user permanently to the broad `input` group.

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
sudo install -Dm0644 \
  ~/.local/share/agentdictate/native-access/70-agentdictate-input.rules \
  /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
systemctl --user daemon-reload
systemctl --user enable --now agentdictate-ydotoold.service
```

Log out and back in if device ACLs remain unchanged. Then run:

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
mkdir -p ~/.local/share/systemd/user
install -m0644 \
  squashfs-root/usr/share/doc/agentdictate/native-access/agentdictate-ydotoold.service \
  ~/.local/share/systemd/user/agentdictate-ydotoold.service
sudo install -Dm0644 \
  squashfs-root/usr/share/doc/agentdictate/native-access/70-agentdictate-input.rules \
  /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
systemctl --user daemon-reload
systemctl --user enable --now agentdictate-ydotoold.service
```

Install `ydotool` and `ydotoold` from the host distribution. AgentDictate never
starts `ydotoold` with elevated privileges.
