# Nebula Lua Configuration

Nebula uses vendored Lua 5.4 for programmable configuration. The API follows
the productive shape of WezTerm's configuration experience, but it is a
Nebula API rather than a compatibility layer: use `require 'nebula'`, and do
not expect an existing `.wezterm.lua` to run unchanged.

Lua configuration is executable local code. Only use configuration files and
modules that you trust. Nebula does not download or execute remote
configuration during discovery.

## Quick Start

Create a localized, annotated configuration:

```text
nebula config init --language system
nebula config init --language zh-CN
nebula config init --language en-US
```

Nebula refuses to overwrite an existing file. `--force` first creates a
timestamped backup and only then atomically replaces the target.

Validate a configuration without opening a window:

```text
nebula config check
nebula config check --config-file D:/configs/nebula.lua
```

## Basic Configuration

```lua
local nebula = require 'nebula'
local config = nebula.config_builder()

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

Lua's `{}` does not distinguish an empty array from an empty object. Nebula
treats an ordinary empty table as an object; use `nebula.array()` for an empty
array:

```lua
config.profiles = nebula.array()

config.profiles = nebula.array {
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
-- nebula.lua
local nebula = require 'nebula'
local config = nebula.config_builder()
config.colors = require 'theme'
return config
```

Files successfully loaded through `require` are added to the live-reload watch
list. Extra local files can be registered with
`nebula.add_to_config_reload_watch_list(path)`.

## Platform Values

The module exposes stable runtime paths and platform data:

```lua
local nebula = require 'nebula'

if nebula.platform.os == 'linux' then
    -- display_server is 'wayland', 'x11', or 'unknown'.
    nebula.log_info('Linux display: ' .. nebula.platform.display_server)
end

-- nebula.config_file, nebula.config_dir, nebula.home_dir
-- nebula.executable_dir, nebula.version, nebula.target_triple
```

## Discovery Order

An explicit `--config-file` wins, followed by `NEBULA_CONFIG_FILE`. Explicit
paths must end in `.lua`, `.toml`, `.yml`, or `.yaml`.

Without an explicit path, Nebula searches all Lua locations before considering
legacy TOML/YAML. An existing but invalid Lua file is authoritative and
reports an error; Nebula does not silently fall back to an older TOML file.

Windows Lua locations:

1. `nebula.lua` beside `nebula.exe` for portable installations.
2. `%APPDATA%/nebula/nebula.lua`.
3. `%USERPROFILE%/.nebula.lua`.

Linux Lua locations:

1. `$XDG_CONFIG_HOME/nebula/nebula.lua`.
2. `$XDG_CONFIG_HOME/nebula.lua`.
3. `$HOME/.config/nebula/nebula.lua`.
4. `$HOME/.nebula.lua`.
5. `/etc/nebula/nebula.lua`.

The same location order is then checked for `.toml`, `.yml`, and `.yaml`.
Existing TOML configurations remain supported; YAML remains a deprecated
transition format.

## Transactional Reload

Configuration parsing and Lua execution run on one serial worker. Rapid file
saves are coalesced, and only the latest successful generation is published.
If Lua syntax, module loading, conversion, or validation fails, Nebula keeps
the last-known-good `UiConfig` and Lua VM alive. Fixing and saving the file
causes the next valid generation to replace them together.

The initial Linux support baseline is x86_64 glibc on Ubuntu 24.04, Debian 12,
and Fedora 42, with both Wayland and X11. Arch Linux is treated as rolling
compatibility coverage rather than a fixed dependency baseline.

## 中文说明

`nebula config init --language zh-CN` 会生成活动代码与英文模板完全一致、
但说明全部为简体中文的 UTF-8 配置。切换 Nebula 界面语言不会改写已经存在的
Lua 文件。普通空表 `{}` 表示对象；空数组必须写成 `nebula.array()`。保存配置
后若出现语法或字段错误，Nebula 会保留上一份可用配置，修正并再次保存即可恢复
自动重载。
