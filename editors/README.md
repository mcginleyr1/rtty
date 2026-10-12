# Editor plugins

An editor running in an rtty pane can claim rtty's focus and resize keys. rtty then hands them to
the editor instead of acting on them. The editor moves between its own windows and, past its last
one, on to rtty's panes, so one set of keys moves through both. They mirror tutti's plugins (the
same shapes, the same CLI), so one muscle memory works at every layer.

Both plugins do nothing outside an rtty pane (no `$RTTY_PANE`), and nothing inside tutti running in
rtty (`$TUTTI_PANE` set), where tutti's own plugins own navigation.

- [Neovim](#neovim): `editors/nvim`, Neovim 0.10 or later (verified on 0.12)
- [Emacs](#emacs): `editors/emacs/rtty.el`, Emacs 29.1 or later, built-ins only

## The contract

A program claims the keys with the OSC 1337 user var `RTTY_KEYS`, as iTerm2 and WezTerm define
user vars: `ESC ] 1337 ; SetUserVar=RTTY_KEYS=<base64 of pane> BEL`. An empty value gives them
back. Anything can speak it, a shell included:

```sh
printf '\033]1337;SetUserVar=RTTY_KEYS=%s\007' "$(printf pane | base64)"
```

While a pane claims them, rtty passes these keys to it:

| Keys | When | As the program asked for keys |
| --- | --- | --- |
| `Cmd+Opt+arrows`, `Cmd+Ctrl+arrows` | always | in the kitty protocol only, Cmd as super: Neovim reads `<D-M-Left>`, `<D-C-Left>` |
| `Ctrl+h/j/k/l`, `Alt+h/j/k/l` | with rtty's `keys = "tutti"` | as any key: `C-h`, `M-l` and so on |

A Cmd chord has no xterm encoding Emacs (or other programs outside the kitty protocol) decodes:
`CSI 1 ; 11 D` reads as an undefined `M-[ 1 ; 1`. So rtty keeps acting on Cmd chords for such a
program, claim or not, and Emacs navigates with the tutti-style keys.

Both plugins claim the keys when the editor starts or resumes and give them back when it suspends
(`Ctrl-Z`) or exits, so the shell underneath gets rtty's keys back. An editor that crashes cannot;
the claim also ends at the shell's next prompt, when the shell marks it with OSC 133;A.

Past the editor's edge, the plugins call the CLI with `--from` set to the editor's own
`$RTTY_PANE`:

- `rtty cli pane focus --direction <left|right|up|down> --from <pane> [--wrap]` exits 0 and prints
  the new pane's id when focus moved; at an edge it exits 1 with `no pane <direction> of <pane>`.
  `--wrap` takes the farthest pane on the other side of the tab instead.
- `rtty cli pane resize --direction <left|right|up|down> [--amount <ratio> | --cells <n>] --from
  <pane>` moves the nearest split border that way, by 0.05 of the split by default, as
  `Cmd+Ctrl+arrows` and tutti's `Alt+h/j/k/l` do.
- `rtty cli pane split <pane> <right|down|left|up>` opens a pane there and prints its id.

The whole CLI is in the [README](../README.md#rtty-cli-and-editors). `rtty` must be on the `PATH`,
or set the plugin's program option.

## Neovim

With [lazy.nvim](https://github.com/folke/lazy.nvim):

```lua
{
  dir = '~/src/rtty/editors/nvim',
  name = 'rtty',
  main = 'rtty',
  opts = { keys = true },
}
```

Any plugin manager works. Put `editors/nvim` on the runtimepath and call
`require('rtty').setup({ keys = true })`.

`keys = true` maps, in normal mode:

| Keys | Calls |
| --- | --- |
| `Ctrl+h/j/k/l`, `Cmd+Opt+arrows` | `require('rtty').navigate(direction)` |
| `Alt+h/j/k/l`, `Cmd+Ctrl+arrows` | `require('rtty').resize(direction)` |

and claims them. `navigate` moves to the Neovim window that way, and from the last window on to the
rtty pane beyond. `resize` moves rtty's split border. With rtty's default keys, `Ctrl+h` reaches
Neovim anyway; the claim is what brings it `Cmd+Opt+arrows`. To map the keys yourself, set
`require('rtty').keys = true` before `VimEnter`, call `require('rtty').activate()`, and call
`navigate`/`resize` from your mappings.

The Lua API is `require('rtty')`:

- `navigate(direction)`: move to the window or pane in that direction.
- `focus(direction, wrap)`: focus the rtty pane in that direction. Returns true when focus moved.
- `resize(direction, amount)`: move rtty's split border; `amount` under 1 is a share of the split,
  1 or more a number of cells, nil rtty's step.
- `split(direction)`: open an rtty pane that way.
- `here()`: true in an rtty pane outside tutti.

`focus`, `resize`, `split` and `navigate` give rtty 500 ms (`require('rtty').timeout`) and never
throw. An rtty that is missing, slow or refusing reads as "did not move". Outside rtty they never
call it: `focus`, `resize` and `split` return false, and `navigate` stays among Neovim's windows.
`require('rtty').program` (default `'rtty'`) names the executable. An unknown direction is an
error.

### smart-splits.nvim

`rtty.smart_splits` is a backend for [smart-splits.nvim](https://github.com/mrjones2014/smart-splits.nvim)
protocol 3.0.0. smart-splits moves and resizes Neovim's windows, and at Neovim's edge hands the
move to rtty:

```lua
{
  'mrjones2014/smart-splits.nvim',
  dependencies = { { dir = '~/src/rtty/editors/nvim', name = 'rtty' } },
  opts = {
    mux = { backend = 'rtty.smart_splits' },
    -- or, inside tutti as well:
    -- mux = { backend = { 'tutti.smart_splits', 'rtty.smart_splits' } },
  },
}
```

Map the keys through smart-splits as usual, and leave `keys` off in rtty's own setup.

- `detect()` is true in an rtty pane outside tutti, with `rtty` on the PATH.
- `activate()` claims the keys, because smart-splits holds them.
- `move` focuses the rtty pane beyond the edge; at rtty's own edge it wraps round the tab when
  smart-splits' `at_edge` is `'wrap'`, and opens a pane there when it is `'split'`.
- `resize` moves rtty's split border by smart-splits' `amount`, in cells, which rtty's layout counts
  in too (tutti's backend has to approximate with its ratio steps).

## Emacs

With `use-package`:

```elisp
(use-package rtty
  :load-path "~/src/rtty/editors/emacs"
  :demand t
  :config (rtty-mode 1)
  :bind (("C-h" . rtty-windmove-left)
         ("C-j" . rtty-windmove-down)
         ("C-k" . rtty-windmove-up)
         ("C-l" . rtty-windmove-right)
         ("M-h" . rtty-resize-left)
         ("M-j" . rtty-resize-down)
         ("M-k" . rtty-resize-up)
         ("M-l" . rtty-resize-right)))
```

Those bindings take Emacs's own `C-h` (help), `C-k`, `C-l` and `M-h`…; pick others, such as
`S-<left>` and friends, to keep them.

`rtty-mode` is a global minor mode. In a terminal frame in an rtty pane, it claims rtty's keys: with
rtty's `keys = "tutti"`, `Ctrl+h/j/k/l` and `Alt+h/j/k/l` then reach Emacs instead of moving rtty's
focus. rtty's `Cmd` chords never reach Emacs, which cannot decode them (see the contract), and stay
rtty's. The mode gives the keys back on `suspend-hook` and `kill-emacs-hook`, and claims them again
on `suspend-resume-hook`.

`rtty-windmove-left`, `-right`, `-up` and `-down` move like `windmove`. Where windmove would
refuse, at the frame's edge or toward an inactive minibuffer, they focus the rtty pane in that
direction instead. Outside an rtty pane they are plain windmove, refusal included, so the same
bindings work everywhere. `rtty-resize-left` and friends move rtty's split border. Nothing is bound
by default. `rtty-program` (default `"rtty"`) names the executable.

## Tests

Both suites run against `editors/tests/rtty`, a stub that logs each call's argv and answers from
control files. They assert the exact commands sent and the plugins' return values:

```sh
nvim --headless -u NONE -l "$PWD/editors/nvim/tests/run.lua"
emacs --batch -l ert -l "$PWD/editors/emacs/rtty.el" -l "$PWD/editors/emacs/rtty-test.el" \
  -f ert-run-tests-batch-and-exit
```

`editors/nvim/tests/osc.py` runs a real Neovim TUI in a pty under a job-control shell. It checks
the exact OSC bytes that reach the terminal on start, `Ctrl-Z`, `fg` and quit, and that nothing is
said without `keys` or inside tutti:

```sh
python3 -I editors/nvim/tests/osc.py "$(command -v nvim)" "$PWD/editors/nvim"
```
