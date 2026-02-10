return {
  {
    'karb94/neoscroll.nvim',
    config = function()
      require('neoscroll').setup {
        -- Enable animations only for mouse scrolling
        mappings = {},
        hide_cursor = true,
        stop_eof = true,
        respect_scrolloff = false,
        cursor_scrolls_alone = true,
        performance_mode = false,
      }
    end,
  },
}
