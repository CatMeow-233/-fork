# Codex CLI 上傳邏輯研究

- 範圍：`codex-rs/` 內所有把使用者或機器資料送出本機的流程。
- 依據：2026-11-09 的程式碼，上游基準為 `8703d32`，加上本分支的啟動速度提交（`Cut serialized waits and duplicate work from TUI startup`）。行號以該提交為準；之後的「內聯測試移到同層檔案」與「大檔案拆分模組」提交會讓部分檔案的行號改變，引用前請以函式名稱再確認。
- 方法：先靜態普查程式碼，再逐條做對抗式驗證。本文只收錄驗證結果為「已確認」或「已更正」的內容，被否定的說法已刪除。
- 本文是唯讀研究，沒有修改任何程式碼。

標記說明：

| 標記 | 意義 |
|---|---|
| `[安全]` `[隱私]` `[可靠性]` `[效能]` `[清理]` | 問題的嚴重度類別 |
| **已確認** | 原說法成立 |
| **部分確認** | 機制成立，但範圍、數字或前提已更正，以本文敘述為準 |

---

## 1. 摘要

本研究列出 11 條流程，其中 10 條會把資料送出本機；流程 2（診斷日誌擷取）本身只寫在本機，但內容會隨流程 1 的回饋上傳。會整份上傳本機檔案的有三條：回饋上傳（Sentry）、Codex Apps 工具的檔案參數（OpenAI Files API）、plugin share（上傳整個 plugin 目錄）。其餘七條是：模型請求內嵌的圖片、Cloud 對話、Cloud 任務、背景 memories 擷取、產品分析事件、OTel 指標，以及即時語音。

最嚴重的兩個問題都是安全類：

1. Codex Apps 工具在不需提示的核准模式下，模型可以指定讀取政策允許的任何檔案（包括絕對路徑），上傳到 OpenAI 儲存空間。
2. 回饋附件 `codex-connectivity-diagnostics.txt` 原樣包含代理伺服器 URL 裡的帳號、密碼與 query 參數。

和啟動速度直接相關的發現有三項：回饋 ring buffer 與 SQLite 日誌預設以 TRACE 擷取全部事件；ring buffer 逐位元組淘汰；分析事件每次請求都新建 HTTP 用戶端，而且和 `/models` 請求共用同一個只有 1 個名額的建置 permit。本分支已修正 ring buffer 逐位元組淘汰，以及 OS 資訊探測的共用與預熱；另外兩項涉及產品取捨，列為後續建議（見第 4、5 節）。

### 1.1 上傳／外送流程一覽

| # | 流程 | 觸發條件 | 目的地 | 預設是否啟用 | 使用者同意門檻 |
|---|---|---|---|---|---|
| 1 | 回饋上傳 | TUI `/feedback`；或任一 app-server 用戶端呼叫 `feedback/upload` | `POST https://o33249.ingest.us.sentry.io/api/4510195390611458/envelope/` | 只在觸發時執行（需 `feedback_enabled`） | TUI 會問「Upload logs?」，但只控制附件。event（備註、tags、`account_id`）一律送出。伺服器端不以 includeLogs 管控 `extraLogFiles` |
| 2 | 診斷日誌擷取 | 程序啟動後一直進行 | 本機 4 MiB ring＋SQLite log DB，經 #1 上傳 | 是，TRACE 等級 | 只有 #1 以 includeLogs=true 上傳時才離開本機 |
| 3 | Codex Apps 檔案參數 | 模型呼叫宣告了 `openai/fileParams` 的 Codex Apps 工具 | `POST {chatgpt_base_url}/files` → `PUT` Azure blob SAS URL → `POST /files/{file_id}/uploaded` | 有這類工具時就會執行 | 跟隨 MCP 工具核准規則。Auto＋`read_only_hint`、Approve 模式、自動核准的 permission profile、已記住的核准都不會提示 |
| 4 | 圖片準備與附件 | turn 中有圖片（使用者或工具輸出）；resume／fork 重播 | 內嵌在 Responses 請求中送往模型供應商（只有 `InlineAttachmentStore`） | 是 | 沒有另外的同意步驟 |
| 5 | Plugin share | app-server 用戶端呼叫 `plugin/share/save`（TUI 沒有入口） | `POST /public/plugins/workspace/upload-url` → `PUT` blob → `POST /public/plugins/workspace[/{id}]` | 需要 `Feature::Plugins`、`Feature::PluginSharing` 與 ChatGPT 認證 | 呼叫端明確要求 |
| 6 | Cloud 對話 turn | TUI 連到 Cloud thread 時送出訊息 | gRPC `ThreadService/AddUserMessage`，origin 為 `https://codex-cloud-backend.chatgpt.com` | 只用於 Cloud session | 使用者送出訊息 |
| 7 | Cloud tasks | 開啟 `codex cloud` TUI（自動偵測環境）；建立任務 | `GET .../environments[/by-repo/github/{owner}/{repo}]`、`POST /wham/tasks` 或 `/api/codex/tasks` | 只在使用 `codex cloud` 時 | 環境偵測不會另外提示，開啟就送出所有 GitHub owner/repo |
| 8 | Memories stage-1 | 每個開始新 turn 且有輸入的 `turn/start` | 設定的模型供應商（Responses） | 否（`MemoryTool` 預設關閉） | 啟用功能後不再逐次提示 |
| 9 | 產品分析事件 | 各種 runtime 事件 | `POST {chatgpt_base_url}/codex/analytics-events/events` | 是，除非 `analytics.enabled=false`（獨立 app-server 也一樣） | opt-out |
| 10 | OTel 指標（Statsig）與自訂 OTLP | 程序啟動 | `POST https://ab.chatgpt.com/otlp/v1/metrics`，以及使用者設定的 OTLP | 只有 TUI 與 exec 的 release build 預設開啟；獨立 app-server 與 `codex app-server` 預設關閉；debug build 關閉 | TUI／exec 為 opt-out（`analytics.enabled=false`）；app-server 為 opt-in（`analytics.enabled=true` 或 `--analytics-default-enabled`） |
| 11 | 即時語音 | 使用者啟動語音對話 | `POST {provider base}/realtime/calls`（或 `live`）＋ websocket 音訊 | 只在語音期間 | 使用者啟動 |

### 1.2 已驗證問題數量

| 類別 | 已確認 | 部分確認 | 合計 |
|---|---|---|---|
| `[安全]` | 1 | 1 | 2 |
| `[隱私]` | 10 | 4 | 14 |
| `[可靠性]` | 9 | 6 | 15 |
| `[效能]` | 16 | 9 | 25 |
| `[清理]` | 4 | 3 | 7 |
| 合計 | 40 | 23 | 63 |

即時語音的音訊與 SDP 日誌問題列在流程 2（F2-3、F2-4），不在流程 11 重複計算。

---

## 2. 主要流程圖

### 2.1 回饋上傳到 Sentry

```mermaid
sequenceDiagram
    autonumber
    actor U as 使用者
    participant T as TUI
    participant S as app-server feedback_processor
    participant F as codex-feedback upload_feedback
    participant Y as Sentry envelope API
    U->>T: /feedback，選分類，回答 Upload logs?，輸入備註
    T->>T: open_feedback_consent 在 UI 執行緒複製 4 MiB ring
    opt include_logs = true
        T->>S: fs/writeFile 寫入 client-logs.txt（ring 最後 1 MiB）
    end
    T->>S: feedback/upload（extraLogFiles、tags，沒有逾時）
    S->>S: 取得 3 個 permit 之一，完整解析 rollout 取 model、effort、prompt_hash
    S->>S: 把 account_id、chatgpt_user_id 寫進 process 級 tag map
    opt include_logs = true
        S->>S: 子樹與附加 thread、SQLite 日誌、doctor（最多 35 s）、history（探索 5 s）
    end
    Note over S: extraLogFiles 不論 include_logs 都會加入
    S->>F: spawn_blocking 加 block_on(upload_feedback)
    F->>F: deadline = now + 300 s，建立 event（gzip fast，最多 1 MiB）
    F->>Y: POST event envelope（只嘗試一次）
    Y-->>F: 2xx，否則整個 RPC 失敗
    loop 每個附件依序送出，共用同一個 deadline
        F->>Y: POST attachment envelope（408/429/5xx 重試 250 ms、500 ms）
    end
    F-->>S: Ok，或 Err（有附件失敗或被略過）
    S-->>T: thread_id、prompt_hash，或錯誤
```

### 2.2 Codex Apps 檔案上傳（OpenAI Files API）

```mermaid
sequenceDiagram
    autonumber
    participant M as 模型
    participant C as core mcp_tool_call 與 mcp_openai_file
    participant X as 執行環境檔案系統
    participant O as chatgpt_base_url /files
    participant B as Azure blob SAS URL
    participant A as Codex Apps MCP server
    M->>C: 呼叫宣告 openai/fileParams 的工具，參數是本機路徑
    C->>C: 核准判斷，可能是 NotRequired，不提示使用者
    Note over C: 下列步驟在 tool_catalog.run_with_snapshot 內執行，且早於 tool timeout 起算
    loop 每個欄位、每個陣列元素依序處理
        C->>X: cwd.join(path)，get_metadata（必須是檔案，最多 512 MiB），先開一次 read stream
        C->>O: POST /files（file_name、file_size、use_case=codex、connector、action、model）
        O-->>C: file_id、upload_url、upload_headers
        loop 最多 5 次，共用 5 分鐘 deadline
            C->>X: 重新開啟 stream，從第 0 位元組開始
            C->>B: PUT BlockBlob，Content-Length 用先前的 metadata.size
            B-->>C: 2xx，或 502/503/504、傳輸錯誤時退避重試
        end
        loop status 為 retry 時每 250 ms 一次，約 30 s
            C->>O: POST /files/{file_id}/uploaded
            O-->>C: success（download_url）、retry 或失敗
        end
    end
    C->>A: 把參數換成 download_url、file_id、mime_type、file_name 後呼叫工具
```

---

## 3. 各流程詳述

### 3.1 流程 1：回饋上傳（`/feedback` → `feedback/upload` → Sentry）

#### 觸發

- TUI `/feedback`：先檢查 `config.feedback_enabled`（`codex-rs/tui/src/chatwidget/slash_dispatch.rs:242`），再讓使用者選分類、回答「Upload logs?」（Yes 代表 include_logs）並輸入備註。
- 任何 app-server 用戶端（stdio、in-process、websocket、remote-control）都能直接呼叫 `feedback/upload`（`codex-rs/app-server/src/message_processor.rs:2033-2035`）。伺服器端只檢查 `config.feedback_enabled`（`codex-rs/app-server/src/request_processors/feedback_processor.rs:59`），沒有依連線來源限制方法。非 loopback 的 websocket listener 若有設定，會用 capability token 或簽章 bearer 驗證（`codex-rs/websocket-auth/src/lib.rs:237-286`）。

#### 步驟

1. 開啟同意視窗時，TUI 在 UI 執行緒上呼叫 `self.feedback.snapshot(self.thread_id)`，只為讀取代理診斷（`codex-rs/tui/src/chatwidget.rs:1058`），然後列出將上傳的檔案（`codex-rs/tui/src/bottom_pane/feedback_view.rs:250-298`）。
2. 送出後建立 `FeedbackUploadParams`。只有 include_logs 時，目前的 rollout 路徑才會放進 `extra_log_files`；`turn_id` 變成 tag（`codex-rs/tui/src/app/background_requests.rs:1307-1312`）。這段工作在 `tokio::spawn` 中執行，不阻塞 UI（`codex-rs/tui/src/app/background_requests.rs:655`）。
3. include_logs 時，`fetch_feedback_upload` 複製 TUI ring 並截取最後 1 MiB，經 `fs/writeFile` RPC 寫到 `$CODEX_HOME/tmp/tui-feedback-<uuid>/client-logs.txt`，再把這個路徑加入 `extra_log_files`（`codex-rs/tui/src/app/feedback_upload.rs:28-47`）。
4. 送出 `feedback/upload` RPC（沒有逾時），結束後刪除暫存目錄（`codex-rs/tui/src/app/feedback_upload.rs:63-75`）。
5. app-server 取得 3 個上傳 permit 之一，額滿時回傳 `OVERLOADED_ERROR_CODE`（`codex-rs/app-server/src/request_processors/feedback_processor.rs:51,99-109`）。
6. 只要 threadId 能對應到 rollout，就用 `RolloutRecorder::load_rollout_items` 完整解析 rollout，取出 model、effort、prompt_hash。這一步不受 include_logs 影響（`codex-rs/app-server/src/request_processors/feedback_processor.rs:131-144,470-493`）。
7. 有值時以 `tracing::info!(target: "feedback_tags", ...)` 送出 `chatgpt_user_id`、`account_id`（`codex-rs/app-server/src/request_processors/feedback_processor.rs:147-158`），接著擷取 ring 與 tags 快照（`codex-rs/app-server/src/request_processors/feedback_processor.rs:159`）。
8. include_logs 時：列出 agent 子樹與附加的 thread（`codex-rs/app-server/src/request_processors/feedback_processor.rs:164-187`），收集 guardian 審查失敗（`codex-rs/app-server/src/request_processors/feedback_processor.rs:188`），最多選 8 個 thread 寫成 `feedback-thread-index.json`（`codex-rs/app-server/src/request_processors/feedback_thread_index.rs:12,84`），flush log DB 並查詢各 thread 的 SQLite 日誌與 legacy 日誌（`codex-rs/app-server/src/request_processors/feedback_processor.rs:201-216,563-620`）。
9. 收集路徑型附件：各 thread 的 rollout 與 guardian rollout（`codex-rs/app-server/src/request_processors/feedback_processor.rs:228-261`）、Windows sandbox 日誌（`codex-rs/app-server/src/request_processors/feedback_processor.rs:271-276`）、Codex Apps 工具與目錄快取（`codex-rs/app-server/src/request_processors/feedback_processor.rs:277-285`），最後加入用戶端的 `extraLogFiles`，這一步沒有 include_logs 判斷（`codex-rs/app-server/src/request_processors/feedback_processor.rs:287-296`）。
10. include_logs 時，先執行 `codex doctor` 子行程（35 s 逾時，`codex-rs/app-server/src/request_processors/feedback_doctor_report.rs:25`），再找 history-base 祖先 rollout 並讀取前綴（`codex-rs/app-server/src/request_processors/feedback_processor.rs:298-321`）。5 s 逾時只涵蓋探索階段（`codex-rs/app-server/src/request_processors/feedback_rollout_history.rs:16,89`），`spawn_blocking` 內的前綴讀取沒有逾時（`codex-rs/app-server/src/request_processors/feedback_rollout_history.rs:97-131`）。
11. 進入 `spawn_blocking`：include_logs 時加入 daemon updater 日誌（`codex-rs/app-server/src/request_processors/feedback_processor.rs:336-339`），然後呼叫 `runtime_handle.block_on(snapshot.upload_feedback(...))`（`codex-rs/app-server/src/request_processors/feedback_processor.rs:333-357`）。
12. `upload_feedback` 設定 `Instant::now() + UPLOAD_TIMEOUT`（300 s）作為所有 envelope 共用的 deadline（`codex-rs/feedback/src/lib.rs:64,647`），建立 Sentry event，以 gzip 編碼並檢查解碼後不超過 1 MiB（`codex-rs/feedback/src/lib.rs:714-721`）。
13. POST event envelope，只嘗試一次，且必須回 2xx（`codex-rs/feedback/src/lib.rs:730-742`；`codex-rs/feedback/src/upload.rs:92-93`）。
14. 依序建立 lazy 附件迭代器：`codex-logs.log`、記憶體內診斷、connectivity 文字、`rollouts.tar.gz`，最後是路徑型附件（sandbox 日誌、工具快取、`extraLogFiles`，含 `client-logs.txt`）（`codex-rs/feedback/src/lib.rs:964-973`）。
15. rollout 封存流程：zstd 解碼每個 rollout，寫成 tar.gz，再把整個封存讀回記憶體（`codex-rs/feedback/src/lib.rs:924-926`；`codex-rs/feedback/src/rollout_archive.rs:20-93`）。失敗時改為逐一附加（`codex-rs/feedback/src/rollout_archive.rs:82-91`）。
16. 每個附件用獨立的 envelope 依序送出，以 event_id 關聯（`codex-rs/feedback/src/lib.rs:755-814`）。有任何附件失敗時回傳錯誤（`codex-rs/feedback/src/lib.rs:822-825`），TUI 顯示 `Failed to upload feedback`（`codex-rs/tui/src/app/background_requests.rs:680-684`）。

#### 目的地與認證

- `POST https://o33249.ingest.us.sentry.io/api/4510195390611458/envelope/`（DSN 在 `codex-rs/feedback/src/lib.rs:62-63`）。
- 用 `RouteAwareClientPool::new_without_redirects_or_request_logging` 建立用戶端，不跟隨重新導向、不記錄請求（`codex-rs/feedback/src/lib.rs:724-727`）。
- 認證：DSN 公開金鑰放在 `X-Sentry-Auth` header（`codex-rs/feedback/src/upload.rs:39-42`）。不送使用者憑證；ChatGPT 的 `account_id`、`chatgpt_user_id` 以 tag 送出。
- 另有一組 report API（`FeedbackTransport`、`prepare_report_event`、`prepare_report_attachment`，`codex-rs/feedback/src/report_upload.rs`），本 workspace 沒有任何呼叫端（見 F1-15）。

#### 載荷內容與大小限制、壓縮、脫敏

Event 一律送出，與 includeLogs 無關，內容有：

- 完整備註文字、分類、client tags。
- 所有 process 級 `feedback_tags`：auth 診斷、usage 與 feature tags、任一 thread 最後一個 turn 的 model 與 approval policy。
- 目前的 `account_id`、`chatgpt_user_id`，也可能是過期的值（見 F1-3）。
- 限制：解碼後最多 1 MiB（`codex-rs/feedback/src/lib.rs:73`）；process tags 最多 512 個（`codex-rs/feedback/src/lib.rs:76,1026`）。client tags 優先於 process tags（`codex-rs/feedback/src/lib.rs:863-872`）。

附件只有 includeLogs=true 時才收集，`extraLogFiles` 例外：

| 附件 | 內容與上限 |
|---|---|
| `codex-logs.log` | 沒有 SQLite 覆寫時是 4 MiB ring（`codex-rs/feedback/src/lib.rs:61,609-614`）。有 SQLite 日誌時，是所選 thread 的 DB 輸出，所有 thread 加上各 thread 最新程序的無 thread 列，合計最多 10 MiB（`codex-rs/state/src/runtime/logs.rs:344-412`）。log DB 曾寫入失敗時改回 ring（`codex-rs/app-server/src/request_processors/feedback_processor.rs:613-615`） |
| `codex-logs-legacy.log` | legacy SQLite 日誌（`codex-rs/app-server/src/request_processors/feedback_processor.rs:210-216`） |
| `feedback-thread-index.json` | 最多 8 個 thread（`codex-rs/app-server/src/request_processors/feedback_thread_index.rs:12,84`） |
| `auto-review-failures.jsonl` | guardian 審查失敗（`codex-rs/feedback/src/guardian.rs:82`） |
| `codex-doctor-report.json` | `codex doctor --json --feedback` 的輸出 |
| `codex-connectivity-diagnostics.txt` | 代理環境變數的原值（只在 include_logs 時附加，`codex-rs/feedback/src/lib.rs:626-631`） |
| `rollouts.tar.gz` | 回報的 thread、子孫與附加 thread（共最多 8 個）及其 guardian rollout、history-base 前綴（單一 64 MiB、合計 126 MiB，`codex-rs/app-server/src/request_processors/feedback_rollout_history.rs:9-10,59,98`） |
| `codex-apps-tools-cache.json`、`codex-app-directory-cache.json` | 原樣上傳，沒有過濾 |
| `windows-sandbox.log` | Windows sandbox 日誌 |
| `daemon-updater.stderr.log`、`app-server-updater.stderr.log` | 各取最後 256 KiB（`codex-rs/feedback/src/daemon_logs.rs:12-13`） |
| `client-logs.txt` | TUI ring 的最後 1 MiB |
| 任意 `extraLogFiles` | 每個最多 200 MiB（`codex-rs/feedback/src/lib.rs:969-972`） |

- 大小：每個附件 envelope 解碼後最多 200 MiB（`codex-rs/feedback/src/lib.rs:72`）。rollout 封存的 200 MiB 上限套用在壓縮後的 tar.gz（`codex-rs/feedback/src/rollout_archive.rs:32-36,135-143`），另有 envelope 框架檢查（`codex-rs/feedback/src/lib.rs:942-945`）。
- 壓縮：每個 envelope 用 `Compression::fast()` gzip，沒有變小就改送原始內容（`codex-rs/feedback/src/upload.rs:165-178`）。rollout 會先 zstd 解碼，再用 `Compression::default()` 打成 tar.gz（`codex-rs/feedback/src/rollout_archive.rs:37`）。tar header 正規化為 mode `0o600`、mtime 0（`codex-rs/feedback/src/rollout_archive.rs:117-121`）。
- 脫敏：除了 doctor 報告由其 CLI 處理外，本研究沒找到其他脫敏步驟。

#### 重試與錯誤處理

- Event 只嘗試一次，這是刻意設計（`codex-rs/feedback/src/upload.rs:63` 註解 `Never replay the core.`；`codex-rs/feedback/src/upload.rs:92-93`）。任何傳輸錯誤或非 2xx 都會讓整個 RPC 失敗，任何附件都還沒送出（`codex-rs/feedback/src/lib.rs:739-742`）。
- 附件遇到 408、429、5xx，以及 connect、request、body 錯誤時，最多重試 2 次，間隔 250 ms、500 ms（`codex-rs/feedback/src/upload.rs:64,85`）。程式會遵守 `Retry-After` 與 `X-Sentry-Rate-Limits`；一旦被限流，其餘附件全部略過（`codex-rs/feedback/src/upload.rs:70-82`；`codex-rs/feedback/src/lib.rs:756-766`）。
- 逾時或限流時會記錄 `remaining feedback attachments skipped`（`codex-rs/feedback/src/lib.rs:758-765`），最後回傳錯誤（`codex-rs/feedback/src/lib.rs:822-825`），不會默默吞掉。
- 同時最多 3 個上傳，第 4 個回傳 OVERLOADED（`codex-rs/app-server/src/request_processors/feedback_processor.rs:99-109`）。
- TUI 的請求沒有逾時（`codex-rs/tui/src/app/feedback_upload.rs:63-68`）。

#### 隱私說明

- 使用者回答「No」時仍會上傳 event，包含完整備註、tags 與帳號 ID（F1-4）。
- 同意清單少列了好幾類實際會上傳的附件（F1-5）。
- 日誌以 TRACE 擷取，包括即時語音的 base64 音訊與 SDP（見流程 2）。
- 代理憑證原樣上傳（F1-2）。

#### 已驗證的問題

- `[安全]` **F1-2 代理伺服器憑證以明文上傳**（已確認）
  include_logs 時，app-server 程序的 `HTTP(S)_PROXY`、`ALL_PROXY` 原值（含 `user:password` 與 query 中的祕密）寫進 `codex-connectivity-diagnostics.txt`。測試還斷言保留 `HTTPS_PROXY = https://user:password@...?secret=1`。TUI 對 good_result 隱藏細節但仍列出檔名（`codex-rs/tui/src/bottom_pane/feedback_view.rs:31-36,290-298`）。TUI 顯示的是自己程序的環境變數（`codex-rs/tui/src/chatwidget.rs:1058`），實際上傳的快照則在 app-server 程序收集（`codex-rs/app-server/src/request_processors/feedback_processor.rs:159` → `codex-rs/feedback/src/lib.rs:299`）。連到 daemon 或遠端 app-server 時兩者可能不同。
  位置：`codex-rs/feedback/src/feedback_diagnostics.rs:45-51`（測試在 `codex-rs/feedback/src/feedback_diagnostics.rs:99-120`）
- `[隱私]` **F1-1 伺服器端未對 `extraLogFiles` 執行同意門檻**（部分確認）
  呼叫端列出的路徑會被讀取（每個最多 200 MiB）並上傳，即使 `includeLogs=false` 也一樣。相對路徑以 app-server 的 cwd 解析，`extra_log_files` 沒有絕對路徑檢查（`codex-rs/app-server-protocol/src/protocol/v2/feedback.rs:21`）。TUI 在用戶端只在 include_logs 時設定（`codex-rs/tui/src/app/background_requests.rs:1307-1311`）。更正：這不是權限提升，因為任何已認證的 app-server 用戶端本來就能用不經沙箱的 `fs/readFile`（`codex-rs/app-server/src/request_processors/fs_processor.rs:64-76`）與 `process/spawn`。實際問題是同意與 API 合約的缺口：第三方用戶端可以在分類標示「不含日誌」的回饋裡附上任意檔案。
  位置：`codex-rs/app-server/src/request_processors/feedback_processor.rs:287-296`
- `[隱私]` **F1-3 前一個帳號的 ID 會殘留在後續回饋中**（已確認）
  `account_id`、`chatgpt_user_id` 只在有值時才送出（`codex-rs/app-server/src/request_processors/feedback_processor.rs:147-158`），寫入 process 級 tag map 後永遠不會移除（`codex-rs/feedback/src/lib.rs:1023-1030`），而互動上傳會合併所有 process tags 且不過濾（`codex-rs/feedback/src/lib.rs:873-880`）。長時間執行的 app-server 在登出，或改用 API key／header 認證之後，之後每一筆互動回饋（包括 include_logs=false）都會帶上舊的 ChatGPT 帳號 ID。只有沒被使用的 report API 會把它們過濾掉（`codex-rs/feedback/src/report_upload.rs:115-120`）。
  位置：`codex-rs/feedback/src/lib.rs:873-880`
- `[隱私]` **F1-4 回答「No」仍會送出備註、tags 與帳號 ID**（已確認）
  event 一律上傳，內容有完整備註、分類、client tags、所有 process 級 feedback tags，以及目前或過期的帳號 ID。產生帳號 tag 的程式不在任何 include_logs 判斷之內。
  位置：`codex-rs/app-server/src/request_processors/feedback_processor.rs:147-158`
- `[隱私]` **F1-5 同意清單少列實際上傳的附件**（已確認）
  清單列出 `codex-logs.log`、`client-logs.txt`、doctor 報告、兩個快取、選用的 sandbox 日誌、目前 rollout 名稱、`auto-review-rollout-<id>.jsonl` 與 connectivity 檔案。但下列項目會上傳卻沒有列出：子樹與附加 thread 的 rollout 及其 guardian rollout（`codex-rs/app-server/src/request_processors/feedback_processor.rs:164-187,228-261`）、history 前綴（`codex-rs/app-server/src/request_processors/feedback_processor.rs:315-321`）、`auto-review-failures.jsonl`（`codex-rs/feedback/src/guardian.rs:82`）、`feedback-thread-index.json`（`codex-rs/app-server/src/request_processors/feedback_thread_index.rs:84`）、`codex-logs-legacy.log`（`codex-rs/app-server/src/request_processors/feedback_processor.rs:210-216`）、兩個 daemon updater 日誌（`codex-rs/feedback/src/daemon_logs.rs:13`）。另外，rollout 實際打包成 `rollouts.tar.gz` 送出，而不是用清單上的檔名。
  位置：`codex-rs/tui/src/bottom_pane/feedback_view.rs:250-298`
- `[可靠性]` **F1-6 核心 event 不重試**（已確認）
  event envelope 只嘗試一次，這是設計（見註解）。任何傳輸錯誤或非 2xx 都會讓整個 `feedback/upload` RPC 在送出附件之前失敗。用同一個 event_id 重送是否安全取決於 Sentry 端的行為，從本儲存庫無法驗證。
  位置：`codex-rs/feedback/src/upload.rs:63,92-93`
- `[可靠性]` **F1-7 所有附件共用一個 300 s deadline 且依序送出**（部分確認）
  路徑型附件（工具快取、sandbox 日誌、`client-logs.txt`、`extraLogFiles`）排在 rollout 封存之後，deadline 或限流先發生時就會被略過。更正：會以 RPC 錯誤回報，並非默默略過。
  位置：`codex-rs/feedback/src/lib.rs:647,755-777`
- `[效能]` **F1-8 rollout 重複壓縮**（部分確認）
  已經 gzip 的封存（最多 200 MiB）會再完整跑一次 gzip(fast)，沒變小就重新序列化為原始內容（`codex-rs/feedback/src/upload.rs:165-178`）。更正：記憶體峰值大約是封存緩衝（最多 200 MiB）加 envelope 緩衝（約 200 MiB），合計約 400 MiB；建立封存期間則最多是 126 MiB 前綴加一個解碼後最多 200 MiB 的 rollout。程式碼不支持「超過 500 MiB」的說法。
  位置：`codex-rs/feedback/src/rollout_archive.rs:37`、`codex-rs/feedback/src/lib.rs:924-926`
- `[效能]` **F1-9 封存失敗時每個 rollout 會重新解碼一次**（部分確認）
  封存失敗（壓縮後超過 200 MiB、envelope 框架超限或 I/O 錯誤）時，每個 rollout 會重新解碼讀取，改為個別上傳。更正：觸發條件看的是壓縮後大小，不是解碼後的合計大小，所以 fallback 比原先描述的少見。
  位置：`codex-rs/feedback/src/rollout_archive.rs:82-91`
- `[效能]` **F1-10 doctor、history、上傳依序執行，TUI 沒有逾時**（已確認）
  include_logs 時，doctor（最多 35 s）、history 探索（5 s）、沒有逾時的前綴讀取（最多 126 MiB）、上傳（300 s）依序執行，TUI 一直等待。沒有 include_logs 時只有 300 s 上傳 deadline。缺少進度 UI 一事未驗證。
  位置：`codex-rs/app-server/src/request_processors/feedback_processor.rs:298-321`
- `[效能]` **F1-11 不含日誌的回饋也會完整解析 rollout**（已確認）
  只要 threadId 能對應到 rollout，整個 rollout 就會逐行以 JSON 解析進記憶體，只為了取 model、effort、prompt_hash（`codex-rs/rollout/src/recorder.rs:1138-1144`）。
  位置：`codex-rs/app-server/src/request_processors/feedback_processor.rs:131-144,470-493`
- `[效能]` **F1-12 `spawn_blocking` 加 `block_on` 長時間佔用 blocking 執行緒**（部分確認）
  說法成立，但這個設計有部分理由：lazy 附件迭代器在每次網路傳送之間，會做同步檔案讀取、tar 與 gzip（`codex-rs/feedback/src/lib.rs:906-973`）。最多同時佔用 3 個 blocking 執行緒，相對 tokio 預設的 blocking pool 是小事。
  位置：`codex-rs/app-server/src/request_processors/feedback_processor.rs:333-357`
- `[效能]` **F1-13 開啟同意視窗時在 UI 執行緒複製 4 MiB ring**（部分確認）
  在 UI 執行緒上複製 4 MiB ring 與 tag map，只為讀取代理環境變數診斷（`codex-rs/feedback/src/lib.rs:285-299`）。更正：之後再複製 4 MiB 並截成 1 MiB 的那一步，是在背景 task 執行（`codex-rs/tui/src/app/feedback_upload.rs:41-45`），不在 UI 執行緒。
  位置：`codex-rs/tui/src/chatwidget.rs:1058`
- `[清理]` **F1-14 embedded 模式下 `client-logs.txt` 與 `codex-logs.log` 重複**（部分確認）
  embedded 模式中 TUI 與 in-process app-server 共用同一個 ring（`codex-rs/tui/src/lib.rs:1328` 的 `feedback.clone()` 經 `codex-rs/tui/src/lib.rs:625-632` 傳給 `start_embedded_app_server`，`codex-rs/tui/src/lib.rs:301`）。沒有 SQLite per-thread 日誌時，`client-logs.txt` 就是已經以 `codex-logs.log` 上傳的同一個 ring 的最後 1 MiB；有 SQLite 日誌時，它會多帶未過濾的 ring 內容。base64 RPC 的成本只有一次編碼加一次 `.clone()`，約 1.3 MiB，可以忽略（`codex-rs/tui/src/app_server_session/fs.rs:61`）。
  位置：`codex-rs/tui/src/app/feedback_upload.rs:41-46`
- `[清理]` **F1-15 report API 沒被使用，註解也已過時**（部分確認）
  report transport 與 `prepare_*` 函式在本 workspace 沒有呼叫端（可能是給外部 host 用的公開 API）。更正：`MAX_ATTACHMENT_BYTES`、`MAX_ATTACHMENTS_BYTES`（64/126 MiB）仍在使用，用來限制互動回饋的 history 前綴（`codex-rs/app-server/src/request_processors/feedback_rollout_history.rs:9-10,59,98`）。所以 `codex-rs/feedback/src/lib.rs:65` 的註解「used by the report API, not the interactive upload」不正確。`codex-rs/feedback/src/report_upload.rs:26-27` 說互動上傳有「ten-second budget」，但實際的 `UPLOAD_TIMEOUT` 是 300 s（`codex-rs/feedback/src/lib.rs:64`）。
  位置：`codex-rs/feedback/src/report_upload.rs:26-27`

---

### 3.2 流程 2：診斷日誌擷取（ring buffer＋SQLite log DB）

#### 觸發

程序啟動後一直進行。TUI（`codex-rs/tui/src/startup_orchestration.rs:956-957`，log DB 在 `codex-rs/tui/src/startup_orchestration.rs:989`）與 app-server（`codex-rs/app-server/src/lib.rs:763-776`）都無條件安裝 feedback logger layer、metadata layer 與 log DB layer。

#### 步驟

1. `CodexFeedback::logger_layer` 安裝一個 fmt layer，寫入 4 MiB `RingBuffer`，過濾器為 `Targets::new().with_default(Level::TRACE)`。只有 `codex_otel.log_only`、`codex_api::responses_websocket_timing`、`codex_core::post_sampling_token_estimate` 被關閉；`codex_http_client::transport`、`codex_api::sse`、`tungstenite` 限制在 DEBUG（`codex-rs/feedback/src/lib.rs:244-268`）。
2. `FeedbackWriter::write` 先取得 mutex 再呼叫 `push_bytes`（`codex-rs/feedback/src/lib.rs:340-345`）。ring 滿了以後，每次寫入都從前端一次 `pop_front` 一個位元組，再逐位元組 `extend`（`codex-rs/feedback/src/lib.rs:383-391`）。
3. `metadata_layer` 把 target 為 `feedback_tags` 的事件收進 process 級 map，最多 512 個，只增不減（`codex-rs/feedback/src/lib.rs:1023-1030`）。
4. log DB layer 同樣預設 TRACE，另外排除一串 target（`codex-rs/state/src/log_db.rs:61-89`）。
5. 上傳時，SQLite 日誌（或 ring）成為 `codex-logs.log`（`codex-rs/app-server/src/request_processors/feedback_processor.rs:563-620`；`codex-rs/feedback/src/lib.rs:609-614`）。

#### 目的地與認證

資料只存在本機。只有流程 1 以 includeLogs=true 上傳時才會送到 Sentry。

#### 載荷內容與大小限制、壓縮、脫敏

- 程序內所有 TRACE／DEBUG 事件，排除少數 target。
- ring 為 4 MiB（`codex-rs/feedback/src/lib.rs:61`）；tag map 最多 512 個（`codex-rs/feedback/src/lib.rs:1026`）。
- 回饋用的 SQLite 查詢只做一次視窗加總，所有選定 thread（加上各 thread 最新程序的無 thread 列）合計最多 10 MiB，不是每個 partition 各 10 MiB（`codex-rs/state/src/runtime/logs.rs:344,389-412`）。
- 沒有脫敏。編譯期也沒有 `max_level`／`release_max_level` feature（`codex-rs/Cargo.toml:548`：`tracing = "0.1.44"`）。

#### 重試與錯誤處理

log DB 寫入失敗會回報到 ring（`codex-rs/feedback/src/lib.rs:205-214`）。發生過寫入失敗時，回饋會捨棄 SQLite 日誌改用 ring（`codex-rs/app-server/src/request_processors/feedback_processor.rs:613-615`）。

#### 隱私說明

即時語音音訊與 SDP 會進入兩個 sink（F2-3、F2-4），並佔用 4 MiB ring 的空間。只有 include_logs 回饋會把它們帶離本機。

#### 已驗證的問題

- `[效能]` **F2-1 兩個 sink 都預設 TRACE**（已確認）
  兩個 sink 都只排除少數 target，所以每個已啟用的 trace／debug 事件都會被格式化兩次（ring 的 fmt layer 一次、SQLite body 一次），與 `RUST_LOG` 無關，啟動期間也一樣。機制確定，但程式碼中沒有量測實際的啟動成本。
  位置：`codex-rs/feedback/src/lib.rs:255-257`、`codex-rs/state/src/log_db.rs:61-63`
- `[效能]` **F2-2 ring 逐位元組淘汰**（已確認，本分支已修正：改為 `VecDeque::drain` 批次淘汰與切片 `extend`）
  `for _ in 0..to_drop { let _ = self.buf.pop_front(); }`，接著 `self.buf.extend(data.iter().copied())`，在 mutex 內執行（`codex-rs/feedback/src/lib.rs:342-343`）。`VecDeque` 沒有穩定的 `extend_from_slice`，修正寫法是 `self.buf.drain(..to_drop)` 加 `self.buf.extend(data)`（走 `Extend<&u8>` 的 slice 快速路徑）。每個事件的成本是 O(行長)，相對格式化成本不大。
  位置：`codex-rs/feedback/src/lib.rs:383-391`
- `[隱私]` **F2-3 即時語音每個麥克風訊框記錄兩次**（已確認）
  以 websocket 傳輸的每個麥克風訊框會記錄兩次：DEBUG 等級的 derived `Debug`（`InputAudioBufferAppend { audio: String }`，`codex-rs/codex-api/src/endpoint/realtime_websocket/protocol.rs:50-54`），以及 TRACE 等級的序列化 JSON（`codex-rs/codex-api/src/endpoint/realtime_websocket/methods.rs:548`，target `codex_api::realtime_websocket::wire`）。伺服器送來的事件也會被 trace（`codex-rs/codex-api/src/endpoint/realtime_websocket/methods.rs:598`）。兩個過濾器都沒有排除這些 target（`codex-rs/feedback/src/lib.rs:255-267`；`codex-rs/state/src/log_db.rs:61-89`），所以它們會進入 ring 與 SQLite，並隨 include_logs 回饋送到 Sentry。音訊路徑：`codex-rs/core/src/realtime_conversation.rs:2927-2935` → `codex-rs/codex-api/src/endpoint/realtime_websocket/methods.rs:362-371`。WebRTC 傳輸的音訊不經過這條路徑。「每秒 50 個訊框」取決於用戶端，此處無法驗證。
  位置：`codex-rs/codex-api/src/endpoint/realtime_websocket/methods.rs:537`
- `[隱私]` **F2-4 完整 SDP 以 TRACE 記錄**（已確認）
  完整的 outbound SDP offer 會進入 ring 與 SQLite，並隨 include_logs 回饋上傳。內容（candidate IP、ICE ufrag/pwd、DTLS fingerprint）是每通電話各自產生的，具體內容取決於 WebRTC stack。
  位置：`codex-rs/codex-api/src/endpoint/realtime_call.rs:135`

---

### 3.3 流程 3：Codex Apps 檔案參數（OpenAI Files API）

#### 觸發

模型呼叫 Codex Apps MCP server 的工具，且該工具在 `_meta["openai/fileParams"]` 宣告了檔案參數（`codex-rs/codex-mcp/src/codex_apps/file_params.rs:46`）。只有 Codex Apps 能這樣做：`openai_file_input_optional_fields_for_server` 只在 `server == CODEX_APPS_MCP_SERVER_NAME` 時保留這些欄位，第三方 MCP server 無法藉此上傳（`codex-rs/core/src/mcp_tool_call.rs:2147-2162`）。工具呼叫只要被允許就會上傳，不一定有使用者提示（F3-1）。

#### 步驟

1. 工具探索時，`prepare_openai_file_params_for_model` 改寫 schema，讓模型看到的是本機路徑參數（`codex-rs/codex-mcp/src/codex_apps/file_params.rs:46-148`）。
2. 核准判斷：`prepare_mcp_tool_approval` 回傳 NotRequired 時，直接以 `McpToolApprovalApplication::NotRequired` 進入 `handle_approved_mcp_tool_call`（`codex-rs/core/src/mcp_tool_call.rs:399-410`）。
3. 建立 `HostedFileUploadContext`（connector_id、action_name、model，`thread_id` 寫死為 `None`），然後呼叫 `rewrite_mcp_tool_arguments_for_openai_files`（`codex-rs/core/src/mcp_tool_call.rs:556-566`）。這整段在 `tool_catalog.run_with_snapshot` 內執行，而且早於 tool timeout deadline 的計算（`codex-rs/codex-mcp/src/binding.rs:452-458`）。
4. 逐欄位、逐陣列元素依序處理（`codex-rs/core/src/mcp_openai_file.rs:52-70,105-125`）：
   - 需要 ChatGPT／Codex backend 認證。
   - 以 `turn_environment.cwd().join(file_path)` 解析路徑；絕對路徑會直接取代 cwd（`codex-rs/core/src/mcp_openai_file.rs:160-163`；`codex-rs/utils/path-uri/src/lib.rs:462-466`）。
   - 依檔案系統讀取政策決定是否經沙箱讀取，並確認 executor 支援沙箱串流（`codex-rs/core/src/mcp_openai_file.rs:164-190`）。
5. `get_metadata` 要求目標是檔案且不超過 512 MiB，接著先開一次 read stream 做權限檢查（`codex-rs/core/src/mcp_openai_file.rs:191-215`）。每個檔案都重新做 runtime destination 驗證、`auth_provider_for_request` 與 `bind_http_client_pool`（`codex-rs/core/src/mcp_openai_file.rs:239-266`）。
6. `POST {chatgpt_base_url}/files`，body 為 `{file_name, file_size, use_case: "codex", codex_connector_id, codex_action_name, codex_model}`（`codex-rs/codex-api/src/files.rs:155-175`）。只嘗試一次，逾時 60 s（`codex-rs/codex-api/src/files.rs:23,176-198`）。
7. `PUT upload_url`（Azure SAS）。header 包括 `x-ms-blob-type: BlockBlob`、每次嘗試新產生的 `x-ms-client-request-id`、`Content-Length`（取自步驟 5 的 `metadata.size`），以及伺服器提供的 headers。最多 5 次，共用 5 分鐘 deadline，每次從頭重開 stream（`codex-rs/codex-api/src/files.rs:205-259`；`codex-rs/core/src/mcp_openai_file.rs:227-238`）。
8. `POST {chatgpt_base_url}/files/{file_id}/uploaded`。回應 `status == "retry"` 時每 250 ms 再送一次，直到大約 30 s（`codex-rs/codex-api/src/files.rs:26-27,368-446`）。
9. 把參數改寫為 `{download_url, file_id, [mime_type], [file_name]}`，然後呼叫 MCP 工具（`codex-rs/core/src/mcp_openai_file.rs:278-300`）。

#### 目的地與認證

- `POST {chatgpt_base_url}/files`、`PUT <Azure blob SAS URL>`、`POST {chatgpt_base_url}/files/{file_id}/uploaded`。得到的 `download_url` 交給 Codex Apps MCP server。
- create 與 finalize 由 `AuthProvider` 加上認證 headers（`codex-rs/codex-api/src/files.rs:449-461`）。
- blob PUT 不帶 Codex 認證，簽章在 URL 裡。錯誤訊息會把 URL 移除（`codex-rs/codex-api/src/files.rs:231` `source.without_url()`）。

#### 載荷內容與大小限制、壓縮、脫敏

- 原始檔案位元組，以串流方式從執行環境檔案系統讀出，不緩衝，最多 `OPENAI_FILE_UPLOAD_LIMIT_BYTES` = 512 MiB（`codex-rs/codex-api/src/files.rs:21`）。
- 不壓縮，不脫敏。
- 中繼資料：檔名、大小、connector 與 action ID、model slug。

#### 重試與錯誤處理

- blob PUT 重試 502、503、504 以及 timeout、connect、body、request 錯誤（`codex-rs/codex-api/src/files.rs:309-327`）。退避從 125 ms 起指數成長並加 ±10% 抖動（125、250、500、1000 ms，合計約 1.9 s），伺服器有 `x-ms-retry-after-ms`／`Retry-After` 時改用伺服器的值，全部都在 5 分鐘 deadline 內（`codex-rs/codex-api/src/files.rs:334-338`；`codex-rs/codex-client/src/retry.rs:43-52`）。
- create 與 finalize 都不重試。
- 非預期狀態碼的錯誤會帶完整回應 body（`codex-rs/codex-api/src/files.rs:192-197`）。

#### 隱私說明

- 檔案內容離開本機，存到 OpenAI 儲存空間，`download_url` 交給 connector。
- 要上傳的路徑由模型決定，範圍只受當次 turn 的檔案系統讀取政策限制（F3-1）。

#### 已驗證的問題

- `[安全]` **F3-1 模型可在沒有提示的情況下上傳任何可讀檔案**（部分確認）
  原說法是「只在 MCP 核准後才上傳」，這不精確。只要工具呼叫被放行就會上傳，以下情況都不會提示使用者：Auto 模式加 `read_only_hint` 的工具、Approve 模式（`codex-rs/core/src/mcp_tool_call.rs:2773-2784`）、自動核准的 permission profile（`codex-rs/core/src/mcp_tool_call.rs:1669-1679`）、已記住或 auto-review 的核准。路徑由模型提供，可以是絕對路徑（`codex-rs/core/src/mcp_openai_file.rs:160-163`；`codex-rs/utils/path-uri/src/lib.rs:462-466`），只受讀取政策／沙箱限制（`codex-rs/core/src/mcp_openai_file.rs:164-190`）。因此在沒有提示的情況下，任何可讀檔案都可能被上傳到 OpenAI 儲存空間，其 `download_url` 再交給 connector。原研究沒有回報這個問題，它是本研究最主要的安全發現。
  位置：`codex-rs/core/src/mcp_tool_call.rs:399-410`
- `[可靠性]` **F3-2 create 與 finalize 不重試**（部分確認）
  create 與 finalize 各只嘗試一次（60 s 逾時），任何傳輸錯誤或非 2xx 都會讓上傳失敗。所以 512 MiB 的 PUT 已經成功時，finalize 只要遇到一次暫時性錯誤，前面的上傳就白費了。伺服器端是否留下孤立的檔案記錄，從用戶端程式碼無法驗證。
  位置：`codex-rs/codex-api/src/files.rs:176-198,380-405`
- `[可靠性]` **F3-3 finalize 以固定 250 ms 輪詢**（已確認）
  沒有退避，30 s 內最多約 120 次 POST。30 s 預算只在收到 `retry` 回應之後才檢查，而單一請求最長可以跑 60 s（`codex-rs/codex-api/src/files.rs:23,459`）。
  位置：`codex-rs/codex-api/src/files.rs:26-27,429-435`
- `[可靠性]` **F3-4 blob 可重試的狀態碼太少**（已確認）
  只重試 502、503、504 與傳輸錯誤；500、408、429 直接失敗。伺服器沒給 Retry-After 時，四次退避加起來約 1.9 s。「Azure 把 500 視為可重試」是外部文件的說法，本儲存庫中沒有依據。
  位置：`codex-rs/codex-api/src/files.rs:309-315,334-335`
- `[可靠性]` **F3-5 單次 PUT 最多 512 MiB、總時限 5 分鐘**（已確認）
  512 MiB／300 s 約需 1.79 MB/s，而且重試與退避時間也算在內。每次重試都從第 0 位元組開始，沒有 Put Block 分塊或續傳。
  位置：`codex-rs/codex-api/src/files.rs:21,24,210,239-259`
- `[可靠性]` **F3-6 `Content-Length` 有 TOCTOU 問題**（部分確認）
  宣告的長度在第一次 metadata 呼叫時就固定了，body／request 錯誤重試時仍用同一個長度，所以檔案大小一旦改變，後續每次嘗試都會失敗，直到次數用完。具體失敗形式（body 錯誤或內容被截斷）取決於 hyper/reqwest 與 executor stream，本儲存庫看不出來。
  位置：`codex-rs/core/src/mcp_openai_file.rs:192-209,272`、`codex-rs/codex-api/src/files.rs:251`
- `[可靠性]` **F3-7 上傳不受 tool timeout 限制，且持有 catalog 讀鎖**（已確認）
  上傳（每個檔案最多 5 分鐘 PUT 加 30 s finalize，依序執行）在 tool timeout 起算之前執行，期間一直持有 catalog 的 `calls` 讀鎖（`codex-rs/codex-mcp/src/client_tool_catalog.rs:290`）。這會在整段上傳期間擋住 catalog 的寫入路徑（`codex-rs/codex-mcp/src/client_tool_catalog.rs:249`）。
  位置：`codex-rs/codex-mcp/src/binding.rs:452-458`
- `[效能]` **F3-8 多個檔案依序上傳**（已確認）
  N 個檔案需要 N 倍的（create＋PUT＋finalize 輪詢）延遲。
  位置：`codex-rs/core/src/mcp_openai_file.rs:52-70,105-125`
- `[清理]` **F3-9 每個檔案重算目的地驗證、auth 與 client pool**（已確認）
  只有 `sess.services.auth_manager.auth().await` 提到迴圈外（`codex-rs/core/src/mcp_openai_file.rs:49`）。相對於網路往返，這部分成本很小。
  位置：`codex-rs/core/src/mcp_openai_file.rs:239-266`
- `[清理]` **F3-10 `codex_thread_id` 從來不會送出**（已確認）
  `HostedFileUploadContext { thread_id: None, ... }` 是寫死的，而 `codex-rs/codex-api/src/files.rs:173-175` 只在有 thread_id 時才加入 `codex_thread_id`。
  位置：`codex-rs/core/src/mcp_tool_call.rs:560-561`

---

### 3.4 流程 4：圖片準備、附件儲存與影像生成

#### 觸發

使用者訊息或工具輸出裡有圖片，例如貼上的圖、本機圖片、data URL、影像生成的結果。每個 turn 記錄歷史時都會處理；resume／fork 重播時也會再處理一次。

#### 步驟

1. LocalImage：用 `std::fs::read` 同步讀取整個檔案，事先不檢查大小。Defer 模式下包成 `data:application/octet-stream` 的 base64 URL（`codex-rs/protocol/src/models.rs:2219,2229-2234`）。音訊的處理方式相同（`codex-rs/protocol/src/models.rs:2249`）。
2. `prepare_response_items` 依序逐一準備每個 InputImage（`codex-rs/core/src/image_preparation.rs:273-298,316-338`）。
3. `prepare_image` 的處理順序：
   - 拒絕 http(s) URL。
   - base64 解碼，上限 1 GiB（`codex-rs/utils/image/src/lib.rs:31,284-304`）。
   - 以 SHA-1 查快取，快取容量 32 筆／64 MiB（`codex-rs/utils/image/src/lib.rs:32,99-100`）。
   - 一定會完整解碼像素（`codex-rs/utils/image/src/lib.rs:157`）。
   - 需要時縮放：HIGH 最大 2048 px，ORIGINAL 最大 6000 px（`codex-rs/utils/image/src/lib.rs:75-83`）。縮放後重新編碼為 PNG/JPEG/WebP，保留 EXIF 與 RGB ICC（`codex-rs/utils/image/src/lib.rs:143-156`）。
   - 不需縮放且格式是 PNG/JPEG/WebP 時，原樣傳遞位元組（`codex-rs/utils/image/src/lib.rs:195-205,354-360`）。
   - 以上全部在 async 函式內同步執行（`codex-rs/core/src/image_preparation.rs:366`）。
4. 建立 `UploadRequest`（`data: prepared.encoded.bytes.to_vec()`，`codex-rs/core/src/image_preparation.rs:376`），呼叫 `image_store.upload`。`InlineAttachmentStore` 原樣傳回位元組（`codex-rs/attachment-store/src/lib.rs:194-202`），再編成 base64 data URL（`codex-rs/core/src/image_preparation.rs:380`）。
5. resume／fork 以 `ImagePreparationMode::DetailBased` 與 `InlineAttachmentStore` 重新準備所有歷史圖片（`codex-rs/core/src/session/mod.rs:2002`；HEAD 為第 1999 行）。
6. 影像生成：先 base64 解碼結果，上傳到附件儲存供 UI 顯示；inline 時又再 base64 編碼一次（`codex-rs/ext/image-generation/src/tool.rs:276,285-289`）。
7. 準備好的 data URL 放進 Responses 請求送往模型供應商。只有同時符合三個條件時才用 zstd 壓縮：`enable_request_compression` 開啟、使用 Codex backend 認證、供應商是 OpenAI（`codex-rs/core/src/client.rs:1937-1945`）。

#### 目的地與認證

- 本儲存庫內只有 inline 方式：圖片放在送往模型供應商的 Responses 請求 body 裡。
- 唯一的非測試 `AttachmentStore` 實作是 `InlineAttachmentStore`（`codex-rs/attachment-store/src/lib.rs:194`）。`codex-rs/core/src/thread_manager.rs:572-574` 與 `codex-rs/app-server/src/message_processor.rs:413` 使用 passthrough store。
- `ThreadManager` 接受 `image_store: Arc<dyn AttachmentStore>`（`codex-rs/core/src/thread_manager.rs:596`），嵌入方可以注入其他實作，但本儲存庫沒有提供。
- 認證與模型請求相同。

#### 載荷內容與大小限制、壓縮、脫敏

- base64 data URL。
- 不需縮放的 PNG/JPEG/WebP 逐位元組原樣送出，所有中繼資料都保留；重新編碼時保留 EXIF（含 GPS）與 RGB ICC。
- 輸入上限 1 GiB（base64 之後才檢查）。

#### 重試與錯誤處理

- 處理失敗時，以文字 placeholder 取代圖片（`codex-rs/core/src/image_preparation.rs:288-295,331-336`）。
- 附件儲存上傳失敗時，退回 inline data URL（`codex-rs/core/src/image_preparation.rs:383-388`）。
- inline store 不需要重試。

#### 隱私說明

EXIF（可能含 GPS 與相機序號）會送給模型供應商（F4-7）。遠端圖片 URL 會被拒絕，不會去抓取。

#### 已驗證的問題

- `[效能]` **F4-1 在 async runtime 上同步讀取檔案，且沒有事先檢查大小**（已確認）
  Defer 模式下，好幾 GiB 的檔案會先被完整讀入並 base64 編碼，之後 1 GiB 上限才把它拒絕（`codex-rs/utils/image/src/lib.rs:284-304`）。
  位置：`codex-rs/protocol/src/models.rs:2219,2249`
- `[效能]` **F4-2 Defer 模式的多餘往返**（部分確認）
  每張本機圖片都要經過：位元組 → base64 → 位元組 → 完整像素解碼 → base64（`codex-rs/utils/image/src/lib.rs:291-297,157`；`codex-rs/core/src/image_preparation.rs:380`）。更正：只有需要縮放或格式不能原樣傳遞時，才會重新編碼。
  位置：`codex-rs/protocol/src/models.rs:2229-2234`
- `[效能]` **F4-3 CPU 密集的解碼、縮放、編碼直接在 async runtime 上執行**（已確認）
  沒有 `spawn_blocking` 或 `block_in_place`，而且一次只處理一張。
  位置：`codex-rs/core/src/image_preparation.rs:366`（迴圈在 `codex-rs/core/src/image_preparation.rs:273-298,316-338`）
- `[效能]` **F4-4 resume／fork 會重新處理所有歷史圖片**（部分確認）
  每張歷史圖片都要算 SHA-1 並完整解碼，除非命中 32 筆／64 MiB 的快取（`codex-rs/utils/image/src/lib.rs:32,99-100`）。更正：只有仍超出限制，或格式不能原樣傳遞（例如舊版未準備的媒體）時，才會縮放與重新編碼。
  位置：`codex-rs/core/src/session/mod.rs:2002`
- `[效能]` **F4-5 為了保留 fallback 而多複製一次**（已確認）
  `prepared.encoded.bytes` 本來是 `Arc<[u8]>`（`codex-rs/utils/image/src/lib.rs:40`），但 `UploadRequest.data` 是 `Vec<u8>`（`codex-rs/attachment-store/src/lib.rs:60-61`），所以每張圖都要複製。
  位置：`codex-rs/core/src/image_preparation.rs:376`
- `[效能]` **F4-6 影像生成結果白白解碼再編碼**（已確認）
  inline store 會原樣傳回 `request.data`（`codex-rs/attachment-store/src/lib.rs:194-202`），所以先 base64 解碼、又立刻重新編碼，沒有任何作用。
  位置：`codex-rs/ext/image-generation/src/tool.rs:276,285-289`
- `[隱私]` **F4-7 圖片中繼資料（含 GPS）送給模型供應商**（部分確認）
  重新編碼時刻意保留 EXIF 與 RGB ICC；不需縮放的圖片連同所有中繼資料原樣送出。更正：限制尺寸方面，ORIGINAL detail 可以到 6000 px，2048 px 是 HIGH detail 的上限；zstd 壓縮還需要 `enable_request_compression`。
  位置：`codex-rs/utils/image/src/lib.rs:143-156,195-205`

---

### 3.5 流程 5：Plugin share（`plugin/share/save`）

#### 觸發

- app-server 用戶端呼叫 `plugin/share/save`（`codex-rs/app-server-protocol/src/protocol/common.rs:1005-1009`）。TUI 沒有呼叫端，實務上是由 Desktop 呼叫。
- 需要 `Feature::Plugins`（`codex-rs/app-server/src/request_processors/plugins.rs:1505`）、`Feature::PluginSharing`（`codex-rs/app-server/src/request_processors/plugins.rs:1298`）與 ChatGPT 認證（`codex-rs/core-plugins/src/remote/share.rs:151`）。
- 目標可以是任何含 `.codex-plugin/plugin.json` 或有效 manifest 的絕對路徑目錄（`codex-rs/core-plugins/src/plugin_bundle_archive.rs:57-70`）。
- 這個 RPC 走 `global("config")` 序列化通道（`codex-rs/app-server-protocol/src/protocol/common.rs:1007`）。

#### 步驟

1. 驗證 feature flag、`remote_plugin_id` 格式與 discoverability（LISTED 會被拒絕），更新時不能指定 targets（`codex-rs/app-server/src/request_processors/plugins.rs:1297-1324`）。
2. 在 `spawn_blocking` 中把整個目錄遞迴打包成 `<dir>.tar.gz`：項目排序、遇到 symlink 等非檔案／目錄的項目就報錯，寫進上限 50 MiB 的記憶體緩衝（`codex-rs/core-plugins/src/remote/share.rs:20,154-160`；`codex-rs/core-plugins/src/plugin_bundle_archive.rs:72-111`）。
3. `POST {chatgpt_base_url}/public/plugins/workspace/upload-url`，取得 `file_id`、`upload_url`、`etag`；沒有 etag 就在 PUT 之前失敗（`codex-rs/core-plugins/src/remote/share.rs:161-171,406`）。
4. `PUT upload_url`（`x-ms-blob-type: BlockBlob`），逾時 30 s，只 `.send()` 一次（`codex-rs/core-plugins/src/remote/share.rs:172,418-452`；`codex-rs/core-plugins/src/remote.rs:114`）。
5. UNLISTED 時，把帳號所屬 workspace 加為 reader target，然後 `POST /public/plugins/workspace` 或 `/public/plugins/workspace/{id}`（`codex-rs/core-plugins/src/remote/share.rs:173-187,461-465`）。
6. 記錄本機 plugin 路徑對應（best effort），清除遠端目錄快取，回傳分享 URL（`codex-rs/core-plugins/src/remote/share.rs:194-209`）。

#### 目的地與認證

- API 端點如上。`PUT /ps/plugins/{id}/shares` 屬於另一個 RPC `plugin/share/updateTargets`，不在 save 流程內（`codex-rs/core-plugins/src/remote/share.rs:300-333`）。
- API 請求使用 ChatGPT 認證（bearer 加帳號 headers）。
- blob PUT 只帶 `default_headers()`（`OAI-Product-Sku`），不帶 bearer（`codex-rs/core-plugins/src/remote.rs:286-290`），並透過不記錄請求的 pool 送出（`codex-rs/core-plugins/src/remote.rs:178-182`）。

#### 載荷內容與大小限制、壓縮、脫敏

- 目錄內所有一般檔案的 gzip tar，壓縮後最多 50 MiB。
- tar header 帶本機的 uid、gid、mode、mtime。
- 沒有忽略規則，也沒有祕密掃描。
- JSON 中繼資料：檔名、大小、share targets。

#### 重試與錯誤處理

所有步驟都不重試。PUT 逾時 30 s；回應不是 200/201 時回傳帶 body 的 `UnexpectedStatus`。記錄本機路徑對應失敗只會發出警告。

#### 隱私說明

整個目錄樹都會發布到 workspace，包括 `.git/` 與 `.env`。

#### 已驗證的問題

- `[隱私]` **F5-1 不排除 `.git`、`.env*`、`node_modules`、`target` 與 `.gitignore` 列出的檔案**（已確認）
  每個一般檔案都會被打包，包括 `.git/` 與 `.env`，還會浪費 50 MiB 的額度。
  位置：`codex-rs/core-plugins/src/plugin_bundle_archive.rs:82-111`
- `[隱私]` **F5-2 tar header 帶本機 uid/gid/mode/mtime**（已確認）
  `tar::Builder::new` 預設用 `HeaderMode::Complete`，檔案與目錄的 header 都取自檔案中繼資料，所以封存結果不可重現，還會帶出數字型的 uid/gid（敏感度低）。可改用 `archive.mode(tar::HeaderMode::Deterministic)`，或像 `codex-rs/feedback/src/rollout_archive.rs:117-121` 那樣正規化 header。
  位置：`codex-rs/core-plugins/src/plugin_bundle_archive.rs:73,99,102`
- `[可靠性]` **F5-3 50 MiB 只給 30 s，而且不重試**（已確認）
  上傳 50 MiB 約需 1.7 MB/s。
  位置：`codex-rs/core-plugins/src/remote/share.rs:423-435`、`codex-rs/core-plugins/src/remote.rs:114`
- `[可靠性]` **F5-4 PUT 或 finalize 失敗後不清理也不重試**（部分確認）
  用戶端在 PUT 或 finalize 失敗後不重試，也不清理。伺服器端是否留下孤立的 upload，從本儲存庫無法驗證。
  位置：`codex-rs/core-plugins/src/remote/share.rs:161-187`
- `[可靠性]` **F5-5 save 佔用全域 `config` 序列化通道**（已確認）
  打包與上傳（最多 30 s PUT 加兩次 API 呼叫）期間，其他走 config 通道的 RPC 都要排隊等待。
  位置：`codex-rs/app-server-protocol/src/protocol/common.rs:1005-1008`

---

### 3.6 流程 6：Cloud 對話 turn（`AddUserMessage`）

#### 觸發

TUI 連到 Cloud thread 時，使用者送出訊息（turn/start 或 steer）。只有 TUI 用這條路徑：`codex-rs/tui/src/lib.rs:2320` 是 `cloud_backend::connect` 唯一的呼叫端。

#### 步驟

1. worker 迴圈一次處理一個 `CloudCommand`，處理完才接下一個；等待期間只轉送 reply stream 的事件（`codex-rs/app-server-client/src/cloud_backend/mod.rs:176-243`）。命令通道容量 32（`codex-rs/app-server-client/src/cloud_backend/mod.rs:153`）。
2. `cloud_message` 轉換輸入：合併文字，傳遞 inline data URL 與 file_id 圖片，拒絕 LocalImage、音訊、skills 與 mentions（`codex-rs/app-server-client/src/cloud_backend/requests.rs:296-380`）。
3. `add_user_message` 驗證輸入並建立 protobuf `AddRequest`，以 `client_user_message_id` 作為冪等鍵（`codex-rs/cloud-client/src/turns.rs:126-217`）。含圖片且編碼後超過 4 MiB − 256 KiB 的訊息會被拒絕（`codex-rs/cloud-client/src/turns.rs:13,218-222`）；任何超過 64 MiB 的訊息也會被拒絕（`codex-rs/cloud-client/src/turns.rs:269`；`codex-rs/cloud-client/src/lib.rs:102`）。
4. `mutate()` 以 gRPC server-streaming 呼叫送出，逾時 150 s（`codex-rs/cloud-client/src/lib.rs:101`），並檢查 trailer 的 `grpc-status` 是否為 0（`codex-rs/cloud-client/src/turns.rs:264-305`）。
5. 解析受理結果：Started、Steered、Duplicate 或 Queued（`codex-rs/cloud-client/src/turns.rs:307-338`）。

#### 目的地與認證

- gRPC `/oaiproto.codex_app_server_backend.ThreadService/AddUserMessage`（以及 `InterruptTurn`）。
- origin 寫死為 `https://codex-cloud-backend.chatgpt.com`（`codex-rs/app-server-client/src/cloud_backend/auth.rs:14`），無法設定。
- 認證：ChatGPT bearer token 加 `ChatGPT-Account-Id` header，兩者都標為 sensitive（`codex-rs/cloud-client/src/types.rs:17-32`）。
- 憑證在連線時擷取一次。拒絕非 ChatGPT 與 FedRAMP 認證（`codex-rs/app-server-client/src/cloud_backend/auth.rs:26-28`），JWT 過期時也拒絕（`codex-rs/app-server-client/src/cloud_backend/auth.rs:33-37`）。憑證變更後需要建立新的 client（`codex-rs/cloud-client/src/lib.rs:105`）。

#### 載荷內容與大小限制、壓縮、脫敏

- 提示文字與 text element 範圍。
- 圖片為 inline base64 data URL 或既有的 file ID。
- 含圖片的訊息上限約 3.75 MiB，其他訊息上限 64 MiB。
- 請求型別的 `Debug` 會遮蔽內容（`codex-rs/cloud-client/src/turns.rs:104-117`）。

#### 重試與錯誤處理

- 刻意不重試：`codex-rs/cloud-client/src/turns.rs:1` 寫明「Explicit, non-retried mutations」，`codex-rs/cloud-client/src/turns.rs:95-96` 寫明「never blindly resubmit」。失敗時回傳 OutcomeUnknown。
- TUI 不會丟掉訊息：它以 `MessageDelivery::Unconfirmed(Some(client_id))` 把訊息放回佇列最前面（`codex-rs/tui/src/chatwidget/input_receipts.rs:103-111`），重新連線時再與已確認的 ID 比對（`codex-rs/tui/src/chatwidget/user_messages.rs:865-893`）。

#### 隱私說明

訊息文字與圖片會送到 Cloud backend。Debug 輸出有遮蔽。

#### 已驗證的問題

- `[可靠性]` **F6-1 單一序列化 worker 會擋住 interrupt**（已確認）
  所有 Cloud 命令（account、list、read、turns/items、interrupt）都會排在處理中的 `AddUserMessage` 後面（最長 150 s）。interrupt 也走同一個通道（`codex-rs/app-server-client/src/cloud.rs:244-245` → `codex-rs/app-server-client/src/cloud_backend/requests.rs:214-223`）。
  位置：`codex-rs/app-server-client/src/cloud_backend/mod.rs:176-243`
- `[可靠性]` **F6-2 不會自動重試**（部分確認）
  這是有文件記載的設計選擇。從用戶端程式碼無法證明伺服器的去重語意是否讓重試安全。更正：使用者不必重新輸入，TUI 會保留 Unconfirmed 訊息並在重新連線時比對。
  位置：`codex-rs/cloud-client/src/turns.rs:1,95-96,312-317`
- `[效能]` **F6-3 重複計算 `encoded_len()` 並複製字串**（已確認，影響小）
  第二次 `encoded_len` 只發生在含圖片的訊息。每個 data URL（最多約 3.75 MiB）會被複製一次到 protobuf，之後 tonic 還會再編碼一次。
  位置：`codex-rs/cloud-client/src/turns.rs:166,193,218-222,269`

---

### 3.7 流程 7：Cloud tasks（`codex cloud`）

#### 觸發

- 每次開啟 cloud tasks TUI 時都會自動偵測環境（`codex-rs/cloud-tasks/src/lib.rs:947-967`），不只是建立任務時。
- 使用者建立任務。

#### 步驟

1. 讀取本機 git origin，用 `SanitizedGitUrl` 去除帳密（`codex-rs/protocol/src/sanitized_git_url.rs:71-99`）。讀取是在 spawned task 裡用阻塞的 `std::process::Command::new("git")` 執行（`codex-rs/cloud-tasks/src/env_detect.rs:225-270`）。
2. 對每個 GitHub owner/repo 依序 `GET .../environments/by-repo/github/{owner}/{repo}`，然後 fallback GET 完整清單（`codex-rs/cloud-tasks/src/env_detect.rs:60-100`）。base URL 含 `/backend-api` 時用 `/wham/environments`，否則用 `/api/codex/environments`（`codex-rs/cloud-tasks/src/env_detect.rs:62-72,96-100`）。
3. 把 git origin、完整的 `/environments` JSON（pretty print）、by-repo URL 與錯誤 body 寫進 `./error.log`（`codex-rs/cloud-tasks/src/env_detect.rs:58,101-114`）。
4. 建立任務：`Tasks::create` 組合 input items，包括提示、`CODEX_STARTING_DIFF` 環境變數中的 diff（若有，`codex-rs/cloud-tasks-client/src/http.rs:340-371`），以及 `best_of_n > 1` 時的 metadata（`codex-rs/cloud-tasks-client/src/http.rs:373-380`）。
5. 依 PathStyle `POST /wham/tasks` 或 `/api/codex/tasks`（`codex-rs/backend-client/src/client.rs:718-722`）。
6. 成功或失敗都在 `./error.log` 寫一行（`codex-rs/cloud-tasks-client/src/http.rs:384-398,919-929`）。

#### 目的地與認證

- `{base_url}/wham/...` 或 `{base_url}/api/codex/...`。
- 使用 backend client headers：ChatGPT bearer 加帳號（`codex-rs/backend-client/src/client.rs:722-727`）。

#### 載荷內容與大小限制、壓縮、脫敏

- 提示、環境 ID、分支、QA flag、best_of_n，以及 `CODEX_STARTING_DIFF` 中可能有的完整 git diff。
- GET 路徑包含 owner/repo。
- 不壓縮。

#### 重試與錯誤處理

只有 bootstrap GET 會重試（`codex-rs/backend-client/src/client.rs:477-478`），task 的 create 與 list 都不重試。非 2xx 錯誤會內嵌完整回應 body（`codex-rs/backend-client/src/client.rs:420-426`）。

#### 隱私說明

`./error.log` 只存在本機、不會上傳，但它寫在程序的 cwd，而 cwd 常常是 git 工作樹。

#### 已驗證的問題

- `[隱私]` **F7-1 把 `error.log` 寫在 cwd**（已確認）
  有兩個獨立的 helper 用相對路徑 `"error.log"` 開檔寫入（cloud-tasks 中有 38 個呼叫點），而且在 async 程式碼中使用阻塞的 `std::fs`。
  位置：`codex-rs/cloud-tasks-client/src/http.rs:919-929`、`codex-rs/cloud-tasks/src/util.rs:19-29`
- `[隱私]` **F7-2 環境偵測把完整回應寫進 `error.log`**（部分確認）
  更正：寫進去的 origin 已去除帳密。比較大的本機外洩是：每次開啟 cloud TUI，都會把完整的 `/environments` 回應 body、by-repo URL 與錯誤 body 寫進 `./error.log`。
  位置：`codex-rs/cloud-tasks/src/env_detect.rs:58,101-114`
- `[效能]` **F7-3 by-repo 查詢依序執行，git 子行程阻塞 worker**（已確認，影響小）
  每個 origin 的查詢依序進行。git 子行程呼叫會佔住一個 tokio worker 執行緒。儲存庫通常只有 1 到 2 個 origin，而且這段在 spawned task 中執行，不會擋住首次繪製。
  位置：`codex-rs/cloud-tasks/src/env_detect.rs:60-87,225-270`
- `[清理]` **F7-4 錯誤訊息內嵌完整回應 body，並寫進本機日誌**（部分確認）
  更正：環境查詢在非 ChatGPT base URL 時也會用 `/api/codex/environments`；自動偵測在開啟時就送出所有 GitHub owner/repo；task create 與 list 不重試；非 2xx 與 decode 失敗都會把完整 body 嵌進錯誤，而 cloud-tasks 會把這些錯誤寫進 `./error.log`。
  位置：`codex-rs/backend-client/src/client.rs:420-426`

---

### 3.8 流程 8：Memories stage-1 擷取

#### 觸發

在 `Feature::MemoryTool` 開啟（預設關閉，`codex-rs/features/src/lib.rs:1446-1449`）且主要環境已設定時，每個開始新 turn 且有輸入的 `turn/start` 都會觸發（`codex-rs/app-server/src/request_processors/turn_processor.rs:740-751`）。這是唯一的非測試呼叫端。觸發時機不是 session 啟動。

#### 步驟

1. 先做速率限制預檢（`codex-rs/memories/write/src/start.rs:87-94`）。預檢出錯，或使用不走 Codex backend 的認證時，一律放行（`codex-rs/memories/write/src/guard.rs:9-13`）。
2. 選版本：`memories.dual_write=true` 且版本是 V1 時，V1 與 V2 都跑（`codex-rs/memories/write/src/start.rs:48-52`）。`dual_write` 預設為 false（`codex-rs/config/src/types.rs:386`）。每個版本都會深複製整個 Config（`codex-rs/memories/write/src/start.rs:53-56`）。
3. 認領最多 `max_rollouts_per_startup` 個（預設 2，`codex-rs/config/src/types.rs:57`）其他互動 thread，條件是 `memory_mode = 'enabled'`、10 天內有更新、閒置至少 6 小時、cwd 不限（`codex-rs/state/src/runtime/memories.rs:244-257`；`codex-rs/memories/write/src/phase1.rs:138`）。
4. 以 `buffer_unordered` 執行，並行上限 8（`codex-rs/memories/write/src/lib.rs:85`；`codex-rs/memories/write/src/phase1.rs:178-188`），實際並行數不會超過認領數。
5. 讀取 rollout 尾端（最多 8 MiB，單筆 1 MiB，`codex-rs/memories/write/src/rollout_input.rs:28-29`）。每個項目先 sanitize，並為了量長度序列化一次（`codex-rs/memories/write/src/rollout_input.rs:69,74`）；之後為了組提示再 sanitize、再序列化一次（V2 在 `codex-rs/memories/write/src/rollout_input.rs:113,240`，V1 在 `codex-rs/memories/write/src/phase1.rs:403,420`）；最後跑 `redact_secrets`。這些在 `sample()` 中執行，位於 async runtime 上（`codex-rs/memories/write/src/phase1.rs:265-287`）。
6. 把 stage-1 提示串流送給模型供應商。token 預算是有效 context window 的 70%，無法得知 window 時 fallback 為 150k（`codex-rs/memories/write/src/prompts.rs:121-129`）。V2 提示包含 git branch（`codex-rs/memories/write/src/prompts.rs:156-165`）。
7. 失敗的 job 標為 failed，1 小時後重試（`codex-rs/memories/write/src/phase1.rs:325-341`）。phase 1 之後還有 phase 2 consolidation，同樣會呼叫模型，本研究未展開。

#### 目的地與認證

送往設定的模型供應商的 Responses 端點，使用 session 啟動時安裝的供應商 auth context（`codex-rs/memories/write/src/start.rs:43-46`）。

#### 載荷內容與大小限制、壓縮、脫敏

- 過去 session 的對話：使用者與助理訊息、工具呼叫與輸出（截斷），以及 cwd、路徑；V2 另含 git branch。
- 脫敏只靠 4 條正規表示式（`codex-rs/secrets/src/sanitizer.rs:4-11`），見 F8-5。

#### 重試與錯誤處理

本次執行內不重試，失敗後 1 小時重試。`ModelClient::stream` 內部的傳輸層重試依供應商政策，這裡沒有呈現。

#### 隱私說明

其他 thread（cwd 不限，可能是其他儲存庫）的內容會在背景重新送給模型供應商，沒有逐次提示。

#### 已驗證的問題

- `[效能]` **F8-1 每個有輸入的新 turn 都會觸發**（部分確認）
  更正：觸發點是 turn/start，不是 session 啟動。每次觸發都要做速率限制預檢與候選查詢；只要還有未認領的候選，每個新 turn 都會再跑一次。這項功能需要使用者主動開啟。
  位置：`codex-rs/app-server/src/request_processors/turn_processor.rs:740-751`
- `[效能]` **F8-2 stage-1 請求與使用者的 turn 同時開始**（部分確認）
  更正：預設最多 2 個並行請求（每個版本），而不是 8 個，除非調高 `max_rollouts_per_startup`。這些請求在背景與使用者的 turn 同時起跑，之後還有 phase 2。
  位置：`codex-rs/memories/write/src/lib.rs:85`、`codex-rs/memories/write/src/phase1.rs:138,178-188`
- `[效能]` **F8-3 dual_write 讓每個 rollout 送出兩次**（部分確認）
  只有使用者在 V1 下設定 `memories.dual_write=true` 時才會發生。V1 與 V2 的 store 各自獨立認領（`codex-rs/state/src/runtime/memory_versions.rs:15-25`），所以同一個 rollout 會各送一次。
  位置：`codex-rs/memories/write/src/start.rs:48-52`
- `[效能]` **F8-4 重複序列化與 sanitize，且在 async runtime 上執行**（已確認）
  每個項目序列化兩次、sanitize 兩次。8 MiB 尾端的序列化、遮蔽與截斷都在 async runtime 上執行，沒有用 `spawn_blocking`。
  位置：`codex-rs/memories/write/src/rollout_input.rs:69,74,113,240`
- `[隱私]` **F8-5 祕密遮蔽的涵蓋範圍很窄**（已確認，缺口比原說法更大）
  只有 4 條正規表示式，而且在 JSON 序列化之後才套用（`codex-rs/memories/write/src/phase1.rs:420-423`；`codex-rs/memories/write/src/rollout_input.rs:240-241`）。具體缺口：
  - `OPENAI_API_KEY=`、`DB_PASSWORD=`、`AWS_SECRET_ACCESS_KEY=`、`GITHUB_TOKEN=` 這類環境變數名稱不會被遮蔽，因為 `\b` 在 `_` 之後不成立。
  - `sk-[A-Za-z0-9]{20,}` 抓不到 `sk-proj-`、`sk-svcacct-`。
  - 遮蔽是在 JSON 跳脫後的文字上做的，`KEY="value"` 會變成 `KEY=\"value\"` 而不再相符；JSON key 如 `"password":` 也永遠不會相符。
  - 另外也漏掉 ghp_／gho_、沒有 `Bearer` 前綴的 JWT、PEM 私鑰、Slack／Google 金鑰，以及帶密碼的 DB URL。
  驗證者以 Python `re` 測試了同樣的 pattern；對這些 ASCII 輸入，它和 Rust regex crate 的行為相同。結論：stage-1 提示可能把常見的祕密帶給模型供應商。
  位置：`codex-rs/secrets/src/sanitizer.rs:4-11`
- `[清理]` **F8-6 每個版本都深複製 Config**（已確認，影響小）
  每個版本深複製一次（通常只有一個版本），但因為觸發方式的關係，每個新 turn 都會重複。
  位置：`codex-rs/memories/write/src/start.rs:53-56`

---

### 3.9 流程 9：產品分析事件

#### 觸發

各種 runtime 事件，例如 thread 初始化、turn、工具呼叫（MCP、命令、檔案變更、網頁搜尋）、plugin 與 app 使用、guardian 審查、compaction、hooks、skills、accepted lines。只有在 `config.analytics_enabled == Some(false)` 時才不建立 client（`codex-rs/core/src/session/session.rs:1824`；app-server 見 `codex-rs/app-server/src/analytics_utils.rs:25-31`）。

#### 步驟

1. `track_*` 以 `try_send` 把事件放進 256 格的 mpsc 佇列；佇列滿時丟棄並發出警告（`codex-rs/analytics/src/client.rs:93,211,293-301`）。
2. 單一 worker 讓每個事件經過 reducer，有輸出時就立刻 `await send_track_events`（`codex-rs/analytics/src/client.rs:273-282`）；沒有事件時提早返回（`codex-rs/analytics/src/client.rs:1073-1075,1086-1088`）。
3. 依產品 SKU 分流，各 stream 在 `JoinSet` 中並行（`codex-rs/analytics/src/client.rs:1090-1119`）；有些事件要單獨成一個請求，這些請求依序送出（`codex-rs/analytics/src/client.rs:1102-1111,1122-1143`）。
4. `send_track_events_request` 每次都呼叫 `create_client_for_route_async` 建立新的 HTTP 用戶端（`codex-rs/analytics/src/client.rs:1208-1214`），然後 POST JSON，逾時 10 s（`codex-rs/analytics/src/client.rs:94,1222-1227`），可能帶 `X-OpenAI-Product-Sku`。
5. `flush()` 最多等 25 s（`codex-rs/analytics/src/client.rs:96,430-450`）。

#### 目的地與認證

- `POST {chatgpt_base_url}/codex/analytics-events/events`（`codex-rs/analytics/src/client.rs:181-186`）。
- 認證：ChatGPT bearer 加帳號；API key 認證時只送允許的事件，並帶 OpenAI-Organization／Project headers。

#### 載荷內容與大小限制、壓縮、脫敏

- JSON `TrackEventsRequest{events:[...]}`，含 thread／turn ID、model slug、產品介面、計數等。
- accepted lines 事件帶 `repo_hash`，`line_fingerprints` 永遠是空的（`codex-rs/analytics/src/accepted_lines.rs:101`），所以只送出行數。
- 不壓縮（F9-4）。

#### 重試與錯誤處理

沒有重試。非 2xx 時記錄含回應 body 的警告，傳輸錯誤也只記錄警告，事件就此丟失（`codex-rs/analytics/src/client.rs:1233-1243`）。佇列滿時丟棄事件。

#### 隱私說明

預設開啟（opt-out），獨立 app-server 也是如此，雖然它的 Statsig 指標預設關閉。turn 與工具的中繼資料會與帳號關聯。

#### 已驗證的問題

- `[效能]` **F9-1 每次 POST 都建新的 HTTP 用戶端**（已確認）
  連線完全不重用，每次都要新的 TCP 與 TLS。建置過程還要排隊等一個程序共用、只有 1 個名額的 semaphore（`codex-rs/login/src/auth/default_client.rs:55-56,373-390`）。啟動時的 `/models` 請求（`codex-rs/model-provider/src/models_endpoint.rs:358`）等其他用戶端也用同一個 permit，所以分析事件的傳送可能拖慢它們的建置。本研究沒有在 `codex-rs/http-client/src` 找到共用的用戶端快取。
  位置：`codex-rs/analytics/src/client.rs:1208-1214`
- `[可靠性]` **F9-2 傳送在單一 worker 中進行，沒有批次處理，佇列滿就丟棄**（部分確認）
  更正：只有 reducer 產生事件時才會傳送。SKU stream 並行，所以最壞情況的停頓約為「單一 stream 內獨立請求批次數 × 10 s」，再加上 semaphore 造成的建置排隊，而不是再乘以 SKU stream 數。除了 reducer 本身的分組，沒有依時間或大小的批次處理。佇列滿時事件會被丟棄。
  位置：`codex-rs/analytics/src/client.rs:273-282,293-301`
- `[可靠性]` **F9-3 沒有重試或退避**（已確認）
  位置：`codex-rs/analytics/src/client.rs:1233-1243`
- `[效能]` **F9-4 JSON 不壓縮**（已確認）
  只有 `Content-Type: application/json` 與 `.json(&payload)`，沒有 `Content-Encoding`。
  位置：`codex-rs/analytics/src/client.rs:1222-1227`
- `[隱私]` **F9-5 錯誤回應 body 記錄進日誌**（已確認，敏感度低）
  body 是分析端點自己的錯誤回應。它會進入 feedback ring（沒有排除分析 target，`codex-rs/feedback/src/lib.rs:255-267`）與 SQLite（`codex-rs/state/src/log_db.rs:61-89`），只有 include_logs 回饋才會把它送到 Sentry。
  位置：`codex-rs/analytics/src/client.rs:1238`
- `[隱私]` **F9-6 `repo_hash` 可以反推**（部分確認）
  更正：是對固定、公開的 domain 前綴（`file-line-v1\0repo\0`）加上正規化 remote URL 做 SHA-1，沒有祕密 salt，所以用公開儲存庫 URL 的字典仍可反推。`line_fingerprints` 永遠是空的。
  位置：`codex-rs/analytics/src/accepted_lines.rs:63-69,106-116`

---

### 3.10 流程 10：OTel 指標（Statsig）與自訂 OTLP

#### 觸發

- 程序啟動時初始化 OTel provider。`analytics.enabled` 沒設定時用 `default_analytics_enabled`（`codex-rs/core/src/otel_init.rs:70-78`）。
- TUI 傳 true（`codex-rs/tui/src/startup_orchestration.rs:710`），exec 也傳 true（`codex-rs/exec/src/lib.rs:185`）。
- 獨立 `codex-app-server` 傳 false（`codex-rs/app-server/src/main.rs:129`）；`codex app-server` 的 `--analytics-default-enabled` 預設為 false（`codex-rs/cli/src/main.rs:631-632`）。
- debug build 關閉 Statsig（`codex-rs/otel/src/config.rs:20-22`）。
- logs 與 traces exporter 預設為 None，需要使用者自行設定。

#### 步驟

1. `resolve_exporter` 把 Statsig 對應到 `OtlpHttp { endpoint: https://ab.chatgpt.com/otlp/v1/metrics, protocol: Json, tls: None }`，並帶固定的 `statsig-api-key` header（`codex-rs/otel/src/config.rs:9-33`）。
2. `MetricsClient::new` 建立 OTLP HTTP exporter（`codex-rs/otel/src/metrics/client.rs:717-729`）：
   - 受管網路政策下用 `RuntimeHttpClient`。
   - 有自訂 TLS 時，用 `block_in_place`（multi-thread runtime）或 spawn 後 join 一個執行緒，建立 blocking reqwest client（`codex-rs/otel/src/otlp.rs:74-92`）。
   - 其他情況由 SDK 自己建立 blocking reqwest client。
3. PeriodicReader 依 SDK 預設週期匯出。

#### 目的地與認證

- `POST https://ab.chatgpt.com/otlp/v1/metrics`（OTLP/HTTP JSON），以及使用者設定的 OTLP gRPC/HTTP 端點。
- 內建 exporter 用固定的 Statsig client key；自訂 exporter 用設定的 headers 與 mTLS。

#### 載荷內容與大小限制、壓縮、脫敏

- 指標的 counter 與 histogram。
- metrics resource 含 service.name／version、env、os、os_version（`codex-rs/otel/src/metrics/client.rs:361-372,601-616`）；host.name 只加在 log resource（`codex-rs/otel/src/provider.rs:395-399`）。
- 不壓縮（F10-1）。

#### 重試與錯誤處理

由 OTel SDK 的 periodic exporter 處理。opentelemetry 的 target 已從 log DB 排除（`codex-rs/state/src/log_db.rs:70-71`）。

#### 隱私說明

TUI 與 exec 的 release build 預設開啟，綁在 analytics 的 opt-out 上。

#### 已驗證的問題

- `[效能]` **F10-1 OTLP JSON 不壓縮，而且也無法開啟壓縮**（已確認）
  沒有編入 `gzip-http`、`zstd-http` feature（`codex-rs/otel/Cargo.toml:29-40`），所以即使設定 `OTEL_EXPORTER_OTLP_COMPRESSION=gzip` 也會在建置時失敗。
  位置：`codex-rs/otel/src/config.rs:24-32`
- `[效能]` **F10-2 啟動時同步建立 blocking reqwest client**（已確認）
  自訂 TLS 路徑用 `block_in_place` 或 spawn＋join。預設的 Statsig 路徑（沒有 tls、非受管網路）也有類似成本：opentelemetry-otlp 0.31.0 的 builder 會在 provider 初始化時，在 `MetricsClient::new` 內同步執行 `std::thread::spawn(|| reqwest::blocking::Client::builder()...build()).join()`。
  位置：`codex-rs/otel/src/otlp.rs:74-92`、`codex-rs/otel/src/metrics/client.rs:717-729`

---

### 3.11 流程 11：即時語音

#### 觸發

使用者啟動即時語音對話，從 TUI 或 app-server 的 realtime API 都可以。

#### 步驟

1. `create_with_session_and_headers` 先以 TRACE 記錄 SDP（`codex-rs/codex-api/src/endpoint/realtime_call.rs:135`），驗證 session 設定，建立 session JSON（`codex-rs/codex-api/src/endpoint/realtime_call.rs:168-170`）。
2. `/backend-api` 供應商收到 JSON body `{sdp, session}`；其他供應商收到 multipart/form-data body，含 sdp 與 session 兩部分（`codex-rs/codex-api/src/endpoint/realtime_call.rs:146-209`）。
3. POST 到 `realtime/calls`（FramelessBidi 用 `live`，`codex-rs/codex-api/src/endpoint/realtime_call.rs:63,72`）。V1，以及 `/backend-api` 供應商的 FramelessBidi，會加上 `intent=quicksilver&architecture=avas`（`codex-rs/codex-api/src/endpoint/realtime_call.rs:218-223`）。answer SDP 從回應 body 取得，`call_id` 從 `Location` header 取得（`codex-rs/codex-api/src/endpoint/realtime_call.rs:163-164,206-207`）。
4. 麥克風訊框放進容量 256 的有界佇列，佇列滿時丟棄並發出警告（`codex-rs/core/src/realtime_conversation.rs:101,931-934`）。
5. websocket 傳輸下，每個訊框以 JSON `input_audio_buffer.append` 送出（`codex-rs/codex-api/src/endpoint/realtime_websocket/methods.rs:362-371`），而且每個訊框都記錄兩次（見 F2-3）。

#### 目的地與認證

- `POST {provider base}/realtime/calls`（或 `live`）送 SDP 與 session。
- 音訊與文字事件透過 websocket 送往 realtime 端點。
- 認證使用供應商 auth（bearer 或 API key）。

#### 載荷內容與大小限制、壓縮、脫敏

- SDP offer 由用戶端提供（`codex-rs/app-server-protocol/src/protocol/v2/realtime.rs:288-291`），是 voice helper 在 ICE gathering 之後產生的（`codex-rs/realtime-webrtc/src/session.rs:24`）。
- session 設定（instructions、tools、voice）。
- 連續的 base64 PCM 麥克風音訊，以及對話文字。
- session JSON 由 serde_json 產生，CR/LF 都被跳脫，所以 instructions 無法注入 multipart 分隔符號。

#### 重試與錯誤處理

- 建立通話的 POST 遵循供應商的重試政策（`codex-rs/codex-api/src/endpoint/session.rs:92-101`）。
- 佇列滿時丟棄音訊訊框。

#### 隱私說明

音訊與 SDP 除了送往預期的 realtime 目的地，也會寫進 TRACE／DEBUG 日誌（F2-3、F2-4），並可能隨 include_logs 回饋上傳。任何 app-server 用戶端（包括已認證的遠端 websocket 用戶端）都能以 `includeLogs=true` 呼叫 `feedback/upload`。唯一的門檻是 `config.feedback_enabled`。

#### 已驗證的問題

- 音訊與 SDP 日誌問題見 F2-3、F2-4，這裡不重複計算。
- `[清理]` **F11-1 建立通話的請求在重試時可能重複建立通話**（已確認）
  request builder closure 每次嘗試都會 clone body（`Bytes::from(sdp.clone())`、`Bytes::from(body.clone())`），但 SDP 與 session JSON 只有幾 KB，可以忽略。比較值得注意的是：建立通話的 POST 套用供應商重試政策，所以重試時可能建立不只一通。
  位置：`codex-rs/codex-api/src/endpoint/realtime_call.rs:118,201`、`codex-rs/codex-api/src/endpoint/session.rs:92-101`

---

## 4. 與啟動速度相關的發現

以下只列出和上傳／外送路徑有關的啟動成本。本研究都沒有量測，只描述機制。

### 4.1 本分支已修正

- **OS 資訊探測集中為一次，並預熱。** 在 Linux 上，`os_info::get` 可能啟動 `lsb_release`、Python 腳本、`getconf` 等子行程（見 `codex-rs/otel/src/lib.rs:87-92` 的說明）。HEAD 原本在三處各自用 `LazyLock` 快取：analytics runtime metadata、User-Agent、OTel metrics resource。本分支把它們合併為 `codex_otel::os_info()`（`codex-rs/otel/src/lib.rs:93`），並用 `codex_otel::prewarm_os_info()`（`codex-rs/otel/src/lib.rs:100`）在背景執行緒預熱。
  - 預熱點：TUI `run_main`（`codex-rs/tui/src/lib.rs:1198`）與 app-server `run_main_with_transport_options`（`codex-rs/app-server/src/lib.rs:524`）。
  - 使用處：`codex-rs/analytics/src/events.rs:1578`、`codex-rs/login/src/auth/default_client.rs:156`、`codex-rs/otel/src/metrics/client.rs:604`。
  - 效果：每個程序只探測一次，而且不在關鍵路徑上。

- **ring buffer 批次淘汰（F2-2）。** `codex-rs/feedback/src/lib.rs` 的 `RingBuffer::push_bytes` 改用 `self.buf.drain(..to_drop)` 一次淘汰，並以切片 `extend` 追加；`snapshot_bytes` 直接複製 `as_slices()` 的兩段。保留的內容與原本逐位元組處理完全相同，並有單元測試比對多種寫入大小。

本分支其他的啟動修改（例如 daemon 遙測不再同步等待上報、SQLite 資料庫平行開啟、`/models` 請求合併）與上傳無關，詳見 `docs/research/startup-performance.zh-TW.md`。

### 4.2 尚未修正

| 發現 | 機制 | 和啟動的關係 | 位置 |
|---|---|---|---|
| F2-1 回饋 ring 與 SQLite 預設 TRACE | 每個 trace／debug 事件格式化兩次，與 `RUST_LOG` 無關；沒有編譯期 `max_level` | 啟動期間的所有 debug／trace 呼叫點都會付出格式化與鎖的成本 | `codex-rs/feedback/src/lib.rs:255-257`、`codex-rs/state/src/log_db.rs:61-63` |
| F9-1 分析事件每次請求都新建 HTTP 用戶端 | 沒有連線重用；建置要排隊等全域只有 1 個名額的 permit | 啟動時的 `/models` 請求用同一個 permit，可能被分析事件的建置延後 | `codex-rs/analytics/src/client.rs:1208-1214`、`codex-rs/login/src/auth/default_client.rs:55-56` |
| F10-2 OTel exporter 同步建立 blocking client | `MetricsClient::new` 內 spawn＋join，或 `block_in_place` | 在 provider 初始化（啟動）的路徑上 | `codex-rs/otel/src/otlp.rs:74-92`、`codex-rs/otel/src/metrics/client.rs:717-729` |
| F4-4 resume／fork 重新處理歷史圖片 | 每張圖 SHA-1 加完整解碼，在 async runtime 上依序進行 | resume／fork 的延遲隨歷史圖片數量增加 | `codex-rs/core/src/session/mod.rs:2002` |
| F8-1／F8-2 memories stage-1 | 第一個有輸入的 turn 就在背景啟動預檢與最多 2 個模型請求 | 和使用者的第一個 turn 搶頻寬與速率限制（只在 MemoryTool 開啟時） | `codex-rs/app-server/src/request_processors/turn_processor.rs:740-751` |
| F7-3 cloud TUI 環境偵測 | 阻塞的 git 子行程佔住 tokio worker；by-repo 查詢依序進行 | 在 spawned task 中執行，不擋首次繪製 | `codex-rs/cloud-tasks/src/env_detect.rs:60-87,225-270` |

另外，F1-13（開啟回饋同意視窗時在 UI 執行緒複製 4 MiB）影響的是 UI 回應速度，與啟動無關。

---

## 5. 建議的後續修正（依優先順序）

### P0 安全

1. **Codex Apps 檔案參數（F3-1）**：上傳前一律向使用者顯示將上傳的路徑並要求確認，即使工具本身屬於免核准類別也一樣；或者限制路徑只能在 workspace／可寫根目錄內，拒絕 cwd 以外的絕對路徑。
2. **代理憑證（F1-2）**：寫入 `codex-connectivity-diagnostics.txt` 之前，移除 URL 的 userinfo 與 query；TUI 顯示時做相同處理，並改為顯示 app-server 實際收集的內容。同時修改 `codex-rs/feedback/src/feedback_diagnostics.rs:99-120` 的測試斷言。

### P1 隱私

3. **`extraLogFiles`（F1-1）**：在伺服器端以 includeLogs 管控；要求絕對路徑，或限制在 `$CODEX_HOME/tmp` 之下。
4. **帳號 tag（F1-3、F1-4）**：每次上傳都以目前的 auth 重新決定 `account_id`／`chatgpt_user_id`，沿用 `codex-rs/feedback/src/report_upload.rs:115-120` 的 `retain` 過濾；並在同意畫面說明，回答「No」時 event 仍會送出哪些內容。
5. **同意清單（F1-5）**：補齊實際上傳的所有附件類別，並寫明會打包成 `rollouts.tar.gz`。
6. **即時語音日誌（F2-3、F2-4）**：讓 `RealtimeOutboundMessage` 的 `Debug` 遮蔽 `audio` 欄位；在 ring 與 SQLite 過濾器中排除 `codex_api::realtime_websocket::wire`。
7. **Plugin share（F5-1、F5-2）**：預設排除 `.git`、`.env*`、`node_modules`、`target`，並遵循 `.gitignore`；改用 `tar::HeaderMode::Deterministic`。
8. **Memories 遮蔽（F8-5）**：處理 `_` 前綴的環境變數名稱、`sk-proj-`／`sk-svcacct-`、JSON key 與跳脫後的引號；在 JSON 序列化前就做遮蔽；補上 GitHub token、JWT、PEM 等格式。
9. **cloud-tasks `error.log`（F7-1、F7-2、F7-4）**：改寫到 `$CODEX_HOME/log`，不再寫入完整回應 body，並改用非阻塞 I/O。
10. **圖片中繼資料（F4-7）**：提供移除 EXIF（至少 GPS）的選項或預設行為。

### P2 啟動與效能

11. ~~**ring 淘汰（F2-2）**~~：已在本分支修正。
12. **TRACE 預設（F2-1）**：先量測；再考慮把預設改為 DEBUG，或把高流量 target 加入排除清單。調整前要評估對診斷能力的影響。
13. **分析用戶端（F9-1、F9-4）**：依 route 快取並重用 HTTP 用戶端，避免和 `/models` 爭用全域 permit；考慮壓縮 body。
14. **OTel 初始化（F10-2）**：把 exporter 的 blocking client 建置移出啟動的關鍵路徑。
15. **圖片準備（F4-1 到 F4-6）**：讀檔前先檢查大小；CPU 工作移到 `spawn_blocking`；去掉 Defer 模式的 base64 往返；resume 時跳過已準備過的圖片；`UploadRequest.data` 改用 `Arc<[u8]>` 或 `Bytes`；影像生成的 inline 路徑直接沿用原本的 base64。
16. **回饋流程（F1-8 到 F1-11）**：doctor 與 history 並行執行；用反向掃描取得 turn metadata；避免對 tar.gz 再做一次 gzip。

### P3 可靠性

17. **Files API（F3-2 到 F3-7）**：create／finalize 遇暫時性錯誤時重試；blob PUT 把 500／408／429 納入可重試；finalize 輪詢加退避，並在每次請求前檢查剩餘預算；大檔改用 Put Block 分塊；上傳移到 catalog 鎖外，並納入 tool timeout；每次嘗試前重新確認檔案大小。
18. **回饋（F1-6、F1-7）**：確認 Sentry 的 event_id 去重語意後，考慮重試一次 event；小附件先送，或為每個附件分配各自的時間額度。
19. **Plugin share（F5-3 到 F5-5）**：依大小調整逾時並加入重試；失敗時清理；評估是否需要佔用全域 `config` 通道。
20. **Cloud worker（F6-1）**：讓 interrupt 不必排在 `AddUserMessage` 後面。
21. **分析事件（F9-2、F9-3）**：加入有上限的重試與批次處理。

### P4 清理

22. 修正過時的註解（`codex-rs/feedback/src/report_upload.rs:26-27`、`codex-rs/feedback/src/lib.rs:65`）；決定 report API 的去留（F1-15）。
23. 填入 `HostedFileUploadContext.thread_id`（F3-10）；把每個檔案重算的 auth 與 pool 提到迴圈外（F3-9）。
24. 確認建立即時通話的 POST 是否冪等，必要時對它停用重試（F11-1）。
25. embedded 模式不再重複上傳 `client-logs.txt`（F1-14）；memories 避免每個 turn 深複製 Config（F8-6）。

---

## 6. 未發現的項目與研究方法

### 6.1 未發現的項目

- **沒有把 rollout／thread 同步到雲端。** 唯一的兩個 `ThreadStore` 實作是 `LocalThreadStore`（`codex-rs/thread-store/src/local/mod.rs:518`）與 `InMemoryThreadStore`（`codex-rs/thread-store/src/in_memory.rs:923`）。Cloud 對話的資料本來就存在伺服器（流程 6）。
- **沒有遠端附件上傳。** 非測試的 `AttachmentStore` 只有 `InlineAttachmentStore`（`codex-rs/attachment-store/src/lib.rs:194`），其他實作都在測試裡（`codex-rs/core/src/image_preparation_tests.rs:31`、`codex-rs/core/tests/suite/image_rollout.rs:75`、`codex-rs/core/tests/suite/audio_duration_store.rs:22`）。
- **沒有其他 blob 上傳路徑。** `x-ms-blob-type` 只出現在 `codex-rs/codex-api/src/files.rs` 與 `codex-rs/core-plugins/src/remote/share.rs`（以及測試）。非測試的 multipart 只有 `codex-rs/codex-api/src/endpoint/realtime_call.rs`。
- **`feedback/save` 只在本機寫 ZIP**（`codex-rs/feedback/src/archive.rs:54-123`），不會上傳。

範圍外、未深入研究的其他外送：

- 模型的 Responses 請求本身。只有在 `enable_request_compression` 開啟、使用 Codex backend 認證、供應商是 OpenAI 時才用 zstd 壓縮（`codex-rs/core/src/client.rs:1937-1945`）。
- message-board 用戶端，送往呼叫端提供的 HTTP(S) endpoint（`codex-rs/agent-message-board-client/src/client.rs:70-84,143,165,298`）。
- `codex-rs/network-proxy/src/aws_sigv4.rs`：網路代理替 workload 的 AWS 請求簽章，不是 Codex 自己的上傳。

### 6.2 研究方法與限制

- **方法**：先對 `codex-rs/` 做靜態普查，列出 11 條流程與問題；再逐條做對抗式驗證，每條都回到程式碼找證據，判定為已確認、部分確認或否定。本文只採用已確認與更正後的敘述。有 1 項說法被否定（即時語音 multipart 分隔符號可被 instructions 注入），已刪除。
- **只做靜態分析**：沒有執行程式、沒有量測啟動時間、沒有抓網路封包。TRACE 擷取、client 建置、OTel 初始化的實際耗時都未量測。
- **伺服器端行為無法驗證**：Sentry 以 event_id 去重、OpenAI Files 或 plugin upload 失敗後是否留下孤立記錄、Cloud 後端去重語意，都不在用戶端程式碼的可驗證範圍內。
- **外部依賴**：hyper／reqwest 在 `Content-Length` 不符時的行為、opentelemetry-otlp 0.31.0 的內部實作、WebRTC stack 產生的 SDP 內容，本文只引用驗證時看到的部分。
- **行號**：以本分支的啟動速度提交為準。後續的測試移動與模組拆分會改變部分檔案的行號，引用前請以函式名稱再確認。
- **未驗證的數字**：「每秒 50 個音訊訊框」取決於用戶端；回饋流程缺少進度 UI 一事未驗證。
