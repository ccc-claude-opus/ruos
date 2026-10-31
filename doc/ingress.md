# 函式的對外入口（ingress）設計

狀態：設計已與使用者確認（2026-10-31），已實作（M23），實作時有一處改動，見「實作時的改動」。前提是 [serverless.md](serverless.md) 的 Function。

## 目標

讓 OS 外面的請求能呼叫 Function，例如瀏覽器打開 `http://127.0.0.1:8790/fn/hello/…`，或用主機上的 curl 呼叫。這是「server 類型」的功能：請求從外面先到 ruos serve，再由 server 主動送進某台機器。現在的 remote 只有「機器問、server 答」，所以要多一條方向相反的路。

OS 跑在瀏覽器分頁（或 QEMU）裡，所以**沒有任何機器開著時，就沒有人能處理請求**（回 503）。這也是課程要讓學生看到的一點。

## 已決定的事

1. **函式另開一個埠。** `ruos serve --fn-port N`，預設是主埠加 10（8780 → 8790），只提供 `/fn/…`。
   - 不同的埠在瀏覽器眼中就是不同的網站，所以函式的網頁和 OS 的頁面，由瀏覽器的同源政策自動隔開，不靠每個回應都記得加 header。
   - 函式的網頁就是一個普通網站：JS、表單、localStorage、`fetch` 自己的 API 都照常運作，就像 Cloudflare Workers 的 `*.workers.dev`。
   - 沒選同埠加 CSP sandbox，因為 sandbox 會讓函式的網頁沒有自己的 origin：`fetch` 自己的 API 要另加 CORS、不能用 localStorage，而且只要有一個回應漏了 header 就會出事。
   - 也沒選每個函式一個子網域（`*.fn.localhost`），因為要靠瀏覽器解析 `*.localhost`，開放給別台電腦時還要設 DNS。
   - 要知道的限制：cookie 只分網域、不分埠，函式設的 cookie 也會送到 OS 的埠。OS 頁面不用 cookie，所以不影響安全。
2. **要明說才公開。** manifest 寫 `expose = true` 的 Function 才有網址，其他的只能從 OS 裡面呼叫。這和 capability 的想法一致：沒給的就沒有。
3. **有熱 instance 的機器優先。** server 讀叢集記錄（`fnstat/NODE`），先挑已經有這個函式 instance 的機器，沒有再從連著的機器中輪流挑。這樣冷啟動比較少，課程裡也看得到請求被送往熱的那台。
4. **HTTP 封包加 `serve_http`。** 交給函式的請求是一個封包，裡面有 method、path、query、headers、body。libsys 提供 `func::serve_http(|req| Response)` 幫忙解析。函式回的如果不是 HTTP 封包，就當成 `200 text/plain` 的內容，所以用 `serve` 寫的舊函式不必改。

另外，不管函式埠怎麼設，server 的 `/v1/remote` 與 `/v1/ingress` WebSocket 只接受從 OS 自己的 origin 連來的。Node 測試不帶 Origin，照樣允許。

## 請求怎麼走

```
外部瀏覽器 / curl ──HTTP──▶ ruos serve 的函式埠  /fn/NAME/…
                              │ 挑一台連著的機器（有熱 instance 的優先）
                              ▼  ingress WebSocket（每台機器第二條，server 可以主動送）
                     kernel_host.js ─▶ 核心的第二個 remote channel ─▶ kube（名稱空間 ingress）
                                                                    │ 同 fn call：交給 instance / 冷啟動
                                                                    ▼
                                                               函式 instance
回覆沿原路回去：instance ─▶ kube ─▶ ingress ─▶ ruos serve ─▶ HTTP 回應
```

1. **核心與 host（只做 wasm）**：`kernel_host.js` 每台機器多開一條 `/v1/ingress?machine=NAME&session=…` WebSocket，斷線時照 remote 的方式重連。核心把它變成第二個 remote channel，init 交給 kube，在名稱空間裡叫 `ingress`。
2. **server**：
   - 函式埠接 `/fn/NAME/…` 的任何 method，並記錄哪些機器的 ingress 連著。
   - 挑一台機器，在它的 ingress 上送 `REQ id` + 封包，等 `RESP id` + 封包。用 id 區分，所以同時可以有很多個請求。
   - 等回覆的時間上限是函式的 `timeout` 加上 15 秒的冷啟動寬限，超過就回 504。
   - 請求與回應的 body 先限 60 KB（一則 channel 訊息），之後再分段。
   - 轉送前去掉 Connection、Transfer-Encoding 這類只屬於單一段連線的 header。
   - 每個請求記一行 log：送到哪台機器、狀態碼、花了多久。
3. **kube**：
   - 收到 `REQ` 後，確認有這個 Function 而且 `expose = true`，否則回 404。
   - 像 `fn call` 一樣交給 instance：kube 自己當呼叫端，把封包寫進新 channel、交出一端，再用 port 等回答，不會卡住。
   - instance 被 timeout 終止（channel 被關掉）時回 504。
4. **函式**：`func::serve_http` 解析封包，把 `Request` 交給函式，再把 `Response` 編回封包。`func::serve` 的回答由 kube 包成 `200 text/plain`。

## 封包格式

```
請求：METHOD PATH?QUERY\n Name: value\n … \n\n BODY
回應：HTTP STATUS\n Name: value\n … \n\n BODY
```

PATH 是 `/fn/NAME` 之後的部分（至少是 `/`）。回應如果不是以 `HTTP ` 開頭，整個回答就當成 body，狀態 200、`Content-Type: text/plain; charset=utf-8`。

## 實作時的改動

- **ingress 先用 HTTP long polling，v2.0 改回 WebSocket。** v1 的 ruos serve 用同步的 tiny_http + tungstenite：一條 WebSocket 一次只能讀或寫，交出來的連線也不能設讀取逾時，所以 server 沒辦法一邊等機器回答、一邊主動推請求。M23 因此改成 long polling（`GET /v1/ingress/next` 掛著等請求，`POST /v1/ingress/answer` 送回答）。v2.0 第 2 步把 ruos serve 換成 tokio + axum 之後，一條 WebSocket 可以同時讀寫，就照原本的設計用 `/v1/ingress?machine=M&session=S` 這條 WebSocket：server 有請求就推過去，回答從同一條回來。機器「在」= 這條 WebSocket 開著；它關掉時，還沒回答的請求回 502。
  - 在 OS 裡面，這一直都是核心的第二個 remote channel：kube 看到的和設計一樣，`REQ ID NAME\n` + 封包、`RESP ID\n` + 封包。
- **server 也先檢查 `expose`**：server 讀叢集記錄 `fn/NAME`，沒有 `expose = true` 就直接回 404，不必送進機器。kube 收到請求時也會再檢查一次。
- **WebSocket 與 ingress 只接受 OS 自己的頁面**：有 `Origin` header 的話必須是自己的 origin；有 `Sec-Fetch-Site` 的話必須是 `same-origin` 或 `none`。函式埠和 OS 頁面在瀏覽器眼中是 same-site，所以 `same-site` 會被拒絕。Node 測試兩個 header 都不帶，所以照樣允許。
- **kube 從 `KV_HELLO` 知道函式埠的網址**：回覆的第四欄是 `http://ADDR:PORT`，用來顯示在 `kube get fn` 的 URL 欄。`/v1/cluster` 的 JSON 裡有 `fn_port`，網頁用它做連結。
- **回應的 header**：server 在函式的回應加上 `X-Ruos-Node: 機器名稱`。
- **測試**：「請求優先送到有熱 instance 的那台」還沒有自動測試，只用兩個分頁手動確認過。

## 觀察與教材

- `kube get fn` 多一欄 URL（有 expose 的才有）。
- 「叢集」頁的函式表直接列出網址，可以點開。server 在 `/v1/cluster` 的 JSON 裡告訴頁面函式埠是哪個。
- ruos serve 的 log：每個請求送到哪台機器、狀態碼、時間。
- 新增一課：寫一個 `expose = true` 的函式（例如一個用 KV 計數的小網頁或 JSON API），開兩個分頁當兩台機器，看請求被送往有熱 instance 的那台；把分頁都關掉，看到 503。

## 測試

- boot.mjs 從 ruos serve 的輸出讀出函式埠。看到輸出裡的記號（測試腳本用 `echo` 印出）之後，自己用 Node 對 `/fn/…` 發 HTTP 請求，把結果印進 log，比對 expect：
  - GET、POST 與 query；
  - 回應的 header；
  - 用 `serve` 寫的函式變成 text/plain；
  - 沒有 expose 的回 404；
  - timeout 回 504；
  - 從別的 Origin 連 WebSocket 被拒絕。
- 兩台機器的叢集測試再加一段：請求要優先送到有熱 instance 的那台；那台離開後，請求改送另一台（冷啟動）。

## 分步實作

1. 核心與 host 的第二個 remote channel、`/v1/ingress`、WebSocket 的 Origin 檢查。
2. server 的函式埠、挑機器、轉送、逾時、log。
3. kube 的 ingress 處理、`expose`、封包；libsys 的 `serve_http`；範例函式。
4. `kube get fn` 的 URL、網頁的連結、一課；測試。

原生平台（QEMU）每次開機只有一條連到 server 的線，要在 store 那條上多工，先不做。每一步只測 wasm，各自一個 commit。
