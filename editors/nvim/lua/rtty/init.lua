-- rtty.nvim: Neovim in an rtty pane. Inert outside rtty ($RTTY_PANE unset), and inside tutti
-- ($TUTTI_PANE set), where tutti's own plugin owns navigation.
local M = {
  -- Whether Neovim handles rtty's focus and resize keys: only then does it claim them.
  keys = false,
  timeout = 500,
  program = 'rtty',
}

local wincmd = { left = 'h', down = 'j', up = 'k', right = 'l' }
local arrow = { left = 'Left', down = 'Down', up = 'Up', right = 'Right' }

-- In an rtty pane, and not in tutti inside it.
function M.here()
  return vim.env.RTTY_PANE ~= nil and vim.env.TUTTI_PANE == nil
end

-- OSC 1337 SetUserVar; an empty value clears the var.
local function user_var(name, value)
  return ('\27]1337;SetUserVar=%s=%s\7'):format(name, vim.base64.encode(value))
end

-- Writes to the terminal rtty reads: through the TUI since 0.12, in order with its own output;
-- before that, through the server's stderr.
local function send(data)
  if vim.api.nvim_ui_send then
    vim.api.nvim_ui_send(data)
  else
    vim.api.nvim_chan_send(vim.v.stderr, data)
  end
end

-- Claims rtty's keys while on (and `keys` is set), hands them back while off.
function M.announce(on)
  if M.here() then
    send(user_var('RTTY_KEYS', on and M.keys and 'pane' or ''))
  end
end

-- Runs `rtty cli <args>`, output discarded, and returns its exit code: -1 when it could not
-- run, 124 when it outlived the timeout. Never throws.
function M.run(args)
  local ok, result = pcall(function()
    local opts = { timeout = M.timeout, stdout = false, stderr = false }
    return vim.system(vim.list_extend({ M.program, 'cli' }, args), opts):wait()
  end)
  return ok and result and result.code or -1
end

-- `rtty cli pane <verb> --direction <direction> --from $RTTY_PANE <extra>`; true when it
-- succeeded. Outside rtty there is no pane to act from.
local function from_here(verb, direction, extra)
  local args = { 'pane', verb, '--direction', direction, '--from', vim.env.RTTY_PANE }
  return M.here() and M.run(vim.list_extend(args, extra)) == 0
end

-- Focuses the rtty pane beyond this one; true when focus moved.
function M.focus(direction, wrap)
  return from_here('focus', direction, wrap and { '--wrap' } or {})
end

-- Moves rtty's split border: by `amount` of the split when it is under 1, by that many cells
-- when it is 1 or more, by rtty's step (5%) when nil.
function M.resize(direction, amount)
  local extra = {}
  if amount and amount >= 1 then
    extra = { '--cells', tostring(math.floor(amount)) }
  elseif amount then
    extra = { '--amount', tostring(amount) }
  end
  return from_here('resize', direction, extra)
end

-- Opens an rtty pane beside this one, which takes the keyboard; true when it opened.
function M.split(direction)
  return M.here() and M.run({ 'pane', 'split', vim.env.RTTY_PANE, direction }) == 0
end

-- Moves to the Neovim window in `direction`, or past Neovim's edge to the rtty pane there.
function M.navigate(direction)
  assert(wincmd[direction], 'rtty: direction must be left, right, up or down')
  local win = vim.api.nvim_get_current_win()
  vim.cmd.wincmd(wincmd[direction])
  if vim.api.nvim_get_current_win() == win then
    M.focus(direction)
  end
end

-- Registers the claims (idempotent), and claims now when VimEnter has already fired, as it has
-- for a lazy-loaded plugin.
function M.activate()
  local group = vim.api.nvim_create_augroup('rtty', {})
  vim.api.nvim_create_autocmd({ 'VimEnter', 'VimResume' }, {
    group = group,
    callback = function()
      M.announce(true)
    end,
  })
  vim.api.nvim_create_autocmd({ 'VimLeavePre', 'VimSuspend' }, {
    group = group,
    callback = function()
      M.announce(false)
    end,
  })
  if vim.v.vim_did_enter == 1 then
    M.announce(true)
  end
end

-- `keys = true` maps, in normal mode, tutti's Ctrl+h/j/k/l (navigate) and Alt+h/j/k/l (resize),
-- and rtty's Cmd+Opt+arrows (navigate) and Cmd+Ctrl+arrows (resize), which reach Neovim as
-- <D-M-arrow> and <D-C-arrow> once it claims them; then claims them.
function M.setup(opts)
  if not M.here() or not (opts or {}).keys then
    return
  end
  M.keys = true
  for direction, key in pairs(wincmd) do
    local function navigate()
      M.navigate(direction)
    end
    local function resize()
      M.resize(direction)
    end
    local focus_desc = { desc = 'rtty: focus ' .. direction }
    local resize_desc = { desc = 'rtty: resize ' .. direction }
    vim.keymap.set('n', '<C-' .. key .. '>', navigate, focus_desc)
    vim.keymap.set('n', '<M-' .. key .. '>', resize, resize_desc)
    vim.keymap.set('n', '<D-M-' .. arrow[direction] .. '>', navigate, focus_desc)
    vim.keymap.set('n', '<D-C-' .. arrow[direction] .. '>', resize, resize_desc)
  end
  M.activate()
end

return M
