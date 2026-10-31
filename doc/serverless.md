# Serverless（Function）設計

狀態：設計已與使用者確認（2026-10-31），尚未實作。

## 想法

AWS Lambda、Cloudflare Workers 這類 serverless 平台的做法是：只交出一個函式，請求來了才跑，忙了就多開幾份，閒了就收掉（scale to zero）。函式是 stateless 的，要保存資料就透過平台給的 binding。

在 k8s 上，這就是「replicas 由請求決定、可以是 0」的 Deployment（Knative、OpenFaaS 都是這樣做）。ruos 有兩點本來就接近 Cloudflare Workers：

- 瀏覽器版的每個任務，都是一個 WebAssembly instance，就像 Workers 的 isolate，所以冷啟動很快。
- Workers 的 binding，就是 ruos 名稱空間裡的 capability：沒拿到的就用不了。

所以 Function 是 kube 的一個新種類，重用現有的 pod、kubelet、Service 交接與叢集記錄。

## manifest

```toml
kind = "Function"
name = "hello"
image = "greet"
uses = ["kv:counter", "http"]   # 它拿得到的 capability（binding）
timeout = 10                     # 每個請求最多幾秒（預設 10）
max = 4                          # 每個節點最多幾份 instance（預設 4）
idle = 30                        # 閒置幾秒就收掉（預設 30）
reuse = true                     # false：每個請求一個新的 instance（像 CGI）
[resources]
mem = "8M"
```

## 已決定的四件事

1. **在呼叫的那台機器上跑。** 每個節點的 kubelet 各自冷啟動、各自擴縮自己的 instance，leader 不參與。Function 記錄存在叢集裡，所以每個節點都知道有哪些函式。這和 Cloudflare Workers 一樣：程式部署到每個據點，在收到請求的那裡執行。這樣也不需要跨機器的 channel。
2. **instance 預設留著重用。** 處理完一個請求後留著等下一個（熱啟動），閒置 `idle` 秒才收掉。`reuse = false` 時每個請求都用新的 instance，用來在課程裡比較冷、熱啟動。
3. **KV 是具名的 namespace。** `uses = ["kv:counter"]` 會給函式名為 counter 的 KV，可以 get / put / delete / list。資料放在 server 的叢集記錄（`data/counter/KEY`），宣告了同一個名字的函式共用同一份。shell 也有 `kv` 指令可以查看與修改。
4. **先沒有對外入口。** 第一版只能從 shell 或別的 pod 呼叫。讓瀏覽器網址 `/fn/NAME` 這類外部請求進到函式，屬於 server 類型的功能：需要 server 主動把請求送進某台機器（現在的 remote 只有機器發問、server 回答），之後另外設計。

## 呼叫的流程

1. 呼叫端（pod 的名稱空間裡有 `fn:hello`，或 shell 的 `fn call hello TEXT`）建一條 channel，經 `fn:hello` 送出一端。
2. kubelet 找一個閒著的 hello instance，把 channel 交給它（熱啟動）。沒有閒著的、而且還沒到 `max` 份，就冷啟動一個，請求先排隊，等它說 ready 再交出去。已經到 `max` 份就排隊等。
3. 函式用 `libsys::fn::serve(|req| resp)`：每條 channel 是一個請求、一則訊息，回一則訊息。instance 一次只處理一個請求。
4. 超過 `timeout` 還沒回覆，kubelet 就終止這個 instance，呼叫端看到 channel 被關掉。
5. instance 閒置 `idle` 秒就停掉，最後可以縮到 0。

## stateless

- instance 沒有可寫層，只看得到映像本身；要存資料就用 KV。
- `reuse = true` 時，同一個 instance 的記憶體在兩次請求之間可能還在，但不保證：可能換到另一個 instance，或已經被收掉重開。課程會用一個「在記憶體裡計數」的函式，示範這個數字不可靠，再改成用 KV。

## 觀察

- `kube get fn`：每個函式在這個節點上的 instance 數、處理中的請求數、總請求數、冷啟動次數與平均時間、熱啟動的平均時間。
- `kube describe NAME`：Function 的事件，例如「Cold start hello-f1 (34 ms)」「Scaled hello to 0 (idle 30s)」「Request timed out」。
- 網頁的「叢集」頁：每個節點卡片上列出函式的 instance。

## 分步實作

1. Function 種類、kubelet 的依請求啟動與縮到 0、`libsys::fn`、shell 的 `fn call`、範例函式、`kube get fn`。
2. KV binding（`kv:NAME`）與 shell 的 `kv` 指令。
3. `reuse = false`、timeout、`max` 與排隊，以及冷 / 熱啟動的量測。
4. 網頁的顯示與一課。

每一步都只測 wasm（`./test.sh wasm`），各自一個 commit。
