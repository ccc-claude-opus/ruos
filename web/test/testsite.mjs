// The small web site that curl in a machine fetches in the tests
// (tests/m21.expect). boot.mjs starts one for itself; test.sh starts one
// for the QEMU machines: `node web/test/testsite.mjs` prints
// `testsite: listening on PORT` and runs until killed.
import http from 'node:http';
import { fileURLToPath } from 'node:url';

export function testSite() {
  return http.createServer((req, res) => {
    let body = '';
    req.on('data', (d) => (body += d));
    req.on('end', () => {
      const send = (status, text, headers = {}) => {
        res.writeHead(status, { 'Content-Type': 'text/plain', ...headers });
        res.end(text);
      };
      if (req.url === '/hello') send(200, 'hello from the test web server\n');
      else if (req.url === '/echo') send(200, `got ${req.method} ${body}\n`);
      else if (req.url === '/big') send(200, Array.from({ length: 20000 }, (_, i) => `line ${i + 1}\n`).join(''));
      // A redirect to the server's own machine: refused like a direct request.
      else if (req.url === '/redirect') send(302, '', { Location: 'http://127.0.0.1:1/' });
      else send(404, 'not here\n');
    });
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const s = testSite();
  s.listen(0, '127.0.0.1', () => console.log(`testsite: listening on ${s.address().port}`));
}
