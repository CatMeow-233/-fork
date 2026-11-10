# Codex CLI 為什麼這麼大，以及這次做的壓縮

- 範圍：`codex` 主程式（crate `codex-cli`）的體積、從原始碼建置的成本，以及安裝後的磁碟佔用。
- 基準：本分支在這次修改之前的提交 `91ccf6c`。與上游相比，那時只有啟動速度與模組拆分的修改，體積與上游一致。
- 環境：x86_64 Linux（glibc），4 核心、15 GB 記憶體，Rust 1.95.0。

## 1. 結論

| 項目 | 修改前 | 修改後 |
|---|---|---|
| `codex` 主程式（strip 後，也就是實際發佈的大小） | 320.5 MB | **201.3 MB（-37%）** |
| 其中機器碼 `.text` | 250.6 MB | 140.4 MB（-44%） |
| 直接執行 `cargo build --release` 得到的檔案 | 約 2 GB（估算，見 3.1） | 約 201 MB |
| 完整 release 建置（這台機器，詳見第 5 節） | 沒能一次跑完，關閉除錯資訊後約 57 分鐘 | 37 分鐘 |
| 第一次啟動 daemon（複製安裝包） | 1.37–1.51 秒 | 0.73–0.81 秒 |

修改前的 320.5 MB 與上游 npm 套件 `@openai/codex@0.167.0-linux-x64` 裡的 `bin/codex`（320.6 MB）一致，所以這個基準可以代表上游。

效能代價（release 版，同一台機器，數字是多輪的中位數範圍）：

| 情境 | 修改前 | 修改後 |
|---|---|---|
| 啟動到第一個模型請求（嵌入模式） | 0.413–0.425 秒 | 0.412–0.413 秒 |
| 啟動到第一個模型請求（daemon 已在執行） | 0.410–0.427 秒 | 0.422–0.444 秒 |
| 收到 300 KB Markdown 回覆到畫面顯示完成 | 2.31–2.54 秒 | 2.46–2.53 秒 |
| 上一列的行程 CPU 時間 | 3.37–3.72 秒 | 2.77–3.59 秒 |

嵌入模式的啟動沒有可量測的差別；daemon 模式慢約 10–20 毫秒；渲染長回覆在量測誤差內，最差的一輪慢約 9%。量測腳本在輸入框出現後固定等 0.3 秒才送出，所以這些啟動數字都包含那 0.3 秒。

## 2. 量測方法

- 建置：`cargo build --release -p codex-cli --bin codex`，thin LTO，與發佈流程相同。量測用的建置關閉了除錯資訊並保留符號表（`CARGO_PROFILE_RELEASE_DEBUG=0`、`CARGO_PROFILE_RELEASE_STRIP=none`），再用發佈流程同樣的 `strip --strip-debug --strip-unneeded` 得到發佈大小。除錯資訊不影響產生的機器碼，所以 strip 後的結果與正式建置相同。
- 拆解：`scripts/binary_size_report.py`（本次新增）用 `size` 與 `nm` 把大小歸到各區段與各 crate，並列出同一路徑出現多份的函式。
- 效能：`scripts/startup_bench.py`。本次加了 `--reply-kb`（讓模擬伺服器回覆一大段含程式碼區塊、表格與清單的 Markdown）與行程 CPU 時間。

## 3. 為什麼這麼大

### 3.1 本機 release 建置帶著除錯資訊，而且沒有 strip

修改前的 release profile 是 `debug = "line-tables-only"`、`strip = false`。發佈流程會在打包時另外 strip，所以官方套件不受影響；但自己執行 `cargo build --release` 得到的檔案帶著全部行號表。

- 我沒有等到這個建置完成：最後的 LTO 步驟在單執行緒下跑了 65 分鐘仍未結束。當時已產生的 20 個最大的目的檔就有 1.72 GB。
- 另外把其中兩個模組單獨連結：未 strip 是 173.2 MB，strip 後是 24.8 MB，約 7 倍。按這個比例，完整的檔案約 2 GB。這是估算，不是實測。
- 除錯資訊也讓建置本身很重：三個大 crate 同時編譯時各佔 3–5.7 GB，最後的 LTO 步驟佔 7.5 GB。

### 3.2 發佈的 320 MB 裡，八成是「自己的程式碼被編譯了很多份」

按符號大小歸類（修改前，合計 261 MB）：

| 類別 | 修改前 MB | 修改後 MB | 變化 |
|---|---|---|---|
| 工作區自身 crate（`codex_*`） | 97.7 | 59.0 | -40% |
| serde／serde_json／toml 的泛型實例 | 54.8 | 24.8 | -55% |
| 標準庫泛型（drop、Vec、HashMap 等） | 26.4 | 16.6 | -37% |
| tokio／futures／hyper／rmcp 等非同步執行環境的泛型 | 33.0 | 14.9 | -55% |
| C、組合語言與匿名靜態資料（aws-lc、SQLite、zstd、tree-sitter、內嵌檔案） | 12.6 | 11.5 | -9% |
| 其餘約 700 個第三方 crate | 36.5 | 24.1 | -34% |
| 合計 | 261.0 | 150.9 | -42% |

前四類加起來佔 81%，而且都不是「第三方函式庫本身很大」：serde、tokio 這些 crate 的大小來自它們為工作區型別產生的實例（每個可反序列化的型別一份解析程式碼，每個被 spawn 的 future 一份任務外殼）。

真正的原因在 rustc 的編譯模型：

1. `async fn` 的函式本體不在定義它的 crate 裡產生機器碼，而是在「驅動」它的地方（`tokio::spawn`、`block_on`）產生，並且沿著 await 鏈一路往下。`codex-core` 的一個 async 函式，如果分別被 `codex-app-server`、`codex-exec`、`codex-tui`、`codex-cli` 裡的 future 等待，就會被編譯四份。
2. 泛型也一樣：每個使用到 `serde_json::from_str::<T>` 的 crate 各自產生一份。
3. rustc 只有在偏向體積的最佳化等級（`s`、`z`，以及 0、1）才會讓下游 crate 重用上游已經產生的實例；在 `-O2`、`-O3` 下每個 crate 各產生各的。修改前整個工作區都是 `-O3`。

佐證：修改前的二進位裡，「路徑相同、只有雜湊不同」的函式副本，扣掉每組最大的一份後還有約 170 MB（這個數字是上限，因為不同型別參數的實例也共用同一個路徑）。例如 `ContentDeserializer::deserialize_struct` 有 879 份、`tokio::runtime::task::harness::Harness::poll` 有 1,732 份、`ConfigToml` 的 TOML 反序列化程式碼在 10 個 crate 裡各有一份。

另外，binary crate 被分成 4 個程式碼產生單元，被內聯的小函式會在每個單元各放一份。

### 3.3 第三方依賴不是主因

依賴樹很深（`codex` 的依賴閉包有 1,039 個 crate，工作區自身 173 個），但逐一看都不大。修改前按符號歸類，佔用最多的第三方部分：

| 依賴 | 佔用 MB | 用途 |
|---|---|---|
| rama HTTP 堆疊 | 3.1 | 沙箱網路代理 |
| zbus + zvariant | 2.7 | Linux 鑰匙圈與桌面設定 |
| AWS SDK（aws-config、aws-smithy 等） | 2.5 | Bedrock 認證 |
| tree-sitter 與 bash／PowerShell 語法表 | 2.3 | 解析 shell 指令 |
| starlark | 2.2 | 執行政策規則語言 |
| sqlx，另加內嵌 SQLite 至少 1.2 | 2.1 | 狀態與日誌資料庫 |
| image 與各格式解碼器 | 1.7 | 圖片附件 |
| OpenTelemetry + tonic + prost | 1.6 | 遙測匯出 |
| h2、rustls | 1.5、1.2 | HTTP/2 與 TLS |
| aws-lc（有大小資訊的符號） | 至少 1.5 | TLS 密碼學 |
| regex 系列 | 1.0 | |
| 語法高亮定義（內嵌資料） | 1.0 | 約 180 種語言 |
| gix、zstd、clap | 各 0.7–0.8 | |
| quinn + h3（QUIC／HTTP/3） | 0.6 | 只有隱藏的 `codex tcp-tunnel` 用到 |
| 內嵌的 `models.json` | 0.5 | 模型目錄 |
| symphonia、hickory | 0.5、0.4 | 音訊長度探測、DNS |

C 函式庫裡沒有符號大小資訊的靜態函式另有約 2 MB 無法歸到個別函式庫。內嵌的提示詞、範本與 schema 合計 1.3 MB。

### 3.4 其餘結構性成本

- **展開（unwind）表**：`.eh_frame`、`.eh_frame_hdr`、`.gcc_except_table` 修改前共 41 MB，修改後 31 MB。程式用到 `catch_unwind` 與 tokio 的 panic 隔離，不能改成 `panic = "abort"`。
- **重定位表**：`.rela.dyn` 約 8–10 MB。官方 x86_64 musl 建置在連結時用 `-z pack-relative-relocs` 把它壓到很小，本機 glibc 建置沒有（那個選項需要 glibc 2.36 以上才能執行，不適合當預設）。

### 3.5 安裝後不只一份

上游 0.167.0 的 linux-x64 套件解開是 459 MB：`codex` 320.6 MB、`codex-code-mode-host`（含 V8）79.2 MB、語音執行環境 52.9 MB、`rg` 5.4 MB、`bwrap` 0.5 MB。再加上：

- 預設啟用的背景 daemon 會在第一次啟動時把整個套件**複製一份**到 `CODEX_HOME/packages/app-server-daemon/releases/`，並保留目前與前一個版本。
- 獨立安裝器（`install.sh`）同樣保留目前與前一個版本；Windows 的 `install.ps1` 不清理舊版本。
- 用 `scripts/build_codex_package.py` 做的獨立套件還包含 `bin/codex-exec-server`。它與 `codex` 共用 936 個（共 1,039 個）依賴 crate，幾乎是第二份主程式。npm 套件已經不含它。

所以一位用獨立安裝器並開著 daemon 的使用者，磁碟上最多有四份完整套件。

## 4. 這次做了什麼

### 4.1 工作區 crate 改用 `opt-level = "s"`，第三方依賴維持 `-O3`

`codex-rs/Cargo.toml`：

```toml
[profile.release]
opt-level = "s"

[profile.release.package."*"]
opt-level = 3
```

- 這是最大的一項：strip 後 320.5 MB → 206.7 MB。
- 效果來自兩方面：體積導向的程式碼產生，以及 3.2 節說的跨 crate 重用實例。
- 熱點迴圈（正規表示式、JSON 解析、TLS、壓縮、終端繪製、語法高亮）都在第三方 crate 裡，維持 `-O3`。工作區自身的程式碼主要是非同步協調與資料轉換。
- 副作用：`"*"` 這個覆寫也套用到第三方的建置腳本與程序巨集，它們現在以 `-O3` 編譯（以前是 `-O0`）。這讓巨集展開變快、巨集本身編譯變慢，對總建置時間的淨影響我沒有單獨量測。

### 4.2 binary crate 用單一程式碼產生單元，內嵌 SQLite 精簡

- `[profile.release.package.codex-cli] codegen-units = 1`：約 -4.3 MB。
- `[profile.release.package.libsqlite3-sys] opt-level = "s"`，並在 `codex-rs/.cargo/config.toml` 用 `LIBSQLITE3_FLAGS` 關掉沒有任何 schema 或查詢用到的 FTS3、FTS5、R-Tree、dbstat、soundex：約 -1.1 MB。JSON 函式保留。Windows x64 的發佈建置自己設定了 `LIBSQLITE3_FLAGS`（停用 SQLite 的 intrinsic），環境變數優先於 `config.toml`，所以那個目標沒有套用這項精簡。
- 合計 206.7 MB → 201.3 MB。

### 4.3 release profile 預設不帶除錯資訊並 strip

`debug = false`、`strip = "symbols"`。`cargo build --release` 直接得到與發佈相同大小的檔案，建置也快很多、佔用記憶體少很多。

發佈流程需要符號檔（Linux 的 `.debug`、macOS 的 dSYM、Windows 的 PDB，缺 PDB 時流程會直接失敗），所以三個用 release profile 建置並封存符號的地方都加了 `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` 與 `CARGO_PROFILE_RELEASE_STRIP=none`，維持原本「先封存符號、再 strip」的流程：

- `.github/workflows/rust-release.yml` 的 `build` job；
- `.github/workflows/rust-release-windows.yml` 的 `build-windows-binaries` job；
- `.github/actions/build-cargo-package-binaries/action.yml`（套件建置用的複合動作）。

這些流程我無法在這裡執行，只確認了 YAML 可以解析、變數落在建置步驟之前。`scripts/codex_package/README.md` 裡「Cargo 產物保留符號」的說明也一併更新。

代價：本機 release 建置的 backtrace 只有位址，沒有函式名稱與行號（panic 訊息本身仍有檔案與行號）。需要符號時用 `CARGO_PROFILE_RELEASE_STRIP=none`，或既有的 `profiling` profile。

### 4.4 Linux 上 daemon 安裝包改用核心的檔案複製

`app-server-daemon/src/prepare_install.rs` 原本把每個檔案整個讀進記憶體再寫出去。在 Linux 上現在用 `std::fs::copy`（核心的 `copy_file_range`），再對複製出來的檔案做串流雜湊：

- 在支援寫入時複製的檔案系統上（btrfs、開啟 reflink 的 XFS），daemon 的那份套件與來源共用磁碟區塊，不再多佔一份空間。ext4 上磁碟佔用不變。
- 不再把數百 MB 的執行檔整個放進記憶體。
- 完整性檢查不變：雜湊的是實際落地的檔案，之後仍與來源再比對一次。
- 第一次啟動（含複製）在這台機器上從 1.37–1.51 秒降到 0.73–0.81 秒，其中一部分來自套件本身變小。
- macOS 與 Windows 維持原本的讀入再寫出。那兩個平台的系統複製會連同隔離標記（quarantine）、替代資料流等屬性一起帶過去，daemon 的套件不應該繼承它們，所以沒有改；APFS 的寫入時複製因此沒有用上。

### 4.5 工具與文件

- `scripts/binary_size_report.py`：體積拆解與兩次建置的對比。
- `scripts/startup_bench.py`：新增 `--reply-kb` 與 CPU 時間。

## 5. 驗證

- 修改前後各做了完整的 release 建置並 strip；上表的大小都是實測。
- 效能數字見第 1 節，每個情境交替執行兩到三輪、每輪 4–7 次。
- 建置時間：修改前帶除錯資訊時，4 個並行編譯工作在編譯 `codex_tui` 與 `codex_core` 時因記憶體不足被系統終止（當時機器上還有其他分析行程在跑）；改成單一工作後，最後的 LTO 步驟跑了 65 分鐘仍未結束，我中止了它。關閉除錯資訊、用 3 個並行工作重做，分三段共約 57 分鐘，其間可用記憶體一度剩 2 GB，我手動停掉其中一個編譯工作後才繼續。修改後同樣 3 個並行工作，從頭建置一次完成，36 分 36 秒。
- 另外用一個小的工作區二進位（`codex-stdio-to-uds`）以 Cargo.toml 的原樣設定建置，確認產物沒有符號表與除錯區段，而且可以執行。
- daemon 模式用修改後的 release 二進位實際跑過（第一次安裝與之後的啟動）。
- `codex-app-server-daemon` 的 85 個測試通過，`cargo clippy --tests -- -D warnings` 無警告。
- 沒有改動任何依賴，`Cargo.lock` 與 `MODULE.bazel.lock` 不變。
- 沒有驗證的部分：macOS 與 Windows 的建置、官方發佈工作流程、Bazel 建置（Bazel 的最佳化設定在 `.bazelrc`，這次沒有同步修改；目前發佈走 Cargo）。

## 6. 沒有做的項目

以下是調查中確認過、但這次沒有動的項目。節省量除特別註明外都是估計值。

### 6.1 會改變行為，需要你決定

| 項目 | 估計節省 | 代價 |
|---|---|---|
| Linux 上不再連結 OpenSSL（改由 rustls 處理所有 HTTPS） | 3.5–5 MB（只影響官方 musl 建置） | 只支援舊版 TLS 的代理或伺服器會連不上 |
| 把隱藏的 `codex tcp-tunnel` 做成可關閉的功能 | 1–1.5 MB | 失去該子命令 |
| Linux 鑰匙圈與桌面設定探測做成可關閉的功能 | 0.7–1 MB | 憑證只能存檔案 |
| 語法高亮只保留常用語言 | 0.3–0.6 MB | 冷門語言不再上色 |
| daemon 與安裝器只保留一個版本 | 每個根目錄一整份套件 | 失去磁碟上的前一版備援 |
| 獨立套件不再附 `codex-exec-server` | 未量測，見 6.3 | 直接呼叫該路徑的工具要改用 `codex exec-server` |
| 預設開啟 rollout 壓縮 | 視 `sessions/` 大小而定 | 該功能目前標示為開發中 |

### 6.2 不改變行為，但每項都小

| 項目 | 估計節省 |
|---|---|
| 重構成 `dyn Future` 邊界，讓大型 async 入口只編譯一份 | 最多數 MB，需要逐一量測 |
| `zip` 只保留 deflate | 0.35–0.55 MB |
| `aws-config` 關掉沒用到的預設 HTTPS 用戶端 | 0.2–1 MB |
| 工具搜尋的分詞器只保留英文 | 0.4–0.9 MB |
| 音訊長度探測不經 symphonia | 0.6 MB |
| `models.json` 以 zstd 壓縮後內嵌 | 0.5 MB |
| `color-eyre` 換成 `eyre` | 0.3–0.4 MB（panic 報告格式會變） |
| 合併 zlib-rs、quick-xml 的重複版本 | 0.1–0.25 MB |

這些加起來大約 3–5 MB，相對於 201 MB 不到 3%，而且多數會改動 `Cargo.lock`（之後要重新產生 `MODULE.bazel.lock`，這裡沒有 Bazel 可用）。

### 6.3 `codex-exec-server`

獨立套件裡的 `bin/codex-exec-server` 我沒有量到大小：它的功能組合與 `codex` 不同，建置時要重新編譯大部分 crate，這台機器的磁碟不夠，我中止了建置。它與 `codex` 共用九成的依賴，合理的預期是與主程式同一個量級，但這是推測。
