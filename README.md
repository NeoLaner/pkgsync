# pkgsync

An interactive TUI for keeping the explicitly-installed packages of two Arch
Linux machines in sync. It diffs **this** machine against another (live over
SSH, or a committed package-list file), shows you what's missing / extra /
version-skewed, and lets you tick exactly what to install, remove, or upgrade.

Built with [ratatui](https://ratatui.rs). Everything runs through `yay`, so both
official-repo and AUR packages are handled, and sudo is requested only when a
command actually needs it.

## Mental model

> **pkgsync makes _this_ machine look more like the thing you point it at.**

It reads your local packages with `pacman -Qe` and compares them to a **remote**
source of truth:

| In the list | Meaning | Action |
|-------------|---------|--------|
| 🟢 `install` | remote has it, you don't | `yay -S --needed <pkg>` |
| 🔴 `remove`  | you have it, remote doesn't | `yay -Rns <pkg>` |
| 🟡 `upgrade` | both have it, versions differ | `yay -S <pkg>` |

Only **explicitly-installed** packages are compared (`pacman -Qe`), never
dependencies — those are pacman's job.

## Install

```sh
cd ~/dev/rust/pkgsync
cargo install --path .      # puts `pkgsync` in ~/.cargo/bin
# or just run it ad-hoc during dev:
cargo run -- <args>
```

## Usage

Just run it with no arguments and pick a source from the menu:

```sh
pkgsync          # menu of remembered sources + ssh_config hosts + "enter new"
pkgsync demo     # sample data — safe, no machines needed
```

The entry menu is pre-populated with:

- **recently-used sources** (persisted in `~/.local/state/pkgsync/recent`), and
- **`Host` aliases from `~/.ssh/config`**,

so you usually just arrow down and press Enter. The last two rows always let you
enter a new target:

- **+ SSH** — type a hostname or IP; pkgsync runs `ssh <host> pacman -Qe`.
- **+ Local file** — type a path to a `.pkgs` snapshot (`~/` is expanded).

Either way the fetch runs on a background thread (so SSH never freezes the UI;
`Esc` cancels a slow one) and the diff appears.

### Passing targets up front (optional)

You can also skip the menu by passing one or more **targets**, then pick in-app:

```sh
pkgsync <dir>                      # every *.pkgs file in a directory
pkgsync <file.pkgs>                # a single state file (auto-selected)
pkgsync <ssh-host>                 # another machine over SSH (auto-selected)
pkgsync <dir> <host-a> <host-b>    # mix files and hosts into one picker
```

Each target is classified automatically: an existing **directory** is scanned
for `*.pkgs` files, an existing **file** becomes a file source, and anything
else is an **SSH host**. With exactly one target, the picker is skipped and the
fetch starts immediately.

## Keybindings

**Entry menu**

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | move |
| `Enter` | choose this source type → input |
| `q` | quit |

**Input field** (typing a host/IP or path)

| Key | Action |
|-----|--------|
| any character | type into the field |
| `Backspace` | delete a character |
| `Enter` | connect / fetch |
| `Esc` | back to the entry menu |

`Ctrl-C` quits from anywhere (even mid-typing).

**Source picker** (when targets are passed on the CLI)

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | move |
| `Enter` | choose this source |
| `q` | quit |

**Diff view**

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | move cursor |
| `Tab` / `Space` | tick / untick the package for action |
| `a` / `i` / `u` / `r` | filter: all / install / upgrade / remove |
| `Enter` | open the confirm screen for ticked packages |
| `y` / `n` | (on confirm) apply / cancel |
| `R` / `F5` | reload (re-fetch the current source) |
| `Esc` | back to the source picker |
| `q` | quit |

After applying, pkgsync reloads automatically so the diff reflects the change.

Selections are tracked by package name, so they **survive filtering** — tick a
few installs, switch to the remove filter, tick a few removes, then `Enter` to
apply everything at once. The confirm screen always shows the literal commands
that will run before anything happens.

After applying, pkgsync runs the commands with the real terminal attached (so
you see yay's output and its sudo prompt), then exits to the shell. Re-run it to
see the updated state.

## Publishing a machine's package list (for the file / offline path)

pkgsync does not publish your own list. To compare via files, each machine dumps
its explicit packages into the shared dotfiles repo:

```sh
pacman -Qe > ~/dev/linux/dotconfigs/state/$(uname -n).pkgs
git -C ~/dev/linux/dotconfigs add state/ && \
  git -C ~/dev/linux/dotconfigs commit -m "pkg state: $(uname -n)" && \
  git -C ~/dev/linux/dotconfigs push
```

(There's also `functional-scripts/pkg-publish.sh --push` in the dotfiles repo
that does exactly this.)

## Live SSH requirements

The SSH source runs `ssh -o BatchMode=yes -o ConnectTimeout=8 <host> pacman -Qe`.
That means:

- **Key-based auth must be set up** (BatchMode disables password prompts).
- The host must be **reachable** — same LAN, or a VPN like Tailscale/WireGuard
  if the machines are across the internet.
- `<host>` can be anything your `~/.ssh/config` understands.

If the host is down or unreachable, pass a `.pkgs` file as a second argument and
pkgsync falls back to it automatically.

## Common journeys

### A. Daily two-way sync via the git repo (no VPN needed)

The simplest, most robust flow — works through the GitHub repo you already push.

On the **office** machine, publish its state and push:
```sh
pacman -Qe > ~/dev/linux/dotconfigs/state/$(uname -n).pkgs
git -C ~/dev/linux/dotconfigs add state/ && git -C ~/dev/linux/dotconfigs commit -m "pkg state" && git -C ~/dev/linux/dotconfigs push
```

On the **home** machine, pull and reconcile:
```sh
git -C ~/dev/linux/dotconfigs pull
pkgsync ~/dev/linux/dotconfigs/state/office.pkgs
```
Tick the packages you want to match, `Enter`, review the commands, `y`. Then
publish home's new state back so office can reconcile in the other direction.

### B. "I installed apps on office — get them on home too"

```sh
pkgsync ~/dev/linux/dotconfigs/state/office.pkgs
i            # filter to just the install candidates
Tab Tab Tab  # tick the ones you actually want (skip office-only stuff)
Enter  y     # apply
```

### C. "I uninstalled junk on office — mirror that removal on home"

```sh
pkgsync ~/dev/linux/dotconfigs/state/office.pkgs
r            # filter to remove candidates (things home has that office doesn't)
Tab          # tick only what you truly want gone — read carefully!
Enter  y
```
Removal uses `-Rns` (drops orphaned deps + config). Don't blind-tick everything;
the remove list includes anything genuinely home-only.

### D. Both machines online — live, no publish step

```sh
pkgsync office                       # over SSH, always current
# or with a safety net if office might be asleep:
pkgsync office ~/dev/linux/dotconfigs/state/office.pkgs
```

### E. Just looking, nothing to install

```sh
pkgsync demo     # or point at a real file/host
```
Browse, filter, read the detail pane. If you never press `Enter`/`y`, nothing is
ever changed.

## Known limitations / TODO

- Auto fallback (try SSH, else a state file) exists in the library
  (`fetch_with_fallback`) but the picker flow treats SSH and files as separate
  choices — if a host is down you pick its file manually. A combined
  "SSH with file fallback" picker entry is a possible addition.
- Upgrades go to the latest repo version, not the other machine's exact version
  (usually fine — a normal `pacman -Syu` on both machines resolves skew anyway).
- The diff reloads in full after applying; very large package sets re-fetch from
  scratch rather than patching the changed entries.
