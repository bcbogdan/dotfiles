-- Configure tab options
return {
  'akinsho/bufferline.nvim',
  dependencies = {
    'nvim-tree/nvim-web-devicons',
  },
  event = 'VeryLazy',
  version = '*',
  keys = {
    { '<leader>bp', '<Cmd>BufferLineTogglePin<CR>', desc = '[B]uffer Pin/Unpin' },
    { '<leader>bD', '<Cmd>BufferLineGroupClose ungrouped<CR>', desc = '[B]uffer [D]elete Non-Pinned Buffers' },
    { '<leader>br', '<Cmd>BufferLineCloseRight<CR>', desc = '[B]uffers Delete to the [R]ight' },
    { '<leader>bl', '<Cmd>BufferLineCloseLeft<CR>', desc = '[B]uffers Delete to the [L]eft' },
    { '<S-h>', '<cmd>BufferLineCyclePrev<cr>', desc = 'Prev Buffer' },
    { '<S-l>', '<cmd>BufferLineCycleNext<cr>', desc = 'Next Buffer' },
    { '[b', '<cmd>BufferLineCyclePrev<cr>', desc = 'Prev Buffer' },
    { ']b', '<cmd>BufferLineCycleNext<cr>', desc = 'Next Buffer' },
    { '[B', '<cmd>BufferLineMovePrev<cr>', desc = 'Move buffer prev' },
    { ']B', '<cmd>BufferLineMoveNext<cr>', desc = 'Move buffer next' },
    { '<leader>bb', '<cmd>e #<cr>', desc = '[B]uffer switch to Other [B]uffer' },
    { '<leader>bd', '<cmd>bdelete<cr>', desc = '[B]uffer [D]elete' },
    { '<leader>qq', '<cmd>qa<cr>', desc = '[Q]uit [Q]uit all buffers and exit' },
  },
  opts = {
    options = {
      -- stylua: ignore
      close_command = function(n) Snacks.bufdelete(n) end,
      -- stylua: ignore
      right_mouse_command = function(n) Snacks.bufdelete(n) end,
      diagnostics = 'nvim_lsp',
      -- always_show_bufferline = false,
      -- diagnostics_indicator = function(_, _, diag)
      --   local icons = {
      --     error = ' ',
      --     warn = ' ',
      --     info = ' ',
      --     hint = ' ',
      --   }
      --   local ret = (diag.error and icons.Error .. diag.error .. ' ' or '') .. (diag.warning and icons.Warn .. diag.warning or '')
      --   return vim.trim(ret)
      -- end,
      -- offsets = {
      --   {
      --     filetype = 'snacks_layout_box',
      --   },
      -- },
    },
  },
  -- config = function()
  --   require('bufferline').setup {
  --     options = {
  --       mode = 'buffers',
  --       show_buffer_icons = vim.g.have_nerd_font or false,
  --       show_buffer_close_icons = vim.g.have_nerd_font or false,
  --       show_close_icon = vim.g.have_nerd_font or false,
  --       diagnostics = 'nvim_lsp',
  --       always_show_bufferline = true,
  --     },
  --   }
  --
  --   -- Keymaps for navigating buffers
  --   vim.keymap.set('n', ']b', '<cmd>BufferLineCycleNext<cr>', { desc = 'Next buffer' })
  --   vim.keymap.set('n', '[b', '<cmd>BufferLineCyclePrev<cr>', { desc = 'Previous buffer' })
  -- end,
  config = function(_, opts)
    require('bufferline').setup(opts)
    -- Fix bufferline when restoring a session
    vim.api.nvim_create_autocmd({ 'BufAdd', 'BufDelete' }, {
      callback = function()
        vim.schedule(function()
          pcall(nvim_bufferline)
        end)
      end,
    })
  end,
}
