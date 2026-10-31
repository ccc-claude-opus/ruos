# ruos

一個拿來教作業系統的能力式（capability）微核心，用 Rust 寫成，內建類 Docker 的 pod。
打開瀏覽器就能操作：左邊是終端機，右邊是課程，以及一張即時的「系統圖」，畫出每個任務手上有哪些 handle、誰能跟誰說話、每個 pod 用了多少資源。

它刻意不模仿 Unix：沒有 fork、沒有 pid、沒有 root、沒有全域的檔名。一個程式能做什麼，完全取決於別人交給它哪些 handle。

同一份核心跑在三種機器上：

| 平台 | 執行環境 | 說明 |
|---|---|---|
| WebAssembly | 瀏覽器 | 核心跑在 Worker 裡，每個任務是一個 wasm instance + 自己的 Worker |
| RISC-V | QEMU `virt`，OpenSBI | Sv39 分頁、PLIC、SBI HSM 多核 |
| ARM | QEMU `virt`，cortex-a72 | 4 層分頁、GIC、PSCI 多核；可從 EL1 或 EL2 開機 |

## 安裝（不用建置）

```sh
cargo install ruos
ruos                      # 啟動並在瀏覽器打開
ruos riscv64              # 在 QEMU 的 RISC-V 上開機（要先裝 QEMU）；Ctrl-A x 離開
ruos aarch64              # ARM
```

`ruos` 這個執行檔帶著整個系統（wasm 核心、網頁、QEMU 核心、映像庫），資料放在 `~/.ruos`。

## 從原始碼建置

需要：rustc nightly（含 `rust-src`、`llvm-tools`）與 target `wasm32-unknown-unknown`、`riscv64imac-unknown-none-elf`、`aarch64-unknown-none-softfloat`、`riscv64gc-unknown-none-elf`、`aarch64-unknown-none`；Node 24；QEMU 10（只有跑 RISC-V / ARM 才需要）。

```sh
./run.sh wasm             # 建置後開 http://127.0.0.1:8780（瀏覽器版）
./run.sh dist             # 建全部並放進 ruos/assets（發布 ruos 套件用）
./run.sh riscv64          # 在 QEMU 開機（Ctrl-A x 離開）
./run.sh aarch64          # ARM，從 EL1；aarch64-el2 從 EL2
./test.sh                 # 所有平台的完整測試
cargo test --release      # 主機端 crate：objstore、podimg、ruos
```

## 課程

網頁右邊有十六課，每一課都是幾段說明加上可以點的指令（點了就送進終端機）：

1. **先四處看看**：`ls`、`cat`、管線與導向、`tree /`、執行 `/bin` 的程式、`save`
1. **管線、變數與腳本**：`cut | sort | uniq -c`、`for`、`$(…)`、`||`、`sh count.sh 5`
1. **handle 就是能力**：`handles`、`ns` 看 shell 自己持有什麼
1. **只給需要的**：`run snoop` 與 `run snoop console store`，程式只拿得到你給的
1. **記憶體物件與寫入時複製**：`mem`
1. **內容定址的映像**：`images`、`image hello`、`ls hello:`、`cat hello:/etc/motd`
1. **pod：隔離與額度**：`dock run -d --mem 4M hog`，看 quota 樹被用滿
1. **可寫層與 commit**：`dock run --name w writer`、`dock commit w snap`
1. **自己做一個映像**：`dock write` 一個 Podfile、`dock build`
1. **同一份程式，三種機器**：多架構映像、wasmrt
1. **宣告想要的狀態（kube）**：`kube apply`、殺掉 pod 看它回來、`kube scale`、back-off、Space
1. **Service 與 Config**：`kube call echo hi`、只有 `uses` 的 pod 連得到、改 Config 觸發滾動更新、`kube rollout undo`、liveness
1. **叢集**：另開一頁 `?machine=b2` 多一個節點、`kube get nodes`、pod 分到哪台、關掉那頁看 pod 搬家
1. **上網**：`curl https://example.com | head`、`curl -o`、server 拒絕連內網、`http` 這個 capability
1. **函式（serverless）**：`kind = "Function"`、`fn call` 的冷 / 熱啟動、閒置縮到 0、timeout、`kv:counter`
1. **從外面呼叫函式**：`expose = true`、函式埠的網址、請求送到哪台機器、關掉所有分頁就是 503

「叢集」頁畫出 server 上 kube 的記錄：每個節點（機器）一張卡片，列出分到它的 pod 與狀態，下面是 Deployment 與最近的事件。

「系統」頁每秒更新一次：任務是圓點，兩個任務各持有同一條 channel 的一端就連一條線；點一個任務會列出它的 handle 表，並用文字說明每個 handle 連到哪裡；下方是 quota 樹，每個 pod 是一棵子樹。

## shell 指令

目標是讓學生能像在 Linux 上一樣練習。開機後在 `/home`，提示是 `目錄 $ `；檔案系統是唯讀的 `root` 映像加上這次開機的可寫層，`save` 把變更 commit 進 `root`，下次開機還在。↑ ↓ 翻之前的指令。

```text
/home $ cat poem.txt | cut -d ' ' -f 2 | sort | uniq -c
      1 capability
      2 sea
      1 sky
      1 that
/home $ for f in *.txt; do echo "$f: $(wc -l < $f) lines"; done
/home $ grep whale poem.txt || echo "no whale"
/home $ sh count.sh 4
/home $ save
```

### 語法

| 寫法 | 意思 |
|---|---|
| `a \| b` | 管線：a 的輸出交給 b |
| `a > F`、`a >> F`、`a < F` | 輸出寫進檔案（覆蓋 / 附加）、從檔案讀入 |
| `a ; b` | 依序執行（換行也一樣） |
| `a && b`、`a \|\| b` | a 成功才執行 b、a 失敗才執行 b |
| `'…'`、`"…"`、`\x` | 原樣不展開、只展開 `$`、跳脫一個字元 |
| `X=值`、`$X`、`${X}` | 設定與取用變數 |
| `$?`、`$#`、`$0`–`$9`、`$@`、`$PWD` | 上一個指令的狀態、腳本引數個數、引數、全部引數、目前目錄 |
| `$(指令)` | 命令替換：換成指令的輸出 |
| `$((運算式))` | 整數運算：`+ - * / %` 與括號 |
| `*`、`?` | 比對檔名（沒對到就保留原字） |
| `if 指令; then …; elif …; else …; fi` | 條件（看指令的狀態） |
| `for X in a b c; do …; done` | 迴圈（省略 `in` 就是腳本引數） |
| `while 指令; do …; done` | 條件成立時重複 |
| `# 註解` | 到行尾都忽略 |
| ↑ ↓ | 叫回之前打過的指令 |

沒打完的 `if`、`for`、`while` 或引號會出現 `>` 提示，接著打下一行。不支援背景執行 `&`（請用 `dock run -d`）、函式、`case`。

### 檔案

| 指令 | 用法 | 說明 |
|---|---|---|
| `ls` | `ls [-l] [PATH...]` | 列出目錄；`-l` 顯示種類與大小；`ls IMAGE:DIR` 看映像裡的目錄 |
| `cd` | `cd [DIR]` | 切換目錄；不加參數回 `/home` |
| `pwd` | `pwd` | 目前目錄 |
| `cat` | `cat [FILE...]` | 印出檔案（沒有參數就印輸入）；`cat IMAGE:FILE` 看映像裡的檔案 |
| `tree` | `tree [DIR]` | 以樹狀列出目錄底下的一切 |
| `stat` | `stat PATH` | 是檔案還是目錄、大小 |
| `find` | `find [DIR] [-name PAT] [-type f\|d]` | 遞迴找檔案，`PAT` 可用 `*` `?` |
| `mkdir` | `mkdir [-p] DIR...` | 建目錄；`-p` 連中間的目錄一起建 |
| `rmdir` | `rmdir DIR...` | 刪空目錄 |
| `rm` | `rm [-r] PATH...` | 刪檔案；`-r` 連目錄與內容一起刪 |
| `cp` | `cp FROM TO` | 複製檔案（`TO` 是目錄就放進去） |
| `mv` | `mv FROM TO` | 搬移或改名 |
| `touch` | `touch FILE...` | 檔案不存在就建一個空的 |
| `basename` | `basename PATH [SUFFIX]` | 路徑的最後一段（可去掉副檔名） |
| `dirname` | `dirname PATH` | 路徑的目錄部分 |
| `save` | `save` | 把這次開機的檔案變更 commit 進 `root` 映像，下次開機還在 |

### 文字（都可以接管線）

| 指令 | 用法 | 說明 |
|---|---|---|
| `echo` | `echo [-n] TEXT...` | 印出文字；`-n` 不換行 |
| `printf` | `printf FORMAT ARGS...` | 格式化輸出：`%s` `%d` `%%`、`\n` `\t`；引數多就重複格式 |
| `head` | `head [-n N] [FILE...]` | 前 N 行（預設 10） |
| `tail` | `tail [-n N] [FILE...]` | 後 N 行（預設 10） |
| `wc` | `wc [-l] [-w] [-c] [FILE...]` | 行數、字數、位元組數；只給一個旗標就只印那個數字 |
| `grep` | `grep [-i] [-v] [-c] WORD [FILE...]` | 含有 WORD 的行；`-i` 不分大小寫、`-v` 反過來、`-c` 只數行數；`^`、`$` 是行首、行尾；沒找到時狀態是 1 |
| `sort` | `sort [-r] [-n] [FILE...]` | 排序；`-r` 反向、`-n` 依開頭的數字 |
| `uniq` | `uniq [-c] [FILE...]` | 去掉相鄰的重複行；`-c` 加上次數 |
| `cut` | `cut -d D -f LIST` / `cut -c LIST` | 取出欄位（以 D 分隔）或字元；`LIST` 如 `1,3`、`2-4`、`3-` |
| `tr` | `tr SET1 SET2` / `tr -d SET` | 換字元或刪字元；可用範圍 `a-z` 與 `\n` |
| `sed` | `sed [-n] 'SCRIPT' [FILE...]` | `s/A/B/[g]` 取代、`d` 刪行、`p` 印行，可加位址（行號、`$`、`/字/`），以 `;` 串接；`-n` 只印 `p` 的行 |
| `seq` | `seq [FIRST [STEP]] LAST` | 一串數字 |
| `tee` | `tee [-a] FILE...` | 輸入照樣輸出，同時寫進檔案；`-a` 附加 |
| `rev` | `rev [FILE...]` | 每一行左右顛倒 |
| `tac` | `tac [FILE...]` | 行的順序顛倒 |
| `nl` | `nl [FILE...]` | 加上行號 |

### shell

| 指令 | 用法 | 說明 |
|---|---|---|
| `test`、`[` | `test EXPR`、`[ EXPR ]` | 條件：`-e -f -d -s` 檔案、`-z -n` 字串、`=` `!=`、`-eq -ne -lt -le -gt -ge`、`!`；為假時狀態是 1 |
| `true`、`false` | | 狀態 0、狀態 1 |
| `read` | `read NAME...` | 讀一行（從管線或鍵盤）存進變數 |
| `export` | `export NAME=值...` | 設定變數 |
| `unset` | `unset NAME...` | 刪除變數 |
| `env`、`set` | `env` | 列出所有變數 |
| `sh` | `sh FILE [ARGS...]` | 執行腳本（有自己的 `$1`…） |
| `.`、`source` | `. FILE` | 在目前的 shell 裡執行腳本 |
| `which`、`type` | `which NAME...` | 是內建指令還是 `/bin` 的程式 |
| `history` | `history` | 打過的指令（有編號） |
| `!N` | `!3` | 再執行第 N 個指令 |
| `help` | `help` | 指令說明 |
| `exit` | `exit [CODE]` | 離開 shell（關機） |

### 系統

| 指令 | 用法 | 說明 |
|---|---|---|
| `date` | `date` | 現在的日期時間（UTC） |
| `uptime` | `uptime` | 開機多久了 |
| `clear` | `clear` | 清除畫面 |
| `sleep` | `sleep SECONDS` | 等幾秒（可有小數，如 `0.5`） |
| `ps` | `ps` | 列出 pod（同 `dock ps`） |
| `kill` | `kill POD...` | 停止 pod（同 `dock stop`） |
| 其他名稱 | `hello`、`/bin/snoop` | 執行 `/bin` 裡（或路徑指定）的程式，只給它一個主控台和引數 |

### 網路（`curl`）

| 指令 | 用法 | 說明 |
|---|---|---|
| `curl` | `curl [-i] [-I] [-s] [-f] [-X METHOD] [-H 'K: V'] [-d DATA] [-o FILE] URL` | HTTP / HTTPS 請求；輸出可以接管線，`-o` 存成檔案，`-f` 遇到 4xx/5xx 就失敗（`$?` = 22） |

瀏覽器裡的網頁不能直接連別的網站，所以請求由 server（`ruos serve`）代為發出、會自動跟著轉址，最多 10 秒、4 MiB。server 不會連到它自己那邊的網路（loopback、10.*、172.16–31.*、192.168.*、169.254.*、100.64.*、IPv6 的 ULA / link-local），轉址或 DNS 指過去也一樣；要開放某個位址用 `ruos serve --http-allow ADDR:PORT`，整個關掉用 `--no-http`。在系統裡，只有拿到 `http` 這個 capability 的程式能上網：shell 有，`run IMAGE http` 才會交給別的程式。

### 能力（capability）

| 指令 | 用法 | 說明 |
|---|---|---|
| `handles` | `handles` | shell 持有的 handle：編號、物件型別、權限（`d` 複製、`t` 轉交、`w` 等待、`r` 讀、`W` 寫、`m` 管理）、名稱 |
| `ns` | `ns` | shell 的名稱空間（名稱 → handle） |
| `run` | `run IMAGE [NAME...]` | 啟動映像，只交給它列出的名稱（`console`、`store`、`fs`、`dock` 等），例如 `run snoop console store` |
| `mem` | `mem` | 示範 Memory 物件的寫入時複製（copy on write） |

### 映像（image）

| 指令 | 用法 | 說明 |
|---|---|---|
| `images` | `images` | 所有映像、它們的 index 雜湊與架構 |
| `image` | `image NAME` | 映像的 index、各架構的 manifest 與 layer |
| `ls` | `ls IMAGE:DIR` | 映像裡的目錄（各 layer 疊起來，列出每個物件的雜湊） |
| `cat` | `cat IMAGE:FILE` | 映像裡的檔案 |

### pod（`dock`）

| 指令 | 用法 | 說明 |
|---|---|---|
| `dock run` | `dock run [-d] [--name N] [--mem SIZE] [--tasks N] IMAGE [ARGS]` | 以 pod 啟動映像：自己的 quota、名稱空間、可寫層；`-d` 在背景 |
| `dock ps` | `dock ps` | pod 的狀態、記憶體與任務用量 |
| `dock stop` | `dock stop POD` | 停止 pod |
| `dock logs` | `dock logs POD` | pod 最近的輸出 |
| `dock rm` | `dock rm POD` | 移除已結束的 pod |
| `dock ls` | `dock ls POD [DIR]` | pod 看到的目錄 |
| `dock cat` | `dock cat POD FILE` | pod 裡的檔案 |
| `dock write` | `dock write POD FILE TEXT` | 寫檔案到 pod 裡（`\n` 是換行） |
| `dock commit` | `dock commit POD IMAGE` | pod 的可寫層 → 新映像 |
| `dock build` | `dock build POD PODFILE` | 依 pod 裡的 Podfile 建新映像 |
| `dock push` | `dock push IMAGE` | 把映像推到 registry（ruos serve） |

### 宣告式 pod（`kube`，類似 Kubernetes）

寫下「想要的狀態」（TOML manifest），kube 會一直比對 dock 回報的實際狀態，並把它修正過來：pod 少了就補、多了就移除、結束了就依重啟政策重啟（等待時間 1 s 起加倍，最多 16 s）。範例在 `/home/kube`。

| 指令 | 用法 | 說明 |
|---|---|---|
| `kube apply` | `kube apply FILE...` | 建立或更新 Deployment（N 個同映像的 pod）或 Space（一群 pod 共用的 quota）；pod 範本變了就全部換掉 |
| `kube get` | `kube get [pods\|deploy\|nodes\|svc\|configs\|spaces]` | 列表（pod：READY、STATUS、RESTARTS、REV、AGE、NODE） |
| `kube describe` | `kube describe NAME` | Deployment 的設定、每個 pod 的狀態與最近的事件 |
| `kube scale` | `kube scale NAME N` | 改 replicas |
| `kube delete` | `kube delete NAME` / `kube delete config NAME` / `kube delete space NAME` | 刪掉 Deployment（與它的 pod）或 Function；Config、Space 沒有人用時才能刪 |
| `kube logs` | `kube logs POD` | pod 的輸出 |
| `kube rollout` | `kube rollout status\|history\|undo NAME` | 滾動更新的進度、歷史、退回上一版 |
| `kube call` | `kube call SERVICE TEXT` | 經 service 交給一個 ready 的 pod，印出它的回答 |

**叢集**：連到同一個 ruos serve 的機器就是一個叢集，每台機器是一個節點（瀏覽器再開一頁 `?machine=b2` 就多一個節點）。記錄（想要的狀態與每個 pod 的狀態）放在 server 上；一台機器的 kube 是 leader（租約），由它決定每個 pod 在哪個節點（活著、架構合、`node = "…"` 指定的、pod 最少的）；每台機器的 kube 跑分給自己的 pod。節點不見了，它的 pod 會在別的節點重建；leader 不見了，別台接手。`kube get nodes` 列出節點。

**函式（serverless）**：`kind = "Function"` 沒有固定的 pod，請求來了才在那台機器上啟動一個 instance（冷啟動，約幾十 ms），之後留著接下一個請求，閒置 `idle` 秒就收掉，最後縮到 0；超過 `timeout` 的請求會被終止。函式是 stateless 的，要保存資料就宣告 KV namespace（`uses = ["kv:counter"]`），資料放在 server 上。設計見 [doc/serverless.md](doc/serverless.md)。

| 指令 | 用法 | 說明 |
|---|---|---|
| `fn call` | `fn call NAME [TEXT...]` | 呼叫一個函式，印出回答 |
| `fn ls` | `fn ls` | 同 `kube get fn`：instance、請求數、冷啟動次數與時間 |
| `kv` | `kv ls NS [PREFIX]` / `kv get NS KEY` / `kv put NS KEY VALUE...` / `kv rm NS KEY` | KV namespace 的內容 |

**從外面呼叫**：寫了 `expose = true` 的函式，在 ruos serve 的函式埠（預設主埠加 10，例如 `http://127.0.0.1:8790/fn/NAME/`）有網址，`kube get fn` 的 URL 欄和「叢集」頁都會列出來。請求由 server 送進一台機器（已經有 instance 的優先），函式用 `libsys::func::serve_http` 拿到 method、path、query、headers、body。函式埠和 OS 的頁面是不同的 origin，函式的網頁碰不到 OS 的連線。沒有任何機器（分頁）開著時回 503。設計見 [doc/ingress.md](doc/ingress.md)。

**Service 沒有位址**：`serves = "echo"` 的 pod 是 service echo；`uses = ["echo"]` 的 pod 名稱空間裡才有 `svc:echo`，沒列的就連不到（預設全部拒絕）。連線時 kube 把一條新 channel 的一端交給一個 ready 的 pod，之後兩邊直接談。kube 每 2 s ping 一次，不回應的 pod 不再分到連線，7 s 後重啟。**Config**（`kind = "Config"`，`[files]`、`[env]`）疊成映像上的一層；改了 Config 或範本就會滾動更新：先多一個新 pod，它 ready 了再移除一個舊的。

```toml
kind = "Deployment"
name = "web"
image = "worker"
replicas = 2
restart = "always"   # always | on-failure | never
space = "small"      # 選填：先 apply 一個 kind = "Space" 的 manifest
serves = "echo"      # 選填：這些 pod 是 service echo
uses = ["echo"]      # 選填：這些 pod 可以呼叫的 service
config = "greeting"  # 選填：kind = "Config" 的檔案與環境變數
node = "b2"          # 選填：只放在這個節點
[resources]
mem = "8M"
tasks = 2
[labels]
app = "web"
```

指令都是 shell 內建的（Rust）；`kube` 的輸出可以接管線（`kube get pods | grep web`）。`grep`、`sed` 比對一般文字（`grep` 認得 `^`、`$`），不是正規表示式。`run`、`dock` 與 `/bin` 的程式直接寫主控台，不能接管線。

## 設計

**核心物件**：Task（位址空間 + handle 表）、Thread、Channel（訊息可夾帶 handle）、Port（等待事件）、Memory（可共享、copy on write）、Quota（階層式的記憶體 / 任務 / 執行緒預算）、Mmio / Irq（給使用者空間驅動）。程式用 `spawn(image, quota, handles)` 啟動；主控台、檔案、磁碟、網路、映像庫都是使用者空間的服務，彼此以 Channel 溝通。

**名稱空間**：子程式的 handle 0 是一條 bootstrap channel，第一則訊息列出「名稱 → handle」。父程式決定給什麼，子程式就只有那些。

**pod**：映像存在內容定址的物件庫（BLAKE3；tree / manifest / index 構成 Merkle tree；一個映像可帶多個架構）。pod = 唯讀映像層 + copy-on-write 可寫層 + 自己的 Quota + 名稱空間。可寫層可以 commit 成新映像、push 到 registry。映像以 `Podfile.toml` 描述，主機上用 `podimg` 建，系統裡用 `dock build`。

**瀏覽器版**：系統呼叫是 wasm import，經 SharedArrayBuffer + `Atomics.wait` 進核心（需要 COOP / COEP，`ruos serve` 會送）。記憶體配額就是 linear memory 的上限。核心有一條 remote channel 經 WebSocket 連到 `ruos serve`：pod 的可寫層、磁碟、registry 都在那裡；連線斷了會自動接回同一個 session。

**原生版**：virtio-blk 磁碟、virtio-net + smoltcp 的網路（store 以同一套協定連到 `ruos serve`），預設 4 顆 CPU。只有 wasm32 版本的映像，在 RISC-V / ARM 上由 wasmrt（自己寫的 wasm 直譯器）執行。

## 目錄

| 路徑 | 內容 |
|---|---|
| `kernel/` | 核心（`arch/` 下有 riscv64、aarch64、wasm32）；`snapshot.rs` 給網頁的系統圖 |
| `abi/` | 系統呼叫號碼、錯誤碼、rights（核心與使用者空間共用） |
| `libsys/` | 使用者空間函式庫：IPC、名稱空間、各服務的 client、映像 |
| `objstore/` | 物件庫格式與 pack（OS 與主機共用） |
| `services/` | init、console、shell、store、overlay、dock、kube、blk、net、wasmrt |
| `apps/` | snoop、hello、writer、hog、bench、ktest 等 |
| `podimg/` | 主機工具：建映像 |
| `ruos/` | 發布的套件：`ruos` 指令（伺服器：registry + 可寫層 + 磁碟 + 網頁；開瀏覽器版、開 QEMU），發布時內含預先建好的系統 |
| `web/` | 網頁：終端機、課程（`lessons.js`）、系統圖（`inspector.js`）、核心 Worker |
| `tests/` | 測試輸入與預期輸出 |

開發細節（記憶體配置、協定、排程規則、測試）寫在 [AGENTS.md](AGENTS.md)。

## 歷史

1–10 版做出核心、pod 與三個平台。11–25 版曾試著走 POSIX 相容（WASI / WASIX、C 程式、coreutils、POSIX shell、tokio），之後決定專心做能力式與 pod 的教學系統，那些都移除了（留在搬到這個 GitHub repo 之前的舊 repo，tag `posix-era`）。

## 授權

MIT（見 [LICENSE](LICENSE)）。
