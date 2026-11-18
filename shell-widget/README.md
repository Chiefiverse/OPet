# shell-widget —— Omarchy 入口插件（唯一事实源）

顶栏图标 + 弹卡控制宠物：启动 / 停止 / 暂停 / 恢复 / 抽一次 / 十连。宠物进程由 `Service.qml`
拉起并监管（一次预检 → spawn → 就绪判据 = stderr 的 `ipc: ` 前缀 → backoff 500·2^(n−1) ms，
连续 3 次失败即停手），控制面走 `opet overlay --ipc`（设计书 §3.12）。

## 这里改代码，那边是产物

`omarchy plugin add <git-url>` 只认「**仓库根就是插件根**」的 git 仓库（实测
`omarchy-plugin-add` = clone 到临时目录后 `omarchy-plugin-validate <stage>`，而 validate 要
`<stage>/manifest.json`），所以主仓的这个**子目录装不了**。发布走摊平：

```sh
bash scripts/publish_plugin.sh            # 摊到 dist/opet.pet 并跑官方 validate（不碰网络）
bash scripts/publish_plugin.sh --verify   # 漂移门禁：产物仓与这里不一致就 exit 1
bash scripts/publish_plugin.sh --push     # 提交并推到 github.com/Chiefiverse/OPetPlugIn
```

⇒ **不要在 `OPetPlugIn` 仓里改代码**，下一次摊平会把它覆盖掉。

三条实测（2026-11-19 拿一个本地裸仓当远端跑出来的）：

- **远端默认分支必须指向有内容的那条**：`plugin add` 克隆的是远端 `HEAD` 指向的分支，不写死 `main`。
  我的裸仓 `HEAD` 指 `master` 而内容在 `main` ⇒ 装的时候 `warning: remote HEAD refers to nonexistent ref`
  + `omarchy-plugin-validate: missing manifest.json` ⇒ **拒装**。所以首推之后要确认 GitHub 那仓的默认分支。
- **`--verify` 抓的是"这里改了没发"，不是"产物被人改"**：摊平排在比较之前，`dist/` 里的任何改动都会先被
  覆盖掉。这符合原意 —— `dist/` 是消耗品，唯一事实源在这一节上面。
- **别用 `--remote` 跑 `--verify`**：安装 URL 会烘进产物 `README.md`，换了 remote 就等于换了内容 ⇒
  门禁会点名 `README.md` 报漂移（这条红是真的，只是不是你改的）。

## 本机这一份

`~/.config/omarchy/plugins/opet.pet` 是 `cp -r` 装的（**没有 `.git`**）⇒ `omarchy plugin update`
对它无效，改完要手动 `cp` 覆盖 + `omarchy-restart-shell`。⚠️ 存盘热重载只刷新 metadata，
**两份 QML 的代码在运行中的 shell 里都不生效**（实测 `shell.qml:1421-1428`）。

## `defaults` 七个键全留空是设计

`manifest.json` 的 `barWidget.defaults` 不含任何路径 ⇒ 发布件里没有作者机器的绝对路径，
没配就 `phase:"refused"` 并点名缺哪一样。配置写进 `~/.config/omarchy/shell.json` 的 layout 项，
由 `BarWidget.qml` 在启动时推给 service（`applyConfig`）。⚠️ 那条 `cfg` **不许写成绑定属性**：
宿主每次写 `shell.json` 都重新注入 `manifest`，绑上去就会被静默清空（设计书 §6.1.23②）。
