# Codex CLI 為什麼這麼大，以及這次做的壓縮

- 範圍：`codex` 主程式（crate `codex-cli`）的體積、從原始碼建置的成本，以及安裝後的磁碟佔用。
- 基準：本分支在這次修改之前的提交 `91ccf6c`。與上游相比，那時只有啟動速度與模組拆分的修改，體積與上游一致。
- 環境：x86_64 Linux（glibc），4 核心、15 GB 記憶體，Rust 1.95.0。

## 1. 結論

| 項目 | 修改前 | 修改後 |
|---|---|---|
| `codex` 主程式（strip 後，也就是實際發佈的大小） | 320.5 MB | **221.0 MB（-31%）** |
| 其中機器碼 `.text` | 250.6 MB | 160.1 MB（-36%） |
| 直接執行 `cargo build --release` 得到的檔案 | 約 2 GB（估算，見 3.1） | 約 221 MB |
| 完整 release 建置（這台機器，詳見第 5 節） | 沒能一次跑完，關閉除錯資訊後分三段共約 54 分鐘 | 45 分 51 秒，一次完成 |
| 第一次啟動 daemon（複製安裝包） | 1.22–1.35 秒 | 0.70–0.84 秒 |

這些數字是本機 glibc 建置的結果。修改前的 320.5 MB 與上游 npm 套件 `@openai/codex@0.167.0-linux-x64` 裡的 `bin/codex`（320.6 MB）幾乎相同，但官方的 x86_64 Linux 版是 musl 目標，並且用 lld 連結、開啟 `--icf=safe`（合併內容相同的函式）與壓縮重定位表。這些連結選項會改變絕對大小，所以 -31% 這個比例套到官方產物上會是多少，我沒有量。

效能代價（release 版，同一台機器，三輪交替執行、每輪 5 次，數字是各輪中位數的範圍）：

| 情境 | 修改前 | 修改後 |
|---|---|---|
| 啟動到第一個模型請求（嵌入模式） | 0.408–0.417 秒 | 0.413–0.424 秒 |
| 啟動到第一個模型請求（daemon 已在執行） | 0.400–0.418 秒 | 0.398–0.412 秒 |
| 從啟動到 300 KB Markdown 回覆顯示完成 | 2.16–2.24 秒 | 2.31–2.60 秒 |
| 上一列扣掉「啟動到第一個請求」之後的部分 | 1.75–1.81 秒 | 1.90–2.18 秒 |
| 回覆情境的行程 CPU 時間 | 2.36–3.32 秒 | 3.38–3.67 秒 |

- 啟動沒有可量測的差別：嵌入模式的中位數差約 5 毫秒，在各輪自己的波動範圍內。量測腳本在輸入框出現後固定等 0.3 秒才送出，所以啟動數字都包含那 0.3 秒。
- 顯示 300 KB 長回覆變慢了：三輪都是修改後較慢，各輪中位數相差 7%、9%、20%，CPU 時間多約 8%。另一次把中間版本一起比的量測裡，同一情境是 +9%、+10%、-4%。六輪合起來看，中位數約慢 9%。我沒有做 profile；推測這段時間花在工作區自己的 Markdown 處理與終端繪製上，這些程式碼現在以體積為目標編譯。300 KB 是刻意放大的壓力情境，一般的回覆是幾 KB。

還有兩條路可以更小，這次都沒有走：

- 201.3 MB（-37%）的設定，代價是第三方函式庫的熱點迴圈失去向量化。原因與切換方法見 4.1。
- 把 `codex` 的 `main` 搬到一個只含入口的小套件，預期可以在不犧牲第三方最佳化的前提下拿回 221 MB 與 201 MB 之間差距的大部分。這是結構性修改，見 6.1。

## 2. 量測方法

- 建置：`cargo build --release -p codex-cli --bin codex`，thin LTO。量測用的建置關閉了除錯資訊並保留符號表（`CARGO_PROFILE_RELEASE_DEBUG=0`、`CARGO_PROFILE_RELEASE_STRIP=none`），再用發佈流程同樣的 `strip --strip-debug --strip-unneeded` 得到發佈大小。除錯資訊不影響產生的機器碼，所以 strip 後的結果與帶除錯資訊的建置相同。與官方發佈流程的差別是目標（glibc 而不是 musl）與連結器選項，見第 1 節。
- 拆解：`scripts/binary_size_report.py`（本次新增）用 `size` 與 `nm` 把大小歸到各區段與各 crate，並列出同一路徑出現多份的函式。
- 效能：`scripts/startup_bench.py`。本次加了 `--reply-kb`（讓模擬伺服器回覆一大段含程式碼區塊、表格與清單的 Markdown）與行程 CPU 時間。
- Cargo 對每個編譯單元實際套用的設定，用 `cargo build --unit-graph` 列出（不需要編譯），用來確認 profile 的覆寫與 CI 的參數真的生效。

## 3. 為什麼這麼大

### 3.1 本機 release 建置帶著除錯資訊，而且沒有 strip

修改前的 release profile 是 `debug = "line-tables-only"`、`strip = false`。發佈流程會在打包時另外 strip，所以官方套件不受影響；但自己執行 `cargo build --release` 得到的檔案帶著全部行號表與符號表。

- 我沒有等到這個建置完成：最後的 LTO 步驟在單執行緒下跑了 65 分鐘仍未結束。當時已產生的 20 個最大的目的檔就有 1.72 GB。
- 另外把其中兩個模組單獨連結：未 strip 是 173.2 MB，strip 後是 24.8 MB，約 7 倍。按這個比例，完整的檔案約 2 GB。這是估算，不是實測；這個倍數裡也包含符號表（不帶除錯資訊的完整建置，符號表本身是 87 MB）。
- 除錯資訊也讓建置本身很重：三個大 crate 同時編譯時各佔 3–5.7 GB，最後的 LTO 步驟佔 7.5 GB。

### 3.2 符號位元組的八成，是工作區程式碼與為它產生的泛型實例

按符號大小歸類（修改前合計 261 MB，是 320.5 MB 檔案裡有符號可歸屬的部分）：

| 類別 | 修改前 MB | 修改後 MB | 變化 |
|---|---|---|---|
| 工作區自身 crate（`codex_*`） | 97.7 | 63.7 | -35% |
| serde 系列（serde、serde_json、toml、schemars 等）的泛型實例 | 54.8 | 30.3 | -45% |
| 標準庫泛型（drop、Vec、HashMap、格式化等） | 30.2 | 19.5 | -35% |
| 非同步執行環境（tokio、futures、hyper、h2、tower、axum、reqwest、tonic、rmcp）的泛型 | 31.1 | 17.7 | -43% |
| C、組合語言與匿名靜態資料（aws-lc、SQLite、zstd、tree-sitter、內嵌檔案） | 12.6 | 11.4 | -10% |
| 其餘約 450 個第三方 crate | 34.6 | 27.7 | -20% |
| 合計 | 261.0 | 170.3 | -35% |

前四類加起來是 213.8 MB，佔符號位元組的 82%（佔整個檔案的 67%）。它們都不是「第三方函式庫本身很大」：serde、tokio 這些 crate 的大小來自它們為工作區型別產生的實例（每個可反序列化的型別一份解析程式碼，每個被 spawn 的 future 一份任務外殼）。

這麼多實例的原因在 rustc 的編譯模型：

1. `async fn` 的函式本體在每個「驅動」它的 crate 裡各產生一份機器碼（`tokio::spawn`、`block_on`，並沿著 await 鏈一路往下）。公開的 `async fn` 在定義它的 crate 裡也會產生一份，但下游 crate 只有在第 3 點的條件成立時才能重用它。`codex-core` 的一個 async 函式，如果分別被 `codex-app-server`、`codex-exec`、`codex-tui`、`codex-cli` 裡的 future 等待，就可能被編譯四份。
2. 泛型也一樣：每個使用到 `serde_json::from_str::<T>` 的 crate 各自產生一份。
3. rustc 只有在偏向體積的最佳化等級（`s`、`z`，以及 0、1）才會讓下游 crate 重用上游已經產生的實例；在 `-O2`、`-O3` 下每個 crate 各產生各的。修改前整個工作區都是 `-O3`。

重複的程度：修改前的二進位裡，「路徑相同、只有雜湊不同」的函式副本，扣掉每組最大的一份後有 171 MB（函式合計 253 MB）。這是上限，不是重複量的實測，因為不同型別參數的實例也共用同一個路徑。例如 `ContentDeserializer::deserialize_struct` 有 879 份、`tokio::runtime::task::harness::Harness::poll` 有 1,732 份、`ConfigToml` 的 TOML 反序列化程式碼在 10 個 crate 裡各有一份。修改後這個數字是 93 MB（函式合計 162 MB）。

另外，binary crate 被分成 4 個程式碼產生單元，被內聯的小函式會在每個單元各放一份。

### 3.3 第三方依賴不是主因

依賴樹很深（`codex` 的依賴閉包有 1,039 個 crate，其中工作區自身 152 個；整個工作區共 173 個），但逐一看都不大。修改前按符號歸類，佔用最多的第三方部分：

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

- **展開（unwind）表**：`.eh_frame`、`.eh_frame_hdr`、`.gcc_except_table` 修改前共 41 MB，修改後 32 MB。程式用到 `catch_unwind` 與 tokio 的 panic 隔離，不能改成 `panic = "abort"`。
- **重定位表**：`.rela.dyn` 約 8–10 MB。官方 x86_64 musl 建置在連結時用 `-z pack-relative-relocs` 把它壓到很小，並用 `--icf=safe` 合併相同的函式；本機 glibc 建置兩者都沒有（前者需要 glibc 2.36 以上才能執行，不適合當預設）。

### 3.5 安裝後不只一份

上游 0.167.0 的 linux-x64 套件解開是 459 MB：`codex` 320.6 MB、`codex-code-mode-host`（含 V8）79.2 MB、語音執行環境 52.9 MB、`rg` 5.4 MB、`bwrap` 0.5 MB。再加上：

- 預設啟用的背景 daemon 會在第一次啟動時把整個套件**複製一份**到 `CODEX_HOME/packages/app-server-daemon/releases/`，並保留目前與前一個版本。
- 獨立安裝器（`install.sh`）同樣保留目前與前一個版本；Windows 的 `install.ps1` 不清理舊版本。
- 用 `scripts/build_codex_package.py` 做的獨立套件還包含 `bin/codex-exec-server`。它與 `codex` 共用 936 個（共 1,039 個）依賴 crate，幾乎是第二份主程式。npm 套件已經不含它。

所以一位用獨立安裝器並開著 daemon 的使用者，磁碟上最多有四份完整套件。

## 4. 這次做了什麼

### 4.1 工作區 crate 改用 `opt-level = "s"`，第三方依賴與 binary crate 維持 `-O3`

`codex-rs/Cargo.toml`：

```toml
[profile.release]
opt-level = "s"

[profile.release.package."*"]      # 所有非工作區成員
opt-level = 3

[profile.release.package.codex-cli] # 連結出 codex 的那個套件
opt-level = 3
codegen-units = 1
strip = "symbols"
```

- 這是最大的一項。效果來自兩方面：體積導向的程式碼產生，以及 3.2 節說的跨 crate 重用實例。工作區自身的程式碼主要是非同步協調與資料轉換；熱點迴圈（正規表示式、JSON 解析、TLS、壓縮、圖片編解碼、語法高亮）在第三方 crate 裡。
- **binary crate 必須也是 `-O3`，第三方依賴的 `-O3` 才算數。** 在 thin LTO 下，rustc 先讓每個 crate 各自做到「連結前」階段，向量化與迴圈展開留到連結後；而連結後的最佳化用的是「被連結的那個 crate」的設定。binary crate 若是 `s`，所有依賴都拿不到向量化。我一開始漏了這點，是審查時從反組譯發現的：
  - 在 binary crate 為 `s` 的版本裡，`png::filter::unfilter` 用到 SIMD 暫存器的指令從 407 個變成 0 個。
  - 比較三個版本裡各只出現一次的 5,584 個非泛型第三方函式：binary crate 為 `s` 時總大小是修改前的 83%，只有 29% 的函式大小不變；改成 `-O3` 後是 98%，96% 的函式大小與修改前完全相同。
  - 工作區的函式庫 crate 仍以體積為目標：它們的函式帶著「為體積最佳化」的標記，連結後階段照這個標記處理。不過連結後的內聯在 `-O3` 下積極一些，工作區與它的泛型實例因此也大了一些（數字見下面的拆解）。
- 逐步的大小（strip 後）：只改 `opt-level` 時 206.7 MB；加上 4.2 的兩項後 201.3 MB；再把 `codex-cli` 設回 `-O3` 後 221.0 MB。
- 最後這一步多出的 19.7 MB 可以拆成兩部分（比較兩個版本的符號）：
  - 兩邊都有的同一份實例，合計從 129.9 MB 變成 134.7 MB，多 4.8 MB。這是連結後改用 `-O3` 的直接代價，包含第三方的向量化。
  - 只出現在其中一邊的符號：`s` 版 21.1 MB，`-O3` 版 35.6 MB，淨增 14.5 MB。原因是 `codex-cli` 這個套件在 `-O3` 下不再重用上游 crate 共用的實例（見 3.2 的第 3 點），而是把它驅動到的非同步程式碼與泛型自己再編譯一份，而且不帶體積最佳化。那 35.6 MB 裡有 16.8 MB 是 async 函式本體與閉包，14.9 MB 是泛型實例；按來源看最多的是 `codex_app_server`、`codex_tui`、serde、tokio。
  - 也就是說，221 MB 裡大約 15 MB 不是「第三方最佳化的代價」，而是「連結用的套件太大」的代價。6.1 節列了消除它的作法。
- **想要 201 MB 的版本**：刪掉 `[profile.release.package.codex-cli]` 裡的 `opt-level = 3` 一行即可。代價是上面說的第三方迴圈不再向量化、不再展開。我量得到的兩個情境（啟動、顯示長回覆）在兩種設定下沒有明顯差別，但圖片處理、純 Rust 的壓縮與雜湊、差異比對這些路徑我沒有基準測試，所以預設選了不改變第三方程式碼的設定。
- 副作用：`"*"` 這個覆寫也套用到第三方的建置腳本與程序巨集，它們現在以 `-O3` 編譯（以前是 `-O0`）。這讓巨集展開變快、巨集本身編譯變慢，對總建置時間的淨影響我沒有單獨量測。
- 限制：其他由工作區套件連結出來的執行檔（`codex-app-server`、`codex-exec-server`、`codex-code-mode-host`、`codex-responses-api-proxy`、Windows 的輔助程式）所屬的套件同時含有大型函式庫，Cargo 的覆寫只能以套件為單位，所以它們仍以 `s` 連結，裡面的第三方程式碼沒有向量化；修改前它們全部是 `-O3`。對執行速度的影響我沒有量。改善的方法同樣是 6.1 的小套件。
- 工作區唯一的 `cargo bench`（`codex-utils-image` 的圖片基準測試）沿用 release 設定，原本也會以 `s` 連結、量到沒有向量化的編解碼器。`[profile.bench.package.codex-utils-image] opt-level = 3` 讓它與 `codex` 一樣以 `-O3` 連結。

### 4.2 binary crate 用單一程式碼產生單元，內嵌 SQLite 精簡

- `[profile.release.package.codex-cli] codegen-units = 1`：約 -4.3 MB（在 binary crate 為 `s` 時量的）。
- `[profile.release.package.libsqlite3-sys] opt-level = "s"`，並在 `codex-rs/.cargo/config.toml` 用 `LIBSQLITE3_FLAGS` 關掉沒有任何 schema 或查詢用到的 FTS3、FTS5、R-Tree、dbstat、soundex：約 -1.1 MB。JSON 函式保留。
- Windows x64 的發佈建置原本就自己設定 `LIBSQLITE3_FLAGS`（停用 SQLite 的 intrinsic），環境變數會取代 `config.toml` 的值，所以那兩處 CI 腳本現在把同一串旗標重複寫上。MSVC 編譯器對 `-U` 旗標的處理我無法在這裡測試。
- Bazel 不讀 `.cargo/config.toml`，所以 Bazel 建置（包含合併前跑的測試）用的仍是完整的 SQLite。日後若有程式碼用到 FTS5 之類的功能，Bazel 測試會過、Cargo 發佈版會失敗。要消除這個落差得在 `MODULE.bazel` 加上對應設定並重新產生 lock 檔，這裡沒有 Bazel 可用，所以沒有做。

### 4.3 release profile 預設不帶除錯資訊並 strip

`debug = false`、`strip = "symbols"`。`cargo build --release` 直接得到與發佈相同大小的檔案，建置也快很多、佔用記憶體少很多。

有一個 Cargo 的細節：套件一旦有自己的 profile 覆寫，profile 層級的 `strip` 就不會套用到它，會退回 Cargo 的預設（只去掉除錯資訊、保留符號表）。`codex-cli` 正好有覆寫，所以 `strip = "symbols"` 在覆寫裡重複了一次；繼承 release 的 `profiling` profile 則用 `[profile.profiling.package.codex-cli] strip = false` 抵銷它。少了前者，`codex` 會多出約 70 MB 的符號表；少了後者，profiling 建置的 `codex` 會沒有符號。

發佈流程需要符號檔（Linux 的 `.debug`、macOS 的 dSYM、Windows 的 PDB，缺 PDB 時流程會直接失敗），所以三個用 release profile 建置並封存符號的地方都設定了 `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` 與 `CARGO_PROFILE_RELEASE_STRIP=none`，並且在建置 `codex` 的那一行加上 `--config 'profile.release.package.codex-cli.strip="none"'`（環境變數管不到套件覆寫）：

- `.github/workflows/rust-release.yml` 的 `build` job；
- `.github/workflows/rust-release-windows.yml` 的 `build-windows-binaries` job；
- `.github/actions/build-cargo-package-binaries/action.yml`（套件建置用的複合動作）。這個動作的 `build-mode: debug`（不最佳化的快速冒煙建置）原本只靠環境變數，同樣管不到套件覆寫，所以也補上對應的 `--config`。

少了那個 `--config`，Linux 的建置仍會成功，只是 `codex` 的符號檔是空的，要等到需要解析當機位置時才會發現。所以 `.github/scripts/archive-release-symbols-and-strip-binaries.sh` 的 Linux 分支加了一道檢查：輸入的執行檔沒有行號表就直接失敗。

這些流程我無法在這裡執行。我做的是：確認 YAML 可以解析；用 `cargo build --unit-graph` 帶同樣的環境變數與參數，確認 `codex` 在發佈設定下是「不 strip、帶行號表」，在 debug 模式下所有單元都是 `opt-level 0`；最後的量測建置本身就是用那個 `--config` 參數做的；封存腳本的新檢查在本機用帶行號表、已 strip、以及沒有 `readelf` 三種情況各跑過一次。

代價：本機 release 建置的 backtrace 只有位址，沒有函式名稱與行號（panic 訊息本身仍有檔案與行號）。需要符號時用既有的 `profiling` profile。

### 4.4 Linux 上 daemon 安裝包改用核心的檔案複製

`app-server-daemon/src/prepare_install.rs` 原本把每個檔案整個讀進記憶體再寫出去。在 Linux 上現在開啟來源與目的兩個檔案，用 `std::io::copy` 在兩者之間複製（走核心的 `copy_file_range`），再把目的檔倒回開頭、經由同一個檔案描述元做串流雜湊：

- 不再把數百 MB 的執行檔整個放進記憶體。
- 套件與 `CODEX_HOME` 位於同一個支援寫入時複製的檔案系統（btrfs、開啟 reflink 的 XFS）時，daemon 的那份套件與來源共用磁碟區塊，不多佔空間。跨檔案系統或在 ext4 上，磁碟佔用與以前相同。這台機器是 ext4，所以共用區塊這件事是依核心與標準庫的行為推論的，沒有實測。
- 完整性檢查不變：雜湊的是實際落地的位元組，之後仍與來源再比對一次。目的檔的建立方式、權限設定的時機也與原本相同。新增一個測試涵蓋空檔案、大於一個雜湊緩衝區的檔案與唯讀檔案。
- 第一次啟動（含複製）在這台機器上從 1.22–1.35 秒降到 0.70–0.84 秒，其中一部分來自套件本身變小。
- macOS 與 Windows 維持原本的讀入再寫出：標準庫的 `std::io::copy` 在這兩個平台沒有核心複製可用，改了只是換成一般的讀寫迴圈。能共用區塊的系統複製（`std::fs::copy`）在這兩個平台會連同隔離標記（quarantine）、替代資料流等屬性一起帶過去，daemon 的套件不應該繼承它們，所以沒有採用；APFS 的寫入時複製因此沒有用上。

### 4.5 工具與文件

- `scripts/binary_size_report.py`：體積拆解與兩次建置的對比。
- `scripts/startup_bench.py`：新增 `--reply-kb` 與 CPU 時間。
- `.bazelrc`、`bazel/rules/runtime_package.bzl`、`codex-rs/bwrap/BUILD.bazel` 裡原本寫著「與 Cargo 的 release profile 保持一致」的註解，改成說明兩者現在不同。Bazel 的實際設定沒有動。

## 5. 驗證

- 修改前後各做了完整的 release 建置並 strip；第 1 節與 3.2 節的大小都是實測。修改後的數字來自對最終程式碼從頭做的那一次建置（strip 後 221,028,768 位元組）。
- 效能數字見第 1 節。
- 建置時間：修改前帶除錯資訊時，4 個並行編譯工作在編譯 `codex_tui` 與 `codex_core` 時因記憶體不足被系統終止（當時機器上還有其他分析行程在跑）；改成單一工作後，最後的 LTO 步驟跑了 65 分鐘仍未結束，我中止了它。關閉除錯資訊、用 3 個並行工作重做，分三段共約 54 分鐘，其間可用記憶體一度剩 2 GB，我手動停掉其中一個編譯工作後才繼續。修改後同樣關閉除錯資訊、3 個並行工作，從頭建置一次完成，45 分 51 秒，其中最後編譯與連結 binary crate 約 25 分鐘；建置期間同一台機器上還有唯讀的審查行程在跑。binary crate 仍為 `s` 的中間版本從頭建置是 36 分 36 秒，差距來自連結後改用 `-O3` 最佳化。
- Cargo 的設定解析用 `--unit-graph` 逐一確認：預設 release、發佈 CI 的環境變數加 `--config`、複合動作的 debug 模式、`profiling` profile。
- daemon 模式用修改後的 release 二進位實際跑過（第一次安裝與之後的啟動）。
- `codex-app-server-daemon` 的 86 個測試通過，`cargo clippy --tests -- -D warnings` 無警告。
- 這批修改另外做了兩輪獨立審查。第一輪找到 4.1 的向量化問題與幾處文件、腳本的錯誤；第二輪（每項發現再由另一個審查者嘗試反駁）沒有找到新的實質缺陷，留下的是本文已列出的限制。
- 沒有改動任何依賴，`Cargo.lock` 與 `MODULE.bazel.lock` 不變。
- 沒有驗證的部分：
  - macOS 與 Windows 的建置，以及官方發佈工作流程本身；
  - 官方 musl 產物的大小變化（見第 1 節）；
  - Bazel 建置（最佳化設定與 SQLite 功能都沒有同步；目前發佈走 Cargo）；
  - `codex` 以外的執行檔的大小與效能；
  - btrfs／XFS 上的區塊共用。

## 6. 沒有做的項目

以下是調查中確認過、但這次沒有動的項目。節省量除特別註明外都是估計值。

### 6.1 最值得做的下一步：讓連結用的套件只剩入口

4.1 說明了 221 MB 裡約 15 MB 來自 `codex-cli` 套件在 `-O3` 下自己重編的副本。`codex-cli` 目前包含整個命令列的實作（`main.rs` 2,812 行，加上登入、MCP、外掛、doctor 等模組），這些程式碼會把 TUI、app-server、exec 的非同步呼叫樹整棵拉進來重編。

- 作法一：把 `main` 搬到一個只含入口的小套件（或反過來，把實作搬到一個函式庫套件，`codex-cli` 只留入口），只有那個小套件用 `-O3`。上限是拿回那 15 MB，也就是接近 206 MB，而且第三方程式碼維持完整最佳化；實際數字要做了才知道。同樣的手法可以套到 `codex-app-server` 等其他執行檔，解掉 4.1 最後列的限制。
- 作法二：不動套件結構，在 TUI、app-server、exec、MCP 伺服器的入口各加一個回傳 `Box<dyn Future>` 的一般函式，讓 `codex-cli` 呼叫它們而不是直接 `await` 那些 `async fn`。改動小，但只能擋掉被包起來的那幾棵呼叫樹。
- 這次沒有做的原因：作法一要新增工作區成員、調整 Bazel 的 BUILD 檔與測試對執行檔的引用，這裡沒有 Bazel 可以驗證；兩種作法都要再經過數輪各約 30–45 分鐘的完整建置來量測。

### 6.2 會改變行為，需要你決定

| 項目 | 估計節省 | 代價 |
|---|---|---|
| Linux 上不再連結 OpenSSL（改由 rustls 處理所有 HTTPS） | 3.5–5 MB（只影響官方 musl 建置） | 只支援舊版 TLS 的代理或伺服器會連不上 |
| 把隱藏的 `codex tcp-tunnel` 做成可關閉的功能 | 1–1.5 MB | 失去該子命令 |
| Linux 鑰匙圈與桌面設定探測做成可關閉的功能 | 0.7–1 MB | 憑證只能存檔案 |
| 語法高亮只保留常用語言 | 0.3–0.6 MB | 冷門語言不再上色 |
| daemon 與安裝器只保留一個版本 | 每個根目錄一整份套件 | 失去磁碟上的前一版備援 |
| 獨立套件不再附 `codex-exec-server` | 未量測，見 6.4 | 直接呼叫該路徑的工具要改用 `codex exec-server` |
| 預設開啟 rollout 壓縮 | 視 `sessions/` 大小而定 | 該功能目前標示為開發中 |

### 6.3 不改變行為，但每項都小

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

這些加起來大約 3–5 MB，相對於 221 MB 約 2%，而且多數會改動 `Cargo.lock`（之後要重新產生 `MODULE.bazel.lock`，這裡沒有 Bazel 可用）。

### 6.4 `codex-exec-server`

獨立套件裡的 `bin/codex-exec-server` 我沒有量到大小：它的功能組合與 `codex` 不同，建置時要重新編譯大部分 crate，這台機器的磁碟不夠，我中止了建置。它與 `codex` 共用九成的依賴，合理的預期是與主程式同一個量級，但這是推測。
