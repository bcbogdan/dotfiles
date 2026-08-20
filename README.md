# dotfiles

Personal dotfiles managed with GNU Stow.

## Layout

Each top-level folder is a Stow package:

- `zsh`
- `bash`
- `git`
- `ghostty`
- `tmux`
- `herdr`
- `lazygit`
- `yazi`
- `kitty`
- `karabiner`
- `mise`
- `nvim`
- `opencode`
- `remote-open` (macOS only)

## Usage

Prerequisites: Bash, Git, GNU Stow, and network access. Mise must already be
installed for the installer to install the tools declared in
`mise/.config/mise/config.toml`.

Install the macOS profile:

```bash
./install macos
```

Install the Linux remote-development profile:

```bash
./install remote
```

The installer uses `stow --no-folding` so tools can write runtime state under
`~/.config` without writing it into this repository. It also installs Oh My
Zsh, zsh-autosuggestions, TPM plugins, and the tools declared in the mise
configuration.

The Git config optionally includes `~/.config/dev-machine/gitconfig`. Remote
machine provisioning owns that file for directory-scoped commit identities; the
include is inert on machines where the file does not exist.

To remove links from the remote profile:

```bash
stow --delete --no-folding --target="$HOME" zsh bash git tmux herdr lazygit yazi mise nvim opencode
```

For macOS, also include `ghostty`, `kitty`, `karabiner`, and `remote-open`.

## Goblin Browser Opener

The macOS profile installs two LaunchAgents from the `remote-open` package. One
accepts validated HTTP(S) URLs over a mode-`0600` Unix socket and opens them with
LaunchServices. The other maintains a dedicated SSH reverse-forward to
`dev@goblin-dev`; it does not share connections with normal SSH, SCP, or Goblin
operator commands. A 256-bit token authenticates requests arriving through the
remote loopback listener through a nonce-based HMAC handshake. It is stored with
mode `0600` on both machines. Setup copies it once through encrypted SSH; opener
requests transmit only nonce-bound HMAC proofs.

Run `goblin-open-setup` as the logged-in macOS user, never with `sudo`. The user
context owns the SSH host verification, token, Unix socket, and LaunchAgents.

Remote `xdg-open` accepts only one HTTP(S) URL, and the local service rejects
credentials, malformed escapes, controls, oversized requests, and excessive
open attempts. A fixed-title macOS dialog displays the hostname and URL, defaults
to Cancel, allows only one pending confirmation, and expires after 30 seconds.
The hostname is shown in ASCII IDNA form; URLs longer than 1,000 characters are
truncated in the dialog but rejected only above 8 KiB. URLs are not logged.

The tunnel requires the verified `goblin-dev` SSH host key and automatically
reconnects while the Mac is logged in and awake. After a compute replacement,
verify the new host key independently before restarting the tunnel if the host
identity changed.

Disable and revoke the integration before removing the Stow package:

```bash
goblin-open-setup uninstall
```

Local access is revoked immediately. Remote token deletion is best-effort when
Goblin is unreachable; rerun uninstall after reconnecting to complete cleanup.

## Secrets

No secrets are stored in this repository.

Use `~/.zshrc.local` for machine-local overrides and secrets.
