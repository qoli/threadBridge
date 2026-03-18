從你這張圖看，這個欄位要填的是 Python 執行檔路徑，不是資料夾。畫面下方也直接舉例了：`/opt/homebrew/bin/python3`。

你可以先試這幾個常見路徑：
- `/opt/homebrew/bin/python3`（Apple Silicon Mac 最常見）
- `/usr/local/bin/python3`（Intel Mac 常見）
- 如果你用虛擬環境，通常會是 `.../.venv/bin/python3`

最穩的方法是在 Terminal 輸入：
`which python3`

它回傳的完整路徑，就是你要填進 `Python Path` 的值。