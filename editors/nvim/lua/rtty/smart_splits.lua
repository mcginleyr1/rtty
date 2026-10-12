-- A smart-splits.nvim backend (protocol 3.0.0): past Neovim's last window, focus and resize
-- move on to rtty's panes.
local rtty = require('rtty')

local M = { name = 'rtty', protocol_version = '3.0.0' }

function M.detect()
  return rtty.here() and vim.fn.executable(rtty.program) == 1
end

-- At an edge, rtty wraps round its tab or opens a pane there when smart-splits asks to.
function M.move(direction, opts)
  opts = opts or {}
  if rtty.focus(direction, opts.at_edge == 'wrap') then
    return true
  end
  return opts.at_edge == 'split' and rtty.split(direction)
end

-- smart-splits counts `amount` in cells, as rtty's layout does.
function M.resize(direction, opts)
  return rtty.resize(direction, (opts or {}).amount)
end

-- smart-splits maps the keys, so rtty can hand them to Neovim.
function M.activate()
  rtty.keys = true
  rtty.activate()
end

function M.health()
  if M.detect() then
    vim.health.ok('in rtty pane ' .. vim.env.RTTY_PANE)
  else
    vim.health.info('not in an rtty pane (or in tutti inside one), or `' .. rtty.program .. '` is not on the PATH')
  end
end

return M
