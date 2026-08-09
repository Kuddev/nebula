// Probe side of scripts/win32_input_matrix.ps1 — runs as the PTY child inside
// Nebula, dumps every stdin chunk as hex. The driver injects a fixed key
// matrix via PostMessage and asserts the concatenated byte sequence against a
// recorded baseline. 'q' (0x71) terminates.
const fs = require('fs');
const out = process.argv[2] || 'D:\\temp_build\\nebula\\.tmp_win32_matrix.log';
const t0 = Date.now();
const log = (s) => fs.appendFileSync(out, `${((Date.now() - t0) / 1000).toFixed(1)}s ${s}\n`);
fs.writeFileSync(out, `matrix-probe ready node=${process.version} isTTY=${process.stdin.isTTY}\n`);
process.stdin.setRawMode(true);
process.stdin.resume();
process.stdin.on('data', (b) => {
  log(`rx ${b.toString('hex')}`);
  if (b.includes(0x71)) { log('end'); process.exit(0); }
});
setTimeout(() => { log('timeout'); process.exit(0); }, 90000);
