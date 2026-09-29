"""Integration check, run only inside the isolated bwrap home described in docs.
Python is a development/test dependency only, not needed by the installed app.
"""
import os
import pathlib
import signal
import sqlite3
import subprocess
import time

root = pathlib.Path('/tmp/work')
home = pathlib.Path.home()
assert (home / '.lantern-isolated-test').is_file(), 'refusing to modify a real home'
commands = home / 'test-bin'
commands.mkdir()
log = home / 'commands.log'
for name, body in {
    'systemctl': '''printf '%s\\n' "systemctl $*" >> "$HOME/commands.log"
case "$*" in
    *show-environment*) exit 0 ;;
    *is-active*) test -f "$HOME/service-active" ;;
    *stop*) rm -f "$HOME/service-active" ;;
    *restart*) touch "$HOME/service-active" ;;
    *) exit 0 ;;
esac
''',
    'loginctl': 'printf "yes\\n"\n',
    'sudo': 'printf "unexpected sudo\\n" >&2; exit 99\n',
}.items():
    path = commands / name
    path.write_text('#!/bin/bash\nset -eu\n' + body)
    path.chmod(0o755)
env = dict(os.environ, PATH=str(commands) + ':/usr/bin:/bin')
env.pop('XDG_CONFIG_HOME', None)
env.pop('XDG_STATE_HOME', None)
env.pop('LANTERN_DB', None)
db_path = home / "history with 'quotes' $ and %" / 'lantern.db'
env['LANTERN_DB'] = str(db_path)
installer = [str(root / 'scripts/install-user.sh'), '--binary', str(root / 'target/release/lantern')]
def run(args, *, expected=0, environment=env):
    result = subprocess.run(args, env=environment, text=True, capture_output=True)
    assert result.returncode == expected, (args, result.returncode, result.stdout, result.stderr)
    return result.stdout
run(installer + ['--no-start'])
assert not log.exists(), 'file-only installation called systemd'
assert not db_path.exists(), 'installation fabricated history'
launcher = home / '.local/bin/lantern'
cli_env = dict(env)
cli_env.pop('LANTERN_DB')
assert str(db_path) in run([str(launcher), '--status'], environment=cli_env)
assert 'lantern 1.0.1' in run([str(launcher), '--version'], environment=cli_env)
# Actual collection, graceful stop, and history survives an update.
p = subprocess.Popen([str(launcher), '--collect'], env=cli_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
try:
    time.sleep(6)
    p.send_signal(signal.SIGTERM)
    _, err = p.communicate(timeout=5)
    assert p.returncode == 0, err
finally:
    if p.poll() is None:
        p.kill(); p.wait()
with sqlite3.connect(db_path) as db:
    assert db.execute('SELECT count(*) FROM samples').fetchone()[0] >= 1
before = db_path.read_bytes()
run(installer, environment=dict(env, LANTERN_DB=str(home / 'wrong.db')))
assert db_path.read_bytes() == before
assert not (home / 'wrong.db').exists()
assert (home / 'service-active').exists()
run(installer)
calls = log.read_text()
assert '--user stop lantern.service' in calls
assert '--user enable lantern.service' in calls
assert '--user restart lantern.service' in calls
assert db_path.read_bytes() == before
# Failed executable preflight must leave both installed app and service alone.
installed = home / '.local/libexec/lantern/lantern'
binary_before = installed.read_bytes()
log_before = log.read_bytes()
run([str(root / 'scripts/install-user.sh'), '--binary', '/usr/bin/false'], expected=1)
assert installed.read_bytes() == binary_before and log.read_bytes() == log_before
# A manual collector is never silently joined by another collector.
(home / 'service-active').unlink()
p = subprocess.Popen([str(launcher), '--collect'], env=cli_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
try:
    time.sleep(0.5)
    run(installer, expected=1)
    assert not (home / 'service-active').exists()
    p.send_signal(signal.SIGTERM)
    _, err = p.communicate(timeout=5)
    assert p.returncode == 0, err
finally:
    if p.poll() is None:
        p.kill(); p.wait()
print('PASS: isolated install/update, native executable preflight, consistent paths with shell metacharacters, preserved history, graceful flush and duplicate collector refusal.')

# Exercise the measurement helper against a real temporary collector. The mock
# supplies systemd's PID/cgroup metadata; this is a functional test, no benchmark.
p = subprocess.Popen([str(launcher), '--collect'], env=cli_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
try:
    (commands / 'systemctl').write_text('#!/bin/bash\nprintf "MainPID=' + str(p.pid) + '\\nMemoryCurrent=1048576\\nNRestarts=0\\n"\n')
    output = run([str(root / 'scripts/measure-service.sh'), '10'])
    assert 'CPU:' in output and 'Process RSS:' in output and '1.00 MiB' in output, output
    (commands / 'systemctl').write_text('#!/bin/bash\nexit 1\n')
    run([str(root / 'scripts/measure-service.sh'), '10'], expected=1)
    for invalid in ['0', 'nine', '3601', '99999999999999999999999999']:
        run([str(root / 'scripts/measure-service.sh'), invalid], expected=2)
finally:
    p.send_signal(signal.SIGTERM)
    try:
        p.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill(); p.communicate()
print('PASS: resource helper reads the existing process and rejects invalid or unavailable measurements.')
