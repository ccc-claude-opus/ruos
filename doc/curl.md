# curl 是怎麼運作的

ruos 的 `curl` 前端和後端都有參與，兩邊分工清楚：

- **前端（瀏覽器）**跑整個 OS，包括 `curl` 本身：解析參數、權限檢查、分段讀回、輸出。
- **後端（`ruos serve`）**只做瀏覽器做不到的那件事：真正連上網路（DNS、TCP、TLS），以及安全把關。

兩邊之間只有一條 WebSocket，JavaScript 沒有任何專為 curl 寫的程式碼。

## 為什麼不能全在前端

網頁不能開 socket，只能用 `fetch()`。`fetch()` 又受同源政策限制：對方網站沒有送 CORS header，網頁就讀不到內容。ruos 的頁面還開了 cross-origin isolation（COOP / COEP，用 SharedArrayBuffer 必須開），限制更嚴。所以大部分網站，網頁都抓不下來，只能請 server 代為連線。

## 一次 `curl https://example.com` 經過哪些地方

```
瀏覽器分頁                                                   你的電腦
┌──────────────────────────────────────────────┐        ┌─────────────────────────┐
│ shell（Worker）       store（Worker）          │        │ ruos serve              │
│  curl 內建指令   ──①──▶ 檢查 http capability    │        │                         │
│  libsys::http         原樣轉送 ──②──┐          │        │                         │
│                                     ▼          │        │                         │
│            核心（kernel Worker，kernel.wasm）   │        │                         │
│             remote channel ──③── kernel_host.js ═WebSocket═▶ remote.rs ──④──▶ http.rs
│                                                │        │   檢查位址、ureq 連線 ──▶ 網站
└──────────────────────────────────────────────┘        └─────────────────────────┘
```

### ① shell 送出請求

`curl` 是 shell 的內建指令（`services/shell/src/cmds.rs` 的 `curl`），用 Rust 寫、編成 wasm，跑在 shell 自己的 Worker 裡。

- 它呼叫 `libsys::http::Http::fetch`（`libsys/src/http.rs`），在名稱空間裡 `http` 這個 handle（一條 channel）上寫一則訊息：`[HTTP_FETCH, method, url, headers, body]`。
- 寫 channel 是一次系統呼叫：shell 的 Worker 把號碼和參數放進 SharedArrayBuffer、通知核心 Worker，然後用 `Atomics.wait` 停下來等。
- 核心把訊息放進 channel，叫醒等在另一端的 store。

### ② store 檢查並轉送

- store 記得每個 client 是怎麼來的：從它名稱空間的 `web` channel 來的（以及它們 `CONNECT` 出來的），才有 http capability。
- shell 的 `http` 就是 init 建的那條 `web` channel 的另一端（`services/init/src/main.rs`）。
- 沒有這個能力的 client，會直接收到 `no http capability`。
- 有的話，store 把訊息原封不動寫進它的 `remote` channel，然後等回覆（`services/store/src/main.rs`）。

### ③ 核心把訊息送出瀏覽器

- remote channel 的另一端不是任何任務，而是 host：核心呼叫 wasm import `remote_send`。
- `web/kernel_host.js` 收到後，原樣用 WebSocket（`/v1/remote`）送給 ruos serve。
- JS 不認得 HTTP_FETCH，它只是轉送 bytes。這條路也用來傳 pod 的可寫層、下載映像、讀寫 kube 的叢集記錄。

### ④ server 真正連線

- ruos serve 為每台機器開一條 thread 處理這條 WebSocket，`ruos/src/remote.rs` 看到 HTTP_FETCH，就交給 `ruos/src/http.rs`。
- `http.rs` 用 `ureq`（TLS 用 rustls）發出請求。
- 把關放在自訂的 resolver（`Guard`）裡：網域名稱解析出來的每個位址都先檢查，屬於 server 自己那邊網路的就丟掉；全被丟掉就回「HOST is on the server's own network (refused)」。被擋的位址有：
  - loopback；
  - 10.*、172.16–31.*、192.168.*；
  - 169.254.*（雲端主機的 metadata 位址）；
  - 100.64.*、0.*；
  - multicast、broadcast；
  - IPv6 的 ULA 與 link-local（包括包在 IPv6 裡的 IPv4 位址）。
- 轉址時 ureq 會再解析一次，也會經過同一個檢查，所以轉址或 DNS 指回內網都擋得住。
- 其他限制：最多 10 秒、4 MiB、5 次轉址，只接受 http / https。
- `ruos serve --http-allow ADDR:PORT` 可以開放特定位址（測試用這個連本機的測試網站），`--no-http` 整個關掉。

## 回覆怎麼回來

### 第一次回覆只有 headers

server 回 `[OK, "STATUS LEN\n" + header 行]`（狀態碼、body 長度、`Name: value` 行），沿原路回來：

1. ruos serve 寫回 WebSocket；
2. `kernel_host.js` 收到後呼叫核心的 `kernel_remote`；
3. 核心把它放進 remote channel，store 讀到，再寫給 shell 的 channel；
4. shell 從 `Atomics.wait` 被叫醒。

### body 分段讀

一則 channel 訊息最多 64 KB，所以 server 先把 body 留在這條連線（session）上。shell 再用 `[HTTP_READ, offset]` 每次拿最多 60000 bytes，直到拿滿長度。

例如測試裡 208 KB 的檔案，就是一次 FETCH 加 4 次 READ，每次都完整走一趟上面的路。

### 輸出跟一般內建指令一樣

- **直接印出**：經 console 服務到核心，再由 `console_write` 交給頁面的終端機。
- **接管線**：交給下一個指令，例如 `curl … | head`。
- **`-o FILE`**：寫進 shell 的檔案系統。可寫層也在 server 上，所以檔案內容又經 WebSocket 存回 server。

## 這樣設計的用意

- **網路是一個 capability**：沒有拿到 `http` handle 的程式根本沒有路可以上網。`run IMAGE http` 才會把它交給別的程式（`run hello console http`），這和 `console`、`store` 的道理一樣。
- **前端不必知道 HTTP**：瀏覽器端只負責轉送 bytes，所以加 curl 時 JS 一行都沒改。
- **三個平台同一條路**：QEMU 版的 store 也有 remote（經 virtio-net、smoltcp 和 QEMU 的 guestfwd 連到 ruos serve），所以原生平台理論上也能用同一套 `curl`。不過目前只在 wasm 上測過。

## 相關的檔案

| 位置 | 做什麼 |
|---|---|
| `services/shell/src/cmds.rs`（`curl`） | 指令：參數、輸出、`-o`、`-f` |
| `libsys/src/http.rs` | `Http::fetch`：FETCH 之後分段 READ |
| `libsys/src/store.rs` | `HTTP_FETCH` / `HTTP_READ` 的編號與格式、`Store::raw` |
| `services/store/src/main.rs` | 只替有 http capability 的 client 轉送 |
| `services/init/src/main.rs` | 建 `web` channel，另一端給 shell 當 `http` |
| `web/kernel_host.js` | remote channel ↔ WebSocket（原樣轉送） |
| `ruos/src/remote.rs` | WebSocket 上的請求：`HTTP_FETCH`、`HTTP_READ` |
| `ruos/src/http.rs` | 真正的連線與位址檢查（`Guard`）、`--http-allow`、`--no-http` |
| `web/test/boot.mjs`、`tests/m21.expect` | 測試用的小網站與預期輸出 |
