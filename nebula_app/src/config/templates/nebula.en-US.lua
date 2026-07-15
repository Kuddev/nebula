local nebula = require 'nebula'
local config = nebula.config_builder()

-- Nebula Lua configuration. Run `nebula config check` after editing to validate syntax and fields.
-- Field names and enum values use stable English identifiers; comments explain safe edits.

-- Number of scrollback lines to retain. Larger values consume more memory.
config.scrolling = {
    history = 10000,
}

-- Font example. Remove the leading `--` from each line below to enable it.
-- config.font = {
--     normal = { family = 'Maple Mono NF CN', style = 'Regular' },
--     size = 13.0,
--     builtin_box_drawing = true,
-- }

-- Window example. opacity ranges from 0.0 to 1.0.
-- config.window = {
--     opacity = 0.96,
--     decorations = 'Full',
--     dynamic_title = true,
--     padding = { x = 8, y = 8 },
-- }

-- Cursor example.
-- config.cursor = {
--     style = { shape = 'Beam', blinking = 'On' },
--     unfocused_hollow = true,
-- }

-- Terminal foreground, background, and the ANSI palette can be overridden independently.
-- config.colors = {
--     primary = { foreground = '#d8dee9', background = '#16181d' },
-- }

-- The default shell can be a program string or a program with arguments.
-- config.terminal = {
--     shell = { program = 'pwsh.exe', args = { '-NoLogo' } },
-- }

-- Linux example: select a shell for the current platform while sharing this file with Windows.
-- if nebula.platform.os == 'linux' then
--     config.terminal = { shell = '/bin/bash' }
-- end

-- Environment variables injected into every new terminal process.
-- config.env = {
--     EDITOR = 'nvim',
-- }

-- Quick-launch profiles. An empty list must use nebula.array(), not a plain empty table {}.
-- config.profiles = nebula.array {
--     { name = 'Production SSH', command = 'ssh', args = { 'user@server' } },
--     { name = 'Project', command = 'pwsh.exe', args = { '-NoLogo' }, cwd = 'D:/src/project' },
-- }

-- Large configurations can use sibling modules; for example, theme.lua can return a colors table:
-- local theme = require 'theme'
-- config.colors = theme
-- Required Lua files are automatically added to the live-reload watch list.

-- Unknown fields are strict by default. This can assist migration but should not stay disabled:
-- config:set_strict_mode(false)

return config
