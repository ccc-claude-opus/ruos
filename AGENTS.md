# AGENTS.md

ruos：用 Rust 寫的能力式（capability）微核心教學作業系統，內建類 Docker 的 pod 機制，主要在瀏覽器裡操作（終端機 + 課程 + 即時系統圖）。
同一份核心跑在三個平台：WebAssembly（瀏覽器，檔案由 server `ruos serve` 提供）、RISC-V（QEMU `virt`）、ARM（QEMU `virt`，cortex-a72）。
方向（使用者確認，2026-10-30）：以 capability 與 pod 為主軸的教學系統；**只用 Rust**；不追 POSIX 相容（M11–M25 的 POSIX / WASI / C 已移除，留在舊 rust-os repo 的 tag `posix-era`）。
與 veryl2chip 無關。所有回覆用繁體中文。

## 指令

```sh
./run.sh riscv64                 # 建置並在 QEMU 開機（Ctrl-A x 離開）
./run.sh aarch64                 # 同上，ARM（QEMU 直接從 EL1 進入）
./run.sh aarch64-el2             # ARM，virtualization=on，從 EL2 進入再降到 EL1
./run.sh wasm [port]             # 建置 kernel.wasm + ruos serve（含 registry），開 http://127.0.0.1:8780（預覽：.claude/launch.json 的 `ruos` 設定，埠 8781）
./run.sh build riscv64 aarch64 wasm   # 只建置，輸出在 build/
./run.sh dist                    # 建全部，放進 ruos/assets（發布用）
./run.sh publish [--dry-run]     # 檢查 git 乾淨 → dist → cargo publish --allow-dirty（使用者自己發布）
NOBUILD=1 PODSRV=127.0.0.1:PORT ./run.sh riscv64   # 不重建；QEMU 經 10.0.2.100:80 連到那個 ruos serve
./test.sh [host] [riscv64] [aarch64] [aarch64-el2] [wasm]   # 預設全部；log 在 build/test-*.log
cargo test --release             # 主機端 crate：objstore（BLAKE3 對照 blake3 crate）、podimg、ruos
target/release/podimg ls --store build/store      # 映像清單；build / pack / verify URL NAME / hash FILE
node web/test/boot.mjs           # WASM 開機測試（需先 ./run.sh build wasm）
```

- 工具：QEMU 10.2（`qemu-system-riscv64` 用內建 OpenSBI `-bios default`）、rustc nightly、target `riscv64imac-unknown-none-elf`、`aarch64-unknown-none-softfloat`、`wasm32-unknown-unknown`；`llvm-objcopy` 取自 rustup 的 `llvm-tools`（run.sh 用 `rustc --print sysroot` 找）；Node 24。
- 核心一律 `--release`。ARM 用 softfloat target：核心不碰 FP/SIMD 暫存器，trap 不必存它們。wasm 建置關掉 debug info（`CARGO_PROFILE_RELEASE_DEBUG=false`，約 33 KB）。
- `kernel/` 是獨立 package（各 target 自己建置，`build.rs` 依 `CARGO_CFG_TARGET_ARCH` 加 linker script 或 wasm `--export`）；根目錄 `Cargo.toml` 是主機端 workspace（`exclude = ["kernel"]`）。
- QEMU 的 raw binary 啟動：RISC-V 由 OpenSBI 載入 0x80200000（a0 = hart、a1 = DTB）；ARM 映像開頭有 arm64 Image header（text_offset 0x80000 → 0x40080000，x0 = DTB）。
- QEMU 關機（SBI SRST / PSCI SYSTEM_OFF）結束碼都是 0，測試結果以主控台輸出判定。

## 套件（使用者的偏好：有合用的套件就用，沒有才自己寫）

- 核心：`fdt`（DTB；`dtb.rs` 只是包一層成核心用的小 API）、`talc` + `lock_api`（heap；鎖是會關中斷的 `RawIrqLock`）。
- 使用者空間：`talc` + `spinning_top`（libsys heap；`Grow` source 在用完時映射新的 Memory 物件或 `memory.grow`）、`blake3`（`pure`：裸機 target 沒有 C 函式庫）、`virtio-drivers`（blk、net；`virtio/` 只剩它的 `Hal`：DMA 用 `MEMORY_CREATE_DMA`，一般 heap 的 buffer 經 DMA bounce buffer）、`smoltcp`（net 的 TCP/IP）、`embedded-websocket`（libsys 的 WebSocket client）、`wasmparser`（wasmrt 驗證與解析模組）、`rand_chacha`（核心亂數）。
- 主機：`tiny_http` + `tungstenite`（ruos serve）、`toml` + `serde`（Podfile）、`ureq`（podimg 讀 registry）。
- 網頁不用框架：原生 ES module（`web/*.js`），SVG 自己畫。
- 仍是自己寫的，因為沒有合用的套件：核心本身、物件庫的格式與 pack、wasm 直譯器（`wasmi` 不支援 shared memory / atomics）、各服務的協定（store、fs、dock、console、remote）。

## 測試

`test.sh` 先建置全部平台，再為 QEMU 啟動一個共用的 ruos serve（`build/podsrv.log`），然後每個平台在新磁碟上開機兩次：第一次打 `tests/m3.input`，比對所有 `tests/mN.expect`、`tests/teach.expect` 與 `tests/shell.expect`（ERE，每行一個，三平台共用）加 `tests/mN.<平台>.expect`，並確認沒有 `panic:` 或 `FAILED`；第二次打 `tests/boot2.input`，比對 `tests/boot2.expect`（第一次 commit 的映像與物件要從磁碟回來）。每次開機後檢查 server（`post_checks`：可寫層、`podimg verify snap`），結果加進 log。
WASM 測試另外檢查 registry（ref、immutable blob、`podimg verify`）、系統圖的 snapshot（shell 的 channel 連到 console、dock、kube、store 與自己檔案系統的 overlay（資料與 control 兩條）），第一次開機時還故意在第 50 個回覆後斷一次 server 連線（`--drop-remote`，要看到 `remote: reconnected`）。shell 印出 `shell: ready` 後，QEMU 平台經 FIFO 從序列埠送入 `tests/m3.input`（太早送會被 OpenSBI 重設 UART 吃掉），WASM 由 `web/test/boot.mjs` 在同一時機送進核心 Worker。
自測（`kernel/src/selftest.rs`）：第 10 個 tick 印 `timer: 10 ticks at 100 Hz`，第 30 個 tick 印 `M1 OK`；init 結束（印 `init exited (code)`）且 M1 自測做完才關機。M2–M4 的核心測試在 `apps/ktest`（init 開機時先跑，印 `M2 OK`、`M3 OK`、`M4 OK`；handle 0 = store），之後 init 啟動 console 與 shell，測試腳本打字驗證 shell。新里程碑照這個模式加 `tests/mN*.expect`。

## 設計（已與使用者確認）

- **核心物件**：Task（位址空間 + handle 表）、Thread、Channel（訊息可夾帶 handle）、Memory（可共享 / COW）、Port（事件等待）、Quota（階層式資源預算）、Irq / Mmio。`spawn(image, quota, handles)` 建立程式。沒有 fork/exec、signal、uid、全域路徑、POSIX；檔案、主控台、裝置都是使用者空間服務，以 Channel 溝通。
- **pod（類 Docker）**：內容定址物件庫（BLAKE3、Merkle tree、層 + manifest），唯讀層 + COW 可寫層、commit；容器 = 父程式給的「名稱 → handle」名稱空間 + Quota；`Podfile.toml`；`dock` 服務與指令 `build/run/ps/stop/commit/images`；多架構 image（riscv64 / aarch64 / wasm32 變體，`dock run` 依平台選）。
- **WASM**：核心編成 wasm32 跑在瀏覽器的核心 Worker；每個 Task = 獨立 WebAssembly instance + 自己的 Worker；系統呼叫 = wasm import，用 SharedArrayBuffer + `Atomics.wait`（需要 COOP/COEP，ruos serve 會送）；記憶體配額 = Memory maximum。`ruos serve`：registry（內容定址 blob / manifest，HTTP，可永久快取）+ 每個 pod 的虛擬檔案系統（可寫層在 server，WebSocket，訊息格式即 IPC 格式）+ 網頁。
- **目錄**：`kernel/`、`objstore/`（物件庫格式，no_std + alloc，OS 與主機共用）、`abi/`（核心 ABI：syscall 號碼、錯誤碼、rights、物件型別，核心與 libsys 共用）、`libsys/`、`services/`（init, console, store, overlay, dock, shell, blk, net, wasmrt）、`apps/`、`podimg/`（主機端 Podfile → 多架構 image）、`ruos/`、`web/`、`run.sh`、`test.sh`。
- **里程碑**（每步三平台都要通過；先 RISC-V 再補 ARM、WASM）：
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
  17. 類 k8s（單機宣告式）：`kube` 服務、Deployment / Space、reconcile、重啟退避 — **完成**（之後：M18 capability 版 Service、設定、probe、滾動更新；M19 多節點、跨架構排程；M20 網頁叢集圖與課程）
- **2026-10-31 使用者決定：專注做 web 版的 OS**。測試只跑 wasm（`./test.sh wasm`），riscv64 / aarch64 不必每步都測（程式碼仍共用，先不刪）。

## kube（M17）

- `services/kube`（init 啟動；名稱空間只有 `dock`（init 經 dock 的 channel 送一端過去而得的新 client）與 `client`；shell 的名稱空間有 `kube`）。協定 `libsys/src/kube.rs`：文字，`apply` 第一行後面接 manifest（`services/kube/src/manifest.rs`，TOML + serde：`kind = "Deployment"|"Space"`、`name`、`space`、`image`、`args`、`replicas`、`restart = always|on-failure|never`、`[resources] mem tasks`、`[labels]`）；`get [pods|deploy|spaces]`、`describe`、`scale`、`delete`、`logs`。狀態只在記憶體（重開機就沒了；M19 搬到 ruos serve 的 KV）。
- 控制迴圈：每次被叫醒（client、dock 事件、計時）都 `reconcile`：pod 多了就移除最新的、少了就建 `NAME-N`（編號不重用），Pending / BackOff 到時間就 `dock run -d --quiet --name … --label kube=NAME [--space S]`（失敗 = Pending + 原因，例如 `worker: QuotaExceeded`，退避後重試）。pod 結束（dock 的 `watch` 事件，或每 2 s `pods` 全表比對）→ 依 restart 政策 BackOff（1 s 起加倍到 16 s，跑滿 10 s 就重設）或 Done。結束的 pod 留在 dock 裡（`logs` 看得到），重啟前才 `rm`。Deployment 的範本變了 → 全部 pod 換掉（Recreate）。
- dock 新增：`run --label K=V --space S --quiet`（輸出只留給 `logs`，不印到主控台）、`pods`（給程式讀的一行一個）、`watch`（之後每個 pod 的 `event start|exit|stop|rm ID NAME [CODE]`）、`space NAME SIZE TASKS` / `spaces`（Space = dock quota 下的子 quota，pod 的 quota 再從它分出來）。
- `apps/worker`：`worker` 一直跑（每 10 s 報一次），`worker SECONDS [CODE]` 時間到就以 CODE 結束。root 映像的 `/home/kube/`：`web.toml`、`flaky.toml`（on-failure）、`small.toml`（Space）、`crowd.toml`（超過 Space 的 task 數 → Pending）。測試：`tests/m3.input` 最後一段、`tests/m17.expect`。

## 發布：`cargo install ruos`（M16）

- 專案名稱是 **ruos**（2026-10-30 從 rust-os 改名；格式字串一併改：`ruos manifest 1`、`ruos index 1`、`ruos ns 1`、`.ruos-dir`、DTB 的 `ruos,wasm*`；`ROSPACK1` / `ROSDISK1` 沒改）。repo：`/Users/Shared/ccc/project/ruos`，GitHub `ccc-claude-opus/ruos`（2026-10-30 搬來，從新的 git 歷史開始；之前的歷史與 tag `posix-era` 留在舊的 rust-os repo，不在這裡）。
- `ruos/` 是唯一要發布的 crate（原本的 podsrv；不依賴 workspace 的其他 crate：物件 id 用自己的 `id.rs` + `blake3`）。指令：`ruos [web] [--port N] [--no-open]`（預設：啟動伺服器、開瀏覽器）、`ruos riscv64|aarch64 [--smp N]`（伺服器開在隨機埠的 thread，QEMU 的 guestfwd 連到它；核心寫到 `HOME/kernel-ARCH.bin`、磁碟 `HOME/disk-ARCH.img`）、`ruos serve …`（開發用：run.sh、test.sh、web/test/boot.mjs）。資料在 `$RUOS_HOME` 或 `~/.ruos`；第一次（或新版本）把內含的 registry 放到 `HOME/store`（物件只補缺的，refs 換新版，push 進來的映像留著）。
- 內含的系統：`./run.sh dist` 建三個平台後填 `ruos/assets/`（`web/*.html|js|css`、`kernel.wasm`、`kernel-riscv64.bin`、`kernel-aarch64.bin`、`store/` = `build/images`）；`ruos/build.rs` 把它打包成 `OUT_DIR/assets.bin`（路徑長度 u32 + 路徑 + 長度 u64 + 內容），`assets.rs` 以 `include_bytes!` 帶著。`ruos/assets` 在 .gitignore 裡，但 Cargo.toml 的 `include` 有它，所以 `cargo package` 會帶上（約 11 MiB，壓縮後 5.2 MiB，crates.io 上限 10 MiB）。沒有 assets 的建置（repo 裡平常的）只能 `ruos serve` 從磁碟供檔。
- 發布步驟：`./run.sh publish`（使用者自己跑；要 crates.io 帳號）：先確認 git 沒有未 commit 的變更，再 `dist`，再 `cargo publish --allow-dirty`（`ruos/assets` 被 git 忽略，cargo 把它們算成「未 commit」，所以要 `--allow-dirty`；多的引數交給 cargo，例如 `--dry-run`）。使用者端只需要穩定版 Rust（`rust-version = 1.85`）與（要跑 QEMU 時）QEMU。

## 教學介面（M11–M13）

- **shell 的檔案系統**：`fs::mount(store, "b{boot}-shell", "root")`（overlay；可寫層每次開機一個）。`root` 映像 = `services/shell/Podfile.root.toml`（`/bin/shell`、`/bin/hello`、`/bin/snoop`、`/etc/motd`、`/home/README.txt`、`/home/poem.txt`、`/home/count.sh`（腳本範例）、空的 `/tmp`）。`save` = overlay 的 `COMMIT root`：ref 更新、記進磁碟日誌，下次開機掛上的就是它（`tests/boot2` 檢查 `/home/note.txt`）。開機印 `/etc/motd`，從 `/home` 開始，提示 `CWD $ `（測試的 expect 用 `^(/[^ ]* )?\$ `）。
- **shell 語言**（`src/lang.rs`）：詞法（`'…'` 原樣、`"…"` 只展開 `$`、`\` 跳脫、`$(…)` 整段保留到展開時才解析、`#` 註解；`&` 單獨出現回錯誤），語法（list = pipeline 以 `;` / 換行、`&&`、`||` 相連；pipeline = 指令以 `|` 相連；指令 = 簡單指令（字 + `<` `>` `>>`）或 `if`/`elif`/`else`/`fi`、`for NAME [in 字…]; do … done`、`while … do … done`；關鍵字只在指令開頭才算），沒打完回 `Incomplete`，`main.rs` 就印 `> ` 再讀一行。展開：`$NAME` `${NAME}` `$?` `$#` `$@` `$0`–`$9` `$PWD`、`$(…)`（遞迴執行，輸出收進 `Sh.captures` 的最內層，去掉結尾換行）、`$((…))`（整數 + - * / % 與括號、變數名）；沒加引號的 `$` 展開結果依空白拆成多個欄位，沒加引號的 `*` `?` 比對最後一段路徑的檔名（排序、預設不含點檔，沒對到就留原字）。只有 `NAME=值` 的指令設變數。狀態 `Sh.status`：內建指令出錯 = 1、`grep` 沒找到 / `false` / `test` 為假 = 1、程式的結束碼；`exit` 結束 shell（腳本裡由 `sh` 執行時只結束腳本）。`while` 跑到 100000 圈就停。複合指令不能接在管線裡。`sh FILE ARGS`（自己的 `$0` `$1`…）、`. FILE`（同一份）。
- **shell 的指令**（`src/main.rs`：讀到完整的指令、`!N`、歷史；`dock …` 整行原樣交給 dock；`src/cmds.rs`：內建指令，每個收引數 + 上一段的輸出、回傳自己的輸出，由 shell 印出、寫檔、交給下一段或收進 `$(…)`）：檔案 `ls [-l]`（`IMAGE:DIR` 就看映像）、`cd`（無引數回 `/home`）、`pwd`、`cat`（`IMAGE:FILE` 看映像）、`tree`、`stat`、`mkdir [-p]`、`rmdir`、`rm [-r]`、`cp`、`mv`（進目錄保留檔名）、`touch`、`save`；`find [DIR] [-name PAT] [-type f|d]`、`basename`、`dirname`；文字 `echo [-n]`、`printf`（`%s` `%d` `%%`、`\n` `\t`，引數多就重複格式）、`head` / `tail [-n N]`、`wc [-l -w -c]`（只給一個旗標就只印數字）、`grep [-i -v -c]`（子字串，`^` `$` 錨點）、`sort [-r -n]`、`uniq [-c]`、`cut -d D -f LIST` / `-c LIST`、`tr [-d] SET1 SET2`（`a-z` 範圍）、`sed [-n]`（`s/A/B/[g]`、`d`、`p`，位址可以是行號、`$`、`/字/`，以 `;` 串接；一般文字）、`seq`、`tee [-a]`、`rev`、`tac`、`nl`；shell `test` / `[`（`-e -f -d -s -z -n`、`=` `!=`、`-eq` 等、`!`）、`true`、`false`、`read`（管線或主控台的一行）、`export`、`unset`、`env` / `set`、`which` / `type`、`sh`、`.` / `source`；系統 `date`（RTC，UTC）、`uptime`、`clear`（`ESC [2J ESC [H`，網頁終端機認得 `J`）、`history`、`sleep 秒`（可有小數）、`ps` / `kill`（dock ps / stop）；其他名稱 = `/bin/NAME`（或路徑）的程式：`read_mem` + `Store::spawn_module`，名稱空間只有新的 console client 與 `exec:`（引數），等它結束、非 0 才印結束碼。`run`、`dock`、`/bin` 程式直接寫主控台，不能接管線。
- **shell 的能力指令**（說明文字是中文，指令輸出用英文以便測試比對）：`handles`（用 `HANDLE_INFO` 逐一試 0..1023，連續 32 個空號就停；名稱取自名稱空間）、`ns`、`run IMAGE [NAME...]`（只給列出的名稱：`console` 是新的 console client、`store` 是新的 store client、`fs` 是 shell 檔案系統的新 client，其他名稱是 `dup`）、`mem`、`images`、`image NAME`（index → 各架構 manifest → layer）、`ls IMAGE:DIR`、`cat IMAGE:FILE`（`libsys::image::lookup` / `list`：由上層往下、whiteout 遮住下層；隱藏 `.ruos-*`）、`dock …`。名稱空間：`console`、`store`、`dock`、（原生）`net`。
- **dock** 多了 `ls POD [DIR]`、`cat POD FILE`（經 pod 的 overlay；超過 16 KiB 或有 NUL 就只報大小）。
- **apps/snoop**：印出自己的名稱、試遍 handle 0..1023 數自己有幾個、有 store 就列映像、有 console 就經它寫字。`run snoop` 與 `run snoop console store` 是第 2 課。
- **console** 只剩行模式（`WRITE`、`READ_LINE`、`CONNECT`）；Ctrl-C 丟掉正在打的那一行。↑ ↓（`ESC [ A` / `ESC [ B`；其他 ESC 序列丟掉）叫回之前交出去的行（最多 64 行，不重複記同一行），正在顯示時以 `ESC [ n D ESC [ K`（依顯示寬度）擦掉再印。網頁的 keydown 把方向鍵送成這兩個序列，Tab 不送任何東西（留在終端機）。
- **網頁**（`web/`）：左邊終端機（`main.js`），右邊分頁「課程」（`lessons.js`：十課，每課幾段說明 + 可點的指令，點了就以 UTF-8 + CR 送進鍵盤）與「系統」（`inspector.js`）。系統頁顯示時每秒向核心 Worker 要一次 snapshot；只有開機時間變了就只更新摘要（不重畫，保住鍵盤焦點）。圖：活著的任務排成一圈，兩個任務各持有同一條 channel 的一端就連線，remote channel 連到「伺服器」；點任務（或 quota 樹裡的晶片）列出它的 handle 表與「連到哪裡」（channel 的對端是誰的哪個 handle、共用的 Memory、task handle 指向誰）；quota 樹：根 → pods 總額 → 每個 pod（用量長條、任務數），任務掛在被記帳的 quota 下。手機寬度上下排列。
- **`kernel_snapshot` / `kernel_snapshot_len`**（只有 wasm32，`kernel/src/snapshot.rs`）：JSON `{uptime_ms, tasks: [{id, name, threads, exit, quota, handles: [{h, type, rights, id, peer?, queued?, task?, size?}]}], quotas: [{id, parent, mem, tasks, threads}]}`；物件的 id 是它在核心裡的位址（同一物件、channel 兩端都對得起來），`peer` 是 `"host"` 表示 remote channel，上限 `null` = 無上限。這是從外面看（像除錯器），系統裡面的程式仍然只看得到自己的 handle。`kernel_host.js` 的 `snapshot()`、kernel Worker 的 `{type:'snapshot'}` 訊息。
- **斷線重連**（`kernel_host.js` 的 `remoteLink`）：WebSocket URL 帶隨機的 `session=`（每次開機一個），ruos serve 以 `w-SESSION` 當 session 名稱（可寫層在 `STORE/pods/w-SESSION/`），所以重連後還是同一份可寫層；store 一次只有一個請求，斷線時沒收到回覆的請求在重連後重送（store 的請求都可以重做），斷線期間的請求先排隊；退避 250 ms 起、加倍到 8 s。原生平台還沒有：QEMU 的 guestfwd（10.0.2.100:80）每次開機只能連一次。

## 物件庫與映像（M4）

- `objstore/`：BLAKE3 用 `blake3` crate；`Id`（64 hex，可加 `b3:` 前綴）；文字格式：tree（`file|exec|dir|whiteout ID SIZE NAME`，依名稱排序 = Merkle tree）、manifest（`ruos manifest 1` / `arch` / `entry` / `layer`（底層在前）/ `env`）、index（`ruos index 1` / `name` / `manifest ARCH ID`，多架構）；`Objects::lookup` 由上層往下找路徑（whiteout 遮住下層）；pack（`ROSPACK1`、`obj ID LEN`+內容、`ref NAME:TAG ID`、`end`）；`Store`（記憶體中的物件 + refs，`closure` 求一個映像在某架構需要的物件）。
- `podimg/`（主機 CLI）：`build PODFILE --store DIR --programs ARCH=DIR...`（每個架構一個 layer + manifest，合成 index，`refs/NAME/TAG`）、`pack --store DIR --arch A --out F`（該架構需要的物件 + 所有 refs）、`ls`、`verify URL NAME`（從 registry 抓 index → manifest → tree → blob，逐一驗 BLAKE3）、`hash`。Store 目錄：`objects/<id>`、`refs/<name>/<tag>`。
- `Podfile.toml`（`podimg/src/podfile.rs`，`toml` + `serde`）：`name`、`tag`、`entry`、`from`（基底映像，layer 疊在上面）、`[programs]`（路徑 = 程式名，各架構從 `--programs` 目錄找，wasm 加 `.wasm`）、`[files]`（相對 Podfile 的檔案）、`[env]`。每個程式目錄都有一個。
- `ruos serve --store DIR` = registry：`GET /v1/blobs/ID`（`Cache-Control: public, max-age=31536000, immutable`）、`GET /v1/refs/NAME/TAG`、`GET /v1/refs`。
- `run.sh build P`：先把所有使用者程式建成三個架構（`build/user-ARCH/`），每個 Podfile 建進 `build/store`，再把 P 架構的 pack 與 init 放進 `build/boot-P/`（`boot.pack`、`init`），以 `ROS_BOOT` 讓核心 `build.rs` 嵌入。核心只認得 `init`；pack 以 Memory 物件交給 init（handle 1）。

## Quota、名稱空間、dock（M5）

- **Quota**（`kernel/src/quota.rs`）：記憶體 bytes、tasks、threads 三種上限；`charge` 要通過自己與所有祖先才記帳（`QuotaExceeded`）。核心給 init 根 quota（handle 2；原生 = frame allocator 管的全部 RAM，wasm = 1 GiB）。記帳的東西：每個 task（`Task.charge`）、thread（`Thread.charge`）、原生 task 自己的頁（`AddrSpace::map_zeroed`；頁表不算）、wasm task 的 linear memory（`maximum` = min(模組宣告, quota 剩餘)，記 initial 大小）、Memory 物件（建立者的 quota，`clone_cow` 記整個大小）。task 結束時就退費（`AddrSpace::release` 釋放使用者頁、`charge.take()`），不等最後一個 handle。`SPAWN` 改收 `abi::SpawnArgs`（含 quota handle，`NO_HANDLE` = 呼叫者的 quota，需 WRITE）；`QUOTA_CREATE`（需 MANAGE）、`QUOTA_INFO`。
- **名稱空間**（`libsys/src/ns.rs`，純使用者空間）：子程式的 handle 0 是 bootstrap channel，第一個訊息 = `ruos ns 1` + 每行一個名稱，帶著對應的 handles；`ros_start` 在 `main` 前 `ns::load()`，之後 `ns::get("console")`。父程式用 `Namespace::new().add(name, h)` 與 `ns.spawn(image, len, name, quota)` / `Store::spawn(name, &ns, quota)`。init 例外（核心直接給 handle 0/1/2）。
- **dock**（`services/dock`，名稱空間 `console`、`store`、`quota`（pods 的 quota，init 建 160 MiB / 32 tasks / 64 threads）、`client`）：文字協定（`libsys/src/dock.rs`）`run [-d] [--name N] [--mem SIZE] [--tasks N] IMAGE` → `ok ID NAME`、`ps`、`stop ID|NAME`、`wait ID|NAME`（pod 結束才回）。pod = 子 quota（預設 32 MiB、8 tasks、16 threads）+ 名稱空間（新的 console client、store client）+ 從 store 啟動的 task；pod 結束以 Port 綁 TERMINATED 得知。shell 的 `dock …` 指令；`apps/hog` 一次拿 1 MiB Memory 直到 quota 拒絕。
- 測試腳本在 `dock run -d` 之後 `sleep 500`（shell 指令），因為 wasm 的 Worker 啟動比原生切換慢。

## 檔案系統、commit、push（M6）

- 協定編碼（`libsys/src/wire.rs`）：op byte + 以 NUL 分隔的欄位（最後一欄是原始 bytes）；回覆以狀態 byte 開頭（`OK` / `NOT_FOUND` / `FAILED`）。
- store 多了：`OBJ_GET` / `OBJ_PUT`（新物件存在 store 記憶體）、`REF_SET`、`INDEX`、可寫層（upper：每個 key 一張 path → 檔案 / whiteout 表）`UP_WRITE` / `UP_READ` / `UP_LIST` / `UP_DELETE`、`PUSH`（映像的物件 + ref 送到 registry；pack 裡沒有的其他架構物件略過，registry 本來就有）。名稱空間有 `remote` 時（wasm32），UP_* 原封不動轉給 server，所以可寫層在 server；沒有時（QEMU）放在 store 記憶體，`PUSH` 回「no registry connection」。
- `services/overlay`（每個 pod 一個，dock 啟動，記在 dock 的 quota）：名稱空間 `store`、`fs`（pod 的請求）、`control`（dock：第一個訊息 `CONFIG key image`，之後 `COMMIT`）。合併檢視：upper（store 的 UP_*，key = `podN`）在上、映像各層在下（物件從 store 取，快取）；whiteout 遮住下層；`LIST` 合併各層。`COMMIT NAME`：upper → 新的 tree（刪除 = whiteout），疊到原映像每個架構的 manifest 上（資料與架構無關），新 index + ref。
- `libsys/src/fs.rs`：`Fs`（`read/write/delete/list`，整個檔案讀寫，≤ 60 KB；`commit`）。pod 的名稱空間多了 `fs`。`apps/writer` 讀 `/etc/motd`、遞增 `/data/count`、刪 `/etc/motd`。dock `commit POD IMAGE`、`push IMAGE`；shell `dock commit` / `dock push`。
- **remote channel**（wasm32）：核心的 `Channel::remote()` 端點，另一端是 host：寫入走 import `remote_send`（不能帶 handle），host 收到的訊息經 `kernel_remote_buf` / `kernel_remote` 進來。核心在 host 有 server 時（import `remote_open`）把它給 init 當 handle 3，init 轉給 store（`remote`）。`kernel_host.js` 以 WebSocket 連 `remoteUrl`（瀏覽器：頁面所在的 `/v1/remote`；Node 測試：ruos serve 的位址），連上前先排隊。
- ruos serve：HTTP 是 `tiny_http`，WebSocket 是 `tungstenite`（`req.upgrade` 之後 `WebSocket::from_raw_socket`）；`remote.rs`（每條 WebSocket 一個 session `sPID-N`；OBJ_PUT 寫 `objects/`、REF_SET 寫 `refs/`、UP_* 存在 `STORE/pods/SESSION/KEY/files/` + `whiteouts`）；`GET /v1/pods`、`GET /v1/pods/SESSION/KEY/files/PATH`。Node 測試結束後檢查 server 上的可寫層（`data/count = 2`）並 `podimg verify` 推上去的 `snap`。

## 裝置、磁碟、網路（M7）

- **核心裝置物件**（`kernel/src/device.rs`，只有原生）：從 DTB 找 `virtio,mmio`，讀 magic 與 DeviceID，存在的裝置以 `Mmio` + `Irq` 一對 handle 交給 init（handle 3 起）。`MMIO_MAP`（映射到使用者空間，device 屬性；記在 `AddrSpace.devices`，釋放時先拆，不當成 frame 釋放）、`MMIO_INFO`（實體位址、大小、virtio id）。中斷：PLIC / GIC 收到非 UART 的中斷 → `device::dispatch` 遮蔽它並設 READABLE；驅動處理完 `IRQ_ACK` 才解除遮蔽。`MEMORY_CREATE_DMA`（實體連續的頁，`frame::alloc_contiguous`）、`MEMORY_PHYS`（需 MANAGE）。virtio-mmio 槽在核心頁表裡也有映射（探測用）。ARM 的槽間隔 0x200，同一頁裡有別的裝置，驅動映射整頁（已知限制）。
- **virtio**（`virtio/`）：`virtio-drivers` 的 `RosHal` 與從 `Mmio` handle 建 `MmioTransport`（QEMU 用 `-global virtio-mmio.force-legacy=false`，MMIO version 2）。
- **blk**（`services/blk`）：`VirtIOBlk`（輪詢等待完成），`libsys::blk` 協定（`INFO` / `READ` / `WRITE`，512 bytes sector，每次最多 64 個）。
- **store 的磁碟日誌**（`services/store/src/disk.rs`）：sector 0 是 superblock（`ROSDISK1`、`used`、`boot`），之後依序附加 `obj ID LEN` + 內容、`ref NAME:TAG ID`；開機時重播，boot 計數加一。只記新的物件（pack 裡已有的不記）。磁碟是 blk 服務（原生）或 server 的磁碟（wasm：`DISK_INFO/READ/WRITE` 經 remote channel，ruos serve 存成 `STORE/disks/MACHINE.img`，`?machine=NAME`；瀏覽器用 `browser`，測試用 `test`）。pod 的可寫層 key 是 `b{boot}-pod{id}`，跨開機不撞名。
- **net**（`services/net`）：`VirtIONet` 當 `smoltcp` 的 device（`Rc<RefCell<…>>` 給 rx/tx token 共用），smoltcp 的 TCP socket 對應到 socket channel。固定位址 10.0.2.15/24、gateway 10.0.2.2（QEMU user 網路）。`libsys::net`：`Net::connect(ip, port)` 回傳一個 socket channel（資料雙向，關閉 = FIN）；`WebSocket` client（`embedded-websocket`）。
- store 在原生平台用 net 連 `10.0.2.100:80`（QEMU `guestfwd` 轉到 `PODSRV`；guestfwd 的 chardev 在 QEMU 啟動時就連上，所以 ruos serve 要先跑，而且每次開機只能一條連線），走 `/v1/remote` WebSocket，協定和 wasm 的 remote channel 完全相同：所以三個平台的 pod 可寫層都在 server、`dock push` 都能用。
- build：`build/images` 由 Podfile 建（每次重建，pack 從這裡來），複製進 `build/store`（registry，會累積 push 進來的映像，pods / disks 也在這）。init 先盤點 handle 再啟動驅動（啟動驅動會把 handle 搬走、空出槽位）。

## SMP（M8）

- QEMU `-smp ${SMP:-4}`。CPU 編號：RISC-V 用 `tp`（核心專用；使用者模式可改 tp，所以從使用者模式進核心時，從 trap frame 上方 16 bytes 的槽讀回，排程器切入 thread 時寫入 `set_thread_cpu`），ARM 用 `TPIDR_EL1`。開機 CPU = 0。`arch::start_cpus`：RISC-V 用 SBI HSM `hart_start`（開機 hart 不一定是 0：OpenSBI 會挑；PLIC context 依開機 hart 的 interrupt-controller phandle 找），ARM 用 PSCI `CPU_ON`；第二顆以後的 CPU 從 `_secondary`（`.text.boot`，同樣的 boot 頁表、ARM 同樣會從 EL2 降級）進入，各自 64 KB 堆疊（`SECONDARY_STACKS`），再設核心頁表、trap 向量、計時器（ARM：GIC CPU interface 與 banked PPI）後進 idle。外部中斷（UART、virtio）只送 CPU 0。
- 排程（`sched.rs`）：一個全域 run queue，每顆 CPU 自己的 current / prev / resched 與 idle context。**一個還在某顆 CPU 上（`on_cpu`）的 thread 絕不放進 run queue**：被喚醒或被搶占時只標記 `requeue`（在 thread 的 state 鎖下），由那顆 CPU 換走它之後在 `finish_switch` 放進 queue（新 thread 也從 `new_thread` 先呼叫 `finish_switch`）。否則兩顆 CPU 會各自挑到對方剛換下的 thread 並互相等待（曾在 channel 一來一回時死結）。喚醒時 `kick` 對閒置的 CPU 送 IPI（RISC-V：SBI IPI → supervisor software interrupt；ARM：GIC SGI 1），自己剛變閒置也可以送給自己。
- 殺掉 task 時，正在別顆 CPU 上跑的 thread 收到 IPI 換走；task 的記憶體要等它所有 thread 都離開 CPU（`Task.running` 歸零）才釋放（`task::try_release`，由 `finish_switch` 再呼叫）。TLB：RISC-V `sfence.vma` + SBI remote fence，ARM `tlbi vmalle1is`。tick 只有 CPU 0 計數（M1 自測），每顆 CPU 都做搶占與 sleeper 檢查。
- 測試：ktest 讓 4 個 thread 做同一份工作，要比一個 thread 做 4 倍快 1.5 倍以上（`M8 OK`；QEMU 的 MTTCG 讓 vCPU 平行，wasm 的 Worker 本來就平行）。

## wasm 程式在原生平台執行（M8）

- `services/wasmrt`：WebAssembly 直譯器（`module.rs` 用 `wasmparser` 驗證、解析並建每個函式的 block 側表；`exec.rs` 以明確的 frame / label 堆疊執行，不吃原生堆疊）。支援 Rust 給 wasm32 產生的指令：MVP、sign-ext、saturating 轉換、bulk memory、passive data（`__wasm_init_memory`）、atomics（原生原子操作；`wait` 用輪詢）、多值 block type。浮點用 nightly 的 `core_float_math`（`core::f64::math::floor` 等）。
- 啟動：名稱空間有 `wasm:module:LEN`（模組的 Memory 物件）與程式自己的名稱；wasmrt 第一件事是建立 bootstrap channel（必須落在 handle 0，給程式的 `ns::load`），保留 8 MiB linear memory（Memory 物件映射，`memory.grow` 只能長到這裡），執行 start 函式後呼叫 `ros_start`。程式的 `sys.call` 轉成真正的 syscall（`sys.rs`：指標參數換算到 linear memory 並檢查範圍，`SPAWN` 的參數結構也換算；`THREAD_CREATE` 開一個原生 thread、各自一個 instance、設 `__stack_pointer` 後呼叫 `ros_thread_start`；映射記憶體與裝置的呼叫回 `NotSupported`，和 wasm32 平台一樣）。trap → `wasmrt: trap: …`，以 Killed 結束。
- 選擇：映像沒有本機架構的變體時，store 給它的 wasm32 變體（`get_program`），overlay 也用 wasm32 manifest 的層；`Store::spawn` 看到 `\0asm` 且本機不是 wasm32 就改啟動 wasmrt。`podimg pack` 對這種映像打包 wasm32 的層（`Store::closure`）。Podfile 新增 `arches = "wasm32"`；測試映像 `hello-wasm`、`writer-wasm`（`apps/*/Podfile.wasm-only.toml`；run.sh 建所有 `Podfile*.toml`）。console 的預先輸入上限 256 行（測試腳本一次送完；超過的行會被丟掉，腳本就停住、等到逾時），行緩衝放在 static（64 KiB 的堆疊放不下）。

## 補強（M9）

- **FP/SIMD**：使用者程式用 `riscv64gc-unknown-none-elf` / `aarch64-unknown-none`（`run.sh` 的 `user_target_of`），核心仍是 imac / softfloat、從不碰 FP 暫存器；排程器在切換前存下換出 thread 的 FP（`arch::fp_save`：RISC-V f0–f31 + fcsr，先把 sstatus.FS 設成 Dirty；ARM q0–q31 + fpcr/fpsr，組語用 `.arch_extension fp/simd`），載入換入 thread 的（`fp_restore`），trap 不必存。新 thread 的 sstatus FS = Initial。ktest 讓 8 個 thread（多於 CPU）做浮點運算並比對結果。
- **ASID**：`mm/aspace.rs` 的 ASID 池（RISC-V 探測 satp 留得住幾位元，QEMU 是 16；ARM 用 8 位元、TTBR0 帶 ASID）；切換不再清整個 TLB，重複使用的 ASID 先在所有 CPU 清掉，池用完就用 0（每次切換全清）。指令快取改在載入程式時同步（`arch::sync_icache`：`fence.i` + SBI remote fence.i；`ic ialluis`）。頁表頁（含 root）記入 task 的 quota。
- **大檔案**：fs 的 `STAT` / `READ_AT` / `WRITE_AT`（數字用十進位文字欄位），`Fs::read/write` 以 60 KB 分段；store 的 `UP_READ_AT` / `UP_WRITE_AT` / `UP_STAT` / `OBJ_READ_AT` / `OBJ_PUT_MEM`（大物件用 Memory 物件交給 store），push 時大物件用 `OBJ_PUT_PART` 分段，ruos serve 組回 `objects/.part-ID` 並驗 hash。
- **dock**：pod 的名稱空間多了 `log`（libsys 的 `println!` 有 `log` 就寫到那裡），dock 保留每個 pod 最後 64 KB 輸出（`dock logs`）並照樣轉給 console；`dock rm`、`dock write POD PATH TEXT`（`\n` 等跳脫）、`dock build POD PODFILE`（Podfile 在 pod 的檔案系統裡：`from` + `[files]`，`[programs]` 只能在主機用 podimg）。`objstore::podfile`（`toml` + `serde`，no_std）由 podimg 與 dock 共用；`libsys::image`（`put`、`layer`、`derive`）由 overlay 的 commit 與 dock build 共用。
- console：Backspace 刪整個 UTF-8 字元、依顯示寬度（東亞全形 2 欄）送 `ESC [ n D` + `ESC [ K`，網頁終端機認得這兩個序列。

## 效能（M10）

- `apps/bench`（`bench`、`bench-wasm`）：系統呼叫（CLOCK）、channel 來回（兩個 thread、兩個 task，對端是 `peer`）、64 KiB Memory 物件建立 + 映射 + 解除、計算迴圈。測試腳本會跑兩者與 `ktest-wasm`（ktest 在 wasmrt 底下跑一遍）。QEMU（M2 Mac、TCG）參考值：系統呼叫 1–2 µs，channel 來回 40–70 µs，映射 70–130 µs（原本 740–920：每頁一次 TLB shootdown），wasmrt 1.5–2.6 M 次/秒（原本 < 1 M）；wasm 平台的系統呼叫約 7 µs（跨 Worker 的 Atomics 來回）。
- wasmrt：`compile.rs` 在載入時把每個函式解碼成 `Op` 陣列（立即數已解、block 的 else / end 已是索引），直譯器不再解 LEB128、不再查表找 block 結尾。要再快需要 JIT（例如以 wasmtime / cranelift 的 no_std 支援接上，需要可執行的映射）。
- `map_memory` 一次映射全部頁面、最後一次 TLB shootdown。
- 排程：每顆 CPU 的 current / prev 有自己的鎖，resched / idle 是 atomic，run queue（`QUEUE`）的鎖只在真的切換時拿；`QUEUED` 計數讓 trap_exit 不必上鎖就知道有沒有工作。thread 記住上次的 CPU（`Thread.cpu`），殺掉時對那顆送 IPI。
- `CHANNEL_RECV`：有訊息就讀，沒有就等；等到後以 ShouldWait 喚醒、使用者端再讀一次（`Channel::recv` 的迴圈）。不在 signal 的情境裡替別的 thread 讀：那可能在某個 task 的 handle 表鎖之下（關閉 handle 讓 channel 消失時），曾因此死結。`HANDLE_CLOSE` 也改成放開鎖之後才丟掉物件。

## 使用者程式（M2–M9）

- 根目錄 workspace 含 `objstore`、`podimg`、`abi`、`libsys`、`services/*`、`apps/*`（`default-members` 只有主機端 crate，所以 `cargo test` 只跑主機端）。workspace release profile `strip = true`。
- 原生：靜態 ELF（riscv64imac / aarch64 softfloat，使用者程式也不用 FP 暫存器，所以切換不存 FP）。`libsys` 提供 `_start`（`.text._start`）→ `ros_start(arg)` → `ros_main`（`libsys::entry!(main)`）→ `exit`。堆疊在 `abi::layout::STACK_TOP`（0x3f_ffff_0000，64 KB）。
- WASM：使用者程式用 `-Zbuild-std=core,alloc` + `+atomics,+bulk-memory,+mutable-globals`，`--import-memory --shared-memory --max-memory=64MiB`，匯出 `ros_start`、`ros_thread_start`、`__stack_pointer`（`run.sh` 的 `WASM_USER_FLAGS`，target dir `target/wasm-user`）。syscall = import `sys.call(nr, a0..a5)`。
- `libsys`：`Handle`（`close/dup/info/wait/kill`）、`spawn(name, &[Handle])`、`thread_create(f, stack, arg)`、`sleep_ms`、`clock_ns`、`print!`（每次呼叫一次 `DEBUG_WRITE`，行不會交錯）、panic → 印 `panic:` 並以 `Fault` 結束；`ipc.rs`：`Channel`（`create/write/read/recv`）、`Port`（`bind/wait/queue`）、`Memory`（`create/read/write/clone_cow/map/unmap`）、`Serial`、`wait_one`；`console.rs`：console 服務的協定（訊息第一個 byte：`WRITE` / `READ_LINE` / `CONNECT`）與 `Console` client、`cprintln!`；`store.rs`：store 服務的協定（`GET` 回 [0, 長度, entry] + 程式的 Memory handle；`LIST`；`CONNECT`）與 `Store` client（`program`、`spawn(name, handles)`、`list`、`connect`）；`heap.rs`：global allocator（first-fit，原生以 Memory 物件映射成長，wasm 用 `memory.grow`）。`spawn(image: Memory, len, name, handles)`。
- 程式：`services/init`（handle 0 = Serial、1 = boot pack、2 = 根 quota；直接從 pack 取出 store 程式啟動，其餘都經 store：ktest、console、dock、shell；shell 結束就停 dock、關 console 後結束）、`services/store`（名稱 `pack`、`client`，載入時驗證每個物件的 hash，依 `libsys::ARCH` 選 manifest）、`services/console`（名稱 `serial`、`client`，其餘 client 由 CONNECT 加入；Port 等待；行編輯：有人在等一行時邊打邊回顯，否則交出那一行時才回顯，所以腳本輸入的紀錄是有順序的）、`services/shell`（名稱 `console`、`store`、`dock`；`help/echo/images/run/mem/sleep/dock/exit`）、`services/dock`、`services/overlay`、`services/blk`、`services/net`、`services/wasmrt`、`apps/hog`、`apps/writer`、`apps/bench`、`apps/ktest`（名稱 `store`）、`apps/peer`（ktest 的對端，名稱 `peer`）、`apps/hello`（印出自己的名稱空間）、`apps/fault`。

## 核心架構（kernel/src）

- **亂數與時間**：`RANDOM`（abi 38，`libsys::random`）是 ChaCha20（`rand_chacha`），種子取自 DTB `/chosen` 的 `rng-seed`（QEMU virt 會放；wasm 由 `web/dtb.js` 以 `crypto.getRandomValues` 放）再混入時鐘。`CLOCK` 參數 1 = 牆上時鐘（開機時讀一次 RTC：riscv64 goldfish-rtc、aarch64 PL031、wasm 的 `ruos,wasm-rtc` 節點；沒有就 2026-01-01）。
- **核心堆疊只有 16 KiB**：syscall 裡別放大的區域陣列（曾在 `dispatch` 放 4 KiB，溢出壓壞別的 thread 的 trap frame）；大的放進 `#[inline(never)]` 的獨立函式或 heap。
- **從 registry 拉映像**：store 被問到不認得的映像（`GET` / `INDEX`）就從 server 拉（`Store::pull`：remote 的 `INDEX` 拿 index id，`OBJ_READ_AT` 分段取這個架構（否則 wasm32）的 manifest、各層 tree 與檔案，逐一驗 hash、記進磁碟日誌；日誌滿了就只放記憶體）。

開機流程：`arch::*::boot`（組語進入點、開 boot 分頁、跳到高位址）→ 解析 DTB 成 `boot::BootInfo` → `kmain`（main.rs，三平台共用）→ `heap::init` → `arch::init_memory`（`mm::init` 建核心頁表並切換）→ `init_traps` → 斷點自測 → `init_irq` → `time::start` → 開中斷；原生平台接著 `arch::idle()`（wfi），wasm 回到 host。

- `arch/mod.rs` 依 target 選 `riscv64` / `aarch64` / `wasm32`，每個提供同一組函式：`NAME`、`HAS_BREAKPOINT`、`console_write`、`init_memory`、`init_traps`、`init_irq`、`breakpoint`、`timer_frequency`、`now_ns`、`timer_start`、`enable_interrupts`、`disable_interrupts`（回傳原本是否開著）、`shutdown(code) -> !`；原生平台另有 `PHYS_OFFSET`、`KERNEL_OFFSET`、`phys_to_virt`、`pte`、`idle`。
- 共用：`dtb.rs`（FDT 讀取：walk、`find` 路徑可省 unit address、`find_compatible`、`stdout`、`reg` 用父節點 cells）、`boot.rs`（`BootInfo`：model、memory、reserved（/memreserve/ + /reserved-memory）、console `Device`、timebase；`usable(kernel)` 扣掉保留區、DTB、核心）、`console.rs`（`print!`/`println!`、輸入環形緩衝）、`sync.rs`（`IrqLock`：持有時關中斷，單核）、`heap.rs`（4 MB 靜態 arena，first-fit free list）、`mm/frame.rs`（bump + free list，經直接映射清零）、`mm/paging.rs`（通用多層頁表，描述子格式來自 `arch::pte`；`map` 自動用最大可用 block）、`time.rs`、`selftest.rs`。
- **位址配置**：
  - RISC-V Sv39：直接映射 `PHYS_OFFSET = 0xFFFF_FFC0_0000_0000`；核心在 `0xFFFF_FFFF_8020_0000`（實體 0x80200000）。boot 頁表 `BOOT_PT` 是 const 靜態表（1 GB 頁：identity、核心、低 4 GB 直接映射）。核心頁表：text RX、rodata R、data/bss RW（皆 global）、RAM 直接映射 RW、UART / PLIC 4 KB 頁。
  - AArch64：TTBR1 放核心，`PHYS_OFFSET = 0xFFFF_0000_0000_0000`，核心在 `0xFFFF_FFFF_C008_0000`（實體 0x40080000）；T0SZ = T1SZ = 16、4 KB granule 4 層；MAIR 0 = device nGnRnE、1 = normal WB。boot 表 `BOOT_L0`（組語填 [0]、[511]）同時當 TTBR0 / TTBR1。切換後 TTBR0 是空表（給之後的 user task）。
  - WASM：沒有分頁；DTB 由 host（`web/dtb.js` 的 `wasmMachine`）產生，寫進 `kernel_dtb_buf()`，再呼叫 `kernel_boot(len)`。
- **中斷**：PL011 的 RX 中斷要先清再讀（反過來的話，中間到的字元會遺失中斷）。RISC-V 用 SBI TIME 設計時器（`sie.STIE`），PLIC context = `interrupts-extended` 中第一個 (phandle, 9) 的位置，UART 16550 RX 中斷；ARM 用 virtual timer（`cntv_cval_el0`，PPI 取自 /timer 第三組 = 27）、GICv2（DTB `arm,cortex-a15-gic`）、PL011 RX/RT 中斷、PSCI conduit 取自 /psci `method`；WASM 由 host 呼叫 `kernel_irq(0, 0)`（timer，setInterval）、`kernel_irq(1, byte)`（console）。
- **trap**：RISC-V `stvec` 直接模式，`Frame` = x0..x31 + sepc + sstatus（272 bytes）；sscratch 在核心為 0、在使用者模式存目前 thread 的核心堆疊頂，入口據此分辨來源；返回使用者時（SPP = 0）重設 sscratch；ebreak 依指令長度跳過。ARM 16 個向量各 128 bytes 共用 `trap_common`，`Frame` = x0..x30 + elr + spsr + sp_el0；EL0 來的是向量 8（sync：SVC EC 0x15 = syscall，0x20/0x24 = page fault）/ 9（IRQ）。兩者都有 `trap_return` 標籤（新 thread 的 context 從這裡進使用者模式），trap 結尾都呼叫 `sched::trap_exit()`。
- **物件與 handle**（`object.rs`）：`Object::{Task, Thread, Channel, Port, Memory, Serial}`，`Handle { obj, rights }`，`HandleTable` 的 handle 值 = slot 索引（關閉後可重用），每個 task 上限 1024。`dup` 只能縮小 rights（需 DUP）、`wait` 需 WAIT、`spawn` 搬移的 handle 需 TRANSFER、`kill` 需 MANAGE。
- **task / thread**（`task.rs`）：`Task { id, name, aspace, handles, state(threads, exit, waiters) }`、`Thread { id, task, start, arch, state(run, exit, waiters) }`，全域 `TASKS` / `THREADS`。最後一個 thread 結束 → task 結束；task 結束 → 殺掉其餘 thread、清空 handle 表、喚醒等待者（exit code 寫進等待者的使用者記憶體）。task 1 = init，結束時通知 selftest 關機。使用者模式的例外 → `task::fault` 印 `fault: task N (name) ...: killed`，exit code = `Killed`。
- **signals**（`signal.rs`）：Task / Thread（TERMINATED）、Channel（READABLE、WRITABLE、PEER_CLOSED）、Serial（READABLE）各有 `SignalCell`；觀察者是一次性的 `Observer::Thread`（`WAIT_ONE`）或 `Observer::Port`（`PORT_BIND`），level triggered。阻塞時 `sched::block_current` 回傳 wait generation（`wgen`；`gen` 是 2024 保留字），喚醒（`wake_gen`）與逾時（`sched::timeout`）都要對得上，所以過期的觀察者不會吵醒後來別的等待。
- **Channel**（`channel.rs`）：一對端點，訊息 = bytes（≤ 64 KB）+ handles（≤ 16，需 TRANSFER，不能把端點送進自己）；端點最後一個 handle 關閉時對端得到 PEER_CLOSED。**Port**（`port.rs`）：封包佇列 {key, signals, data}。**Serial**（`serial.rs`）：核心主控台輸入環（4 KB）的物件，只給 init。
- **Memory**（`memory.rs`）：頁面以 `Arc<Page>` 計數（原生是 frame，wasm 是 heap 區塊）；`clone_cow` 共用頁面，寫入前 `make_private` 複製。原生可 `MEMORY_MAP`（`mm/aspace.rs` 的 VMA，放在 0x10_0000_0000 起，中間留 guard page）：共用頁面以唯讀映射，寫入 fault → `AddrSpace::fault` → `make_private` → `Memory::remap` 更新所有映射；核心寫入使用者 COW 頁（例如寫回結果）也走這條。AddrSpace 釋放時先拆掉映射，頁面屬於 Memory 物件不會被 `free_user` 釋放。wasm 的 linear memory 不能互相別名，`MEMORY_MAP` 回 `NotSupported`，只能 `MEMORY_READ/WRITE`。
- **syscall**（`syscall.rs`，三平台共用）：`dispatch` 回傳 `Sys::Ret(v)` 或 `Sys::Block`；阻塞的 thread 由喚醒者以 `sched::wake(t, v)` 給結果（原生寫進其 trap frame 的 a0/x0，WASM 呼叫 host `thread_resume`）。所以同一份 syscall 程式碼同時適用有核心堆疊的原生平台與事件驅動的 WASM。
- **排程**（`sched.rs`，原生）：單核 round robin，每 tick 搶占；核心不可搶占，只在 `trap_exit` 切換（`arch::switch` 存 callee-saved 暫存器）；開機 thread 是 idle（`IDLE` context，wfi）。死掉的 thread 放在 `dead`，離開它的堆疊後才釋放。每個 thread 16 KB 核心堆疊（`kstack.rs`，堆疊頂就是使用者 trap frame）。WASM 版是 `arch/wasm32/sched.rs`（thread 都在自己的 Worker 平行跑，只管睡眠與喚醒）。
- **位址空間**（`mm/aspace.rs`，原生）：每個 task 一個頁表，RISC-V 的 root 256..511 複製核心的（共用核心半邊），ARM 用 TTBR0（整張都是使用者的）；切換 thread 時 `arch::activate`（RISC-V 寫 satp + `sfence.vma` + `fence.i`，ARM 寫 TTBR0 + `tlbi vmalle1` + `ic iallu`，沒有 ASID）。核心讀寫使用者記憶體一律經頁表查詢（檢查 U / 寫入權限）再走直接映射，不需 SUM / PAN。`elf.rs` 載入 PT_LOAD。釋放時 `PageTable::free_user` 回收使用者頁與頁表。
- **WASM host**（`web/kernel_host.js`，瀏覽器 Worker 與 Node worker_threads 共用）：imports `env.console_write / timer_start / now_ms / shutdown / task_create / task_destroy / thread_start / thread_resume / thread_kill / user_read / user_write`；exports `kernel_boot / kernel_irq / kernel_syscall / kernel_fault / kernel_scratch / kernel_dtb_buf`。`shutdown` 丟 `KernelExit` 讓 wasm 堆疊回捲，所有進入核心的呼叫經 `enter()`。
  - task = 編譯好的 `WebAssembly.Module` + 一塊 shared `WebAssembly.Memory`（上下限由 `memoryLimits` 從 import section 讀出）；thread = 該 module 在自己 Worker 裡的一個 instance（`task_runtime.js` `runThread`；非 main thread 先設 `__stack_pointer` 再呼叫 `ros_thread_start(f, arg)`）。
  - syscall：thread 把號碼與參數寫進自己的控制區（SharedArrayBuffer：i32[0] 狀態 0/1/2，i64[1..7] 號碼與參數，i64[8] 結果），postMessage 通知核心 Worker，再 `Atomics.wait`；核心 `kernel_syscall` 回傳結果或 `i64::MIN`（阻塞，之後 `thread_resume`）。instance trap → `fault` 訊息 → `kernel_fault`。
  - 頁面鍵盤：隱藏的 textarea，`keydown` 處理 Enter / Backspace / Ctrl，`input` 處理文字（輸入法、手機鍵盤、貼上也能用）。
  - 瀏覽器：task Worker 由頁面建立（`main.js` 收到 `spawn` 就開 `task-worker.js`，並把 MessagePort 交給它；`kill` 時 terminate），因為巢狀 Worker 不是每個瀏覽器都能用（Claude 的內建瀏覽器就載不起來）。Node 測試直接用巢狀 worker_threads（`test/node_task_worker.mjs`）。
- **ruos serve**（`ruos/src/main.rs`）：靜態檔 + `/kernel.wasm`，每個回應帶 COOP `same-origin`、COEP `require-corp`、CORP `same-origin`；`--port 0` 自選埠並印 `ruos: listening on http://…`。
- 瀏覽器預覽：`./target/release/ruos serve --port 8781 --web web --kernel build/kernel.wasm`，頁面應顯示 `crossOriginIsolated`。
