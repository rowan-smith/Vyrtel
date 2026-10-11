// Stand-in for the vyrtel binary in release.test.mjs: answers `--version` and
// `serve --bind <addr>` like the real server, broken in the way FAKE_VYRTEL_MODE asks.
//
//   ok             built UI, matching version
//   placeholder    the page build.rs embeds when web/dist was missing
//   wrong-version  API reports FAKE_VYRTEL_SERVED_VERSION
//   no-asset       index.html references a script that 404s
//   crash          exits before it listens

import http from 'node:http';

const mode = process.env.FAKE_VYRTEL_MODE ?? 'ok';
const version = process.env.FAKE_VYRTEL_VERSION ?? '0.0.0';
const served = mode === 'wrong-version' ? (process.env.FAKE_VYRTEL_SERVED_VERSION ?? '9.9.9') : version;
const args = process.argv.slice(2);

if (args[0] === '--version') {
  console.log(`vyrtel ${process.env.FAKE_VYRTEL_PRINTED_VERSION ?? version}`);
  process.exit(0);
}
if (mode === 'crash') {
  console.error('vyrtel: configuration error: simulated');
  process.exit(2);
}

const ui = '<!doctype html><html><head><script type="module" crossorigin src="/assets/index-abc123.js"></script></head><body><div id="root"></div></body></html>';
const placeholder = '<!doctype html><html><body><h1>Vyrtel</h1><p>The web UI was not built into this binary. Run npm ci.</p></body></html>';

const [host, port] = args[args.indexOf('--bind') + 1].split(':');
http
  .createServer((req, res) => {
    const send = (status, type, body) => res.writeHead(status, { 'content-type': type }).end(body);
    if (req.url === '/health') return send(200, 'application/json', '{"status":"ok"}');
    if (req.url === '/api/v1/system/info') return send(200, 'application/json', JSON.stringify({ name: 'Vyrtel', version: served }));
    if (req.url === '/') return send(200, 'text/html', mode === 'placeholder' ? placeholder : ui);
    if (req.url === '/assets/index-abc123.js' && mode !== 'no-asset') return send(200, 'text/javascript', 'console.log(1)');
    send(404, 'text/plain', 'not found');
  })
  .listen(Number(port), host);
