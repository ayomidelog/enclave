# Contributing

Enclave builds Linux namespace sandboxes and workspaces on one host. Changes here
are lifecycle changes: they mount, unmount, create interfaces, write cgroups, and
signal processes on a real machine, and the state they leave behind outlives the
process that made it. Read the [architecture](docs/architecture.md) page and the
[limitations](docs/limitations.md) page before your first change, so you know what
the product promises and what it deliberately does not.

## Getting set up

The toolchain is pinned in `rust-toolchain.toml` to Rust 1.85.0 with `clippy` and
`rustfmt`. Install it with `rustup toolchain install 1.85.0`, or let
`rust-toolchain.toml` select it for you.

```bash
cargo build --release
```

The tests that run without root need nothing else. The privileged suite drives a
real daemon and real host resources, and it checks the host before it runs and
names what is missing rather than failing partway through. Its requirements are
`iproute2`, `iptables`, `util-linux`, `coreutils`, `e2fsprogs`, `tar`, and
`busybox-static`, plus a kernel with namespaces, overlayfs, cgroup v2, loop
devices, and network namespaces. Run it with the pinned toolchain visible,
because `sudo` resets `PATH`:

```bash
sudo env "PATH=$PATH" "HOME=$HOME" bash tools/ci/privileged-suite.sh
```

Any extra arguments are passed to the test binary, so one test can be run the
same way it runs in the suite. The suite runs its tests one at a time, because two
would compete for the same bridge and cgroup root; pass your own `--test-threads`
and the script forwards that instead:

```bash
sudo env "PATH=$PATH" "HOME=$HOME" bash tools/ci/privileged-suite.sh --test-threads=1 integration::lifecycle
```

## Running the tests

There are four entry points, and which one a test belongs in is decided by what
it needs rather than by what it covers.

| Command | What it runs | Needs root |
|---|---|---|
| `cargo test --all-targets -- --skip integration:: --skip stress::` | Everything that runs unprivileged | no |
| `cargo test --test unit_suite` | The unit suite alone | no |
| `cargo test --test integration_suite -- --ignored` | The privileged lifecycle suite | yes |
| `cargo test --test stress_suite -- --ignored` | The stress suite | yes |

The `Makefile` wraps the common cases: `make test`, `make test-unit`,
`make test-integration`, `make test-stress`, `make check`, and `make clippy`.

Before you push, run what CI runs. `tools/ci/verify.sh` is the unprivileged gate,
and it is the same script the release workflow runs, so there is one definition of
"verified" rather than two that drift apart:

```bash
bash tools/ci/verify.sh
```

It runs, in order: `cargo fmt --check`, `cargo check --all-targets --locked`,
`cargo clippy --all-targets --locked -- -D warnings`, `cargo doc` with
`RUSTDOCFLAGS=-D warnings`, `shellcheck -x -S warning` over `scripts/`,
`tools/ci/`, and `tools/perf/`, and the tests. The pull-request workflow runs the
same commands as separate steps so a failure names the check that produced it;
when you add one to either, add it to both.

The rest of CI is the part that needs more than a compiler:

```bash
tools/perf/check-thresholds.sh
tools/ci/check-lifecycle-report.sh
```

`check-thresholds.sh` is the performance gate, and it fails on a regression rather
than requiring an improvement. `check-lifecycle-report.sh` fails when the lifecycle
report predates the script that generates it, since a hosted runner cannot take
that measurement and a report nobody re-took should not ship. The privileged suite
and the stress suite each get their own runner, because neither can share a host
with anything else, and the workflows themselves are linted so a typo in one costs
a ten-second job rather than a failed run.

The jobs are `workflows`, `checks`, `perf`, `privileged`, and `stress`, and a
`summary` job reports all five. Point a branch protection rule at `summary`: it
fails when any job fails or is cancelled, so a job that never ran cannot read as
green, and adding a job does not need a change to the rule.

When a privileged or stress job fails, CI collects the host state the run left
behind and uploads it as an artifact: cgroups and what is in them, interfaces,
firewall rules, mounts, loop devices, processes, and each daemon's log. A lifecycle
failure is a statement about host state, and the runner is destroyed when the job
ends, so that artifact is often the whole diagnosis. You can run the same collector
locally with `bash tools/ci/collect-host-state.sh host-state`.

### Where a test goes

The test tree is split by what a test needs to be true, and the split is enforced
by where the file lives rather than by a marker attribute.

- **`tests/unit/`** is for decisions that can be made without a host: parsing,
  path validation, allocation, ordering, and the policy engine. These compile
  into `unit_suite` and run everywhere.
- **`tests/src/`** is for a module's own tests. A source module opts in with

  ```rust
  #[cfg(test)]
  #[path = "../tests/src/error.rs"]
  mod tests;
  ```

  which keeps the tests beside the code they exercise while leaving the source
  file about the code. A test here reaches private items through `use super::*`.
- **`tests/integration/`** is for anything that needs root and a host: a real
  namespace, mount, cgroup, interface, firewall rule, or loop device. These are
  `#[ignore]`d and compile into `integration_suite`. Every test builds its own
  state directory and its own cached rootfs, so it never touches a sandbox that is
  already running, and it registers a cleanup guard so a failing assertion does not
  leave a sandbox behind.

A lifecycle test should assert on host state rather than on the record the code
wrote. A registry record saying `stopped` is a claim; `/proc/self/mountinfo` and
`/sys/fs/cgroup` are the answer. `tests/integration/support/` has the probes for
this, and a test that says a resource is gone should ask the kernel, not the
daemon.

Add a test when it proves something a reader would otherwise have to take on
trust: a bug you fixed, a contract another module depends on, or an invariant a
refactor could quietly break. A test that restates the implementation is noise.
When you fix a bug, say in the commit body what the test did before the fix, so the
next reader knows the test has teeth.

## Code

The tree is organised by concern, and a module is named for the job it does. When a
file approaches roughly 300 lines, split it by concern rather than by size: the
largest file in `src/` is under that today, and the splits along the way were what
kept the lifecycle readable. `src/workspace/control/` and `src/workspace/session/`
are worth reading as examples of how the pieces are divided.

A few rules the code follows that are easy to miss:

- **Errors carry a code.** A failure that a client might branch on is raised with
  `crate::error::coded` and a category from `ErrorCode`. Message text is for a
  person reading it and changes; the code does not.
- **Host commands have a deadline.** Run them through `HostCommand::new`
  (`src/hostcmd/`) rather than `std::process::Command`, so a hung `ip` or `mount`
  fails with a structured error instead of holding a worker and its lease. The
  long-lived processes — `debootstrap`, the session helper, the workspace runtime —
  are the exception, and they have their own supervision.
- **A lifecycle operation has an operation id** and writes a journal record. If you
  add a phase, note it with `journal.phase(...)` so a failure names where it
  happened.
- **Never release a resource whose ownership you cannot prove.** A mount, an
  interface, a cgroup, or a PID is only acted on when Enclave's own record of it
  still matches the host. A PID is only signalled when its start time, and where it
  exists its namespace inode, still match. This is the rule that keeps a name
  collision from turning into data loss, and a change that weakens it is not a
  cleanup fix.
- **Comments say why.** The code already says what it does. The comment worth
  writing is the measurement behind a constant, the reason one order was chosen
  over another, or the failure mode a branch exists to prevent. A comment that
  restates the next line is one to delete.

Prefer the smallest change that makes the behaviour correct. Enclave has no async
runtime, no OCI stack, and a small dependency list on purpose; a new dependency
needs a reason that the standard library cannot supply.

## Commits

Use a prefix that says what kind of change it is, then a subject that completes the
sentence "this commit will ...".

| Prefix | Use for |
|---|---|
| `fix:` | A behaviour that was wrong |
| `feat:` | A behaviour that did not exist |
| `perf:` | A change made to make something faster |
| `refactor:` | A change that moves code without changing behaviour |
| `test:` | A test that adds coverage |
| `docs:` | Documentation only |
| `ci:` | The CI workflow or the suites it runs |
| `build:` | The build, the release archive, or the install scripts |
| `chore:` | Maintenance with no behaviour change |

The body is where the work is explained, and a body is expected on anything
substantial. Write it for the person reading `git log` in six months: what was
wrong or missing, what the change does about it, what the alternative was and why
it was not taken, and how you know it works. For a `perf:` commit that means the
measurement, before and after. For a `fix:` commit it means the failure and what it
did to the host.

Keep a commit to one logical change. A refactor and a behaviour change in the same
commit cannot be reviewed, and cannot be reverted separately.

## Documentation

A change that alters behaviour, output, or a limit belongs with the document that
states it. The mapping is usually obvious from the file names: commands and flags
in `docs/commands.md`, Enclavefile keys in `docs/enclavefile.md`, configuration in
`docs/configuration.md`, the on-disk layout in `docs/storage.md`, the isolation
model in `docs/security.md`, and the runtime behaviour, timings, and stability
guarantees in `docs/runtime-details.md`. `README.md` is the front door and should
stay short. Add a `CHANGELOG.md` entry under `Unreleased` for anything a user would
notice.

Numbers in the documentation are measurements, not estimates, and each one says
what it was measured on. If you change something on a measured path, re-take the
number with the same command the document names, and if you publish a lifecycle
timing, regenerate the report:

```bash
ENCLAVE_LIVE_ITERATIONS=12 ./tools/perf/lifecycle-report.sh
```

That needs a privileged host and a cached rootfs, and it writes
`docs/lifecycle-report.md`, which a release ships. It records the kernel, CPU,
filesystem, CPU count, and load average it was taken under, because a lifecycle
number without its host is not comparable to anything. Take it on an idle host if
you can, and say so if you could not.

## Performance

Measure before you optimise, and record the measurement in the code comment that
explains the choice. Several optimisations this project considered were declined
after measuring them, and the numbers are in the comments where the decision lives
(`src/network/veth.rs` and `src/network/bridge.rs` are the clearest examples). A
declined optimisation with its measurement is a useful contribution; the same
change without one is a guess.

The harness is in `tools/perf/` and is documented in
[tools/perf/README.md](tools/perf/README.md). It is deliberately outside the test
path, and its scripts build their own daemon and state directory so they never
touch a running sandbox. `live-lifecycle.sh` measures the stop-and-start tier,
`live-pause.sh` the pause-and-resume tier, `live-quota.sh` a workspace whose
storage is an ext4 image, `live-ports.sh` the published-port lifecycle, and
`live-loaded.sh` a sandbox whose workspaces are all busy at once. They are
separate runs because a change to one tier is not a change to another.

The gate in CI is `tools/perf/check-thresholds.sh`. It is a floor rather than a
target: it fails on a regression, and it does not require you to make anything
faster than it already is.

## Reporting a problem

For a lifecycle bug, the operation id printed by the failing command is the
starting point. It names the command's journal record under the state directory's
`operations/` directory, its log lines, and its phase timings, so a report that
includes the id, the command, and `enclave doctor` output is one that can be
reproduced rather than guessed at. Please include the kernel version, the
filesystem of the state directory, and whether the workspace was on the directory
or quota storage tier, since all three change what the lifecycle costs and which
code path runs.

## Releases

A release is a tag matching `v*`. `.github/workflows/release.yml` then re-runs
`tools/ci/verify.sh` — the same gate a branch runs — plus the performance gate and
the lifecycle report check, and builds the archive on Ubuntu 22.04 so it runs
against the older glibc of the supported distributions. That is worth remembering
if you touch the build. The archive is unpacked, checked against the checksum
published beside it, and executed before it is attached, because the archive is
what a user downloads rather than the binary in the build directory.

The release also attaches the committed lifecycle report to the GitHub release and
keeps it as a run artifact, so the numbers a release claims travel with it.

Preparing a release means bumping `Cargo.toml` and `Cargo.lock`, moving the entries
under `Unreleased` into a dated section, and leaving `Unreleased` in place for what
comes next. The version says what an operator has to do: a change to durable state or
to the daemon protocol is a major version, because a registry written by a newer
Enclave is refused rather than guessed at and a strict client has to accept the new
fields. Such a release needs a `Breaking` section at the top of its changelog entry
and the upgrade path in the README updated to match. 2.0.0 is the example: the
registry gained a schema version, and every response gained an operation id.
