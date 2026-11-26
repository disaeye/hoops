# Web 篮球模拟页面

```bash
cargo run --release -p hoops-server -- --bind 127.0.0.1:31110
```

在浏览器打开 `127.0.0.1:31110`。移动端从顶部切换新建对局、比赛记录、执教、战报、技术统计和系统状态，球场和播放控制位于所选面板下方。宽统计表可以在表格区域内横向滚动。

- **实时直播**：创建者在实时位置可以暂停或继续服务器模拟。拖动时间轴进入回看，可以调节回看倍率；「回到实时」会跳到最新帧。球队暂停由教练按联赛规则请求。观战身份的播放操作只影响其页面。
- **完整批量录像**：计算结束后可以播放、暂停、调速和拖动时间轴，教练身份可查看战术叠加层。
- **极速批量统计**：计算结束后提供技术统计。

## 验证

```bash
cargo test --release -p hoops-server -p hoops-output -j 2 -- --test-threads=1
node scripts/verify_viewer_codec.mjs
node scripts/verify_court_geometry.mjs
npm install --no-save --no-package-lock playwright
npx playwright install chromium
```

在独立数据目录启动测试服务，再运行浏览器测试，测试会创建比赛并通过页面发起弃权：

```bash
cargo run --release -p hoops-server -- --bind 127.0.0.1:38110 --data-dir data/viewer-verification/standalone
node --max-old-space-size=160 scripts/verify_viewer_browser.mjs http://127.0.0.1:38110/
```
