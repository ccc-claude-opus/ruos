// Guided lessons: short explanations and commands to try. Clicking a
// command types it into the terminal; the system view shows what happened.

export const LESSONS = [
  {
    title: '1. 先四處看看',
    text: [
      '開機後你在 `/home`。檔案來自唯讀的 root 映像，你做的變更放在這次開機的可寫層裡；`save` 會把它們 commit 進映像，下次開機還在。',
      '指令可以用 `|` 串起來，用 `>`、`>>` 寫進檔案。打 `help` 看所有指令。',
    ],
    steps: [
      { cmd: 'ls -l', note: '目前目錄的檔案' },
      { cmd: 'cat README.txt', note: '看一個檔案' },
      { cmd: 'cat poem.txt | grep sea | wc', note: '管線：行數、字數、位元組數' },
      { cmd: 'echo 我的第一個檔案 > note.txt', note: '寫進檔案' },
      { cmd: 'cat note.txt', note: '讀回來' },
      { cmd: 'tree /', note: '整個檔案系統' },
      { cmd: 'hello', note: '執行 /bin/hello' },
      { cmd: 'save', note: '存進 root 映像，重新開機後還在' },
    ],
  },
  {
    title: '2. 管線、變數與腳本',
    text: [
      'shell 是一個小小的 sh：`;`、`&&`、`||`，變數與 `$(…)`、`$((…))`，`*.txt` 這類樣式，`if`、`for`、`while`。打不完的 `for` 或 `if` 會出現 `>`，繼續打下一行就好。',
      '按 ↑ ↓ 可以叫回之前打過的指令。`sed` 和 `grep` 比對的是一般文字，不是正規表示式。',
    ],
    steps: [
      { cmd: 'cat poem.txt | cut -d " " -f 2 | sort | uniq -c', note: '每一行的第二個字，數一數' },
      { cmd: 'for f in *.txt; do echo "$f 有 $(wc -l < $f) 行"; done', note: '迴圈、樣式與命令替換' },
      { cmd: 'grep whale poem.txt || echo 沒有鯨魚', note: '前一個失敗才做' },
      { cmd: 'echo $((6 * 7))', note: '算術' },
      { cmd: 'cat count.sh', note: '一個腳本' },
      { cmd: 'sh count.sh 5', note: '執行它，$1 = 5' },
      { cmd: 'sed "s/sea/ocean/g" poem.txt > ocean.txt', note: '取代後存成新檔' },
      { cmd: 'help', note: '所有指令' },
    ],
  },
  {
    title: '3. handle 就是能力',
    text: [
      '在 ruos 裡，程式能碰到的每一樣東西（通道、記憶體、別的任務）都是一個 handle：核心幫每個任務保管一張表，handle 只是表裡的編號。',
      '沒有全域的檔名、沒有 pid、沒有 root 帳號。一個程式手上有什麼 handle，它就只能做那些事。',
      '先看看 shell 自己有什麼。再到右邊「系統」頁，點 shell 那個任務，看它的 handle 連到誰。',
    ],
    steps: [
      { cmd: 'handles', note: 'shell 的 handle 表：編號、物件型別、權限' },
      { cmd: 'ns', note: '名稱空間：啟動 shell 的人（init）給它的名稱 → handle' },
    ],
  },
  {
    title: '4. 只給需要的',
    text: [
      '啟動程式時，由父程式決定要給它哪些 handle。`run IMAGE 名稱…` 只把列出的名稱交給新程式。',
      'snoop 會試著摸遍所有 handle 編號：它只找得到別人給它的那幾個，猜編號沒有用。',
    ],
    steps: [
      { cmd: 'run snoop', note: '什麼都不給：看不到任何映像，連主控台都沒有' },
      { cmd: 'run snoop console store', note: '給它主控台和映像庫' },
      { cmd: 'run hello console', note: 'hello 只印出它拿到的名稱' },
    ],
  },
  {
    title: '5. 記憶體物件與寫入時複製',
    text: [
      '記憶體也是物件。複製一個記憶體物件時，兩份先共用同樣的頁面，有人寫入時才真的複製那一頁（copy on write）。',
    ],
    steps: [{ cmd: 'mem', note: '原本的還是 hello，副本被寫成 HELLO' }],
  },
  {
    title: '6. 內容定址的映像',
    text: [
      '程式以「映像」存放：每個檔案、目錄、manifest 都是一個物件，名稱就是內容的 BLAKE3 雜湊。內容一樣，名稱就一樣；內容一改，名稱跟著變，所以不可能被偷改。',
      '一個映像 = index（各架構的 manifest）→ manifest（入口程式、layer）→ layer（目錄樹）→ 檔案。',
    ],
    steps: [
      { cmd: 'images', note: '映像與它們有哪些架構' },
      { cmd: 'image hello', note: 'index、三個架構的 manifest、各自的 layer' },
      { cmd: 'ls hello:', note: 'layer 的最上層目錄（每一項都是一個雜湊）' },
      { cmd: 'cat hello:/etc/motd', note: '映像裡的檔案' },
    ],
  },
  {
    title: '7. pod：隔離與額度',
    text: [
      'pod 是一個執行中的映像：自己的 quota（記憶體、任務數上限）、自己的名稱空間、自己的可寫層。',
      'hog 會一直要記憶體直到被 quota 拒絕。看右邊的 quota 樹：它的額度用滿了，但別的部分不受影響。',
    ],
    steps: [
      { cmd: 'dock run -d --name h1 --mem 4M hog', note: '在背景跑 hog，限 4 MiB' },
      { cmd: 'dock ps', note: 'pod 的狀態與用量' },
      { cmd: 'dock stop h1', note: '停掉它：額度全數收回' },
    ],
  },
  {
    title: '8. 可寫層與 commit',
    text: [
      '映像是唯讀的。pod 寫入的東西放在它自己的可寫層（存在伺服器上）。commit 把可寫層變成一個新的 layer，疊在原映像上，成為新映像。',
    ],
    steps: [
      { cmd: 'dock run --name w writer', note: 'writer 讀 /etc/motd、把 /data/count 加一' },
      { cmd: 'dock ls w /data', note: 'pod 看到的檔案系統' },
      { cmd: 'dock cat w /data/count', note: '它寫的檔案' },
      { cmd: 'dock commit w snap', note: '可寫層 → 新映像 snap' },
      { cmd: 'image snap', note: '比 writer 多了一個 layer' },
      { cmd: 'dock run snap', note: '從新映像啟動：count 從 1 接著數' },
    ],
  },
  {
    title: '9. 自己做一個映像',
    text: ['在 pod 裡寫一個 Podfile，再用 dock build 以它為基礎做出新映像。'],
    steps: [
      { cmd: 'dock write w /src/motd.txt 你好，這是我做的映像', note: '寫一個檔案到 pod 裡' },
      { cmd: 'dock write w /src/Podfile.toml name = "mine"\\nfrom = "writer"\\n[files]\\n"/etc/motd" = "/src/motd.txt"', note: 'Podfile：以 writer 為底，換掉 /etc/motd' },
      { cmd: 'dock build w /src/Podfile.toml', note: '建出 mine' },
      { cmd: 'dock run mine', note: '新的歡迎訊息' },
    ],
  },
  {
    title: '10. 宣告想要的狀態（kube）',
    text: [
      '`dock run` 是命令：「現在跑一個」。kube 則是宣告：寫下「想要兩個 worker 一直在跑」（一個 TOML manifest），kube 就不停比對實際狀態並修正：少了補、多了刪、掛了重啟。這就是 Kubernetes 的核心想法（reconcile loop）。',
      'kube 自己也只是一個程式：它手上只有一個 dock client（和映像庫），所有動作都是請 dock 去做。到右邊「叢集」頁看 pod 怎麼變。',
    ],
    steps: [
      { cmd: 'cat kube/web.toml', note: '一個 Deployment：image、replicas、restart' },
      { cmd: 'kube apply kube/web.toml', note: '交出想要的狀態' },
      { cmd: 'kube get pods', note: 'READY、STATUS、RESTARTS、REV、NODE' },
      { cmd: 'kill web-1', note: '殺掉一個……' },
      { cmd: 'kube get pods', note: '……一秒後它又回來了，RESTARTS 加一' },
      { cmd: 'kube scale web 4', note: '要四個' },
      { cmd: 'kube describe web', note: '設定、每個 pod、最近的事件' },
      { cmd: 'kube apply kube/flaky.toml', note: '一個兩秒後就失敗的 pod：重啟的間隔愈來愈長（back-off）' },
      { cmd: 'kube describe flaky | grep -i pod', note: '看它的事件' },
      { cmd: 'kube apply kube/small.toml kube/crowd.toml', note: 'Space：一群 pod 共用的額度；放不下的就 Pending' },
      { cmd: 'kube get pods | grep crowd', note: 'crowd-3 等著有空間' },
      { cmd: 'kube delete web; kube delete flaky; kube delete crowd', note: '收拾' },
    ],
  },
  {
    title: '11. Service 與 Config：沒有位址的網路',
    text: [
      '`serves = "echo"` 的 pod 就是 service echo；`uses = ["echo"]` 的 pod 名稱空間裡才會有 `svc:echo`。要連線時，pod 建一條新的 channel，把一端經 `svc:echo` 交出去，kube 把它交給一個就緒的 echo pod（輪流），之後兩邊直接談。沒有 IP、沒有 DNS：沒拿到 `svc:echo` 的 pod 根本連不到。',
      'kube 每 2 秒 ping 一次提供服務的 pod：不回應就不再分到連線（readiness），太久沒回就重啟（liveness）。Config 的檔案和環境變數疊成映像上的一層；改了 Config 或範本，就會滾動更新：先多一個新 pod，它就緒了才刪一個舊的。',
    ],
    steps: [
      { cmd: 'kube apply kube/echo.toml', note: '兩個 echo pod 組成 service echo' },
      { cmd: 'kube call echo hi', note: '看是誰回答的……' },
      { cmd: 'kube call echo hi', note: '……輪到另一個' },
      { cmd: 'cat kube/greeting.toml kube/front.toml', note: '一個 Config、一個會呼叫 echo 的 pod' },
      { cmd: 'kube apply kube/greeting.toml kube/front.toml', note: '套用' },
      { cmd: 'kube logs front-1', note: '它讀到的設定，與 echo 的回答' },
      { cmd: 'kube get svc', note: '每個 service：就緒的 pod、使用者、交出的連線' },
      { cmd: "sed 's/v1/v2/' kube/greeting.toml > /tmp/greeting.toml", note: '改一下設定' },
      { cmd: 'kube apply /tmp/greeting.toml', note: '新的 revision 開始滾動更新' },
      { cmd: 'kube rollout status front', note: '更新到哪裡了' },
      { cmd: 'kube rollout history front', note: '每個 revision 跑哪個映像（Config 疊上去的衍生映像）' },
      { cmd: 'kube rollout undo front', note: '退回上一版' },
      { cmd: 'kube apply kube/stuck.toml', note: '一個兩秒後就不回應的 pod：等十秒看 liveness' },
      { cmd: 'kube describe stuck', note: 'Liveness probe failed → 重啟' },
    ],
  },
  {
    title: '12. 叢集：很多台機器',
    text: [
      '連到同一個 server 的機器就是一個叢集，每台機器是一個節點。在新分頁打開這個網址再加上 `?machine=b2`，就多了一台機器（自己的磁碟、自己的核心）。',
      '想要的狀態和每個 pod 的狀況都記在 server 上。其中一台機器的 kube 是 leader（靠租約：一直續約的那台），由它決定每個 pod 放哪個節點：活著的、架構合的、`node = "…"` 指定的、pod 最少的。每台機器的 kube 只跑分給自己的 pod。',
      '把另一個分頁關掉：它不再續約，8 秒後就算掉線，它的 pod 在別的節點重建；如果它是 leader，別台會接手。',
    ],
    steps: [
      { cmd: 'kube get nodes', note: '節點、架構、誰是 leader' },
      { cmd: 'kube apply kube/spread.toml', note: '四個 pod' },
      { cmd: 'kube get pods', note: 'NODE 欄：分到哪台機器' },
      { cmd: 'cat kube/pinned.toml', note: '只放在 node-b 的 pod' },
      { cmd: 'kube apply kube/pinned.toml', note: '沒有 node-b 的話就 Unschedulable' },
      { cmd: 'kube get pods', note: '關掉另一頁後再看一次' },
      { cmd: 'kube delete spread; kube delete pinned', note: '收拾' },
    ],
  },
  {
    title: '13. 上網：curl 與 http 這個能力',
    text: [
      '瀏覽器裡的網頁不能隨便連到別的網站，所以由 server 代為連線：`curl` 把請求經 store 交給 server，server 連過去，再把回答傳回來。為了不讓網頁上的人透過它碰到 server 那邊的內網，server 不連 localhost、10.*、192.168.*、169.254.* 這類位址（轉址過去也不行）。',
      '能上網的也只有拿到 `http` 這個 capability 的程式：shell 有，`run` 啟動的程式要你明說才給。',
    ],
    steps: [
      { cmd: 'curl https://example.com | head -n 5', note: '抓一個網頁，接管線' },
      { cmd: 'curl -I https://example.com', note: '只看回應的 headers' },
      { cmd: 'curl -o /tmp/page.html https://example.com', note: '存成檔案' },
      { cmd: 'grep -i title /tmp/page.html', note: '在檔案裡找' },
      { cmd: 'curl http://localhost:8780/', note: 'server 自己那台機器：拒絕' },
      { cmd: 'curl http://169.254.169.254/', note: '雲端主機的 metadata 位址：也拒絕' },
      { cmd: 'handles', note: 'shell 手上那條 http channel' },
      { cmd: 'run hello console http', note: '把 http 交給一個程式；不給就沒有' },
    ],
  },
  {
    title: '14. 函式：請求來了才啟動（serverless）',
    text: [
      'AWS Lambda、Cloudflare Workers 這類 serverless：只交出一個函式，平台在請求來時才把它跑起來（冷啟動），忙了多開幾份，閒了收掉，最後一份都不留（scale to zero）。這就是「replicas 由請求決定、可以是 0」的 Deployment。',
      '在 ruos 裡，函式在收到請求的那台機器上跑，就像 Workers 在收到請求的據點執行。每個 instance 是一個新的 WebAssembly instance，所以冷啟動只要幾十 ms。函式是 stateless 的：instance 記憶體裡的東西不可靠，要保存就用 KV（`uses = ["kv:counter"]`），沒宣告的函式拿不到。到「叢集」頁看 instance 怎麼出現又消失。',
    ],
    steps: [
      { cmd: 'cat kube/fn-hello.toml', note: '一個 Function：image、max、idle、timeout' },
      { cmd: 'kube apply kube/fn-hello.toml', note: '套用：這時一個 instance 都沒有' },
      { cmd: 'fn call hello world', note: '第一個請求：冷啟動' },
      { cmd: 'fn call hello again', note: '第二個：同一個 instance（#2 here），熱啟動' },
      { cmd: 'kube get fn', note: 'instance 數、請求數、冷啟動次數與時間' },
      { cmd: 'kube describe hello', note: '事件：Cold start…；等 15 秒會看到 Scaled hello to 0' },
      { cmd: 'fn call hello sleep 5', note: '超過 timeout（3 秒）：instance 被停掉' },
      { cmd: 'kube apply kube/fn-count.toml', note: '同一個程式，加上 kv:counter' },
      { cmd: 'fn call count a', note: '#N here 會歸零，kv:counter 的數字不會' },
      { cmd: 'kv get counter count', note: 'shell 也能看 KV' },
      { cmd: 'kube apply kube/fn-cgi.toml', note: 'reuse = false：每個請求一個新的 instance' },
      { cmd: 'fn call cgi 1; fn call cgi 2; kube get fn', note: '每次都是冷啟動' },
      { cmd: 'kube delete hello; kube delete count; kube delete cgi', note: '收拾' },
    ],
  },
  {
    title: '15. 從外面呼叫函式（對外入口）',
    text: [
      '`expose = true` 的函式有一個網址，在 ruos serve 的「函式埠」（預設是主埠加 10，例如 8790）。外面的請求先到 ruos serve，它挑一台機器（已經有這個函式 instance 的優先）把請求送進去，kube 交給 instance，回答再沿原路回去。沒有 expose 的函式，外面連不到。',
      '函式埠和這一頁是不同的 origin：函式回的網頁就是一個普通網站，但碰不到 OS 自己的連線。OS 跑在瀏覽器分頁裡，所以把所有分頁都關掉之後，網址會回 503：沒有機器可以處理請求。',
    ],
    steps: [
      { cmd: 'cat kube/fn-site.toml', note: 'expose = true，加上 kv:visits' },
      { cmd: 'kube apply kube/fn-site.toml', note: '套用' },
      { cmd: 'kube get fn | grep site', note: 'URL 欄：在新分頁打開它（或到「叢集」頁點連結）' },
      { cmd: 'kube describe site', note: '從外面來的請求一樣會冷啟動、重用、閒置縮到 0' },
      { cmd: 'kv get visits count', note: '網頁上按「來訪 +1」的次數' },
      { cmd: 'fn call site 從 OS 裡面呼叫', note: '裡面照樣能用 fn call' },
      { cmd: 'kube delete site', note: '收拾：網址變成 404' },
    ],
  },
  {
    title: '16. 現成的程式（WASI）',
    text: [
      'WASI 是 WebAssembly 程式和作業系統之間的標準介面：用 Rust、C、Go 寫的一般程式，編成 `wasm32-wasip1` 就能在 wasmtime、瀏覽器……以及 ruos 上跑。ruos 的 `wasi` 程式把 WASI 的呼叫（讀檔、寫檔、時間、參數）轉成 ruos 的 capability。',
      '程式只拿到主控台和**目前的目錄**：相對路徑照常，`/` 開頭的路徑也還是在這個目錄底下，看不到上層——要給它多少，就是讓它從哪個目錄開始。可以接管線。`coreutils` 是 Rust 版的 GNU coreutils（uutils），第一次用時從 server 的 registry 拉下來。',
    ],
    steps: [
      { cmd: 'wasi-hello one two', note: '一個一般的 Rust 程式：參數與 PWD' },
      { cmd: 'wasi-hello write notes.txt; cat notes.txt', note: '它用 std::fs 寫的檔案，shell 看得到' },
      { cmd: 'wasi-hello read /notes.txt', note: '/ 開頭也是目前目錄底下' },
      { cmd: 'coreutils ls', note: 'uutils coreutils（第一次會從 registry 拉，約 10 MB）' },
      { cmd: 'coreutils sort poem.txt | coreutils head -n 2', note: '兩個 WASI 程式接管線' },
      { cmd: 'cat poem.txt | coreutils wc -l', note: '內建指令接 WASI 程式' },
      { cmd: 'coreutils sha256sum poem.txt', note: '雜湊' },
      { cmd: 'cd /; coreutils ls; cd /home', note: '從 / 開始，它就看得到全部' },
    ],
  },
  {
    title: '17. 網站範例：資料在後端',
    text: [
      '五個小網站，都是 kube 的 Function（`expose = true`），資料放在後端（ruos serve）的 SQLite：留言板、待辦清單、wiki、短網址、天氣。函式本身沒有狀態，instance 隨時會被收掉、換一台機器；資料在 `sql:NAME` 這個 capability 後面，manifest 的 `uses` 列了它，函式才拿得到。',
      '網頁由函式回給瀏覽器，畫面在瀏覽器裡由 JavaScript 依 JSON API 畫出來；函式只回答 API、跟資料庫說話（原始碼在 `apps/webapps`）。天氣還用了 `http`：經 server 問 Open-Meteo，結果在資料庫裡存 10 分鐘，所以不管哪個 instance 回答，同一個城市 10 分鐘內只問一次。',
    ],
    steps: [
      { cmd: 'cat kube/app-guestbook.toml', note: 'uses = ["sql:guestbook"]' },
      { cmd: 'kube apply kube/app-guestbook.toml kube/app-todo.toml kube/app-wiki.toml kube/app-short.toml kube/app-weather.toml', note: '五個網站（映像第一次用時從 registry 拉）' },
      { cmd: 'kube get fn', note: 'URL 欄：先在新分頁打開它們，留言、加待辦、寫 wiki、縮網址（表格是函式第一次收到請求時才建的，沒打開過，下面的 sql 會說 no such table）' },
      { cmd: 'sql -d guestbook "SELECT * FROM notes"', note: 'shell 也拿得到這些資料庫：剛才在網頁上的留言' },
      { cmd: 'sql -d todo "SELECT * FROM todos"', note: '待辦' },
      { cmd: 'sql -d wiki "SELECT name, revisions, updated FROM pages"', note: 'wiki 的每一版都留著' },
      { cmd: 'sql -d short "SELECT * FROM links"', note: '短網址與點擊次數' },
      { cmd: 'run snoop console sql:guestbook', note: '別的程式要用，也得被給這個 capability' },
      { cmd: 'kube delete guestbook; kube delete todo; kube delete wiki; kube delete short; kube delete weather', note: '收拾（資料庫還在後端）' },
    ],
  },
  {
    title: '18. 資料放在哪裡：前端或後端',
    text: [
      '同樣的 `sql`、`redis`、`kv`，名字後面加上 `@local`，資料就放在這台機器自己的瀏覽器（IndexedDB）裡，不在伺服器。SQLite 是 C 寫的，用 wasi-sdk 編成 WASI 程式 `sqlited`，在 ruos 裡讀寫 `local` 服務給它的檔案；Redis 是 ruos 自己寫的小版本。',
      '程式完全不知道差別：`uses = ["sql:guestbook@local"]` 讓程式的名稱空間裡一樣是 `sql:guestbook`，只是另一端換成 `local` 服務。這就是 capability 的好處：資料在哪裡由「給的人」決定。但意思不同：放在瀏覽器的資料，只有這個瀏覽器的這台機器看得到。',
    ],
    steps: [
      { cmd: 'sql -d app@local "CREATE TABLE t (id INTEGER PRIMARY KEY, who TEXT)"', note: '前端的 SQLite（第一次會從 registry 拉 sqlited）' },
      { cmd: 'sql -d app@local "INSERT INTO t (who) VALUES (\'ann\'), (\'bob\')"; sql -d app@local "SELECT * FROM t"', note: '存在瀏覽器的 IndexedDB' },
      { cmd: 'redis -d cache@local SET greeting hello EX 60; redis -d cache@local TTL greeting', note: 'ruos 自己的小 Redis，60 秒後過期' },
      { cmd: 'kv put notes@local today 寫作業; kv get notes@local today', note: 'KV 也一樣' },
      { cmd: 'local', note: '這台機器自己存了什麼' },
      { cmd: 'sqlite3 music.db "CREATE TABLE songs (title TEXT, plays INTEGER); INSERT INTO songs VALUES (\'a\', 7), (\'b\', 8);"; sqlite3 -box music.db "SELECT * FROM songs"', note: 'SQLite 官方的 C 程式 sqlite3，在 ruos 裡跑（檔案在目前目錄）' },
      { cmd: 'run snoop console sql:app@local', note: 'snoop 看到的名字是 sql:app，分不出在哪裡' },
      { cmd: 'sed \'s/sql:guestbook/sql:guestbook@local/; s/name = "guestbook"/name = "lguest"/\' kube/app-guestbook.toml > /tmp/lguest.toml; cat /tmp/lguest.toml', note: '同一個留言板，只改 uses' },
      { cmd: 'kube apply /tmp/lguest.toml; kube get fn', note: '打開 lguest 的網址留言：留言存在這個瀏覽器' },
      { cmd: 'sql -d guestbook@local "SELECT * FROM notes"', note: '重新整理頁面（重開機）後資料還在' },
      { cmd: 'sql copy app@local moved; sql -d moved "SELECT * FROM t"', note: '改變主意：把資料從瀏覽器搬到後端' },
      { cmd: 'local rm sql:app; local', note: '刪掉這台機器的那一份' },
    ],
  },
];

export function renderLessons(root, type) {
  let i = 0;
  const show = () => {
    const l = LESSONS[i];
    root.replaceChildren();
    const nav = document.createElement('div');
    nav.className = 'lesson-nav';
    const prev = Object.assign(document.createElement('button'), { textContent: '‹ 上一課', disabled: i === 0, onclick: () => { i--; show(); } });
    const next = Object.assign(document.createElement('button'), { textContent: '下一課 ›', disabled: i === LESSONS.length - 1, onclick: () => { i++; show(); } });
    const pick = document.createElement('select');
    pick.setAttribute('aria-label', '選課');
    LESSONS.forEach((x, k) => pick.append(Object.assign(document.createElement('option'), { value: k, textContent: x.title, selected: k === i })));
    pick.onchange = () => { i = Number(pick.value); show(); };
    nav.append(prev, pick, next);
    const h = Object.assign(document.createElement('h3'), { textContent: l.title });
    root.append(nav, h);
    for (const t of l.text) {
      const p = document.createElement('p');
      // `code` spans in the text.
      t.split(/`([^`]+)`/).forEach((part, k) => p.append(k % 2 ? Object.assign(document.createElement('code'), { textContent: part }) : part));
      root.append(p);
    }
    const ol = document.createElement('ol');
    ol.className = 'steps';
    for (const s of l.steps) {
      const li = document.createElement('li');
      const b = Object.assign(document.createElement('button'), { className: 'cmd', textContent: s.cmd, title: '在終端機執行', onclick: () => type(s.cmd) });
      b.setAttribute('aria-label', `在終端機執行 ${s.cmd}`);
      li.append(b, Object.assign(document.createElement('span'), { className: 'note', textContent: s.note }));
      ol.append(li);
    }
    root.append(ol);
  };
  show();
}
