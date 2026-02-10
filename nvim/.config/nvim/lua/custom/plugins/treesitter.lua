local parsers = {
  'bash',
  'c',
  'diff',
  'go',
  'gomod',
  'gowork',
  'gosum',
  'html',
  'javascript',
  'json',
  'lua',
  'markdown',
  'markdown_inline',
  'python',
  'query',
  'regex',
  'swift',
  'toml',
  'tsx',
  'typescript',
  'vim',
  'vimdoc',
  'xml',
  'yaml',
}

return {
  {
    'nvim-treesitter/nvim-treesitter',
    config = function()
      local ts = require 'nvim-treesitter'
      ts.setup {}

      local group = vim.api.nvim_create_augroup('custom-treesitter-start', { clear = true })
      vim.api.nvim_create_autocmd('FileType', {
        group = group,
        callback = function(event)
          pcall(vim.treesitter.start, event.buf)
        end,
      })

      vim.api.nvim_create_user_command('TSInstallRequired', function()
        if vim.fn.executable 'tree-sitter' == 0 then
          vim.notify('tree-sitter CLI not found. Install it with: brew install tree-sitter', vim.log.levels.ERROR)
          return
        end

        local ok = require('nvim-treesitter').install(parsers, { summary = true }):wait(300000)
        if not ok then
          vim.notify('Some treesitter parsers failed to install. Run :messages for details.', vim.log.levels.WARN)
        end
      end, { desc = 'Install required treesitter parsers' })
    end,
  },
  {
    'nvim-treesitter/nvim-treesitter-textobjects',
    dependencies = { 'nvim-treesitter/nvim-treesitter' },
    keys = {
      {
        ']f',
        function()
          require('nvim-treesitter-textobjects.move').goto_next_start('@function.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Next Function Start',
      },
      {
        ']F',
        function()
          require('nvim-treesitter-textobjects.move').goto_next_end('@function.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Next Function End',
      },
      {
        '[f',
        function()
          require('nvim-treesitter-textobjects.move').goto_previous_start('@function.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Prev Function Start',
      },
      {
        '[F',
        function()
          require('nvim-treesitter-textobjects.move').goto_previous_end('@function.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Prev Function End',
      },
      {
        ']c',
        function()
          if vim.wo.diff then
            vim.cmd 'normal! ]c'
            return
          end
          require('nvim-treesitter-textobjects.move').goto_next_start('@class.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Next Class Start',
      },
      {
        ']C',
        function()
          if vim.wo.diff then
            vim.cmd 'normal! ]C'
            return
          end
          require('nvim-treesitter-textobjects.move').goto_next_end('@class.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Next Class End',
      },
      {
        '[c',
        function()
          if vim.wo.diff then
            vim.cmd 'normal! [c'
            return
          end
          require('nvim-treesitter-textobjects.move').goto_previous_start('@class.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Prev Class Start',
      },
      {
        '[C',
        function()
          if vim.wo.diff then
            vim.cmd 'normal! [C'
            return
          end
          require('nvim-treesitter-textobjects.move').goto_previous_end('@class.outer', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Prev Class End',
      },
      {
        ']a',
        function()
          require('nvim-treesitter-textobjects.move').goto_next_start('@parameter.inner', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Next Parameter',
      },
      {
        ']A',
        function()
          require('nvim-treesitter-textobjects.move').goto_next_end('@parameter.inner', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Next Parameter End',
      },
      {
        '[a',
        function()
          require('nvim-treesitter-textobjects.move').goto_previous_start('@parameter.inner', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Prev Parameter',
      },
      {
        '[A',
        function()
          require('nvim-treesitter-textobjects.move').goto_previous_end('@parameter.inner', 'textobjects')
        end,
        mode = { 'n', 'x', 'o' },
        desc = 'Prev Parameter End',
      },
      {
        'if',
        function()
          require('nvim-treesitter-textobjects.select').select_textobject('@function.inner', 'textobjects')
        end,
        mode = { 'x', 'o' },
        desc = 'Inner Function',
      },
      {
        'af',
        function()
          require('nvim-treesitter-textobjects.select').select_textobject('@function.outer', 'textobjects')
        end,
        mode = { 'x', 'o' },
        desc = 'Around Function',
      },
    },
    config = function()
      require('nvim-treesitter-textobjects').setup {
        select = {
          lookahead = true,
        },
        move = {
          set_jumps = true,
        },
      }
    end,
  },
}
