return { -- Adds git related signs to the gutter, as well as utilities for managing changes
  'lewis6991/gitsigns.nvim',
  opts = {
    signs = {
      add = { text = '+' },
      change = { text = '~' },
      delete = { text = '_' },
      topdelete = { text = '‾' },
      changedelete = { text = '~' },
    },
    on_attach = function(bufnr)
      local gitsigns = require 'gitsigns'

      local function map(mode, l, r, opts)
        opts = opts or {}
        opts.buffer = bufnr
        vim.keymap.set(mode, l, r, opts)
      end

      -- Navigation
      map('n', ']c', function()
        if vim.wo.diff then
          vim.cmd.normal { ']c', bang = true }
        else
          gitsigns.nav_hunk 'next'
        end
      end, { desc = 'Jump to next git [c]hange' })

      map('n', '[c', function()
        if vim.wo.diff then
          vim.cmd.normal { '[c', bang = true }
        else
          gitsigns.nav_hunk 'prev'
        end
      end, { desc = 'Jump to previous git [c]hange' })

      -- Actions
      -- visual mode
      map('v', '<leader>gs', function()
        gitsigns.stage_hunk { vim.fn.line '.', vim.fn.line 'v' }
      end, { desc = '[G]it [S]tage Hunk' })
      map('v', '<leader>gr', function()
        gitsigns.reset_hunk { vim.fn.line '.', vim.fn.line 'v' }
      end, { desc = '[G]it [R]eset Hunk' })
      -- normal mode
      map('n', '<leader>gs', gitsigns.stage_hunk, { desc = '[G]it [S]tage Hunk' })
      map('n', '<leader>gr', gitsigns.reset_hunk, { desc = '[G]it  [R]eset Hunk' })
      map('n', '<leader>gS', gitsigns.stage_buffer, { desc = '[G]it [S]tage Buffer' })
      map('n', '<leader>gu', gitsigns.stage_hunk, { desc = '[G]it [u]ndo Stage Hunk' })
      map('n', '<leader>gR', gitsigns.reset_buffer, { desc = '[G]it [R]eset Buffer' })
      map('n', '<leader>gp', gitsigns.preview_hunk, { desc = '[G]it [P]review Hunk' })
      map('n', '<leader>gb', gitsigns.blame_line, { desc = '[G]it [B]lame Line' })
      map('n', '<leader>gd', gitsigns.diffthis, { desc = '[G]it [D]iff Against Index' })
      map('n', '<leader>gD', function()
        gitsigns.diffthis '@'
      end, { desc = '[G]it [D]iff Against Last Commit' })
      -- Toggles
      map('n', '<leader>tb', gitsigns.toggle_current_line_blame, { desc = 'Git: [T]oggle [B]lame line' })
      map('n', '<leader>tD', gitsigns.preview_hunk_inline, { desc = 'Git: [T]oggle Show [D]eleted' })
      map('n', '<leader>tg', gitsigns.toggle_signs, { desc = 'Git: [T]oggle [G]itsigns' })
    end,
  },
}
