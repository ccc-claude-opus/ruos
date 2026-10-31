# ruos

A capability microkernel teaching OS with Docker-like pods, written in Rust.
The OS runs in the browser (WebAssembly); this program is its backend on your desktop.

一個拿來教作業系統的能力式（capability）微核心，內建類 Docker 的 pod。打開瀏覽器就能操作：終端機、十堂課，以及即時的系統圖（每個任務持有哪些 handle、誰能跟誰說話、每個 pod 用了多少資源）。

## Install and run

```sh
cargo install ruos
ruos                 # starts the server and opens the OS in your browser
ruos --port 9000 --no-open
```

The binary carries everything: the wasm kernel, the web page and the image
registry. Data (the registry, the machines' disks,
the pods' writable layers) lives in `~/.ruos` (or `$RUOS_HOME`).

## Try

```text
/home $ ls -l
/home $ cat poem.txt | cut -d ' ' -f 2 | sort | uniq -c
/home $ for f in *.txt; do echo "$f: $(wc -l < $f) lines"; done
/home $ handles                       # this shell's capabilities
/home $ run snoop                     # a program given nothing can reach nothing
/home $ run snoop console store       # ... and exactly what it is given
/home $ dock run -d --mem 4M hog      # a pod with a 4 MiB quota
/home $ image hello                   # a content-addressed image
/home $ help
```

The source, the design notes and how to build the kernels are in the
project's repository.

## License

MIT
