# shellcheck shell=bash
export PATH="$HOME/.local/bin:$PATH"
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

[[ -f "$HOME/.atuin/bin/env" ]] && source "$HOME/.atuin/bin/env"

[[ -f ~/.bash-preexec.sh ]] && source ~/.bash-preexec.sh
command -v mise >/dev/null && eval "$(mise activate bash)"
command -v atuin >/dev/null && eval "$(atuin init bash)"
command -v zoxide >/dev/null && eval "$(zoxide init bash)"

[[ -f "$HOME/.local/bin/env" ]] && source "$HOME/.local/bin/env"

if [[ $(uname -s) == Linux ]]; then
  export DOCKER_HOST="unix://${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/docker.sock"
fi
