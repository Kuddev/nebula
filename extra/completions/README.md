# Shell completion snapshots

The files in this directory describe the Unix GPUI CLI used by Linux and macOS.
`windows/` contains the Windows GPUI CLI snapshots, including Windows-only commands.
Linux packaging installs the Unix files; neither platform should reuse the other's snapshots.

After changing CLI options, regenerate on both Windows and Unix with the same locked dependencies:

```sh
cargo test --locked -p nebula --bin pebrel cli::tests::regenerate_completions -- --ignored --exact
cargo test --locked -p nebula --bin pebrel cli::tests::completions -- --exact
```

The regeneration command selects the host's snapshot directory. Set
`PEBREL_COMPLETION_OUTPUT` to write generated files to a separate review directory instead.
`NEBULA_COMPLETION_OUTPUT` remains a fallback for existing maintenance scripts.
