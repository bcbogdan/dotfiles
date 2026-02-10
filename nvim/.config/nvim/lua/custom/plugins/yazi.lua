return {
  {
    'mikavilpas/yazi.nvim',
    event = { 'BufReadPost', 'BufNewFile', 'BufWritePre' },
    keys = {
      -- 👇 in this section, choose your own keymappings!
      {
        '<leader>ty',
        function()
          require('yazi').yazi()
        end,
        desc = '[T]oggle [Y]azy (on current file)',
      },
      {
        -- Open in the current working directory
        '<leader>tY',
        function()
          require('yazi').yazi(nil, vim.fn.getcwd())
        end,
        desc = '[T]oggle [Y]azy (on root directory)',
      },
      {
        '<c-up>',
        function()
          -- NOTE: requires a version of yazi that includes
          -- https://github.com/sxyazi/yazi/pull/1305 from 2024-07-18
          require('yazi').toggle()
        end,
        desc = 'Resume the last yazi session',
      },
    },
    ---@type YaziConfig
    opts = {
      -- if you want to open yazi instead of netrw, see below for more info
      open_for_directories = true,

      -- enable these if you are using the latest version of yazi
      -- use_ya_for_events_reading = true,
      -- use_yazi_client_id_flag = true,

      keymaps = {
        show_help = '<f1>',
      },
    },
  },
}
