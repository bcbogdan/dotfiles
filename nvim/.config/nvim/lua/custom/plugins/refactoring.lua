return {
  {
    'ThePrimeagen/refactoring.nvim',
    commit = '6784b54587e6d8a6b9ea199318512170ffb9e418',
    event = { 'BufReadPre', 'BufNewFile' },
    dependencies = {
      'nvim-lua/plenary.nvim',
      'nvim-treesitter/nvim-treesitter',
    },
    keys = {
      { '<leader>r', '', desc = '[R]efactor', mode = { 'n', 'v' } },
      -- {
      --   '<leader>rs',
      --   pick,
      --   mode = 'v',
      --   desc = 'Refactor',
      -- },
      {
        '<leader>ri',
        function()
          require('refactoring').refactor 'Inline Variable'
        end,
        mode = { 'n', 'v' },
        desc = '[R]efactor [I]nline Variable',
      },
      {
        '<leader>rb',
        function()
          require('refactoring').refactor 'Extract Block'
        end,
        desc = '[R]efactor Extract [B]lock',
      },
      {
        '<leader>rf',
        function()
          require('refactoring').refactor 'Extract Block To File'
        end,
        desc = '[R]efactor Extract Block To [F]ile',
      },
      {
        '<leader>rP',
        function()
          require('refactoring').debug.printf { below = false }
        end,
        desc = '[R]efactor Debug [P]rint',
      },
      {
        '<leader>rp',
        function()
          require('refactoring').debug.print_var { normal = true }
        end,
        desc = '[Refactor] Debug [P]rint Variable',
      },
      {
        '<leader>rc',
        function()
          require('refactoring').debug.cleanup {}
        end,
        desc = '[R]efactor Debug [C]leanup',
      },
      {
        '<leader>rf',
        function()
          require('refactoring').refactor 'Extract Function'
        end,
        mode = 'v',
        desc = '[R]efactor Extract [F]unction',
      },
      {
        '<leader>rF',
        function()
          require('refactoring').refactor 'Extract Function To File'
        end,
        mode = 'v',
        desc = '[R]efactor Extract [F]unction To File',
      },
      {
        '<leader>rx',
        function()
          require('refactoring').refactor 'Extract Variable'
        end,
        mode = 'v',
        desc = '[R]efactor E[x]tract Variable',
      },
      {
        '<leader>rp',
        function()
          require('refactoring').debug.print_var()
        end,
        mode = 'v',
        desc = '[R]efactor Debug [P]rint Variable',
      },
    },
    opts = {
      prompt_func_return_type = {
        go = false,
        java = false,
        cpp = false,
        c = false,
        h = false,
        hpp = false,
        cxx = false,
      },
      prompt_func_param_type = {
        go = false,
        java = false,
        cpp = false,
        c = false,
        h = false,
        hpp = false,
        cxx = false,
      },
      printf_statements = {},
      print_var_statements = {},
      show_success_message = true, -- shows a message with information about the refactor on success
      -- i.e. [Refactor] Inlined 3 variable occurrences
    },
  },
}
