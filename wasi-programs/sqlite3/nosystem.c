/* The SQLite shell (shell.c) calls system() for .system, .shell and
   .open --hexdb; WASI has no processes, so they fail. */
int system(const char *cmd) { (void)cmd; return -1; }
