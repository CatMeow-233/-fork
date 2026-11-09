# Codex CLI 啟動速度：調查、量測與修正

- 範圍：從輸入 `codex` 到第一個 turn 真正送出請求為止（看到輸入框、可以打字、按 Enter 後模型收到請求）。
- 依據：上游基準 `8703d32`，修正在本分支的提交 `Cut serialized waits and duplicate work from TUI startup`。
- 方法：先由多個平行調查逐段閱讀啟動關鍵路徑（CLI 入口、首次繪製前、首次繪製後、app-server 的 `thread/start`、daemon 與傳輸層、共用基礎設施），每項發現都附程式碼證據；再用偽終端實測；修正後經過對抗式審查、clippy 與單元／整合測試。

## 1. 結論

預設的 `codex` 啟動會走「背景 daemon」模式（`DaemonAutoStart` 預設開啟）。這條路徑上最奇怪、也最昂貴的一點是：

> **每次啟動都會另外建立一個 OTel provider，只為了記錄一筆 `codex.daemon.start` 計數，然後同步等待它把指標上傳完成（上限 500 ms）才繼續。**

release build 預設使用 Statsig 匯出器（`https://ab.chatgpt.com/otlp/v1/metrics`），所以每個使用者每次啟動都要付出一次 DNS＋TCP＋TLS＋POST 的往返，網路慢時就是整整 500 ms。debug build 沒有預設匯出器，所以開發者在本機通常感覺不到。

其餘成本是一連串可以重疊卻被串行執行、或重複執行的小步驟：每次啟動 fork 兩次 `codex` 子程序（`--version`、`pid-update-loop --help`）、連到 daemon 三次並各做一次 `initialize` 握手、`/models` 同時被請求兩到三次、`account/read` 讀完就丟、在不回應 OSC 10/11 的終端上白等 250 ms 等。

## 2. 量測

### 2.1 方法

`scripts/startup_bench.py`（只用 Python 標準函式庫）：

- 在偽終端中執行 TUI，並像一般終端一樣回應啟動探測（DA1、CPR、OSC 10/11、kitty 鍵盤）。
- 輸入框一出現就輸入 `hello`，0.3 秒後按 Enter（太快會被當成貼上）。
- 本機啟動一個 Responses API 模擬伺服器，記錄**第一個 `/responses` 請求到達的時間**，這就是「第一個 turn 真正開始」的時間點。
- daemon 模式需要打包好的 CLI，腳本會把二進位檔複製進最小的套件目錄結構。
- `--metrics-rtt-ms` 會設定一個 OTLP HTTP 指標匯出器，指向模擬伺服器並延遲回應，用來重現 release build 的 Statsig 上傳。

以下數字都是 4 核心雲端容器上的 **debug build**，CODEX_HOME 是空的，使用 API key 認證與本機模擬模型供應商，所以不包含 ChatGPT 帳號檢查、token 更新、`/models` 網路請求等真實網路成本。release build 的 CPU 部分會快很多，但等待類的成本（網路、子程序、輪詢）不會變。

### 2.2 結果（第一個 `/responses` 請求的時間，秒）

修正前的數字是在同一台機器、同一套量測方式下對上游基準 `8703d32` 的量測；修正後是本分支最終版本（包含模組拆分）。每格是多次執行的範圍。

| 情境 | 修正前 | 修正後 |
|---|---|---|
| daemon 已在執行，指標匯出器延遲 400 ms（模擬 release 預設的 Statsig 上傳） | 1.50–1.76 | **0.87–0.97** |
| daemon 已在執行 | 0.89–1.32 | 0.86–1.03 |
| daemon 已安裝但未執行 | 1.52–1.61 | 1.30–1.64 |
| 嵌入模式（`--no-daemon`） | 0.85–1.13 | 0.76–1.00 |
| 第一次啟動（安裝 daemon 套件並啟動） | 3.97 | 4.06–4.23 |

怎麼讀這張表：

- 量測腳本在輸入框出現後先打字、等 0.3 秒才按 Enter，所以約 0.4 秒是固定的下限。扣掉後，第一列「按下 Enter 到請求送出」從約 1.1–1.35 秒降到約 0.46–0.56 秒，少了一半以上。這一列最接近使用者在 release build 上的實際情況。
- 其餘情境沒有網路等待，主要剩 debug build 的 CPU 成本，改善幅度較小。release build 的 CPU 部分快很多，所以等待類成本在總時間裡的比例更高。
- 第一次啟動要複製並雜湊整個 CLI 套件（debug build 約 675 MB），本分支只把它移出 UI 任務、讓輸入框保持可用，並沒有減少總工作量；兩次數字的差異在量測誤差範圍內。
- 這些情境都沒有 ChatGPT 登入、`/models` 網路請求或 token 刷新，所以第 3 節裡「合併 `/models` 請求」等修正的效果沒有反映在表中。

## 3. 找到的原因與處理方式

狀態：✅ 已修正 · ⏸ 未修正（理由見備註）

### 3.1 daemon 啟動路徑

| 狀態 | 原因 | 成本 | 處理 |
|---|---|---|---|
| ✅ | 每次啟動另建 OTel provider 並同步等待 `codex.daemon.start` 上傳（`tui/src/daemon_telemetry.rs`） | 每次 150–500 ms（release 預設） | 計數改記在主 provider 上，由其週期性或結束時上傳；只有會中止啟動的失敗才立即上傳。指標名稱與標籤不變 |
| ✅ | 每次啟動在 daemon 鎖內執行受管二進位檔的 `codex --version`，結果 TUI 根本不用 | 一次完整程序啟動（Linux 15–60 ms，macOS 更久，無逾時） | 新增 `start_for_session_classified`，TUI 不再查詢版本；CLI 指令維持原行為 |
| ✅ | 每次啟動執行 `codex app-server daemon pid-update-loop --help` 探測更新器支援 | 一次完整程序啟動 | 以 release 二進位檔的路徑、大小、修改時間為鍵，快取成功的探測結果（`update-loop-support.json`） |
| ✅ | daemon 就緒與鎖等待固定每 50 ms 輪詢 | 每次最多多等 50 ms | 改為 5 ms 起、倍增到 50 ms 的退避 |
| ✅ | 冷啟動時依序做 blake3 雜湊與 `--managed-daemon --help` 探測 | 數十到數百 ms | 兩者同時進行 |
| ✅ | 相容性檢查連到 daemon、做完 `initialize` 就關閉，接著正式連線再做一次 | 每次多一輪 UDS＋WebSocket＋`initialize` | 檢查通過且目標相同時，直接沿用該連線（2 秒內） |
| ✅ | 第一次安裝時在啟動任務上同步複製並雜湊整個 CLI 套件，輸入框凍結 | 0.5–3 s（一次性） | 移到 blocking 執行緒池，輸入框保持可用 |

### 3.2 首次繪製前

| 狀態 | 原因 | 成本 | 處理 |
|---|---|---|---|
| ✅ | 第一個 await 就是 D-Bus 減少動態效果查詢（上限 250 ms），之後才開始載入設定 | 通常 2–10 ms，最差 250 ms | 改為背景執行，建立本機設定前才等待結果 |
| ✅ | 三處各自做 OS 偵測；Linux 上會啟動 `lsb_release`（Debian/Ubuntu 上是 Python 腳本）與 `getconf`，其中一次在啟動任務上同步執行 | 每次 50–150 ms，最多三次 | 合併成一次 `codex_otel::os_info()`，並在 TUI 與 app-server 啟動時於背景預熱 |
| ✅ | 終端探測把 DA1 放在中間，終端不回 OSC 10/11（screen、mosh、Linux console 等）時要等滿 250 ms | 每次 250 ms | DA1 改為最後送出，收到後再等 25 ms 處理順序錯亂的回覆即結束；鍵盤功能偵測結果不變 |
| ✅ | tmux 選項在首次繪製前探測兩次 | 20–40 ms | 啟動時切換備用畫面沿用第一次的結果 |
| ✅ | WSL 上即使環境變數已決定結果，仍先啟動 `cmd.exe` 偵測 | 50–300 ms（上限 1 s） | 先看環境變數 |
| ✅ | arg0 在主執行緒建立前同步清理過期的別名目錄 | 本機很小；NFS 上數十 ms | 改在背景執行緒，永不清理自己的目錄，並收尾被 `process::exit` 中斷的清理 |

### 3.3 session 與 `thread/start`

| 狀態 | 原因 | 成本 | 處理 |
|---|---|---|---|
| ✅ | 預設模型目錄沒有合併請求：refresh worker、session 啟動與 TUI 的 `model/list` 各自請求 `/models` | 每次冷啟動一次 200–800 ms 的網路往返，加上重複請求 | 同一身分的請求合併：快取感知的載入會等待進行中的請求並沿用結果，失敗也不會再各自重試 |
| ✅ | 未設定 `model` 時，`resolve_startup_model` 會再列一次模型；第一次失敗時第二次又走網路 | 網路差時多一個 5 s 逾時 | 第二次改讀記憶體中的目錄 |
| ✅ | 啟動時讀到的帳號沒顯示任何畫面也被丟掉，bootstrap 再讀一次 | 一次 RPC 加完整設定載入 | 沒有顯示互動畫面時沿用 |
| ✅ | 全新啟動的 `config/read` 在其他啟動 RPC 之後才串行送出 | 一次 RPC 加兩次伺服器端設定載入 | 併入既有的啟動 `tokio::join!` |
| ✅ | `start_dynamic_tool_mcp` 串行做 RPC 與設定層重新載入 | 3–20 ms | 兩者同時進行 |
| ✅ | 同一份 `.rules` 檔在啟動路徑上被 Starlark 解析三次 | 每次 1–5 ms，規則多時數十 ms | 內容完全相同時重用上一次乾淨的解析結果 |
| ✅ | 嵌入模式 bootstrap 建立多個等價的 AuthManager 與受管設定載入器 | 每次 10–50 ms，keyring 模式更多 | 輸入完全相同時重用 |
| ✅ | 5 個 SQLite 資料庫依序開啟並做 quick_check 與遷移 | 50–300 ms | 本機磁碟上同時開啟（NFS 上維持依序） |
| ✅ | 每次開 thread 都以 8 KiB 的非同步讀取掃描整個（預設無上限的）`history.jsonl` | 每 MB 1–3 ms，100 MB 以上數百 ms | 單一 blocking 任務、256 KiB 緩衝區 |
| ✅ | Linux 上 bubblewrap 探測在 App::run 中同步 fork 並以 50 ms 輪詢 | 50–60 ms，上限 500 ms | 不需要啟動 bwrap 就能判定的情況維持同步（警告順序不變），需要探測時才移到 blocking 執行緒池並以 5 ms 輪詢 |
| ✅ | 首次啟動的狀態資料庫回填逐一解析每個 rollout | 大量 session 時數十秒（一次性） | 每批最多 8 個檔案同時解析，寫入資料庫的順序不變 |
| ⏸ | session 初始化等待 skills 掃描完成（只用來記錄錯誤） | 5–100 ms | 試過移到背景，但會讓 `thread/start` 的 trace span 延後關閉，風險大於收益，已撤回 |
| ⏸ | `models_cache.json` 超過 5 分鐘就在 `thread/start` 上等待網路刷新 | 200–800 ms | 「先用舊快取、背景更新」會改變第一個 turn 看到的模型目錄，屬產品決策，未修改 |
| ⏸ | ChatGPT token 快到期時，`thread/start` 會等待主動刷新 | 300 ms–1 s | 牽涉 Codex Apps 401 恢復流程，需要另外驗證 |
| ⏸ | 回饋 ring buffer 與 SQLite 日誌預設以 TRACE 擷取全部事件 | 持續的格式化與鎖成本 | 會減少回饋報告內容，屬產品決策 |
| ⏸ | HTTP 用戶端建置共用一個只有 1 個名額的全域 permit，分析事件每次請求都新建用戶端 | 可能延後啟動時的 `/models` 用戶端 | 需要確認該 permit 的設計原因 |
| ⏸ | 回填租約 900 秒，啟動被中斷後之後 15 分鐘的啟動都可能失敗 | 最差 30 s 後失敗 | 屬產品決策 |

## 4. 驗證

- `cargo clippy --tests -- -D warnings`（與 CI 相同）：所有修改過的 crate 皆無警告；最終版本上再對拆分過的 8 個 crate 跑一次，也沒有警告。
- 單元測試（最終版本）：tui 6069、core 2981、app-server 430、app-server-protocol 324（含 schema 夾具比對）、cli 293 通過；app-server-daemon、models-manager、state、rollout、message-history、otel、login、analytics、arg0、feedback、sandboxing、protocol、config 等也全部通過。新增的測試涵蓋 `/models` 合併請求、終端探測、tmux 結果重用、WSL 偵測順序、arg0 清理、ring buffer、執行策略快取、帳號與設定重用、資料庫平行開啟等。
- app-server 整合測試：`thread_start`、`model_list`、`sqlite` 相關 71 個全部通過。
- 倉庫的 blob 大小檢查（512,000 bytes 上限）通過。
- 與修改無關、在此容器中失敗的測試（已在未修改的基準程式碼上確認，或原因明確是環境）：
  - core 的 skills 整合測試與 `shell_snapshot` 單元測試：容器中的沙箱無法運作（fs sandbox helper 以 SIGABRT 結束）。`shell_snapshot` 的 5 個失敗在基準 `8703d32` 上完全相同。
  - core 的 `code_mode_host` 測試：需要另外建置依賴 V8 的 `codex-code-mode-host` 二進位檔。
  - `thread_start_jsonrpc_span_exports_server_span_and_parents_children`：基準程式碼上同樣逾時。
  - 少數測試在 `cargo test` 同一程序多執行緒下會互相干擾（全域 tracing、insta 內嵌快照重複偵測），單獨執行或用倉庫慣用的 nextest（每個測試一個程序）都會通過。
  - 以 root 身分執行時，`chmod 000` 的檔案仍可讀；環境變數開啟 backtrace 會改變錯誤訊息。

## 5. 重現量測

```shell
cd codex-rs && cargo build -p codex-cli --bin codex && cd ..
scripts/startup_bench.py --codex codex-rs/target/debug/codex --no-daemon
scripts/startup_bench.py --codex codex-rs/target/debug/codex --daemon --runs 5
scripts/startup_bench.py --codex codex-rs/target/debug/codex --daemon --stop-daemon
scripts/startup_bench.py --codex codex-rs/target/debug/codex --daemon --metrics-rtt-ms 400
```

## 6. 同一分支上的程式碼整理

為了讓這些啟動路徑之後更容易維護，本分支另外做了兩個純搬移的提交，不改變行為：

- 53 個檔案的大型內聯 `mod tests` 移到同層的 `<name>_tests.rs`（模組路徑不變，insta 快照不受影響）。
- 12 個 3,200–7,000 行的檔案拆成以職責分組的子模組，例如 `core/src/session/mod.rs` 從 6,124 行變成 750 行加 9 個子模組，`tui/src/bottom_pane/chat_composer.rs` 從原本的 12,866 行（含測試）變成 948 行加 8 個子模組。

上面第 2.2 節「修正後」的數字就是在包含這兩個提交的最終版本上量測的。
