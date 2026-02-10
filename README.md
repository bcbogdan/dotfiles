# dotfiles

Personal dotfiles managed with GNU Stow.

## Layout

Each top-level folder is a Stow package:

- `zsh`
- `bash`
- `git`
- `ghostty`
- `tmux`
- `lazygit`
- `yazi`
- `kitty`
- `karabiner`
- `mise`
- `nvim`
- `opencode`

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

To remove links from the remote profile:

```bash
stow --delete --no-folding --target="$HOME" zsh bash git tmux lazygit yazi mise nvim opencode
```

For macOS, also include `ghostty`, `kitty`, and `karabiner`.

## Secrets

No secrets are stored in this repository.

Use `~/.zshrc.local` for machine-local overrides and secrets.
