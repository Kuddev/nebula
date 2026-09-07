# Pebrel Lua Configuration

Pebrel uses vendored Lua 5.4 for programmable configuration. Load its API with
`require 'pebrel'` and generate a configuration with `pebrel config init`.
Existing `require 'nebula'` calls resolve to the same API table, so old Lua
configurations continue to work.

Lua configuration is executable local code. Only use configuration files and
modules that you trust. Pebrel does not download or execute remote
configuration during discovery.

## Quick Start

Create a localized, annotated configuration:

```text
pebrel config init --language system
pebrel config init --language zh-CN
pebrel config init --language en-US
```

Pebrel refuses to overwrite an existing file. `--force` first creates a
timestamped backup and only then atomically replaces the target.

Validate a configuration without opening a window:

```text
pebrel config check
pebrel config check --config-file D:/configs/pebrel.lua
```

## Basic Configuration

```lua
local pebrel = require 'pebrel'
local config = pebrel.config_builder()

config.window = {
    opacity = 0.96,
    decorations = 'Full',
}

config.font = {
    normal = { family = 'Maple Mono NF CN', style = 'Regular' },
    size = 13.0,
}

config.scrolling = {
    history = 10000,
}

config.env = {
    EDITOR = 'nvim',
}

return config
```

Returning a plain table is also supported, but `config_builder()` is
recommended because it validates top-level assignments as they are made.
Unknown fields and invalid values are errors by default.

## Arrays And Modules

Lua's `{}` does not distinguish an empty array from an empty object. Pebrel
treats an ordinary empty table as an object; use `pebrel.array()` for an empty
array:

```lua
config.profiles = pebrel.array()

config.profiles = pebrel.array {
    {
        name = 'Production SSH',
        command = 'ssh',
        args = { 'user@server' },
    },
}
```

Non-empty tables with consecutive integer keys are arrays automatically.
Sparse arrays and tables mixing integer and string keys are rejected.

Split large configurations into sibling modules:

```lua
-- theme.lua
return {
    primary = {
        foreground = '#d8dee9',
        background = '#16181d',
    },
}
```

```lua
-- pebrel.lua
local pebrel = require 'pebrel'
local config = pebrel.config_builder()
config.colors = require 'theme'
return config
```

Files successfully loaded through `require` are added to the live-reload watch
list. Extra local files can be registered with
`pebrel.add_to_config_reload_watch_list(path)`.

## Platform Values

The module exposes stable runtime paths and platform data:

```lua
local pebrel = require 'pebrel'

if pebrel.platform.os == 'linux' then
    -- display_server is 'wayland', 'x11', or 'unknown'.
    pebrel.log_info('Linux display: ' .. pebrel.platform.display_server)
end

-- pebrel.config_file, pebrel.config_dir, pebrel.home_dir
-- pebrel.executable_dir, pebrel.version, pebrel.target_triple
```

## Discovery Order

An explicit `--config-file` wins, followed by `PEBREL_CONFIG_FILE` and then the
legacy `NEBULA_CONFIG_FILE` alias. Empty environment values are ignored. Explicit
paths must end in `.lua`, `.toml`, `.yml`, or `.yaml`.

Without an explicit path, Pebrel searches its own filenames before legacy
`nebula.*` names. Within each name, the format order is Lua, TOML, YML, then YAML.
For example, `pebrel.toml` takes precedence over `nebula.lua`. An existing but
invalid chosen file reports an error; Pebrel does not silently load another file.

The default data directories are:

| Platform | Directory |
| --- | --- |
| Windows | `%APPDATA%/Pebrel` |
| macOS | `$HOME/Library/Application Support/Pebrel` |
| Linux | `${XDG_CONFIG_HOME:-$HOME/.config}/pebrel` |

`PEBREL_CONFIG_DIR` overrides the data directory, with `NEBULA_CONFIG_DIR` as a
fallback alias. When either nonempty override is set, automatic discovery stays
inside that directory; it does not read configuration from the executable,
home, or system directories. `pebrel config init` writes `pebrel.lua` into the
active data directory.

Windows Lua locations:

1. `pebrel.lua` beside `pebrel.exe` for portable installations.
2. `%APPDATA%/Pebrel/pebrel.lua`.
3. `%USERPROFILE%/.pebrel.lua`.

Linux Lua locations:

1. `$XDG_CONFIG_HOME/pebrel/pebrel.lua`.
2. `$XDG_CONFIG_HOME/pebrel.lua`.
3. `$HOME/.config/pebrel/pebrel.lua`.
4. `$HOME/.pebrel.lua`.
5. `/etc/pebrel/pebrel.lua`.

On macOS, the data-directory location is checked before the Linux-style XDG,
home, and system locations listed above. The same location order applies to
`.toml`, `.yml`, and `.yaml`, followed by legacy `nebula.*` names and locations.
Existing TOML configurations remain supported; YAML is a deprecated transition
format.

## Existing Nebula Data

On first startup, Pebrel copies legacy data into its data directory without
overwriting existing Pebrel files. Preferences use `pebrel_settings.txt`; new
main configuration files use `pebrel.lua`, `pebrel.toml`, `pebrel.yml`, or
`pebrel.yaml`. Session, SSH, and other files whose names do not contain the old
brand retain their names.

The source directory and old configuration names remain available for recovery
and for modules that import an absolute legacy path. The migration records
completion only after all copies succeed, and can retry after an error. Once
complete, later launches do not copy old data back over deliberately removed
files. Existing newer files remain authoritative throughout the migration.

## Transactional Reload

Configuration parsing and Lua execution run on one serial worker. Rapid file
saves are coalesced, and only the latest successful generation is published.
If Lua syntax, module loading, conversion, or validation fails, Pebrel keeps
the last-known-good `UiConfig` and Lua VM alive. Fixing and saving the file
causes the next valid generation to replace them together.

Linux and macOS package targets and native acceptance requirements are listed
in the [installation guide](../INSTALL.md). Configuration support alone is not
evidence that a native package has passed its runtime checks.

## 中文说明

`pebrel config init --language zh-CN` 会生成活动代码与英文模板完全一致、
但说明全部为简体中文的 UTF-8 配置。切换 Pebrel 界面语言不会改写已经存在的
Lua 文件。普通空表 `{}` 表示对象；空数组必须写成 `pebrel.array()`。保存配置
后若出现语法或字段错误，Pebrel 会保留上一份可用配置，修正并再次保存即可恢复
自动重载。

旧的 `require 'nebula'` 与新的 `require 'pebrel'` 指向同一 API。显式配置文件优先，
其次为 `PEBREL_CONFIG_FILE`、兼容的 `NEBULA_CONFIG_FILE`，然后依次查找 Pebrel 与
Nebula 文件；每种名称内按 Lua、TOML、YML、YAML 排序。首次迁移会非覆盖复制旧数据，
保留旧目录与文件供恢复及绝对路径导入使用，不删除来源，也不覆盖已有的新配置。
