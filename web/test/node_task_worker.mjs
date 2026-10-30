// Node stand-in for task-worker.js.
import { parentPort } from 'node:worker_threads';
import { runThread } from '../task_runtime.js';

parentPort.once('message', (m) => runThread(m, (x) => parentPort.postMessage(x)));
