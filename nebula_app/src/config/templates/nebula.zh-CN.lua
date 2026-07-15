local nebula = require 'nebula'
local config = nebula.config_builder()

-- Nebula Lua 配置文件。修改后可运行 `nebula config check` 检查语法和字段。
-- 字段名和枚举值使用稳定的英文标识；注释使用简体中文，方便直接修改。

-- 保留的滚动历史行数。较大的值会占用更多内存。
config.scrolling = {
    history = 10000,
}

-- 字体示例。取消下面各行开头的 `--` 即可启用。
-- config.font = {
--     normal = { family = 'Maple Mono NF CN', style = 'Regular' },
--     size = 13.0,
--     builtin_box_drawing = true,
-- }

-- 窗口示例。opacity 范围为 0.0 到 1.0。
-- config.window = {
--     opacity = 0.96,
--     decorations = 'Full',
--     dynamic_title = true,
--     padding = { x = 8, y = 8 },
-- }

-- 光标示例。
-- config.cursor = {
--     style = { shape = 'Beam', blinking = 'On' },
--     unfocused_hollow = true,
-- }

-- 终端前景色、背景色与 ANSI 调色板可以分别覆盖。
-- config.colors = {
--     primary = { foreground = '#d8dee9', background = '#16181d' },
-- }

-- 默认 Shell 可以是程序字符串，也可以带参数。
-- config.terminal = {
--     shell = { program = 'pwsh.exe', args = { '-NoLogo' } },
-- }

-- Linux 示例：根据运行平台选择 Shell。此段可以和 Windows 共用同一份配置。
-- if nebula.platform.os == 'linux' then
--     config.terminal = { shell = '/bin/bash' }
-- end

-- 注入到所有新终端进程的环境变量。
-- config.env = {
--     EDITOR = 'nvim',
-- }

-- 快速启动配置。空列表必须写成 nebula.array()，不能写普通空表 {}。
-- config.profiles = nebula.array {
--     { name = 'Production SSH', command = 'ssh', args = { 'user@server' } },
--     { name = 'Project', command = 'pwsh.exe', args = { '-NoLogo' }, cwd = 'D:/src/project' },
-- }

-- 大型配置可拆分到同目录模块，例如 theme.lua 返回一个 colors 表：
-- local theme = require 'theme'
-- config.colors = theme
-- 被 require 的 Lua 文件会自动加入热重载监视列表。

-- 配置默认严格检查未知字段。迁移旧配置时可临时关闭，但不建议长期使用：
-- config:set_strict_mode(false)

return config
