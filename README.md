# lagom

A Go sibling in the Hylla workspace. Bootstrap stage — the domain is not yet
decided; this repository currently ships the shared agent-dispatch + cascade
scaffolding and a canonical [mage](https://magefile.org) build gate.

Module path: `github.com/hylla-io/lagom`

## Layout

This repository uses the workspace's bare-root + linked-worktree convention:
the repository root is a bare git directory, and the working tree lives in the
`main/` linked worktree alongside it. Run tooling from `main/`.

## Build

All build/test/lint flows route through mage — never the raw Go toolchain.

```sh
mage -l        # list targets
mage ci        # the gate: FormatCheck + Vet + Race + Cover + Tidy + Build
mage build     # compile cmd/lagom
```

## License

[MIT](LICENSE).
