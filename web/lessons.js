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
    title: '10. 同一份程式，三種機器',
    text: [
      '同一個核心跑在 RISC-V、ARM 與瀏覽器（WebAssembly）。映像可以帶好幾個架構的程式，dock 依機器挑一個。',
      '只有 wasm32 版本的映像，在 RISC-V / ARM 上由 wasmrt（wasm 直譯器）執行；在瀏覽器裡，每個任務本來就是一個 WebAssembly instance 加一個 Worker。',
    ],
    steps: [
      { cmd: 'image hello-wasm', note: '只有 wasm32' },
      { cmd: 'dock run hello-wasm', note: '照樣能跑' },
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
