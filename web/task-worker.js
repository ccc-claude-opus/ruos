// A task thread's Worker in the browser (see task_runtime.js). The page
// starts it and passes the MessagePort that leads to the kernel Worker.
import { runThread } from './task_runtime.js';

self.onmessage = (e) => {
  const port = e.data.port;
  port.onmessage = (m) => runThread(m.data, (x) => port.postMessage(x));
};
