return {
  {
    'folke/snacks.nvim',
    priority = 1000,
    lazy = false,
    ---@type snacks.Config
    opts = {
      bigfile = { enabled = true },
      bufdelete = { enabled = true },
      explorer = { enabled = true },
      indent = { enabled = true },
      input = { enabled = true },
      notifier = {
        enabled = true,
        timeout = 3000,
      },
      picker = { enabled = true },
      quickfile = { enabled = true },
      scope = { enabled = true },
      scroll = { enabled = true },
      statuscolumn = { enabled = true },
      words = { enabled = true },
      styles = {
        notification = {
          -- wo = { wrap = true } -- Wrap notifications
        },
      },
    },
    keys = {
      -- Top Pickers & Explorer
      {
        '<leader><space>',
        function()
          Snacks.picker.smart()
        end,
        desc = 'Smart Find Files',
      },
      {
        '<leader>,',
        function()
          Snacks.picker.buffers()
        end,
        desc = 'Find Buffers',
      },
      {
        '<leader>/',
        function()
          Snacks.picker.grep()
        end,
        desc = 'Grep',
      },
      {
        '<leader>:',
        function()
          Snacks.picker.command_history()
        end,
        desc = 'Search Command History',
      },
      {
        '<leader>sn',
        function()
          Snacks.picker.notifications()
        end,
        desc = '[S]earch [N]otification History',
      },
      {
        '<leader>tf',
        function()
          Snacks.explorer()
        end,
        desc = '[T]oggle [F]ile Explorer',
      },
      -- find
      -- {
      --   '<leader>sb',
      --   function()
      --     Snacks.picker.buffers()
      --   end,
      --   desc = '[S]ind [B]uffers',
      -- },
      {
        '<leader>sc',
        function()
          Snacks.picker.files { cwd = vim.fn.stdpath 'config' }
        end,
        desc = '[S]earch [C]onfig Files',
      },
      {
        '<leader>sf',
        function()
          Snacks.picker.files()
        end,
        desc = '[S]earch [F]iles',
      },
      -- {
      --   '<leader>fg',
      --   function()
      --     Snacks.picker.git_files()
      --   end,
      --   desc = 'Find Git Files',
      -- },
      -- {
      --   '<leader>fp',
      --   function()
      --     Snacks.picker.projects()
      --   end,
      --   desc = 'Projects',
      -- },
      -- {
      --   '<leader>sr',
      --   function()
      --     Snacks.picker.recent()
      --   end,
      --   desc = '[S]earch [R]ecent',
      -- },
      {
        '<leader>sb',
        function()
          Snacks.picker.git_branches()
        end,
        desc = '[S]earch Git [B]ranches',
      },
      -- git
      {
        '<leader>gl',
        function()
          Snacks.picker.git_log()
        end,
        desc = '[G]it [L]og',
      },
      {
        '<leader>gL',
        function()
          Snacks.picker.git_log_line()
        end,
        desc = '[G]it [L]og Line',
      },
      {
        '<leader>gs',
        function()
          Snacks.picker.git_status()
        end,
        desc = '[G]it [S]tatus',
      },
      -- {
      --   '<leader>gS',
      --   function()
      --     Snacks.picker.git_stash()
      --   end,
      --   desc = 'Git Stash',
      -- },
      {
        '<leader>gd',
        function()
          Snacks.picker.git_diff()
        end,
        desc = '[G]it [D]iff (Hunks)',
      },
      {
        '<leader>gf',
        function()
          Snacks.picker.git_log_file()
        end,
        desc = '[G]it Log [F]ile',
      },
      -- Grep
      {
        '<leader>s/',
        function()
          Snacks.picker.lines()
        end,
        desc = '[S]earch in Current Buffer',
      },
      -- {
      --   '<leader>fB',
      --   function()
      --     Snacks.picker.grep_buffers()
      --   end,
      --   desc = '[F]ind in Open [B]uffers',
      -- },
      -- {
      --   '<leader>fg',
      --   function()
      --     Snacks.picker.grep()
      --   end,
      --   desc = '[F]ind wit [G]rep',
      -- },
      {
        '<leader>sw',
        function()
          Snacks.picker.grep_word()
        end,
        desc = '[S]earch [W]ord',
        mode = { 'n', 'x' },
      },
      -- search
      {
        '<leader>s"',
        function()
          Snacks.picker.registers()
        end,
        desc = '[S]earch Registers',
      },
      {
        '<leader>sh',
        function()
          Snacks.picker.search_history()
        end,
        desc = '[S]earch [H]istory',
      },
      -- {
      --   '<leader>sa',
      --   function()
      --     Snacks.picker.autocmds()
      --   end,
      --   desc = 'Autocmds',
      -- },
      -- {
      --   '<leader>sb',
      --   function()
      --     Snacks.picker.lines()
      --   end,
      --   desc = 'Buffer Lines',
      -- },
      -- {
      --   '<leader>s:',
      --   function()
      --     Snacks.picker.command_history()
      --   end,
      --   desc = '[F]ind in Command History',
      -- },
      -- {
      --   '<leader>sC',
      --   function()
      --     Snacks.picker.commands()
      --   end,
      --   desc = 'Commands',
      -- },
      {
        '<leader>sd',
        function()
          Snacks.picker.diagnostics()
        end,
        desc = '[S]earch [D]iagnostics',
      },
      {
        '<leader>sD',
        function()
          Snacks.picker.diagnostics_buffer()
        end,
        desc = '[S]earch Buffer [D]iagnostics',
      },
      {
        '<leader>sH',
        function()
          Snacks.picker.help()
        end,
        desc = '[S]earch [H]elp Pages',
      },
      -- {
      --   '<leader>sH',
      --   function()
      --     Snacks.picker.highlights()
      --   end,
      --   desc = 'Highlights',
      -- },
      -- {
      --   '<leader>si',
      --   function()
      --     Snacks.picker.icons()
      --   end,
      --   desc = 'Icons',
      -- },
      {
        '<leader>sj',
        function()
          Snacks.picker.jumps()
        end,
        desc = '[S]earch [J]umps',
      },
      {
        '<leader>sk',
        function()
          Snacks.picker.keymaps()
        end,
        desc = '[S]earch [K]eymaps',
      },
      -- {
      --   '<leader>sl',
      --   function()
      --     Snacks.picker.loclist()
      --   end,
      --   desc = 'Location List',
      -- },
      -- {
      --   '<leader>sm',
      --   function()
      --     Snacks.picker.marks()
      --   end,
      --   desc = 'Marks',
      -- },
      -- {
      --   '<leader>sM',
      --   function()
      --     Snacks.picker.man()
      --   end,
      --   desc = 'Man Pages',
      -- },
      -- {
      --   '<leader>sp',
      --   function()
      --     Snacks.picker.lazy()
      --   end,
      --   desc = 'Search for Plugin Spec',
      -- },
      {
        '<leader>sq',
        function()
          Snacks.picker.qflist()
        end,
        desc = '[S]earch [Q]uickfix List',
      },
      {
        '<leader>sR',
        function()
          Snacks.picker.resume()
        end,
        desc = '[S]earch [R]esume',
      },
      {
        '<leader>su',
        function()
          Snacks.picker.undo()
        end,
        desc = '[S]earch [U]ndo History',
      },
      -- {
      --   '<leader>uC',
      --   function()
      --     Snacks.picker.colorschemes()
      --   end,
      --   desc = 'Colorschemes',
      -- },
      -- LSP
      {
        'gd',
        function()
          Snacks.picker.lsp_definitions()
        end,
        desc = '[G]oto Definition',
      },
      {
        'gD',
        function()
          Snacks.picker.lsp_declarations()
        end,
        desc = '[G]oto Declaration',
      },
      {
        'gr',
        function()
          Snacks.picker.lsp_references()
        end,
        nowait = true,
        desc = '[G]oto [R]eferences',
      },
      {
        'gI',
        function()
          Snacks.picker.lsp_implementations()
        end,
        desc = '[G]oto [I]mplementation',
      },
      {
        'gt',
        function()
          Snacks.picker.lsp_type_definitions()
        end,
        desc = '[G]oto [T]ype Definition',
      },
      {
        '<leader>ss',
        function()
          Snacks.picker.lsp_symbols()
        end,
        desc = '[S]earch LSP Symbols',
      },
      {
        '<leader>s.c',
        function()
          Snacks.picker.lsp_symbols { filter = { default = { 'Class' } } }
        end,
        desc = '[S]earch LSP File Classes',
      },
      {
        '<leader>s.p',
        function()
          Snacks.picker.lsp_symbols { filter = { default = { 'Property' } } }
        end,
        desc = '[S]earch LSP File Properties',
      },
      {
        '<leader>s.f',
        function()
          Snacks.picker.lsp_symbols { filter = { default = { 'Function' } } }
        end,
        desc = '[S]earch LSP File Functions',
      },
      {
        '<leader>s.m',
        function()
          Snacks.picker.lsp_symbols { filter = { default = { 'Method' } } }
        end,
        desc = '[S]earch LSP File Methods',
      },
      {
        '<leader>s.x',
        function()
          Snacks.picker.lsp_symbols { filter = { default = { 'Function', 'Method' } } }
        end,
        desc = '[S]earch LSP File Functions and Methods',
      },
      {
        '<leader>s.i',
        function()
          Snacks.picker.lsp_symbols { filter = { default = { 'Interface' } } }
        end,
        desc = '[S]earch LSP Interfaces',
      },
      {
        '<leader>s.C',
        function()
          Snacks.picker.lsp_workspace_symbols { filter = { default = { 'Class' } } }
        end,
        desc = '[S]earch LSP Workspace Classes',
      },
      {
        '<leader>s.P',
        function()
          Snacks.picker.lsp_workspace_symbols { filter = { default = { 'Property' } } }
        end,
        desc = '[S]earch LSP Workspace Properties',
      },
      {
        '<leader>s.F',
        function()
          Snacks.picker.lsp_workspace_symbols { filter = { default = { 'Function' } } }
        end,
        desc = '[S]earch LSP Workspace Functions',
      },
      {
        '<leader>s.M',
        function()
          Snacks.picker.lsp_workspace_symbols { filter = { default = { 'Method' } } }
        end,
        desc = '[S]earch LSP Workspace Methods',
      },
      {
        '<leader>s.X',
        function()
          Snacks.picker.lsp_workspace_symbols { filter = { default = { 'Function', 'Method' } } }
        end,
        desc = '[S]earch LSP Workspace Functions and Methods',
      },
      {
        '<leader>s.I',
        function()
          Snacks.picker.lsp_workspace_symbols { filter = { default = { 'Interface' } } }
        end,
        desc = '[S]earch LSP Workspace Interfaces',
      },
      {
        '<leader>sS',
        function()
          Snacks.picker.lsp_workspace_symbols()
        end,
        desc = '[S]earch LSP Workspace Symbols',
      },
      -- Other
      {
        '<leader>tz',
        function()
          Snacks.zen()
        end,
        desc = '[T]oggle [Z]en Mode',
      },
      {
        '<leader>tz',
        function()
          Snacks.zen.zoom()
        end,
        desc = '[T]oggle [Z]oom',
      },
      {
        '<leader>tb',
        function()
          Snacks.scratch()
        end,
        desc = '[T]oggle Scratch [B]uffer',
      },
      {
        '<leader>uS',
        function()
          Snacks.scratch.select()
        end,
        desc = '[U]i [S]elect Scratch Buffer',
      },
      {
        '<leader>un',
        function()
          Snacks.notifier.show_history()
        end,
        desc = '[U]i [N]otification History',
      },
      {
        '<leader>bd',
        function()
          Snacks.bufdelete()
        end,
        desc = '[B]uffer [D]elete',
      },
      {
        '<leader>cR',
        function()
          Snacks.rename.rename_file()
        end,
        desc = 'Rename File',
      },
      {
        '<leader>gB',
        function()
          Snacks.gitbrowse()
        end,
        desc = '[G]it Open in [B]rowser',
        mode = { 'n', 'v' },
      },
      {
        '<leader>gg',
        function()
          Snacks.lazygit()
        end,
        desc = 'Lazygit',
      },
      {
        '<leader>un',
        function()
          Snacks.notifier.hide()
        end,
        desc = 'Dismiss All Notifications',
      },
      -- {
      --   '<c-/>',
      --   function()
      --     Snacks.terminal()
      --   end,
      --   desc = 'Toggle Terminal',
      -- },
      -- {
      --   '<c-_>',
      --   function()
      --     Snacks.terminal()
      --   end,
      --   desc = 'which_key_ignore',
      -- },
      {
        ']]',
        function()
          Snacks.words.jump(vim.v.count1)
        end,
        desc = 'Next Reference',
        mode = { 'n', 't' },
      },
      {
        '[[',
        function()
          Snacks.words.jump(-vim.v.count1)
        end,
        desc = 'Prev Reference',
        mode = { 'n', 't' },
      },
      --   {
      --     '<leader>N',
      --     desc = 'Neovim News',
      --     function()
      --       Snacks.win {
      --         file = vim.api.nvim_get_runtime_file('doc/news.txt', false)[1],
      --         width = 0.6,
      --         height = 0.6,
      --         wo = {
      --           spell = false,
      --           wrap = false,
      --           signcolumn = 'yes',
      --           statuscolumn = ' ',
      --           conceallevel = 3,
      --         },
      --       }
      --     end,
      --   },
    },
    init = function()
      vim.api.nvim_create_autocmd('User', {
        pattern = 'VeryLazy',
        callback = function()
          -- Setup some globals for debugging (lazy-loaded)
          _G.dd = function(...)
            Snacks.debug.inspect(...)
          end
          _G.bt = function()
            Snacks.debug.backtrace()
          end
          vim.print = _G.dd -- Override print to use snacks for `:=` command

          -- Create some toggle mappings
          Snacks.toggle.option('spell', { name = 'Spelling' }):map '<leader>us'
          Snacks.toggle.option('wrap', { name = 'Wrap' }):map '<leader>uw'
          Snacks.toggle.option('relativenumber', { name = 'Relative Number' }):map '<leader>uL'
          Snacks.toggle.diagnostics():map '<leader>ud'
          Snacks.toggle.line_number():map '<leader>ul'
          Snacks.toggle.option('conceallevel', { off = 0, on = vim.o.conceallevel > 0 and vim.o.conceallevel or 2 }):map '<leader>uc'
          Snacks.toggle.treesitter():map '<leader>uT'
          Snacks.toggle.option('background', { off = 'light', on = 'dark', name = 'Dark Background' }):map '<leader>ub'
          Snacks.toggle.inlay_hints():map '<leader>uh'
          Snacks.toggle.indent():map '<leader>ug'
          Snacks.toggle.dim():map '<leader>uD'

          -- floating terminal
          vim.keymap.set('n', '<leader>tT', function()
            Snacks.terminal()
          end, { desc = 'Terminal (cwd)' })
          vim.keymap.set('n', '<leader>tT', function()
            Snacks.terminal(nil, { cwd = vim.fn.stdpath 'config' })
          end, { desc = 'Terminal (Root Dir)' })
          vim.keymap.set('n', '<c-/>', function()
            Snacks.terminal(nil, { cwd = vim.fn.stdpath 'config' })
          end, { desc = 'Terminal (Root Dir)' })

          -- Terminal Mappings
          vim.keymap.set('t', '<C-_>', '<cmd>close<cr>', { desc = 'which_key_ignore' })
        end,
      })
    end,
  },
}
