-- Headless tests for rtty.nvim against the stub editors/tests/rtty:
--   nvim --headless -u NONE -l <abs path>/editors/nvim/tests/run.lua
local plugin = vim.fn.fnamemodify(arg[0], ':p:h:h')
vim.opt.rtp:prepend(plugin)
local stub = vim.fn.tempname()
vim.env.RTTY_STUB = stub
vim.env.PATH = vim.fn.fnamemodify(plugin, ':h') .. '/tests:' .. vim.env.PATH
vim.env.RTTY_PANE = nil
vim.env.TUTTI_PANE = nil

local failed = 0
local function test(name, fn)
  vim.fn.delete(stub, 'rf')
  vim.fn.mkdir(stub, 'p')
  vim.cmd('silent! only')
  local ok, err = pcall(fn)
  io.stdout:write((ok and 'ok   ' or 'FAIL ') .. name .. (ok and '' or '\n     ' .. tostring(err)) .. '\n')
  failed = failed + (ok and 0 or 1)
end

local function eq(expected, got)
  if not vim.deep_equal(expected, got) then
    error(('expected %s, got %s'):format(vim.inspect(expected), vim.inspect(got)), 2)
  end
end

-- How the stub answers its nth call: { out =, err =, sleep =, exit = }.
local function answer(n, files)
  for name, value in pairs(files) do
    vim.fn.writefile({ tostring(value) }, ('%s/%d.%s'):format(stub, n, name))
  end
end

local function calls()
  return vim.fn.filereadable(stub .. '/log') == 1 and vim.fn.readfile(stub .. '/log') or {}
end

local sent = {}
vim.api.nvim_ui_send = function(data)
  table.insert(sent, data)
end
local function keys(value)
  return ('\27]1337;SetUserVar=RTTY_KEYS=%s\7'):format(vim.base64.encode(value))
end

local rtty = require('rtty')
local backend = require('rtty.smart_splits')

test('inert outside rtty', function()
  rtty.setup({ keys = true })
  eq('', vim.fn.maparg('<C-h>', 'n'))
  eq(false, backend.detect())
  vim.cmd.vsplit()
  rtty.navigate('left')
  eq({ false, false, false }, { rtty.focus('left'), rtty.resize('up', 0.1), rtty.split('down') })
  rtty.announce(true)
  eq({}, sent)
  eq({}, calls())
end)

test('inert inside tutti, whose plugin owns navigation there', function()
  vim.env.RTTY_PANE, vim.env.TUTTI_PANE = '7', '3'
  rtty.setup({ keys = true })
  eq('', vim.fn.maparg('<C-h>', 'n'))
  eq(false, backend.detect())
  eq(false, rtty.focus('left'))
  rtty.announce(true)
  vim.env.RTTY_PANE, vim.env.TUTTI_PANE = nil, nil
  eq({}, sent)
  eq({}, calls())
end)

vim.env.RTTY_PANE = '7'

test('navigate moves between windows, then past the edge to rtty', function()
  vim.cmd.vsplit()
  local left = vim.api.nvim_get_current_win()
  rtty.navigate('right')
  assert(vim.api.nvim_get_current_win() ~= left, 'stayed in the left window')
  eq({}, calls())
  rtty.navigate('right')
  eq({ '[cli][pane][focus][--direction][right][--from][7]' }, calls())
  eq(false, pcall(rtty.navigate, 'north'))
end)

test('backend: protocol fields and detect', function()
  eq({ 'rtty', '3.0.0' }, { backend.name, backend.protocol_version })
  eq(true, backend.detect())
end)

test('backend: move is true iff rtty moved focus, wrapping or splitting as asked', function()
  answer(2, { exit = 1 })
  answer(5, { exit = 1 })
  eq(true, backend.move('left', {}))
  eq(false, backend.move('down', { at_edge = 'stop' }))
  eq(true, backend.move('up', { at_edge = 'wrap' }))
  eq(true, backend.move('right'))
  eq(true, backend.move('down', { at_edge = 'split' }))
  eq({
    '[cli][pane][focus][--direction][left][--from][7]',
    '[cli][pane][focus][--direction][down][--from][7]',
    '[cli][pane][focus][--direction][up][--from][7][--wrap]',
    '[cli][pane][focus][--direction][right][--from][7]',
    '[cli][pane][focus][--direction][down][--from][7]',
    '[cli][pane][split][7][down]',
  }, calls())
end)

test('backend: move gives up at the timeout', function()
  answer(1, { sleep = 3 })
  rtty.timeout = 100
  local start = vim.uv.hrtime()
  local moved = backend.move('left', {})
  rtty.timeout = 500
  eq(false, moved)
  assert((vim.uv.hrtime() - start) / 1e6 < 1000, 'waited past the timeout')
end)

test('backend: move without rtty to run is false, not an error', function()
  local program = rtty.program
  rtty.program = '/nonexistent/rtty'
  local ok, moved = pcall(backend.move, 'left', {})
  rtty.program = program
  eq({ true, false }, { ok, moved })
end)

test("backend: resize takes smart-splits' cells; rtty.resize takes a fraction too", function()
  answer(2, { exit = 1 })
  eq(true, backend.resize('left', { amount = 3 }))
  eq(false, backend.resize('down', {}))
  eq(true, rtty.resize('up', 0.1))
  eq({
    '[cli][pane][resize][--direction][left][--from][7][--cells][3]',
    '[cli][pane][resize][--direction][down][--from][7]',
    '[cli][pane][resize][--direction][up][--from][7][--amount][0.1]',
  }, calls())
end)

test('backend: activate claims the keys', function()
  sent = {}
  backend.activate()
  eq({ keys('pane') }, sent)
  sent = {}
  vim.api.nvim_exec_autocmds('VimResume', {})
  eq({ keys('pane') }, sent)
  rtty.keys = false
end)

test('suspend and leave hand the keys back, resume claims them again', function()
  rtty.keys = true
  rtty.activate()
  sent = {}
  vim.api.nvim_exec_autocmds('VimSuspend', {})
  vim.api.nvim_exec_autocmds('VimResume', {})
  vim.api.nvim_exec_autocmds('VimLeavePre', {})
  eq({ keys(''), keys('pane'), keys('') }, sent)
  rtty.keys = false
end)

test("setup({ keys = true }) maps tutti's keys and rtty's chords and claims them", function()
  sent = {}
  rtty.setup({ keys = true })
  eq({ keys('pane') }, sent)
  vim.cmd.vsplit()
  vim.api.nvim_feedkeys(vim.keycode('<C-l><C-l><C-h><C-h><C-j><M-k>'), 'x', false)
  -- Back in the left window: the chords move right within Neovim, then left past its edge.
  vim.api.nvim_feedkeys(vim.keycode('<D-M-Right><D-M-Left><D-M-Left><D-C-Down>'), 'x', false)
  eq({
    '[cli][pane][focus][--direction][right][--from][7]',
    '[cli][pane][focus][--direction][left][--from][7]',
    '[cli][pane][focus][--direction][down][--from][7]',
    '[cli][pane][resize][--direction][up][--from][7]',
    '[cli][pane][focus][--direction][left][--from][7]',
    '[cli][pane][resize][--direction][down][--from][7]',
  }, calls())
end)

if failed > 0 then
  io.stdout:write(failed .. ' failed\n')
  os.exit(1)
end
