# AGENTS.md

ruos：用 Rust 寫的能力式（capability）微核心教學作業系統，內建類 Docker 的 pod 機制，在瀏覽器裡操作（終端機 + 課程 + 即時系統圖）。
**v2.0（2026-10-31 使用者確認，規劃在 `doc/v2.md`）：前端是 ruos（核心編成 wasm32，跑在瀏覽器的 Worker），後端是桌機的 OS 上的 `ruos serve`（Rust；可以呼叫其他語言寫的軟體，例如 SQLite、PostgreSQL、Redis，也可以用 tokio）。RISC-V 與 ARM 已放棄**（v1 = tag `v1`，三個平台都通過的最後一版）。前端的 OS 只用 Rust；不追 POSIX 相容，但前端要做 WASI preview1 讓現成的 wasm 程式能跑。
與 veryl2chip 無關。所有回覆用繁體中文。

## 指令

```sh
./run.sh wasm [port]             # 建置 + ruos serve（含 registry），開 http://127.0.0.1:8780（函式埠 8790）；預覽：.claude/launch.json 的 `ruos`，埠 8781
./run.sh build                   # 只建置：build/kernel.wasm、build/images、build/store、target/release/ruos
./run.sh dist                    # 建全部，放進 ruos/assets（發布用）
./publish.sh [-y] [--dry-run]    # 一鍵發布（使用者自己跑）：檢查 → dist → release → crates.io；= ./run.sh publish
./test.sh [host] [wasm]          # 預設兩個；log 在 build/test-*.log
cargo test --release             # 主機端 crate：objstore（BLAKE3 對照 blake3 crate）、podimg、ruos
target/release/podimg ls --store build/store      # 映像清單；build / pack / verify URL NAME / hash FILE
node web/test/boot.mjs           # 開機測試（需先 ./run.sh build）
```

- 工具：rustc nightly（`rust-src`）、target `wasm32-unknown-unknown`；Node 24。
- 核心一律 `--release`，不帶 debug info（`CARGO_PROFILE_RELEASE_DEBUG=false`）。`kernel/` 是獨立 package（`build.rs` 加 wasm 的 `--export`、嵌入 `ROS_BOOT` 的檔案）；根目錄 `Cargo.toml` 是 workspace（`exclude = ["kernel"]`）。
- 映像的格式仍有架構欄位（manifest 的 `arch`、index 的 `manifest ARCH ID`），但只建 wasm32。

## 套件（使用者的偏好：有合用的套件就用，沒有才自己寫）

- 核心：`fdt`（DTB；`dtb.rs` 只是包一層成核心用的小 API）、`talc` + `lock_api`（heap；鎖是會關中斷的 `RawIrqLock`）、`rand_chacha`（核心亂數）。
- 使用者空間：`talc` + `spinning_top`（libsys heap；`Grow` source 在用完時 `memory.grow`）、`blake3`（`pure`）、`toml` + `serde`（Podfile 與 kube 的 manifest）。
- 主機：`rusqlite`（bundled SQLite）、`tokio-postgres`、`redis`、`serde_json`（後端服務）、`tokio` + `axum`（ruos serve 的 HTTP 與 WebSocket；`futures-util` 拆 WebSocket 的讀寫）、`ureq`（podimg 讀 registry；ruos serve 代為發 HTTP，rustls）。
- 網頁不用框架：原生 ES module（`web/*.js`），SVG 自己畫。
- 仍是自己寫的，因為沒有合用的套件：核心本身、物件庫的格式與 pack、各服務的協定（store、fs、dock、console、remote、kube）。

## 測試

`test.sh` 先 `./run.sh build`，清掉 `build/store` 的 pods / disks / cluster，然後：host = `cargo test`；wasm = `web/test/boot.mjs`（自己起一個 ruos serve 與給 curl 的測試網站 `web/test/testsite.mjs`，以 `--http-allow` 開放它，輸入檔裡的 `@HTTP_PORT@` 換成它的埠）在新磁碟上開機兩次：第一次打 `tests/m3.input`，比對所有 `tests/mN.expect`、`tests/mN.wasm.expect`、`tests/teach.expect`、`tests/shell.expect`（ERE，每行一個），並確認沒有 `panic:` 或 `FAILED`；第二次打 `tests/boot2.input`，比對 `tests/boot2.expect`（第一次 commit 的映像與物件要從磁碟回來）。boot.mjs 也檢查 registry（ref、immutable blob、`podimg verify`）、系統圖的 snapshot（shell 的 channel 連到 console、dock、kube、store（一般的與 http 兩條）與自己檔案系統的 overlay（資料與 control 兩條）），第一次開機時故意在第 50 個回覆後斷一次 server 連線（`--drop-remote`，要看到 `remote: reconnected`），看到輸出 `ingress-test: go` 時自己從外面送請求給函式（`tests/m23.wasm.expect`）。最後 `cluster_test`：兩台機器共用一個 server（`tests/cluster*.input`、`tests/cluster.expect`）。
自測（`kernel/src/selftest.rs`）：第 10 個 tick 印 `timer: 10 ticks at 100 Hz`，第 30 個 tick 印 `M1 OK`；init 結束（印 `init exited (code)`）且 M1 自測做完才關機。M2–M4、M8、M9 的核心測試在 `apps/ktest`（init 開機時先跑；handle 0 = store），之後 init 啟動 console、dock、kube 與 shell，測試腳本打字驗證 shell。新里程碑照這個模式加 `tests/mN*.expect`。

## 設計（已與使用者確認）

- **核心物件**：Task（一個 wasm instance 的 memory + handle 表）、Thread、Channel（訊息可夾帶 handle）、Memory（可共享 / COW）、Port（事件等待）、Quota（階層式資源預算）。`spawn(image, quota, handles)` 建立程式。沒有 fork/exec、signal、uid、全域路徑、POSIX；檔案、主控台都是使用者空間服務，以 Channel 溝通。
- **pod（類 Docker）**：內容定址物件庫（BLAKE3、Merkle tree、層 + manifest），唯讀層 + COW 可寫層、commit；容器 = 父程式給的「名稱 → handle」名稱空間 + Quota；`Podfile.toml`；`dock` 服務與指令 `build/run/ps/stop/commit/images`。
- **WASM**：核心編成 wasm32 跑在瀏覽器的核心 Worker；每個 Task = 獨立 WebAssembly instance + 自己的 Worker；系統呼叫 = wasm import，用 SharedArrayBuffer + `Atomics.wait`（需要 COOP/COEP，ruos serve 會送）；記憶體配額 = Memory maximum。`ruos serve`：registry（內容定址 blob / manifest，HTTP，可永久快取）+ 每個 pod 的虛擬檔案系統（可寫層在 server，WebSocket，訊息格式即 IPC 格式）+ 網頁。
- **目錄**：`kernel/`、`objstore/`（物件庫格式，no_std + alloc，OS 與主機共用）、`abi/`（核心 ABI：syscall 號碼、錯誤碼、rights、物件型別，核心與 libsys 共用）、`libsys/`、`services/`（init, console, store, overlay, dock, shell, kube）、`apps/`、`podimg/`（主機端 Podfile → image）、`ruos/`、`web/`、`run.sh`、`test.sh`。
- **里程碑**（v1；M1–M10 時每步三個平台都要通過）：
  1. 開機印字（DTB、分頁、trap、計時器中斷）— **完成**
  2. Task/Thread、使用者模式、系統呼叫、handle 表 — **完成**
  3. Channel/Memory/Port、init、console、shell — **完成**
  4. podimg + registry，從物件庫啟動程式 — **完成**
  5. Quota + 名稱空間 + `dock run/ps/stop` — **完成**
  6. server 端可寫層、commit/push、Podfile — **完成**（Podfile 由主機端 podimg 建；QEMU 的可寫層在 store 記憶體，push 要等 M7 網路）
  7. virtio-blk/net — **完成**
  8. SMP、wasm 程式在原生平台執行 — **完成**
  9. 補強與收尾：FP/SIMD 狀態保存、ASID、頁表記入 quota、大檔案、UTF-8 backspace、`dock build/rm/logs`，以及改用套件 — **完成**
  10. 效能：量測、wasmrt 預先解碼、映射批次 TLB、排程 per-CPU 鎖、阻塞式 CHANNEL_RECV — **完成**（wasm JIT 留作之後）
- **教學系統**（M11 起重新編號，2026-10-30 使用者確認：以 capability 與 pod 為主軸，在瀏覽器裡操作；先前 M11–M25 的 POSIX 路線已移除，在舊 rust-os repo 的 tag `posix-era`）：
  11. 整併：移除 POSIX / WASI / C、瀏覽器的 server 連線斷線重連 — **完成**
  12. 教學 shell（handle、名稱空間、映像、pod）、`snoop` — **完成**
  13. 網頁：課程 + 即時系統圖（`kernel_snapshot`）— **完成**
  14. 像一般 OS 一樣用：shell 自己的檔案系統（`root` 映像）、檔案 / 文字 / 系統指令、管線與導向、`save` — **完成**
  15. 「學生能像在 Linux 上一樣練習」（使用者確認的目標）：shell 語言（小 sh）、更多文字指令、↑ ↓ 歷史 — **完成**
  17. 類 k8s（單機宣告式）：`kube` 服務、Deployment / Space、reconcile、重啟退避 — **完成**
  18. capability 版 Service、Config、probe、滾動更新 — **完成**
  19. 多節點叢集：ruos serve 的 KV（版本 + CAS + 租約）、leader、controller / scheduler、kubelet — **完成**
  20. 網頁的「叢集」分頁與第 11–13 課 — **完成**
  21. `curl`：server 代為發出 HTTP 請求（擋住 server 自己的網路），`http` capability — **完成**
  22. serverless：kube 的 `kind = "Function"`（在呼叫的節點上冷啟動、重用、縮到 0、timeout）、KV namespace（`kv:NAME`）— **完成**（設計：`doc/serverless.md`）
  23. 函式的對外入口：ruos serve 的函式埠 `/fn/NAME/…`、`expose = true`、送到有熱 instance 的機器、HTTP 封包與 `serve_http` — **完成**（設計：`doc/ingress.md`）
- **v2.0**（`doc/v2.md`）：1. 清理（拿掉 RISC-V、ARM、裝置、QEMU、wasmrt）— **完成**；2. ruos serve 改 tokio（axum；ingress 改回 WebSocket）— **完成**；3. 後端服務是 capability（`sql:NAME`、`redis:NAME`）— **完成**；4. 前端 WASI preview1 — **完成**；5. 網站範例、課程、發布 2.0.0 — 網站範例與課程完成，發布由使用者做。之後：資料放前端或後端（`@local`，doc/storage.md）— **完成**。

## 後端服務（v2.0 第 3 步）

- ruos serve 的 `backend.rs`：`sql:NAME` 預設是 SQLite（`rusqlite`，SQLite 編進 ruos serve；檔案 `STORE/sql/NAME.db`，第一次用時建），`--sql NAME=sqlite:PATH` 或 `--sql NAME=postgres://…`（`tokio-postgres`，在 `spawn_blocking` 裡以 `Handle::block_on`；值一律是文字（simple query），還不支援參數；這台機器沒有 PostgreSQL，**沒有測過**）。`redis:NAME` 要 `--redis NAME=redis://HOST:PORT/`（`redis` crate，同步連線，斷了下次重連）。資料庫在 server 上，所有機器共用。
- remote 的 `SQL [name, sql, params(JSON 陣列), format]` → `{"columns","rows"}` / `{"changed","last_id"}`，format `text` 時是文字表格；`REDIS [name, args(JSON 陣列), format]`。
- **capability**：store 記每個 client 的 grants（`Grants`：`*`、`http`、`sql:NAME`、`redis:NAME`）。`web` 的 client 是 `*`；`CONNECT` 繼承；`GRANT names` + channel 端 = 只有那些（且是自己有的）的新 client（`Store::grant`）。HTTP_* / SQL / REDIS 先檢查 `needs()`，沒有就回 `no X capability`。init 把 `web` 給 shell 當 `backend`（全部）與 `http`（只有 http）；kube 拿 `backend` 的 CONNECT，kubelet 依 `uses` 給 pod / 函式 `Store::grant(["sql:app"])` 等；shell 的 `run IMAGE sql:app` 也是 grant。
- libsys `sql.rs`（`Sql::get(name)` = 名稱空間的 `sql:NAME`；`Sql::with(store, name)`；`query(sql, params_json)`、`query_text`）、`redis.rs`（`Redis::get`、`cmd(&[…])`、`cmd_text`）。shell：`sql [-d DB] STATEMENT`（預設 `app`，也接管線）、`redis [-d NAME] CMD ARGS`（預設 `cache`）。snoop 會試著經自己的 `store` 用 `app`（被拒），有 `sql:app` 就查一次。
- 測試：`tests/m24.expect`（SQL、snoop 的 capability、另一個資料庫）；boot.mjs 有 `redis-server` 就起一個（隨機埠）給 `--redis cache=…`，test.sh 那時才比對 `tests/redis.expect`。test.sh 每次清掉 `build/store/sql`，逾時預設 600 s。

## WASI（v2.0 第 4 步）

- `services/wasi`（ruos 程式，就是那個 task）：實作 `wasi_snapshot_preview1`（`wasi_*` 匯出，`build.rs` 掃 `src/main.rs` 的 `pub extern "C" fn wasi_*` 加 `--export`）。guest（`wasi:module:LEN` 的 Memory 物件）由 `web/task_runtime.js` 的 `wasiHost` 在同一個 Worker 裡 instantiate，它的 WASI import 直接接到 shim 的匯出（沒有的回 ENOSYS 52；import memory 等非函式的 import 不支援）；guest 的 memory 是它自己的（不共享），shim 經 `wasi_host.read/write` 複製；`proc_exit` = `wasi_host.exit` 丟例外讓 guest 回捲；trap → `wasi: trap: …`，結束碼 134。
- 對應：fd 0/1/2 = `console`（stdin 一次一行；`console::EOF` 回覆 = 檔案結尾）；fd 3 = 唯一的 preopen `.` = `PWD` 指的目錄（`fs`）。**只有 `.`**：wasi-libc 的工作目錄固定從 `/` 開始，會先把相對路徑變成絕對路徑再找 preopen，有 `/` preopen 時相對路徑都會跑到 `/`；所以只給 `.`，`/x` 也就是那個目錄下的 `x`（像 `wasmtime --dir .`），程式看不到目錄以上。檔案經 fs 的 `READ_AT`/`WRITE_AT`/`STAT`/`LIST`/`TRUNCATE`…；時鐘、亂數、`poll_oneoff`（睡）用系統呼叫。guest 的 memory 沒有記入 quota（已知限制）。
- `libsys::store::is_wasi`（前 256 KiB 有 `wasi_snapshot_preview1`）→ `Store::spawn_module` 改啟動 `wasi`。shell 執行程式時：`/bin/NAME`，否則同名的映像（store 會從 registry 拉）；WASI 程式另外拿到 shell 的 `fs`（新 client）與 `PWD`。程式的輸入是管線 / 檔案，或輸出要接下去（管線、`>`、`$(…)`：`Sh.piped_out`）時，console 是 shell 自己服務的（`serve_console`：WRITE 收進輸出、READ_LINE 給一行，沒了回 EOF），所以 `coreutils sort f | coreutils head` 可以。
- WASI 程式：`wasi-programs/`（各自獨立的 crate，`[workspace]`，`wasm32-wasip1`；`wasi-hello` 放進 root 映像的 `/bin`）；uutils coreutils 0.12（crates.io，`cargo install … --features feat_wasm`，快取在 `target/wasi-tools`）10 MB，是只在 registry（`build/store`）的映像 `coreutils`，不放進 boot pack。store 拉大物件：先 `OBJ_SIZE`（新的 remote op 49）再 `try_reserve_exact`，大於 1 MiB 的不寫進磁碟日誌（下次開機再拉）——曾因每次加倍與多份複製在 64 MiB 的 store 裡 OOM、整個 store 掛掉。C 程式（sqlite3、Lua）要 wasi-sdk，這台機器沒有。
- 測試 `tests/m25.expect`。

## 資料放在前端或後端（`@local`，doc/storage.md）

- 核心的 remote channel 2 = 這台機器自己的儲存（`web/local_store.js`：key → bytes，`GET`/`PUT`/`DELETE`/`LIST prefix after`（分頁）；瀏覽器 IndexedDB `ruos-local-MACHINE`，Node 測試是 `build/local/MACHINE/` 的檔案，檔名是 key 的 hex）。有沒有 server 都給（`kernel_host.js` 的 `remote_open`）；核心給 init 的 channel 依序是 server、ingress、storage 中有的那些（handle 3 起），init 的 `main(arg)` 收到哪些有（bit i = remote channel i，`task::spawn_arg`），不再靠順序猜。沒有 server（`boot.mjs --offline`，`tests/offline.*` = 第三次開機）時 kv、redis 照樣能用，sql 不行（sqlited 映像要從 registry 拉）。
- `services/local`（init 啟動；shell 與 kube 各有一個 `local` client，`libsys::local`：`connect KIND:NAME` + channel → `ok`/`error`，`dup`，`ls`，`rm KIND:NAME`：刪掉它存的一切，sql 會先 kill 它的 sqlited）：`fs:ROOT`（`files.rs`：fs 協定，`m ROOT PATH` 記 `file SIZE`/`dir`，`c ROOT N PATH` 是 32 KiB 區塊，樹放記憶體，區塊快取 128 個）、`kv:NAME`（`kv.rs`，kube 的 kv 協定，`k NAME HEX`）、`redis:NAME`（`cache.rs`：store 的 REDIS 請求，自己寫的小 Redis，每次修改整份寫進 `cache` 樹的 `/NAME`；到期用牆上時鐘）、`sql:NAME`（`sql.rs`：store 的 SQL 請求，交給每個資料庫一個的 `sqlited`）。一條 channel 只能用自己的名字。事件驅動：等 sqlited 回答時照樣服務它的 fs。
- `wasi-programs/sqlited`：rusqlite（bundled）以 wasi-sdk 編（`run.sh` 的 `WASI_SDK`，預設 `~/opt/wasi-sdk-34.0-arm64-macos`；`CC_wasm32_wasip1`、`CFLAGS --sysroot`，用完整路徑連結 `libwasi-emulated-{mman,getpid,signal,process-clocks}.a`：不能用 `-L` 指到 wasi-sdk 的 sysroot，會和 Rust 自帶的 crt1 / libc 混在一起）；`unix-none` VFS（沒有檔案鎖）。stdin 一行 JSON 請求，stdout 一行 `{"ok"}`/`{"err"}`，回答格式與 backend.rs 相同。映像只在 registry：sqlited、sqlite3 建進 `build/upstream`（也複製進 `build/store`），發布時不放進 crate（見「發布」）。console 的 `read_line` 現在可以收超過 256 bytes 的行（served console），WASI 轉接層的 stdin 一次收到 64 KiB。
- `wasi-programs/sqlite3`：SQLite 官方 shell（amalgamation 在 `SQLITE_SRC`，預設 `~/opt/sqlite-src/sqlite-amalgamation-3540100`，不放進 repo；`nosystem.c` 補 `system()`），`run.sh` 直接用 wasi-sdk 的 clang 編（`-DSQLITE_OMIT_WAL -DSQLITE_OMIT_POPEN …`，這裡用 wasi-sdk 自己的 libc，所以可以 `-l`）。shell 給 WASI 程式 `HOME=/`（不然 sqlite3 警告找不到家目錄）。
- 名字 `X@local`：kube 的 `uses`（`check_use` 接受 kv/sql/redis 加 `@local`；`bindings` 向 local 要 channel，放進名稱空間的名字去掉 `@local`）、shell 的 `sql -d`、`redis -d`、`kv … NS@local`、`run IMAGE sql:X@local`、`local [ls]`、`local rm KIND:NAME`。搬資料：`sql copy FROM TO`（shell 的 `copy_db`：讀 `sqlite_master`，目的地有同名表格就拒絕；先建表格、逐列 `INSERT … VALUES (?1…)`，最後才建 index / view / trigger；只限 SQLite，BLOB 會變成後端 JSON 的 `(N bytes)` 字串）、`kv copy FROM TO`（每個 key，已有的覆蓋）；兩邊都可以加 `@local`。shell 現在依賴 serde_json（no_std alloc）。
- store：映像的程式物件不在（大於 1 MiB 的不寫進磁碟日誌）時 `GET` 會重新 pull、`OBJ_MEM` 會補抓物件——之前第二次開機後 coreutils / sqlited 會「找得到映像卻啟動不了」。
- 測試 `tests/m27.expect`（第一次開機；`lguest` = 同一個 guestbook 程式加 `sql:guestbook@local`，資料和後端的分開）、`tests/boot2.expect`（第二次開機讀回 redis、kv、sql）。

## 網站範例（v2.0 第 5 步）

- `apps/webapps`：一個 package、五個 bin（`guestbook`、`todo`、`wiki`、`short`、`weather`；`run.sh` 的 `WEB_APPS`），共用的 `lib.rs`：`Db`（`sql:NAME`，`rows` 把 `{columns, rows}` 轉成物件陣列）、`body` / `field` / `param`（%-decode）、`answer` / `error`（JSON `{"error"}`）、`page`（共用的 HTML 外框 + JS 的 `api()` / `esc()`）。JSON 用 `serde_json`（no_std + alloc）。網頁在瀏覽器裡由 JS 依 JSON API 畫；程式啟動時 `CREATE TABLE IF NOT EXISTS`。字串常數用 `r##"…"##`（HTML 裡有 `"#`）。
- 每個網站一個 `Podfile.NAME.toml`，建進 `build/extra`（只在 registry，連同 coreutils；`build/store` 與 `dist` 都會複製進去），不放 boot pack：第一次用時 store 從 registry 拉。manifest 在 `services/shell/root/kube/app-*.toml`（`expose = true`、`uses = ["sql:NAME"]`；weather 另有 `http`、`timeout = 15`，`args` 第一個可以換掉 Open-Meteo 的位址）。
- 測試：腳本 `kube apply` 五個（weather 的 args 指向 testsite 的假 `/v1/forecast`），印 `apps-test: go`，`boot.mjs` 的 `appsTest` 從函式埠打 API（`apps: …` 行），再由 shell 的 `sql -d` 看資料庫；`tests/m26.expect`。

## kube（M17）

- `services/kube`（init 啟動；名稱空間只有 `dock`（init 經 dock 的 channel 送一端過去而得的新 client）與 `client`；shell 的名稱空間有 `kube`）。協定 `libsys/src/kube.rs`：文字，`apply` 第一行後面接 manifest（`services/kube/src/manifest.rs`，TOML + serde：`kind = "Deployment"|"Space"`、`name`、`space`、`image`、`args`、`replicas`、`restart = always|on-failure|never`、`[resources] mem tasks`、`[labels]`）；`get [pods|deploy|spaces]`、`describe`、`scale`、`delete`、`logs`。狀態只在記憶體（重開機就沒了；M19 搬到 ruos serve 的 KV）。
- 控制迴圈：每次被叫醒（client、dock 事件、計時）都 `reconcile`：pod 多了就移除最新的、少了就建 `NAME-N`（編號不重用），Pending / BackOff 到時間就 `dock run -d --quiet --name … --label kube=NAME [--space S]`（失敗 = Pending + 原因，例如 `worker: QuotaExceeded`，退避後重試）。pod 結束（dock 的 `watch` 事件，或每 2 s `pods` 全表比對）→ 依 restart 政策 BackOff（1 s 起加倍到 16 s，跑滿 10 s 就重設）或 Done。結束的 pod 留在 dock 裡（`logs` 看得到），重啟前才 `rm`。Deployment 的範本變了 → 全部 pod 換掉（Recreate）。
- dock 新增：`run --label K=V --space S --quiet`（輸出只留給 `logs`，不印到主控台）、`pods`（給程式讀的一行一個）、`watch`（之後每個 pod 的 `event start|exit|stop|rm ID NAME [CODE]`）、`space NAME SIZE TASKS` / `spaces`（Space = dock quota 下的子 quota，pod 的 quota 再從它分出來）。
- **M18**：
  - Service（`libsys/src/svc.rs`）：`serves = "X"` 的 pod 名稱空間有 `listen`，`uses = ["X"]` 的有 `svc:X`；沒有位址。呼叫端建一對 channel，經 `svc:X` 送 `connect` + 一端，kube 依序（round robin）交給一個 ready 的 X pod（`listen` 上的 `conn` + handle），之後兩個 pod 直接談，kube 不在資料路徑上。沒有 ready 的 pod 就等 5 s，再關掉（呼叫端看到 PEER_CLOSED）。shell 的 `kube call SVC TEXT`：shell 以 `connect SVC` + handle 經 kube client channel 送。
  - probe：serving pod 在 `listen` 上說 `ready` 才算 ready；kube 每 2 s 送 `ping`（`Listener::poll` 回 `pong`），5 s 沒回 = not ready（不分流量），7 s = liveness 失敗 → `dock stop` → 依政策重啟。不 serve 的 pod 跑滿 1 s 就算 ready。
  - Config（`kind = "Config"`、`[files]` 路徑 = 文字、`[env]`）：用它的 Deployment 跑 `kube-NAME:rREV`（`libsys::image::derive`：原映像 + 檔案 layer + env；kube 名稱空間有 `store`）。Config 改了 → 用它的 Deployment 出新 revision。
  - 滾動更新：範本或 Config 變了 = 新 revision（最多留 10 個）；pod 記自己的 rev。maxSurge 1 / maxUnavailable 0：先多建一個新 pod，ready 的數量超過 replicas 就移除一個舊的；沒 ready 的舊 pod 直接移除。`kube rollout status|history|undo`（undo = 以前一個 revision 的範本出新 revision）。
  - dock：`run --handle NAME`（依序對應訊息帶的 handle，放進 pod 名稱空間；只有 handle、沒有文字的訊息才是「新 client」）；pod 的環境有 `POD=名稱`。
  - `apps/echo`（serve：回 `POD: 訊息`；`echo hang N` N 秒後不再回應）；worker 會印 `MESSAGE` 與 `/etc/message.txt`，每 3 s 呼叫每個 `svc:`。範例 `echo.toml`、`front.toml`、`greeting.toml`、`stuck.toml`；測試 `tests/m18.expect`。
- **M19（叢集）**：
  - 共用同一個 ruos serve 的機器 = 一個叢集，節點名稱 = 機器名稱（`?machine=NAME`；頁面的網址也可以帶 `?machine=`，預設 `browser`；測試用 `test`、`node-b`）。沒有 server 的機器是一個節點的叢集，記錄放在 kube 自己的記憶體（`api.rs` 的 local）。
  - ruos serve 的 `cluster.rs`：記錄 = 版本（全域遞增）+ 最後寫入的 server 時間 + 值，存在 `STORE/cluster/`（每筆一個檔案，`/` 換成 `+`）；remote 的 `KV_HELLO`（機器名稱、session、時間）、`KV_LIST prefix`、`KV_PUT key expect value`（expect：空 = 不管、0 = 必須不存在、N = 版本必須是 N）、`KV_DEL`。store 原樣轉送（`libsys::store::Store::kv`）。test.sh 每次開始清掉 `build/store/cluster`。
  - 記錄（值是 `欄位 值` 行，見 `services/kube/src/api.rs` 開頭）：`nodes/N`（arch、session；記錄時間 = 租約）、`leader`、`deploy/D`（replicas、rev、created）、`rev/D/R`（`image` + `---` + manifest 原文）、`ctl/D`（leader 寫：next、rolling）、`pod/P`（leader 寫：deploy、rev、node 或 `-`）、`status/P`（節點寫：phase、ready、restarts、started、wait、reason、dock）、`config/C`（`version N` + `---` + manifest）、`space/S`、`events/D`（最後 14 行，CAS 加上去）。
  - 每個 kube 的迴圈：每 1 s `refresh`（整個 KV_LIST）+ 寫自己的 `nodes/N` + 搶 / 續 `leader`（6 s 租約，CAS）；leader 跑 `control.rs`（orphan 清理；節點租約（8 s）過期 → 移除它的 pod；滾動 / 擴縮同 M18；新 pod 排程：節點活著、`node` 選擇器、映像有那個架構（wasm32）、pod 最少者優先），每個 kube 的 `kubelet.rs` 跑分到自己節點的 pod（M18 的重啟、退避、probe、Service），寫 `status/P`。client 指令之後馬上跑一輪（單節點時效果是立即的）。Space 是每個節點的 dock 各自建（第一次需要時）。Config 的衍生映像 `push` 到 registry，別的節點自己拉。
  - 限制：Service 連線只交給同一個節點上的 pod（channel 不能跨機器）；`kube logs` 只看得到本機的 pod；沒有容量篩選（dock 的 quota 拒絕就 Pending）。
  - `kube get nodes`（STATUS Ready / NotReady、ROLE leader）；`get pods` 多了 NODE 欄。init 在 shell 結束時先停 kube，再請 dock `shutdown`（停掉所有 pod：pod 拿著 console client，否則 console 不會結束、機器不會關）。
  - 測試：test.sh 的 wasm 在 boot 2 之後跑 `cluster_test`：另起一個 ruos serve，先開 `node-b`（`tests/cluster-b.input`：sleep 25 後 exit），它的 shell ready 後再開 `test`（`tests/cluster.input`），比對 `tests/cluster.expect`（不是 m 開頭，所以 boot 1 不比對它）：兩個節點、pod 分到兩邊、`node = "node-b"` 的 pod、node-b 離開後它的 pod 在 test 重建、留下的 pinned 變 Unschedulable、leader 交接。`web/test/boot.mjs --server URL` 用已經在跑的 server。範例 `spread.toml`、`pinned.toml`。
- **M20（網頁）**：ruos serve 的 `GET /v1/cluster`（所有記錄的 JSON：`{now, records: {KEY: {version, updated, value}}}`，`cluster::json`）。`web/cluster.js` 的 `ClusterView`：「叢集」分頁顯示時每秒抓一次（內容沒變就不重畫）；節點卡片（Ready / NotReady、leader、「這一頁」= 頁面的 `?machine=`）裡列出分到它的 pod（顏色 = 狀態）、Unschedulable 的另一張卡；Deployment 表（就緒 / 想要、revision、更新中、映像、serves / uses）；最近的事件（所有 `events/*` 合併，新的在前）。`lessons.js` 第 11 課（宣告式、自我修復、擴縮、back-off、Space）、第 12 課（Service、Config、滾動更新、liveness）、第 13 課（叢集：另開 `?machine=b2` 分頁、排程、節點離開）。leader 會把 5 分鐘沒更新的 `nodes/N` 刪掉。`.claude/launch.json` 的 `ruos`（埠 8781）是預覽用的 ruos serve。
- **網路指令的模式（使用者確認，2026-10-31：之後的網路「用戶端」指令都照這樣做；server 類型（例如 httpd，等別人連進來）不一定，另外設計）**：OS 裡的指令做參數、權限、分段、輸出；真正的連線由 ruos serve 代做（瀏覽器不能開 socket、CORS 擋住 `fetch`）。新的 remote op 經 store 原樣轉送到 server，JS 只轉送 bytes 不必改。能用的程式要有對應的 capability（像 `http`：store 只替特定 channel 來的 client 轉送，再交給 shell，`run IMAGE NAME` 才給別人）。server 端擋住自己的網路（檢查放在位址解析那一層，轉址與重新解析也擋），並限制時間與大小，可用選項開放或關閉。超過 64 KB 的回覆留在 session 上分段讀。說明寫在 `doc/`（範例 `doc/curl.md`）。
- **M21（curl）**：
  - ruos serve 的 `http.rs`：`fetch` 用 `ureq` 3（rustls），10 s、body 最多 4 MiB、最多 5 次轉址、只有 http / https。檢查在自訂的 `Resolver`（`ureq::unversioned::resolver`，包 `DefaultResolver`）裡：解析出的位址是 loopback、private、link-local、0/8、100.64/10、multicast、broadcast、IPv6 ULA / link-local（含 v4-mapped）就丟掉，除非在 `--http-allow ADDR:PORT` 裡；全被丟掉就回「HOST is on the server's own network (refused)」。所以轉址與 DNS 重新解析也擋得住。`--no-http` 整個關掉。
  - remote 的 `HTTP_FETCH [method, url, headers, body]` → `STATUS LEN\n` + header 行（body 留在 session 的 `fetched`）、`HTTP_READ [offset]` → 最多 60000 bytes（channel 訊息上限 64 KB）。store 原樣轉送，但只接受「http capability」的 client：store 名稱空間的 `web`（init 建的 channel）來的 client，以及它們 `CONNECT` 出來的；其他 client 回 `no http capability`。init 把 `web` 的另一端給 shell 當 `http`。
  - `libsys/src/http.rs`：`Http::get()`（名稱 `http`）、`connect()`、`fetch(method, url, headers, body)`（FETCH 之後分段 READ）。`Store::raw(op, fields)` → (狀態, 內容)（原本的 `kv`；kube 也用）。
  - shell 的 `curl`（內建、可接管線）：`-i -I -s -f -L -X -H -d -o`；沒有 `://` 就補 `http://`；`-d` 預設 POST 與 form 的 Content-Type；二進位內容（前 1 KB 有 NUL）只報大小；`-f` 遇到 ≥ 400 狀態碼 22；連不到狀態 6。`run IMAGE http` 給新的 http client。
  - 測試：`web/test/boot.mjs` 自己起一個 Node http server（`/hello`、`/echo`、`/big`（20000 行，測分段）、`/redirect`（轉到 127.0.0.1:1，要被擋）、其他 404），以 `--http-allow 127.0.0.1:PORT` 開放它，輸入檔裡的 `@HTTP_PORT@` 換成它的埠（`web/test/testsite.mjs`）；`tests/m21.expect`。第 13 課。
- **M22（Function、KV）**：設計與決定見 `doc/serverless.md`。
  - manifest `kind = "Function"`（`image`、`args`、`uses`、`timeout` 秒（10）、`max` 每節點 instance 數（4）、`idle` 秒（30）、`reuse`（true）、`[resources]`）→ 記錄 `fn/F`（`version N` + `---` + manifest）。和 Deployment 不能同名。
  - `uses` 的項目（Deployment 與 Function 都可以）：`X` = service（`svc:X`）、`fn:X`、`kv:X`、`http`（kube 名稱空間的 `http` 的新 client；init 用 shell 那條 `web` 先 `connect` 一條給 kube）。kubelet 的 `bindings` 建 channel，自己留的那端記在 `links`（`Bind::Svc|Fn|Kv`）。
  - `services/kube/src/funcs.rs`：每個節點自己管（leader 不參與）。請求（`fn:F` 上的 `connect` + channel 端，或 shell 經 kube client 的 `connect fn:F`）→ 交給閒著的 instance（`listen` 上的 `conn`），沒有就冷啟動（`dock run -d --quiet --name F-fN --label kube-fn=F --handle listen …`，最多 `max` 個），請求排隊等它說 `ready`；instance 一次一個請求，答完在 `listen` 上說 `done` → 閒著（`reuse = false` 就停掉）。tick：忙超過 `timeout` → 停掉（呼叫端看到 PEER_CLOSED，事件 `Request to … timed out`）；閒置 `idle` → 停掉（最後一個時事件 `Scaled F to 0`）；版本變了的閒置 instance 停掉；15 s 沒 ready 停掉；排隊 20 s 的請求關掉。統計（請求、冷啟動次數與時間、答完的平均時間、timeout）只在本節點；寫進 `fnstat/NODE` 給網頁。不 ping instance（只靠 timeout）。
  - `libsys/src/func.rs`：`serve(|req| answer)`（說 ready、等 `conn`、讀一則訊息、回一則、說 done）、`call(name, req, timeout)`。`libsys/src/kv.rs`：`Kv`（`get/put/delete/list`；key ≤ 64 bytes、value ≤ 32 KiB 文字）。
  - KV：kubelet 把 `kv:K` 的請求變成 server 記錄 `data/K/HEX(key)`，每次直接讀寫 server（`Api::data` / `set_data`），不進每秒的快取：server 的 `KV_LIST ""` 不列 `data/`（要明說 prefix）。沒有 server 時放在 kube 的記憶體。
  - shell：`fn call NAME [TEXT]`（等 60 s）、`fn ls`、`kv ls|get|put|rm NS …`（每次經 kube 的 `connect kv:NS` 拿一條）。kube：`get fn`、`describe F`、`delete F`。`apps/greet`（記憶體裡的計數 + 有 `kv:counter` 就加 KV 計數；`sleep N` 請求）。範例 `fn-hello.toml`（idle 15、max 2、timeout 3）、`fn-count.toml`、`fn-cgi.toml`（reuse false）。測試 `tests/m22.expect`。網頁：節點卡片列出函式 instance、函式表；第 15 課。
- `kube delete space NAME`（沒有 Deployment 用時；各節點 dock 的 quota 留到重開機）。只在 wasm 檢查的 ingress 項目在 `tests/m23.wasm.expect`。
- **M23（對外入口）**：設計與實作時的改動見 `doc/ingress.md`。
  - ruos serve：`--fn-port N`（預設主埠 + 10；`--port 0` 時也是 0），印 `ruos: functions on http://ADDR:PORT/fn/`；`run()` 另開一個 axum Router（只有 `/fn/NAME/…` 與 `/` 列出公開的函式；body ≤ 60000，axum 的 `DefaultBodyLimit`）。`ruos/src/ingress.rs`：連著的機器（依 session，`attach` / `detach`）與等回答的請求（`oneshot`）、`call`（讀 `fnstat/` 挑有熱 instance 的機器，否則輪流；等 timeout + 15 s，否則 504；沒有機器 503；機器在回答前斷線 502）、`answer`、`exposed`（讀 `fn/NAME` 的 `expose` / `timeout`）。`/v1/ingress` 是 WebSocket（M23 時是 long polling，v2.0 換 tokio 後改回來）。`own_origin` 擋 `/v1/remote` 與 `/v1/ingress` 的跨 origin（含 `Sec-Fetch-Site: same-site`）。函式的回應加 `X-Ruos-Node`；去掉 hop-by-hop header；body ≤ 60000。log：`ruos: fn NAME METHOD PATH → MACHINE: STATUS (N ms)`。
  - 核心：remote channel 有編號（`Channel.remote: Option<u8>`，`arch::remote_channel(i)` / `remote_send(i, …)`，wasm imports `remote_open(i)` / `remote_send(i, p, len)`、export `kernel_remote(i, len)`）；init 拿到 handle 3（server 連線）與 4（ingress），init 把第一個 channel 給 store 當 `remote`、第二個給 kube 當 `ingress`。`kernel_host.js` 的 `ingressLink`（WebSocket，和 remote 同一個 session，斷線就重連，不重送）。
  - kube：`ingress` 上的 `REQ ID NAME\n` + 封包 → 檢查 `expose` → 自己建 channel 寫入封包、交給 `fn_request`，回答（或 channel 關掉 = 504）以 `RESP ID\n` + 封包送回；不是 `HTTP ` 開頭的回答包成 `200 text/plain`。`kube get fn` 多 URL 欄（`Api.fn_base`，來自 `KV_HELLO` 第四欄）。
  - libsys `func::serve_http`、`Request`（`parse`、`header`、`param`；`fn call` 的純文字 = method `CALL`）、`Response`（`text`、`html`、`json`、`encode`）。`apps/site`（網頁 + `/api/hello`、`/api/hit`（kv:visits）、`/api/sleep`）、`fn-site.toml`。
  - 測試：boot.mjs 讀出函式埠，看到輸出 `ingress-test: go` 後自己送請求，印 `ingress: …`（`tests/m23.expect`）。網頁的函式表有連結；第 16 課。
- `apps/worker`：`worker` 一直跑（每 10 s 報一次），`worker SECONDS [CODE]` 時間到就以 CODE 結束。root 映像的 `/home/kube/`：`web.toml`、`flaky.toml`（on-failure）、`small.toml`（Space）、`crowd.toml`（超過 Space 的 task 數 → Pending）。測試：`tests/m3.input` 最後一段、`tests/m17.expect`。

## 發布：`cargo install ruos`（M16）

- 專案名稱是 **ruos**（2026-10-30 從 rust-os 改名；格式字串一併改：`ruos manifest 1`、`ruos index 1`、`ruos ns 1`、`.ruos-dir`、DTB 的 `ruos,wasm*`；`ROSPACK1` / `ROSDISK1` 沒改）。repo：`/Users/Shared/ccc/project/ruos`，GitHub `ccc-claude-opus/ruos`（2026-10-30 搬來，從新的 git 歷史開始；之前的歷史與 tag `posix-era` 留在舊的 rust-os repo，不在這裡）。
- `ruos/` 是唯一要發布的 crate（原本的 podsrv；不依賴 workspace 的其他 crate：物件 id 用自己的 `id.rs` + `blake3`）。指令：`ruos [web] [--port N] [--no-open]`（預設：啟動伺服器、開瀏覽器）、`ruos serve …`（開發用：run.sh、test.sh、web/test/boot.mjs）。資料在 `$RUOS_HOME` 或 `~/.ruos`；第一次（或新版本）把內含的 registry 放到 `HOME/store`（物件只補缺的，refs 換新版，push 進來的映像留著）。
- 內含的系統：`./run.sh dist` 建好後填 `ruos/assets/`（`web/*.html|js|css`、`kernel.wasm`、`store/` = `build/images`）；`ruos/build.rs` 把它打包成 `OUT_DIR/assets.bin`（路徑長度 u32 + 路徑 + 長度 u64 + 內容），`assets.rs` 以 `include_bytes!` 帶著。`ruos/assets` 在 .gitignore 裡，但 Cargo.toml 的 `include` 有它，所以 `cargo package` 會帶上（crates.io 上限 10 MiB）。沒有 assets 的建置（repo 裡平常的）只能 `ruos serve` 從磁碟供檔。
- 太大的映像（sqlited、sqlite3，`build/upstream`）不放進 crate：`dist` 把它們的 refs 放進 `ruos/assets/store/refs`，物件 id（不在其他映像裡的）寫進 `ruos/assets/store/upstream`，物件本身複製到 `build/release/`（檔名就是 id），要上傳到 GitHub release `vVERSION`（`gh release upload vVERSION build/release/*`，使用者做）。`ruos` 缺一個列在 `upstream` 裡的物件時（`/v1/blobs/ID`、remote 的 `OBJ_SIZE`/`OBJ_READ_AT`），從 `--upstream URL`（預設 `https://github.com/ccc-claude-opus/ruos/releases/download/vVERSION`，`none` 關掉）抓 `URL/ID`，驗 BLAKE3 後存進 `STORE/objects`（`ruos/src/upstream.rs`）。`install` 在新版本時也換掉 `upstream` 清單。
- 發布步驟：`./publish.sh`（使用者自己跑；`./run.sh publish` 也是它）：檢查 git 乾淨且已 push、crates.io 還沒有這個版本（`/api/v1/crates/ruos/VERSION` 要 404）、有 crates.io token → `run.sh dist` → `cargo publish --dry-run`（印出 crate 大小）→ 問一次（`-y` 不問）→ GitHub release `vVERSION`：沒有就 `gh release create`（tag 建在這個 commit），有就只補上缺的物件，再確認 `build/release` 的每個檔案都在 → `cargo publish --allow-dirty`（`ruos/assets` 被 git 忽略，cargo 算它們「未 commit」）。`--dry-run` 什麼都不發布，只說 release 缺幾個物件。release 一定先於 crates.io：新安裝的人第一次用 sqlite 就要從那裡拿。2.0.0 已於 2026-11-01 發布（release v2.0.0 的 10 個物件與 crate 的 `store/upstream` 一致）。使用者端只需要穩定版 Rust（`rust-version = 1.85`）。

## 教學介面（M11–M13）

- **shell 的檔案系統**：`fs::mount(store, "b{boot}-shell", "root")`（overlay；可寫層每次開機一個）。`root` 映像 = `services/shell/Podfile.root.toml`（`/bin/shell`、`/bin/hello`、`/bin/snoop`、`/etc/motd`、`/home/README.txt`、`/home/poem.txt`、`/home/count.sh`（腳本範例）、空的 `/tmp`）。`save` = overlay 的 `COMMIT root`：ref 更新、記進磁碟日誌，下次開機掛上的就是它（`tests/boot2` 檢查 `/home/note.txt`）。開機印 `/etc/motd`，從 `/home` 開始，提示 `CWD $ `（測試的 expect 用 `^(/[^ ]* )?\$ `）。
- **shell 語言**（`src/lang.rs`）：詞法（`'…'` 原樣、`"…"` 只展開 `$`、`\` 跳脫、`$(…)` 整段保留到展開時才解析、`#` 註解；`&` 單獨出現回錯誤），語法（list = pipeline 以 `;` / 換行、`&&`、`||` 相連；pipeline = 指令以 `|` 相連；指令 = 簡單指令（字 + `<` `>` `>>`）或 `if`/`elif`/`else`/`fi`、`for NAME [in 字…]; do … done`、`while … do … done`；關鍵字只在指令開頭才算），沒打完回 `Incomplete`，`main.rs` 就印 `> ` 再讀一行。展開：`$NAME` `${NAME}` `$?` `$#` `$@` `$0`–`$9` `$PWD`、`$(…)`（遞迴執行，輸出收進 `Sh.captures` 的最內層，去掉結尾換行）、`$((…))`（整數 + - * / % 與括號、變數名）；沒加引號的 `$` 展開結果依空白拆成多個欄位，沒加引號的 `*` `?` 比對最後一段路徑的檔名（排序、預設不含點檔，沒對到就留原字）。只有 `NAME=值` 的指令設變數。狀態 `Sh.status`：內建指令出錯 = 1、`grep` 沒找到 / `false` / `test` 為假 = 1、程式的結束碼；`exit` 結束 shell（腳本裡由 `sh` 執行時只結束腳本）。`while` 跑到 100000 圈就停。複合指令不能接在管線裡。`sh FILE ARGS`（自己的 `$0` `$1`…）、`. FILE`（同一份）。
- **shell 的指令**（`src/main.rs`：讀到完整的指令、`!N`、歷史；`dock …` 整行原樣交給 dock；`src/cmds.rs`：內建指令，每個收引數 + 上一段的輸出、回傳自己的輸出，由 shell 印出、寫檔、交給下一段或收進 `$(…)`）：檔案 `ls [-l]`（`IMAGE:DIR` 就看映像）、`cd`（無引數回 `/home`）、`pwd`、`cat`（`IMAGE:FILE` 看映像）、`tree`、`stat`、`mkdir [-p]`、`rmdir`、`rm [-r]`、`cp`、`mv`（進目錄保留檔名）、`touch`、`save`；`find [DIR] [-name PAT] [-type f|d]`、`basename`、`dirname`；文字 `echo [-n]`、`printf`（`%s` `%d` `%%`、`\n` `\t`，引數多就重複格式）、`head` / `tail [-n N]`、`wc [-l -w -c]`（只給一個旗標就只印數字）、`grep [-i -v -c]`（子字串，`^` `$` 錨點）、`sort [-r -n]`、`uniq [-c]`、`cut -d D -f LIST` / `-c LIST`、`tr [-d] SET1 SET2`（`a-z` 範圍）、`sed [-n]`（`s/A/B/[g]`、`d`、`p`，位址可以是行號、`$`、`/字/`，以 `;` 串接；一般文字）、`seq`、`tee [-a]`、`rev`、`tac`、`nl`；shell `test` / `[`（`-e -f -d -s -z -n`、`=` `!=`、`-eq` 等、`!`）、`true`、`false`、`read`（管線或主控台的一行）、`export`、`unset`、`env` / `set`、`which` / `type`、`sh`、`.` / `source`；系統 `date`（RTC，UTC）、`uptime`、`clear`（`ESC [2J ESC [H`，網頁終端機認得 `J`）、`history`、`sleep 秒`（可有小數）、`ps` / `kill`（dock ps / stop）；其他名稱 = `/bin/NAME`（或路徑）的程式：`read_mem` + `Store::spawn_module`，名稱空間只有新的 console client 與 `exec:`（引數），等它結束、非 0 才印結束碼。`run`、`dock`、`/bin` 程式直接寫主控台，不能接管線。
- **shell 的能力指令**（說明文字是中文，指令輸出用英文以便測試比對）：`handles`（用 `HANDLE_INFO` 逐一試 0..1023，連續 32 個空號就停；名稱取自名稱空間）、`ns`、`run IMAGE [NAME...]`（只給列出的名稱：`console` 是新的 console client、`store` 是新的 store client、`fs` 是 shell 檔案系統的新 client，其他名稱是 `dup`）、`mem`、`images`、`image NAME`（index → 各架構 manifest → layer）、`ls IMAGE:DIR`、`cat IMAGE:FILE`（`libsys::image::lookup` / `list`：由上層往下、whiteout 遮住下層；隱藏 `.ruos-*`）、`dock …`。名稱空間：`console`、`store`、`dock`、`kube`、`http`。
- **dock** 多了 `ls POD [DIR]`、`cat POD FILE`（經 pod 的 overlay；超過 16 KiB 或有 NUL 就只報大小）。
- **apps/snoop**：印出自己的名稱、試遍 handle 0..1023 數自己有幾個、有 store 就列映像、有 console 就經它寫字。`run snoop` 與 `run snoop console store` 是第 2 課。
- **console** 只剩行模式（`WRITE`、`READ_LINE`、`CONNECT`）；Ctrl-C 丟掉正在打的那一行。↑ ↓（`ESC [ A` / `ESC [ B`；其他 ESC 序列丟掉）叫回之前交出去的行（最多 64 行，不重複記同一行），正在顯示時以 `ESC [ n D ESC [ K`（依顯示寬度）擦掉再印。網頁的 keydown 把方向鍵送成這兩個序列，Tab 不送任何東西（留在終端機）。
- **網頁**（`web/`）：左邊終端機（`main.js`），右邊分頁「課程」（`lessons.js`：十三課，每課幾段說明 + 可點的指令，點了就以 UTF-8 + CR 送進鍵盤）與「系統」（`inspector.js`）。系統頁顯示時每秒向核心 Worker 要一次 snapshot；只有開機時間變了就只更新摘要（不重畫，保住鍵盤焦點）。圖：活著的任務排成一圈，兩個任務各持有同一條 channel 的一端就連線，remote channel 連到「伺服器」；點任務（或 quota 樹裡的晶片）列出它的 handle 表與「連到哪裡」（channel 的對端是誰的哪個 handle、共用的 Memory、task handle 指向誰）；quota 樹：根 → pods 總額 → 每個 pod（用量長條、任務數），任務掛在被記帳的 quota 下。手機寬度上下排列。
- **`kernel_snapshot` / `kernel_snapshot_len`**（只有 wasm32，`kernel/src/snapshot.rs`）：JSON `{uptime_ms, tasks: [{id, name, threads, exit, quota, handles: [{h, type, rights, id, peer?, queued?, task?, size?}]}], quotas: [{id, parent, mem, tasks, threads}]}`；物件的 id 是它在核心裡的位址（同一物件、channel 兩端都對得起來），`peer` 是 `"host"` 表示 remote channel，上限 `null` = 無上限。這是從外面看（像除錯器），系統裡面的程式仍然只看得到自己的 handle。`kernel_host.js` 的 `snapshot()`、kernel Worker 的 `{type:'snapshot'}` 訊息。
- **斷線重連**（`kernel_host.js` 的 `remoteLink`）：WebSocket URL 帶隨機的 `session=`（每次開機一個），ruos serve 以 `w-SESSION` 當 session 名稱（可寫層在 `STORE/pods/w-SESSION/`），所以重連後還是同一份可寫層；store 一次只有一個請求，斷線時沒收到回覆的請求在重連後重送（store 的請求都可以重做），斷線期間的請求先排隊；退避 250 ms 起、加倍到 8 s。ingress（remote channel 1）不重送：server 自己回 502。

## 物件庫與映像（M4）

- `objstore/`：BLAKE3 用 `blake3` crate；`Id`（64 hex，可加 `b3:` 前綴）；文字格式：tree（`file|exec|dir|whiteout ID SIZE NAME`，依名稱排序 = Merkle tree）、manifest（`ruos manifest 1` / `arch` / `entry` / `layer`（底層在前）/ `env`）、index（`ruos index 1` / `name` / `manifest ARCH ID`，多架構）；`Objects::lookup` 由上層往下找路徑（whiteout 遮住下層）；pack（`ROSPACK1`、`obj ID LEN`+內容、`ref NAME:TAG ID`、`end`）；`Store`（記憶體中的物件 + refs，`closure` 求一個映像在某架構需要的物件）。
- `podimg/`（主機 CLI）：`build PODFILE --store DIR --programs ARCH=DIR...`（每個架構一個 layer + manifest，合成 index，`refs/NAME/TAG`）、`pack --store DIR --arch A --out F`（該架構需要的物件 + 所有 refs）、`ls`、`verify URL NAME`（從 registry 抓 index → manifest → tree → blob，逐一驗 BLAKE3）、`hash`。Store 目錄：`objects/<id>`、`refs/<name>/<tag>`。
- `Podfile.toml`（`podimg/src/podfile.rs`，`toml` + `serde`）：`name`、`tag`、`entry`、`from`（基底映像，layer 疊在上面）、`[programs]`（路徑 = 程式名，各架構從 `--programs` 目錄找，wasm 加 `.wasm`）、`[files]`（相對 Podfile 的檔案）、`[env]`。每個程式目錄都有一個。
- `ruos serve --store DIR` = registry：`GET /v1/blobs/ID`（`Cache-Control: public, max-age=31536000, immutable`）、`GET /v1/refs/NAME/TAG`、`GET /v1/refs`。
- `run.sh build P`：先把所有使用者程式建成三個架構（`build/user-ARCH/`），每個 Podfile 建進 `build/store`，再把 P 架構的 pack 與 init 放進 `build/boot-P/`（`boot.pack`、`init`），以 `ROS_BOOT` 讓核心 `build.rs` 嵌入。核心只認得 `init`；pack 以 Memory 物件交給 init（handle 1）。

## Quota、名稱空間、dock（M5）

- **Quota**（`kernel/src/quota.rs`）：記憶體 bytes、tasks、threads 三種上限；`charge` 要通過自己與所有祖先才記帳（`QuotaExceeded`）。核心給 init 根 quota（handle 2，1 GiB）。記帳的東西：每個 task（`Task.charge`）、thread（`Thread.charge`）、task 的 linear memory（`maximum` = min(模組宣告, quota 剩餘)，記 initial 大小）、Memory 物件（建立者的 quota，`clone_cow` 記整個大小）。task 結束時就退費（`charge.take()`），不等最後一個 handle。`SPAWN` 改收 `abi::SpawnArgs`（含 quota handle，`NO_HANDLE` = 呼叫者的 quota，需 WRITE）；`QUOTA_CREATE`（需 MANAGE）、`QUOTA_INFO`。
- **名稱空間**（`libsys/src/ns.rs`，純使用者空間）：子程式的 handle 0 是 bootstrap channel，第一個訊息 = `ruos ns 1` + 每行一個名稱，帶著對應的 handles；`ros_start` 在 `main` 前 `ns::load()`，之後 `ns::get("console")`。父程式用 `Namespace::new().add(name, h)` 與 `ns.spawn(image, len, name, quota)` / `Store::spawn(name, &ns, quota)`。init 例外（核心直接給 handle 0/1/2）。
- **dock**（`services/dock`，名稱空間 `console`、`store`、`quota`（pods 的 quota，init 建 160 MiB / 32 tasks / 64 threads）、`client`）：文字協定（`libsys/src/dock.rs`）`run [-d] [--name N] [--mem SIZE] [--tasks N] IMAGE` → `ok ID NAME`、`ps`、`stop ID|NAME`、`wait ID|NAME`（pod 結束才回）。pod = 子 quota（預設 32 MiB、8 tasks、16 threads）+ 名稱空間（新的 console client、store client）+ 從 store 啟動的 task；pod 結束以 Port 綁 TERMINATED 得知。shell 的 `dock …` 指令；`apps/hog` 一次拿 1 MiB Memory 直到 quota 拒絕。
- 測試腳本在 `dock run -d` 之後 `sleep 500`（shell 指令），因為 Worker 啟動要一點時間。

## 檔案系統、commit、push（M6）

- 協定編碼（`libsys/src/wire.rs`）：op byte + 以 NUL 分隔的欄位（最後一欄是原始 bytes）；回覆以狀態 byte 開頭（`OK` / `NOT_FOUND` / `FAILED`）。
- store 多了：`OBJ_GET` / `OBJ_PUT`（新物件存在 store 記憶體）、`REF_SET`、`INDEX`、可寫層（upper：每個 key 一張 path → 檔案 / whiteout 表）`UP_WRITE` / `UP_READ` / `UP_LIST` / `UP_DELETE`、`PUSH`（映像的物件 + ref 送到 registry；pack 裡沒有的其他架構物件略過，registry 本來就有）。名稱空間有 `remote` 時，UP_* 原封不動轉給 server，所以可寫層在 server；沒有 server 時放在 store 記憶體，`PUSH` 回「no registry connection」。
- `services/overlay`（每個 pod 一個，dock 啟動，記在 dock 的 quota）：名稱空間 `store`、`fs`（pod 的請求）、`control`（dock：第一個訊息 `CONFIG key image`，之後 `COMMIT`）。合併檢視：upper（store 的 UP_*，key = `podN`）在上、映像各層在下（物件從 store 取，快取）；whiteout 遮住下層；`LIST` 合併各層。`COMMIT NAME`：upper → 新的 tree（刪除 = whiteout），疊到原映像每個架構的 manifest 上（資料與架構無關），新 index + ref。
- `libsys/src/fs.rs`：`Fs`（`read/write/delete/list`，整個檔案讀寫，≤ 60 KB；`commit`）。pod 的名稱空間多了 `fs`。`apps/writer` 讀 `/etc/motd`、遞增 `/data/count`、刪 `/etc/motd`。dock `commit POD IMAGE`、`push IMAGE`；shell `dock commit` / `dock push`。
- **remote channel**：核心的 `Channel::remote()` 端點，另一端是 host：寫入走 import `remote_send`（不能帶 handle），host 收到的訊息經 `kernel_remote_buf` / `kernel_remote` 進來。核心在 host 有 server 時（import `remote_open`）把它給 init 當 handle 3，init 轉給 store（`remote`）。`kernel_host.js` 以 WebSocket 連 `remoteUrl`（瀏覽器：頁面所在的 `/v1/remote`；Node 測試：ruos serve 的位址），連上前先排隊。
- ruos serve：tokio + axum（`main.rs` 的 `run`：兩個 Router，OS 的與函式埠的；同步的檔案 I/O 在 `spawn_blocking` 裡）；`remote.rs`（同步的 `Session::handle`，每則 WebSocket 訊息一次）（每條 WebSocket 一個 session `sPID-N`；OBJ_PUT 寫 `objects/`、REF_SET 寫 `refs/`、UP_* 存在 `STORE/pods/SESSION/KEY/files/` + `whiteouts`）；`GET /v1/pods`、`GET /v1/pods/SESSION/KEY/files/PATH`。Node 測試結束後檢查 server 上的可寫層（`data/count = 2`）並 `podimg verify` 推上去的 `snap`。

## 磁碟（M7）

- **store 的磁碟日誌**（`services/store/src/disk.rs`）：sector 0 是 superblock（`ROSDISK1`、`used`、`boot`），之後依序附加 `obj ID LEN` + 內容、`ref NAME:TAG ID`；開機時重播，boot 計數加一。只記新的物件（pack 裡已有的不記）。磁碟是 server 上的（`DISK_INFO/READ/WRITE` 經 remote channel，ruos serve 存成 `STORE/disks/MACHINE.img`，`?machine=NAME`；瀏覽器用 `browser`，測試用 `test`）。pod 的可寫層 key 是 `b{boot}-pod{id}`，跨開機不撞名。
- build：`build/images` 由 Podfile 建（每次重建，pack 從這裡來），複製進 `build/store`（registry，會累積 push 進來的映像，pods / disks / cluster 也在這）。

## 多執行緒（M8）

- 每個 thread 是自己 Worker 裡的一個 instance，本來就平行跑；ktest 讓 4 個 thread 做同一份工作，要比一個 thread 做 4 倍快 1.5 倍以上（`M8 OK`）。console 的預先輸入上限 1024 行（測試腳本一次送完；超過的行會被丟掉，腳本就停住、等到逾時：曾在腳本長到 258 行時發生），行緩衝放在 static。

## 補強（M9）

- **大檔案**：fs 的 `STAT` / `READ_AT` / `WRITE_AT`（數字用十進位文字欄位），`Fs::read/write` 以 60 KB 分段；store 的 `UP_READ_AT` / `UP_WRITE_AT` / `UP_STAT` / `OBJ_READ_AT` / `OBJ_PUT_MEM`（大物件用 Memory 物件交給 store），push 時大物件用 `OBJ_PUT_PART` 分段，ruos serve 組回 `objects/.part-ID` 並驗 hash。
- **dock**：pod 的名稱空間多了 `log`（libsys 的 `println!` 有 `log` 就寫到那裡），dock 保留每個 pod 最後 64 KB 輸出（`dock logs`）並照樣轉給 console；`dock rm`、`dock write POD PATH TEXT`（`\n` 等跳脫）、`dock build POD PODFILE`（Podfile 在 pod 的檔案系統裡：`from` + `[files]`，`[programs]` 只能在主機用 podimg）。`objstore::podfile`（`toml` + `serde`，no_std）由 podimg 與 dock 共用；`libsys::image`（`put`、`layer`、`derive`）由 overlay 的 commit 與 dock build 共用。
- console：Backspace 刪整個 UTF-8 字元、依顯示寬度（東亞全形 2 欄）送 `ESC [ n D` + `ESC [ K`，網頁終端機認得這兩個序列。

## 效能（M10）

- `apps/bench`：系統呼叫（CLOCK）、channel 來回（兩個 thread、兩個 task，對端是 `peer`）、計算迴圈。系統呼叫約 7 µs（跨 Worker 的 Atomics 來回）。
- `CHANNEL_RECV`：有訊息就讀，沒有就等；等到後以 ShouldWait 喚醒、使用者端再讀一次（`Channel::recv` 的迴圈）。不在 signal 的情境裡替別的 thread 讀：那可能在某個 task 的 handle 表鎖之下（關閉 handle 讓 channel 消失時），曾因此死結。`HANDLE_CLOSE` 也改成放開鎖之後才丟掉物件。

## 使用者程式

- 根目錄 workspace 含 `objstore`、`podimg`、`abi`、`libsys`、`services/*`、`apps/*`（`default-members` 只有主機端 crate，所以 `cargo test` 只跑主機端）。workspace release profile `strip = true`。
- 使用者程式用 `-Zbuild-std=core,alloc` + `+atomics,+bulk-memory,+mutable-globals`，`--import-memory --shared-memory --max-memory=64MiB`，匯出 `ros_start`、`ros_thread_start`、`__stack_pointer`（`run.sh` 的 `WASM_USER_FLAGS`，target dir `target/wasm-user`）。syscall = import `sys.call(nr, a0..a5)`。
- `libsys`：`Handle`（`close/dup/info/wait/kill`）、`spawn(name, &[Handle])`、`thread_create(f, stack, arg)`、`sleep_ms`、`clock_ns`、`print!`（每次呼叫一次 `DEBUG_WRITE`，行不會交錯）、panic → 印 `panic:` 並以 `Fault` 結束；`ipc.rs`：`Channel`（`create/write/read/recv`）、`Port`（`bind/wait/queue`）、`Memory`（`create/read/write/clone_cow/map/unmap`）、`Serial`、`wait_one`；`console.rs`：console 服務的協定（訊息第一個 byte：`WRITE` / `READ_LINE` / `CONNECT`）與 `Console` client、`cprintln!`；`store.rs`：store 服務的協定（`GET` 回 [0, 長度, entry] + 程式的 Memory handle；`LIST`；`CONNECT`）與 `Store` client（`program`、`spawn(name, handles)`、`list`、`connect`）；`heap.rs`：global allocator（`memory.grow`）。`spawn(image: Memory, len, name, handles)`。
- 程式：`services/init`（handle 0 = Serial、1 = boot pack、2 = 根 quota、3 = server 連線、4 = ingress；直接從 pack 取出 store 程式啟動，其餘都經 store：ktest、console、dock、kube、shell；shell 結束就停 kube、請 dock `shutdown`、關 console 後結束）、`services/store`（名稱 `pack`、`client`、`web`、`remote`，載入時驗證每個物件的 hash）、`services/console`（名稱 `serial`、`client`，其餘 client 由 CONNECT 加入；Port 等待；行編輯：有人在等一行時邊打邊回顯，否則交出那一行時才回顯，所以腳本輸入的紀錄是有順序的）、`services/shell`（名稱 `console`、`store`、`dock`；`help/echo/images/run/mem/sleep/dock/exit`）、`services/dock`、`services/overlay`、`services/kube`、`apps/hog`、`apps/writer`、`apps/bench`、`apps/ktest`（名稱 `store`）、`apps/peer`（ktest 的對端，名稱 `peer`）、`apps/hello`（印出自己的名稱空間）、`apps/fault`。

## 核心（kernel/src）

- **亂數與時間**：`RANDOM`（abi 38，`libsys::random`）是 ChaCha20（`rand_chacha`），種子取自 DTB `/chosen` 的 `rng-seed`（`web/dtb.js` 以 `crypto.getRandomValues` 放）再混入時鐘。`CLOCK` 參數 1 = 牆上時鐘（DTB 的 `ruos,wasm-rtc` 節點；沒有就 2026-01-01）。
- **從 registry 拉映像**：store 被問到不認得的映像（`GET` / `INDEX`）就從 server 拉（`Store::pull`：remote 的 `INDEX` 拿 index id，`OBJ_READ_AT` 分段取 wasm32 的 manifest、各層 tree 與檔案，逐一驗 hash、記進磁碟日誌；日誌滿了就只放記憶體）。

開機流程：host（`web/kernel_host.js`）產生 DTB（`web/dtb.js` 的 `wasmMachine`）、寫進 `kernel_dtb_buf()`、呼叫 `kernel_boot(len)` → 解析 DTB 成 `boot::BootInfo` → `kmain` → `heap::init` → `time::start` → `random::init` → `task::start_init` → 回到 host。之後 host 呼叫 `kernel_irq(0, 0)`（timer，setInterval）、`kernel_irq(1, byte)`（console）、`kernel_syscall`、`kernel_remote(i, len)`。

- `arch/wasm32`：和 host 的介面（imports 與 exports）、`AddrSpace`（task 的 instance 與它的 memory）、`remote_channel(i)` / `remote_send(i, …)`（0 = server 連線，1 = ingress）。
- 共用：`dtb.rs`（FDT 讀取）、`boot.rs`（`BootInfo`）、`console.rs`（`print!`/`println!`、輸入環形緩衝）、`sync.rs`（`IrqLock`）、`heap.rs`、`time.rs`、`selftest.rs`、`sched.rs`（thread 都在自己的 Worker 平行跑，這裡只管阻塞、睡眠與喚醒）。
- **物件與 handle**（`object.rs`）：`Object::{Task, Thread, Channel, Port, Memory, Quota, Serial}`，`Handle { obj, rights }`，`HandleTable` 的 handle 值 = slot 索引（關閉後可重用），每個 task 上限 1024。`dup` 只能縮小 rights（需 DUP）、`wait` 需 WAIT、`spawn` 搬移的 handle 需 TRANSFER、`kill` 需 MANAGE。
- **task / thread**（`task.rs`）：最後一個 thread 結束 → task 結束；task 結束 → 殺掉其餘 thread、清空 handle 表、喚醒等待者。task 1 = init，結束時通知 selftest 關機。instance trap → `task::fault` 印 `fault: task N (name) ...: killed`，exit code = `Killed`。
- **signals**（`signal.rs`）：Task / Thread（TERMINATED）、Channel（READABLE、WRITABLE、PEER_CLOSED）、Serial（READABLE）各有 `SignalCell`；觀察者是一次性的 `Observer::Thread`（`WAIT_ONE`）或 `Observer::Port`（`PORT_BIND`），level triggered。阻塞時 `sched::block_current` 回傳 wait generation（`wgen`），喚醒與逾時都要對得上，所以過期的觀察者不會吵醒後來別的等待。
- **Channel**（`channel.rs`）：一對端點，訊息 = bytes（≤ 64 KB）+ handles（≤ 16，需 TRANSFER，不能把端點送進自己）；端點最後一個 handle 關閉時對端得到 PEER_CLOSED。remote 端點（`Channel::remote(i)`）的另一端是 host，只能送 bytes。**Port**（`port.rs`）：封包佇列 {key, signals, data}。**Serial**（`serial.rs`）：核心主控台輸入環（`console.rs` 的 `IN_SIZE`，64 KiB：測試腳本一次全部送進來，超過的 byte 會被丟掉——曾因 4 KB 而讓 4.4 KB 的腳本在中間停住）的物件，只給 init。
- **Memory**（`memory.rs`）：頁面以 `Arc<Page>` 計數（heap 區塊）；`clone_cow` 共用頁面，寫入前 `make_private` 複製。linear memory 不能互相別名，`MEMORY_MAP` 回 `NotSupported`，只能 `MEMORY_READ/WRITE`。
- **syscall**（`syscall.rs`）：`dispatch` 回傳 `Sys::Ret(v)` 或 `Sys::Block`；阻塞的 thread 由喚醒者以 `sched::wake(t, v)` 給結果（host 的 `thread_resume`）。
- **WASM host**（`web/kernel_host.js`，瀏覽器 Worker 與 Node worker_threads 共用）：imports `env.console_write / timer_start / now_ms / shutdown / task_create / task_destroy / thread_start / thread_resume / thread_kill / user_read / user_write`；exports `kernel_boot / kernel_irq / kernel_syscall / kernel_fault / kernel_scratch / kernel_dtb_buf`。`shutdown` 丟 `KernelExit` 讓 wasm 堆疊回捲，所有進入核心的呼叫經 `enter()`。
  - task = 編譯好的 `WebAssembly.Module` + 一塊 shared `WebAssembly.Memory`（上下限由 `memoryLimits` 從 import section 讀出）；thread = 該 module 在自己 Worker 裡的一個 instance（`task_runtime.js` `runThread`；非 main thread 先設 `__stack_pointer` 再呼叫 `ros_thread_start(f, arg)`）。
  - syscall：thread 把號碼與參數寫進自己的控制區（SharedArrayBuffer：i32[0] 狀態 0/1/2，i64[1..7] 號碼與參數，i64[8] 結果），postMessage 通知核心 Worker，再 `Atomics.wait`；核心 `kernel_syscall` 回傳結果或 `i64::MIN`（阻塞，之後 `thread_resume`）。instance trap → `fault` 訊息 → `kernel_fault`。
  - 頁面鍵盤：隱藏的 textarea，`keydown` 處理 Enter / Backspace / Ctrl，`input` 處理文字（輸入法、手機鍵盤、貼上也能用）。
  - 瀏覽器：task Worker 由頁面建立（`main.js` 收到 `spawn` 就開 `task-worker.js`，並把 MessagePort 交給它；`kill` 時 terminate），因為巢狀 Worker 不是每個瀏覽器都能用（Claude 的內建瀏覽器就載不起來）。Node 測試直接用巢狀 worker_threads（`test/node_task_worker.mjs`）。
- **ruos serve**（`ruos/src/main.rs`）：靜態檔 + `/kernel.wasm`，每個回應帶 COOP `same-origin`、COEP `require-corp`、CORP `same-origin`；`--port 0` 自選埠並印 `ruos: listening on http://…`。
- 瀏覽器預覽：`./target/release/ruos serve --port 8781 --web web --kernel build/kernel.wasm`，頁面應顯示 `crossOriginIsolated`。
