#!/usr/bin/env bash
# Run as the account that will collect and view history, on the target machine.
set -euo pipefail
fail() { printf 'lantern install: %s\n' "$*" >&2; exit 1; }
usage() {
    cat <<'HELP'
Usage: scripts/install-user.sh [--binary PATH] [--no-start]
Without --binary, builds a native release without NVIDIA support (suitable for Hermit).
Installs ~/.local/bin/lantern, saves the database path, and enables lantern.service.
--no-start installs files only, without stopping/starting services or enabling linger.
Run again to update. Run as your normal user, not with sudo.
HELP
}
source_binary=''
start_service=true
while (($#)); do
    case $1 in
        --binary) (($# >= 2)) || fail '--binary needs a path'; source_binary=$2; shift 2 ;;
        --no-start) start_service=false; shift ;;
        --help|-h) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done
[[ $(uname -s) == Linux ]] || fail 'Linux is required'
[[ $(id -u) != 0 ]] || fail 'run as your normal account, without sudo'
[[ ${HOME:-} == /* ]] || fail 'HOME must be absolute'
project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
config_root=${XDG_CONFIG_HOME:-"$HOME/.config"}
[[ $config_root == /* ]] || fail 'XDG_CONFIG_HOME must be absolute'
app_config="$config_root/lantern"
unit_dir="$config_root/systemd/user"
launcher="$HOME/.local/bin/lantern"
installed_binary="$HOME/.local/libexec/lantern/lantern"
settings="$app_config/environment.sh"
if [[ -f $settings ]]; then
    # Owned and editable by this user, like their other shell configuration.
    source "$settings"
else
    if [[ ${LANTERN_DB+x} ]]; then
        [[ -n $LANTERN_DB ]] || fail 'LANTERN_DB is empty'
    elif [[ ${XDG_STATE_HOME:-} == /* ]]; then
        LANTERN_DB="$XDG_STATE_HOME/lantern/lantern.db"
    else
        LANTERN_DB="$HOME/.local/state/lantern/lantern.db"
    fi
fi
[[ -n ${LANTERN_DB:-} ]] || fail 'saved database path is empty'
[[ $LANTERN_DB == /* ]] || LANTERN_DB="$PWD/$LANTERN_DB"
export LANTERN_DB

if [[ -z $source_binary ]]; then
    command -v cargo >/dev/null || fail 'Cargo is missing; install Rust or pass --binary PATH'
    cargo build --manifest-path "$project_dir/Cargo.toml" --release --locked --no-default-features --jobs "${CARGO_BUILD_JOBS:-1}" --target-dir "$project_dir/target/hermit"
    source_binary="$project_dir/target/hermit/release/lantern"
fi
[[ -f $source_binary && -x $source_binary ]] || fail "not an executable file: $source_binary"
# Detect wrong architecture or missing runtime libraries before changing services.
version=$("$source_binary" --version) || fail 'binary cannot run on this machine'
[[ $version == 'Lantern '* || $version == 'lantern '* ]] || fail 'binary is not Lantern'

if $start_service; then
    systemctl --user show-environment >/dev/null || fail 'user systemd is unavailable; log in via SSH as this user'
    account=$(id -un)
    if [[ $(loginctl show-user "$account" --property=Linger --value) != yes ]]; then
        # This is the only privileged step; collection itself stays unprivileged.
        sudo loginctl enable-linger "$account"
    fi
fi

mkdir -p -- "$app_config" "$unit_dir" "$(dirname -- "$launcher")" "$(dirname -- "$installed_binary")"
staging=$(mktemp -d "$HOME/.local/libexec/lantern/.install.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
install -m 755 -- "$source_binary" "$staging/lantern"
if [[ ! -f $settings ]]; then
    (umask 077; printf 'LANTERN_DB=%q\n' "$LANTERN_DB" > "$settings")
fi
# Bake in paths with shell quoting, including spaces and literal $, quotes or %.
{
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'if [[ ! ${LANTERN_DB+x} ]]; then source %q; fi\n' "$settings"
    printf 'export LANTERN_DB\nexec %q "$@"\n' "$installed_binary"
} > "$staging/launcher"
chmod 755 "$staging/launcher"
install -m 644 -- "$project_dir/packaging/systemd/lantern.service" "$staging/lantern.service"
if $start_service; then
    if systemctl --user is-active --quiet lantern.service; then
        systemctl --user stop lantern.service
    fi
    # Never silently start a second collector beside an existing manual one.
    report=$("$staging/lantern" --status) || fail 'cannot read existing history'
    if [[ $report == *'Collector  detected'* ]]; then
        fail 'a manual collector is running for this database; stop it with Ctrl+C, then rerun installation'
    fi
fi
# Rename on the same filesystem: running viewers can finish with their old binary.
mv -f -- "$staging/lantern" "$installed_binary"
install -m 755 -- "$staging/launcher" "$launcher.new"
mv -f -- "$launcher.new" "$launcher"
install -m 644 -- "$staging/lantern.service" "$unit_dir/lantern.service.new"
mv -f -- "$unit_dir/lantern.service.new" "$unit_dir/lantern.service"
if $start_service; then
    systemctl --user daemon-reload
    systemctl --user enable lantern.service
    systemctl --user reset-failed lantern.service
    systemctl --user restart lantern.service
    sleep 2
    systemctl --user is-active --quiet lantern.service || fail 'service did not start; inspect journalctl --user -u lantern.service'
    printf 'Collector enabled for boot and running. First save takes about 30 seconds.\n'
else
    printf 'Files installed only; no service or boot settings changed.\n'
fi
printf 'Installed %s\nDatabase: %s\nOpen: %s\nStatus: %s --status\n' "$version" "$LANTERN_DB" "$launcher" "$launcher"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) printf 'Add ~/.local/bin to PATH to use the short command: lantern\n' ;; esac
