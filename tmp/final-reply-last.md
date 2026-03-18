這個工具就是你剛剛找的那個清理類工具，功能上是對的。

`mole --help` 顯示它的定位是 `Deep clean and optimize your Mac`，主要命令其實是 `mo`，不是直接打子命令在 `mole` 後面。重點功能有：
- `mo clean`：清理磁碟空間
- `mo purge`：刪除舊的專案產物
- `mo analyze`：分析磁碟使用
- `mo uninstall`：完整移除 App
- `mo optimize`：系統維護

如果你是想找能清 Xcode 衍生垃圾的工具，`mo purge` 和 `mo clean` 最值得看。  
要的話我可以下一步幫你跑 `mo purge --help` 或 `mo clean --dry-run`，看看它會不會掃到 Xcode 的 `DerivedData`、Archives 這類內容。