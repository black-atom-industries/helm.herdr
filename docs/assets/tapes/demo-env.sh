# Isolated Herdr home for the release clips. Source it with REPO set to the repo root.
# The home is a fixed path directly under /tmp: Herdr's socket path must fit the 104-byte Unix socket limit, and a
# stable path keeps the path-derived workspace colors the same on every recording.

HERDR_BIN_DIR=$(dirname "$(mise which herdr)")
ZOXIDE_BIN_DIR=$(dirname "$(mise which zoxide)")
export PATH="$HERDR_BIN_DIR:$ZOXIDE_BIN_DIR:$PATH"
export MISE_DATA_DIR="$HOME/.local/share/mise"

export HOME=/tmp/helm-demo
if [ -S "$HOME/.config/herdr/herdr.sock" ]; then
  HERDR_SOCKET_PATH="$HOME/.config/herdr/herdr.sock" herdr server stop >/dev/null 2>&1
fi
rm -rf "$HOME"
mkdir -p "$HOME"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export _ZO_DATA_DIR="$XDG_DATA_HOME/zoxide"
unset HERDR_ENV HERDR_SESSION HERDR_SOCKET_PATH

mkdir -p "$XDG_CONFIG_HOME/herdr/plugins/config/helm-herdr" "$_ZO_DATA_DIR"
cp "$REPO/docs/assets/tapes/herdr.toml" "$XDG_CONFIG_HOME/herdr/config.toml"
cp "$REPO/docs/assets/tapes/helm.toml" "$XDG_CONFIG_HOME/herdr/plugins/config/helm-herdr/config.toml"

for project in api web docs infra; do
  mkdir -p "$HOME/code/$project"
  git -C "$HOME/code/$project" init -q -b main
done
printf "PROMPT='%%F{8}%%1~%%f ❯ '\n" > "$HOME/.zshrc"
source "$HOME/.zshrc"

for dir in "$HOME/code/api" "$HOME/code/web" "$HOME/notes" "$HOME/dotfiles"; do
  mkdir -p "$dir"
  zoxide add "$dir"
done

cd "$HOME/code/api"
