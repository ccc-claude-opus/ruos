照下面的步驟，就能從頭到尾跑一次範例。

## 1. 啟動

在專案目錄執行：

```bash
./run.sh wasm
```

它會先建置，再啟動 ruos serve，終端機會印出兩個網址：

```
ruos: listening on http://127.0.0.1:8780
ruos: functions on http://127.0.0.1:8790/fn/
```

用瀏覽器打開 `http://127.0.0.1:8780`，等左邊終端機出現 `shell: ready`。

- 8780 是 OS 的頁面，8790 是函式埠。
- 如果要換埠，例如 `./run.sh wasm 9000`，函式埠就會變成 9010。
- 停止 server 用 Ctrl-C。

## 2. 在 OS 的終端機裡套用範例

```
kube apply kube/fn-site.toml
kube get fn | grep site
```

最後一欄就是網址：`http://127.0.0.1:8790/fn/site/`。也可以到右邊的「課程」分頁選第 16 課，點指令就會自動送進終端機。

## 3. 從外面打開它

- **瀏覽器**：在新分頁打開 `http://127.0.0.1:8790/fn/site/`，或到右邊「叢集」頁的函式表點連結。按「來訪 +1」，會看到次數，以及是哪個 instance、哪台機器回的。
- **主機的終端機**：
  ```bash
  curl "http://127.0.0.1:8790/fn/site/api/hello?name=ruos"
  ```
  ```bash
  curl -X POST http://127.0.0.1:8790/fn/site/api/hit
  ```
  ```bash
  curl -i "http://127.0.0.1:8790/fn/site/api/sleep?s=8"
  ```
  第三個會等太久，回 504。

## 4. 回到 OS 裡看發生了什麼

```
kube describe site
kv get visits count
kube get fn
```

- `kube describe site` 的事件裡有 `Cold start site-fN`；閒置 30 秒後會出現 `Scaled site to 0`。
- `kv get visits count` 是網頁上按過的次數。

## 5. 再多試幾件事

- **多一台機器**：再開一個分頁 `http://127.0.0.1:8780/?machine=b2`，就多了一個節點。之後的請求會優先送到已經有 instance 的那台，回應裡的 `X-Ruos-Node` 和網頁上「在 …」會顯示是哪台。
- **沒有機器**：把 OS 的分頁全部關掉，再打開函式網址，會回 503，因為沒有機器可以處理請求。
- **收掉函式**：`kube delete site` 之後，網址會變成 404。

注意：這些功能還沒發布到 crates.io，用 `cargo install ruos` 裝的版本沒有。要照上面用 `./run.sh wasm` 從原始碼跑。